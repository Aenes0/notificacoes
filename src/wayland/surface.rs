//! Uma layer surface com a própria memória wl_shm (porte do `logi-osd/src/osd/wayland.rs`).
//!
//! Orçamento: a superfície, o `SlotPool` e o buffer só existem enquanto o cartão está na tela;
//! soltar o `Surface` destrói tudo e a memória volta.

use smithay_client_toolkit::reexports::client::protocol::wl_shm;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::LayerSurface;
use smithay_client_toolkit::shm::Shm;
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use tiny_skia::Pixmap;

use crate::render::card::Rect;

/// Recursos de uma superfície. Ordem de queda: superfície, buffer, pool.
pub struct Surface {
    pub layer: LayerSurface,
    buffer: Option<Buffer>,
    pool: Option<SlotPool>,
    /// Primeiro `configure` já veio: pode anexar buffer.
    configured: bool,
    /// Desenho à espera do primeiro `configure`, com a escala em que foi feito.
    pending: Option<(Pixmap, u32)>,
    /// Tamanho lógico pedido com `set_size`.
    size: (u32, u32),
    /// Escala preferida pelo compositor para esta superfície; 0 = nenhuma ainda.
    pub preferred_scale: u32,
}

impl Surface {
    pub fn new(layer: LayerSurface, size: (u32, u32)) -> Surface {
        Surface { layer, buffer: None, pool: None, configured: false, pending: None, size, preferred_scale: 0 }
    }

    /// O primeiro `configure` já veio (os quadros saem direto).
    pub fn is_configured(&self) -> bool {
        self.configured
    }

    /// Anexa já, ou guarda uma cópia para o primeiro `configure` (só o quadro mais recente vale).
    /// `damage` em px lógicos; `None` = tudo.
    pub fn present(&mut self, shm: &Shm, pm: &Pixmap, scale: u32, damage: Option<Rect>) {
        if self.configured {
            self.draw(shm, pm, scale, damage);
            return;
        }
        let size = logical_size(pm, scale);
        if size != self.size {
            // Ainda sem configure: só atualiza o pedido; o desenho sai no configure.
            self.layer.set_size(size.0, size.1);
            self.size = size;
            self.layer.commit();
        }
        self.pending = Some((pm.clone(), scale));
    }

    /// Primeiro `configure`: libera o desenho guardado.
    pub fn on_configure(&mut self, shm: &Shm) {
        self.configured = true;
        if let Some((pm, scale)) = self.pending.take() {
            self.draw(shm, &pm, scale, None);
        }
    }

    /// Escreve no buffer anterior se o compositor já o soltou; senão, num novo do pool.
    fn draw(&mut self, shm: &Shm, pm: &Pixmap, scale: u32, damage: Option<Rect>) {
        let (Ok(w), Ok(h)) = (i32::try_from(pm.width()), i32::try_from(pm.height())) else { return };
        let stride = w * 4;
        let len = pm.data().len();
        let pool = match &mut self.pool {
            Some(pool) => pool,
            None => match SlotPool::new(len, shm) {
                Ok(pool) => self.pool.insert(pool),
                Err(e) => {
                    eprintln!("notificacoes: pool wl_shm não criado: {e}");
                    return;
                }
            },
        };
        let reuse = self
            .buffer
            .as_ref()
            .is_some_and(|b| b.height() == h && b.stride() == stride && b.canvas(pool).is_some());
        // Buffer novo não tem o quadro anterior: o dano é a superfície toda.
        let damage = if reuse { damage } else { None };
        if !reuse {
            match pool.create_buffer(w, h, stride, wl_shm::Format::Argb8888) {
                // O buffer anterior cai aqui: se o compositor ainda o usa, o sctk o destrói no release.
                Ok((buffer, _)) => self.buffer = Some(buffer),
                Err(e) => {
                    eprintln!("notificacoes: buffer wl_shm não criado: {e}");
                    return;
                }
            }
        }
        let Some(buffer) = self.buffer.as_ref() else { return };
        let Some(canvas) = buffer.canvas(pool) else { return };
        to_argb8888(pm.data(), canvas);

        let size = logical_size(pm, scale);
        let layer = &self.layer;
        if self.size != size {
            layer.set_size(size.0, size.1);
            self.size = size;
        }
        if layer.set_buffer_scale(scale).is_err() && scale != 1 {
            eprintln!("notificacoes: wl_surface < v3 não aceita escala {scale}");
        }
        let (dx, dy, dw, dh) = match damage {
            Some(r) => device_rect(r, scale),
            None => (0, 0, w, h),
        };
        layer.wl_surface().damage_buffer(dx, dy, dw, dh);
        if let Err(e) = buffer.attach_to(layer.wl_surface()) {
            eprintln!("notificacoes: buffer não anexado: {e}");
            return;
        }
        layer.commit();
    }
}

fn logical_size(pm: &Pixmap, scale: u32) -> (u32, u32) {
    let s = scale.max(1);
    (pm.width() / s, pm.height() / s)
}

/// Retângulo lógico → px de dispositivo, arredondado para fora (1 px de folga pelo antisserrilhado).
fn device_rect(r: Rect, scale: u32) -> (i32, i32, i32, i32) {
    let s = scale.max(1) as f32;
    let x0 = ((r.x * s).floor() - 1.0).max(0.0);
    let y0 = ((r.y * s).floor() - 1.0).max(0.0);
    let x1 = ((r.x + r.w) * s).ceil() + 1.0;
    let y1 = ((r.y + r.h) * s).ceil() + 1.0;
    (x0 as i32, y0 as i32, (x1 - x0) as i32, (y1 - y0) as i32)
}

/// RGBA pré-multiplicado (tiny-skia) → Argb8888 little-endian (B, G, R, A na memória).
pub fn to_argb8888(rgba_premul: &[u8], out: &mut [u8]) {
    let (src, _) = rgba_premul.as_chunks::<4>();
    let (dst, _) = out.as_chunks_mut::<4>();
    for (s, d) in src.iter().zip(dst) {
        *d = [s[2], s[1], s[0], s[3]];
    }
}

/// Cópia com largura e altura múltiplas da escala (o protocolo exige; a sobra fica transparente).
/// `None` = já serve como está.
pub fn pad_to_scale(pm: &Pixmap, scale: u32) -> Option<Pixmap> {
    let s = scale.max(1);
    let (w, h) = (pm.width().div_ceil(s) * s, pm.height().div_ceil(s) * s);
    if (w, h) == (pm.width(), pm.height()) {
        return None;
    }
    let mut out = Pixmap::new(w, h)?;
    out.draw_pixmap(0, 0, pm.as_ref(), &tiny_skia::PixmapPaint::default(), tiny_skia::Transform::identity(), None);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argb_swaps_red_and_blue() {
        let mut o = [0u8; 8];
        to_argb8888(&[1, 2, 3, 4, 5, 6, 7, 8], &mut o);
        assert_eq!(o, [3, 2, 1, 4, 7, 6, 5, 8]);
    }

    #[test]
    fn pad_to_scale_rounds_up_only_when_needed() {
        let padded = pad_to_scale(&Pixmap::new(5, 4).unwrap(), 2).unwrap();
        assert_eq!((padded.width(), padded.height()), (6, 4));
        assert!(pad_to_scale(&Pixmap::new(6, 4).unwrap(), 2).is_none());
    }

    #[test]
    fn device_rect_rounds_outward_and_clamps() {
        let r = Rect { x: 30.0, y: 100.5, w: 320.0, h: 3.0 };
        assert_eq!(device_rect(r, 2), (59, 200, 642, 8));
        assert_eq!(device_rect(Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 }, 1), (0, 0, 2, 2));
    }
}

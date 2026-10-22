//! Formas do cartão em px de dispositivo: retângulo arredondado, sombra gaussiana, pílula e ×.

use tiny_skia::{
    FillRule, LineCap, Paint, Path, PathBuilder, Pixmap, Stroke, Transform,
};

use super::card::Rect;
use super::theme::Rgba;

pub fn skia_color(c: Rgba) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c.0, c.1, c.2, c.3)
}

fn paint(c: Rgba) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(skia_color(c));
    p.anti_alias = true;
    p
}

/// Retângulo arredondado (raio limitado à metade do menor lado). `None` se degenerado.
pub fn rounded_rect_path(r: Rect, radius: f32) -> Option<Path> {
    if r.w <= 0.0 || r.h <= 0.0 {
        return None;
    }
    let rad = radius.clamp(0.0, r.w.min(r.h) / 2.0);
    let k = rad * 0.552_284_75;
    let (x0, y0, x1, y1) = (r.x, r.y, r.x + r.w, r.y + r.h);
    let mut pb = PathBuilder::new();
    pb.move_to(x0 + rad, y0);
    pb.line_to(x1 - rad, y0);
    pb.cubic_to(x1 - rad + k, y0, x1, y0 + rad - k, x1, y0 + rad);
    pb.line_to(x1, y1 - rad);
    pb.cubic_to(x1, y1 - rad + k, x1 - rad + k, y1, x1 - rad, y1);
    pb.line_to(x0 + rad, y1);
    pb.cubic_to(x0 + rad - k, y1, x0, y1 - rad + k, x0, y1 - rad);
    pb.line_to(x0, y0 + rad);
    pb.cubic_to(x0, y0 + rad - k, x0 + rad - k, y0, x0 + rad, y0);
    pb.close();
    pb.finish()
}

pub fn fill_rounded_rect(pm: &mut Pixmap, r: Rect, radius: f32, color: Rgba) {
    if let Some(path) = rounded_rect_path(r, radius) {
        pm.fill_path(&path, &paint(color), FillRule::Winding, Transform::identity(), None);
    }
}

/// Contorno de 1 lado de `width` centrado em `r` (o chamador desloca meio traço para dentro).
pub fn stroke_rounded_rect(pm: &mut Pixmap, r: Rect, radius: f32, width: f32, color: Rgba) {
    if let Some(path) = rounded_rect_path(r, radius) {
        let stroke = Stroke { width, ..Stroke::default() };
        pm.stroke_path(&path, &paint(color), &stroke, Transform::identity(), None);
    }
}

/// Pílula: raio = metade da altura.
pub fn fill_pill(pm: &mut Pixmap, r: Rect, color: Rgba) {
    fill_rounded_rect(pm, r, r.h / 2.0, color);
}

/// × centrado em (`cx`, `cy`), meio-lado `half`, traço `width`, pontas redondas.
pub fn draw_cross(pm: &mut Pixmap, cx: f32, cy: f32, half: f32, width: f32, color: Rgba) {
    let mut pb = PathBuilder::new();
    pb.move_to(cx - half, cy - half);
    pb.line_to(cx + half, cy + half);
    pb.move_to(cx + half, cy - half);
    pb.line_to(cx - half, cy + half);
    let Some(path) = pb.finish() else { return };
    let stroke = Stroke { width, line_cap: LineCap::Round, ..Stroke::default() };
    pm.stroke_path(&path, &paint(color), &stroke, Transform::identity(), None);
}

/// Janelas (`lo`, `hi`) das 3 passadas de box blur que aproximam um gaussiano de `sigma`
/// (receita W3C/SVG `feGaussianBlur`).
fn box_windows(sigma: f32) -> [(usize, usize); 3] {
    let d = ((sigma * 3.0 * (2.0 * std::f32::consts::PI).sqrt() / 4.0) + 0.5).floor().max(1.0) as usize;
    if d % 2 == 1 {
        let r = d / 2;
        [(r, r); 3]
    } else {
        [(d / 2, d / 2 - 1), (d / 2 - 1, d / 2), (d / 2, d / 2)]
    }
}

/// Média móvel de janela `[i-lo, i+hi]`, zeros fora da linha.
fn blur_line(line: &[f32], out: &mut [f32], lo: usize, hi: usize) {
    let n = line.len();
    let k = (lo + hi + 1) as f32;
    let mut sum: f32 = line.iter().take(hi + 1).sum();
    for i in 0..n {
        out[i] = sum / k;
        if let Some(add) = line.get(i + hi + 1) {
            sum += add;
        }
        if i >= lo {
            sum -= line[i - lo];
        }
    }
}

fn blur_mask(mask: &mut [f32], w: usize, h: usize, sigma: f32) {
    if w == 0 || h == 0 || mask.len() != w * h {
        return;
    }
    let mut line = vec![0.0f32; w.max(h)];
    let mut out = vec![0.0f32; w.max(h)];
    for (lo, hi) in box_windows(sigma) {
        for y in 0..h {
            blur_line(&mask[y * w..(y + 1) * w], &mut out[..w], lo, hi);
            mask[y * w..(y + 1) * w].copy_from_slice(&out[..w]);
        }
        for x in 0..w {
            for y in 0..h {
                line[y] = mask[y * w + x];
            }
            blur_line(&line[..h], &mut out[..h], lo, hi);
            for y in 0..h {
                mask[y * w + x] = out[y];
            }
        }
    }
}

fn alpha_mask(w: u32, h: u32, r: Rect, radius: f32) -> Option<Vec<f32>> {
    let mut m = Pixmap::new(w, h)?;
    fill_rounded_rect(&mut m, r, radius, Rgba(0, 0, 0, 255));
    Some(m.pixels().iter().map(|p| f32::from(p.alpha()) / 255.0).collect())
}

/// Sombra do retângulo arredondado `card` (device px): máscara borrada (σ), deslocada `dy`
/// para baixo, preta com `alpha`, e **recortada por dentro do cartão** (como o `box-shadow` do CSS,
/// que não aparece sob a caixa). Desenhada antes do cartão.
pub fn draw_shadow(pm: &mut Pixmap, card: Rect, radius: f32, sigma: f32, dy: f32, alpha: f32) {
    let (w, h) = (pm.width(), pm.height());
    let shifted = Rect { y: card.y + dy, ..card };
    let (Some(mut mask), Some(inside)) = (alpha_mask(w, h, shifted, radius), alpha_mask(w, h, card, radius)) else {
        return;
    };
    blur_mask(&mut mask, w as usize, h as usize, sigma);
    for ((px, m), inside) in pm.pixels_mut().iter_mut().zip(mask).zip(inside) {
        let a = (m * (1.0 - inside) * alpha).clamp(0.0, 1.0);
        if a <= 0.0 {
            continue;
        }
        // Preto sobre o destino pré-multiplicado: cor *= (1 - a); alfa = a + alfa*(1 - a).
        let keep = 1.0 - a;
        let ch = |v: u8| (f32::from(v) * keep).round() as u8;
        let na = ((a + f32::from(px.alpha()) / 255.0 * keep) * 255.0).round() as u8;
        if let Some(c) = tiny_skia::PremultipliedColorU8::from_rgba(
            ch(px.red()),
            ch(px.green()),
            ch(px.blue()),
            na,
        ) {
            *px = c;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_is_black_below_card_and_clear_inside() {
        let mut pm = Pixmap::new(200, 200).unwrap();
        let card = Rect { x: 50.0, y: 40.0, w: 100.0, h: 80.0 };
        draw_shadow(&mut pm, card, 16.0, 8.0, 6.0, 0.45);
        let inside = pm.pixel(100, 80).unwrap();
        assert_eq!(inside.alpha(), 0, "dentro do cartão não há sombra");
        let below = pm.pixel(100, 125).unwrap();
        assert!(below.alpha() > 10 && below.alpha() <= 115, "alfa abaixo = {}", below.alpha());
        assert_eq!((below.red(), below.green(), below.blue()), (0, 0, 0));
    }

    #[test]
    fn blur_keeps_total_mass_in_the_middle() {
        let mut m = vec![0.0f32; 21 * 21];
        m[10 * 21 + 10] = 1.0;
        blur_mask(&mut m, 21, 21, 1.0);
        let sum: f32 = m.iter().sum();
        assert!((sum - 1.0).abs() < 1e-3, "massa = {sum}");
    }
}

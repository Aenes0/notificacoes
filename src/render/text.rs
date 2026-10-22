//! Texto com `cosmic-text`: fallback de fonte, emoji colorido, RTL/CJK, quebra e reticências.

use std::path::Path;

use cosmic_text::{
    Attrs, Buffer, Color, Ellipsize, EllipsizeHeightLimit, Family, FontSystem, Metrics, Shaping,
    SwashCache, Weight, Wrap,
};
use tiny_skia::Pixmap;

use super::theme::{Rgba, latest_version_dir, top_level_field};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextStyle { pub px: f32, pub weight: u16, pub line_height: f32 }

pub const TITLE: TextStyle = TextStyle { px: 13.0, weight: 600, line_height: 1.3 };
pub const APP: TextStyle = TextStyle { px: 11.0, weight: 400, line_height: 1.3 };
pub const BODY: TextStyle = TextStyle { px: 12.0, weight: 400, line_height: 1.35 };
pub const BUTTON: TextStyle = TextStyle { px: 13.0, weight: 600, line_height: 1.2 };

const DEFAULT_FAMILY: &str = "Open Sans";

pub struct Text {
    fonts: FontSystem,
    family: String,
    cache: Option<SwashCache>,
}

/// Texto já composto (em px de dispositivo); `width`/`height` em px lógicos.
pub struct Laid {
    pub width: f32,
    pub height: f32,
    pub lines: usize,
    buffer: Option<Buffer>,
}

impl Text {
    /// `family`: já resolvida (`cfg.font` → `interface_font_family`); `None` vira "Open Sans".
    pub fn new(family: Option<&str>) -> Text {
        let family = family.filter(|f| !f.trim().is_empty()).unwrap_or(DEFAULT_FAMILY);
        Text { fonts: FontSystem::new(), family: family.to_string(), cache: None }
    }

    /// `max_width` e o resultado em px lógicos; layout em px de dispositivo (px * scale).
    pub fn layout(&mut self, s: &str, style: TextStyle, max_width: f32, max_lines: usize, scale: u32) -> Laid {
        let empty = Laid { width: 0.0, height: 0.0, lines: 0, buffer: None };
        if s.is_empty() || max_lines == 0 {
            return empty;
        }
        let scale_f = scale.max(1) as f32;
        // `Buffer::new` entra em pânico com altura de linha 0; piso de 1 px de dispositivo.
        let font_px = if style.px.is_finite() { style.px.max(1.0) } else { 1.0 };
        let line_px = (font_px * style.line_height).max(1.0);
        let line_px = if line_px.is_finite() { line_px } else { font_px };
        let metrics = Metrics::new(font_px * scale_f, line_px * scale_f);
        let mut buffer = Buffer::new(&mut self.fonts, metrics);
        buffer.set_wrap(Wrap::WordOrGlyph);
        buffer.set_ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(max_lines)));
        let attrs = Attrs::new().family(Family::Name(&self.family)).weight(Weight(style.weight));
        {
            let mut b = buffer.borrow_with(&mut self.fonts);
            b.set_size(Some(max_width.max(1.0) * scale_f), None);
            b.set_text(s, &attrs, Shaping::Advanced, None);
            b.shape_until_scroll(false);
        }
        let (mut lines, mut width) = (0usize, 0.0f32);
        for run in buffer.layout_runs() {
            lines += 1;
            width = width.max(run.line_w - lead_in(&run).1);
        }
        if lines == 0 {
            return empty;
        }
        Laid {
            width: width / scale_f,
            height: lines as f32 * metrics.line_height / scale_f,
            lines,
            buffer: Some(buffer),
        }
    }

    /// `x`, `y` lógicos (canto superior esquerdo do bloco).
    pub fn draw(&mut self, pm: &mut Pixmap, laid: &Laid, x: f32, y: f32, scale: u32, color: Rgba) {
        let Some(buffer) = laid.buffer.as_ref() else { return };
        let scale_f = scale.max(1) as f32;
        let cache = self.cache.get_or_insert_with(SwashCache::new);
        let base = Color::rgba(color.0, color.1, color.2, 255);
        let opacity = u32::from(color.3);
        let (pw, ph) = (pm.width() as i32, pm.height() as i32);
        let stride = pm.width() as usize;
        let origin = (x * scale_f, y * scale_f);
        for run in buffer.layout_runs() {
            let (skip, shift) = lead_in(&run);
            for glyph in run.glyphs.iter().skip(skip) {
                let physical = glyph.physical((origin.0 - shift, origin.1), 1.0);
                let (gx, gy) = (physical.x, physical.y + run.line_y.round() as i32);
                cache.with_pixels(&mut self.fonts, physical.cache_key, base, |px, py, c| {
                    let (dx, dy) = (gx + px, gy + py);
                    if dx < 0 || dy < 0 || dx >= pw || dy >= ph {
                        return;
                    }
                    let a = u32::from(c.a()) * opacity / 255;
                    if a == 0 {
                        return;
                    }
                    let idx = (dy as usize * stride + dx as usize) * 4;
                    let Some(dst) = pm.data_mut().get_mut(idx..idx + 4) else { return };
                    // source-over, destino pré-multiplicado.
                    let inv = 255 - a;
                    let src = [c.r(), c.g(), c.b()];
                    for (d, s) in dst.iter_mut().zip(src) {
                        let premul = u32::from(s) * a / 255;
                        *d = (premul + u32::from(*d) * inv / 255).min(255) as u8;
                    }
                    dst[3] = (a + u32::from(dst[3]) * inv / 255).min(255) as u8;
                });
            }
        }
    }

    /// Solta o `SwashCache` (rasterizações em cache); chamado quando a tela esvazia.
    pub fn release_caches(&mut self) {
        self.cache = None;
    }
}

/// Em linha quebrada (continuação), o espaço da quebra fica no início da linha: devolve
/// quantos glifos pular e o deslocamento em x que alinha a tinta à borda esquerda.
fn lead_in(run: &cosmic_text::LayoutRun) -> (usize, f32) {
    let continuation = run.glyphs.first().is_some_and(|g| g.start > 0);
    if run.rtl || !continuation {
        return (0, 0.0);
    }
    let is_space = |g: &cosmic_text::LayoutGlyph| {
        run.text.get(g.start..g.end).is_some_and(|t| !t.is_empty() && t.chars().all(char::is_whitespace))
    };
    let skip = run.glyphs.iter().take_while(|g| is_space(g)).count();
    match run.glyphs.get(skip) {
        Some(g) if skip > 0 => (skip, g.x),
        _ => (0, 0.0),
    }
}

/// Família de `~/.config/cosmic/com.system76.CosmicTk/v*/interface_font` (campo `family`).
pub fn interface_font_family(cosmic_dir: &Path) -> Option<String> {
    let dir = latest_version_dir(&cosmic_dir.join("com.system76.CosmicTk"))?;
    let text = std::fs::read_to_string(dir.join("interface_font")).ok()?;
    let family = top_level_field(&text, "family")?.trim();
    (!family.is_empty()).then(|| family.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn text() -> Text { Text::new(None) }

    /// Pixmap transparente com o texto desenhado em branco.
    fn render(t: &mut Text, s: &str, style: TextStyle, max_w: f32, lines: usize, scale: u32) -> (Laid, Pixmap) {
        let laid = t.layout(s, style, max_w, lines, scale);
        let w = (max_w * scale as f32).ceil() as u32 + 8;
        let h = ((laid.height + 8.0) * scale as f32).ceil() as u32;
        let mut pm = Pixmap::new(w, h.max(1)).expect("pixmap");
        t.draw(&mut pm, &laid, 0.0, 0.0, scale, Rgba(255, 255, 255, 255));
        (laid, pm)
    }

    fn alpha(pm: &Pixmap, x: u32, y: u32) -> u8 { pm.data()[((y * pm.width() + x) * 4 + 3) as usize] }

    fn ink_rows(pm: &Pixmap) -> Vec<bool> {
        (0..pm.height()).map(|y| (0..pm.width()).any(|x| alpha(pm, x, y) > 128)).collect()
    }

    fn bands(rows: &[bool]) -> usize {
        rows.windows(2).filter(|w| !w[0] && w[1]).count() + usize::from(rows.first() == Some(&true))
    }

    fn ink_width(pm: &Pixmap) -> u32 {
        let xs: Vec<u32> = (0..pm.width()).filter(|&x| (0..pm.height()).any(|y| alpha(pm, x, y) > 128)).collect();
        match (xs.first(), xs.last()) { (Some(a), Some(b)) => b - a + 1, _ => 0 }
    }

    fn count_ink(pm: &Pixmap) -> usize {
        pm.data().chunks_exact(4).filter(|p| p[3] > 128).count()
    }

    #[test]
    fn title_one_line_ellipsized_within_width() {
        let laid = text().layout(&"palavra ".repeat(25), TITLE, 280.0, 1, 1);
        assert_eq!(laid.lines, 1);
        assert!(laid.width <= 280.0, "largura {}", laid.width);
        assert!(laid.width > 200.0, "deveria preencher a linha: {}", laid.width);
    }

    #[test]
    fn body_three_lines_then_ellipsis() {
        let s = "lorem ipsum dolor sit amet ".repeat(23);
        let mut t = text();
        let (laid, pm) = render(&mut t, &s, BODY, 300.0, 3, 1);
        assert_eq!(laid.lines, 3);
        let rows = ink_rows(&pm);
        assert_eq!(bands(&rows), 3, "faixas de tinta");
        // Última linha: tinta junto à borda direita (as reticências fecham a linha cheia).
        let last_row_start = (2.0 * BODY.px * BODY.line_height) as u32;
        let right = (0..pm.width())
            .filter(|&x| (last_row_start..pm.height()).any(|y| alpha(&pm, x, y) > 128))
            .max()
            .unwrap_or(0);
        assert!(right as f32 > 300.0 - 40.0, "última linha termina em {right}");
    }

    #[test]
    fn short_text_is_one_line_and_narrow() {
        let laid = text().layout("Oi", BODY, 300.0, 3, 1);
        assert_eq!(laid.lines, 1);
        assert!(laid.width < 40.0, "largura {}", laid.width);
    }

    #[test]
    fn empty_text_has_zero_height() {
        let laid = text().layout("", BODY, 300.0, 3, 1);
        assert_eq!((laid.lines, laid.height, laid.width), (0, 0.0, 0.0));
    }

    #[test]
    fn emoji_renders_color_pixels() {
        let mut t = text();
        let style = TextStyle { px: 24.0, weight: 400, line_height: 1.3 };
        let (_, pm) = render(&mut t, "🙂", style, 100.0, 1, 1);
        let colorful = pm.data().chunks_exact(4).any(|p| {
            // pré-multiplicado: normaliza antes de medir o espalhamento.
            let a = u32::from(p[3]);
            a > 200 && {
                let (r, g, b) = (u32::from(p[0]), u32::from(p[1]), u32::from(p[2]));
                r.max(g).max(b) - r.min(g).min(b) > 60
            }
        });
        assert!(colorful, "nenhum pixel colorido");
    }

    #[test]
    fn cjk_and_arabic_render_ink() {
        let mut t = text();
        for s in ["你好", "مرحبا"] {
            let (_, pm) = render(&mut t, s, TITLE, 100.0, 1, 1);
            assert!(count_ink(&pm) > 30, "{s}: pouca tinta");
        }
    }

    #[test]
    fn scale_two_doubles_ink_width() {
        let mut t = text();
        let (_, one) = render(&mut t, "Notificação de teste", TITLE, 300.0, 1, 1);
        let (_, two) = render(&mut t, "Notificação de teste", TITLE, 300.0, 1, 2);
        let (w1, w2) = (ink_width(&one) as i32, ink_width(&two) as i32);
        assert!((w2 - 2 * w1).abs() <= 2, "{w1} x {w2}");
    }

    #[test]
    fn interface_font_family_from_ron() {
        let dir = std::env::temp_dir().join(format!("notificacoes-test-font-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(interface_font_family(&dir), None);
        let v1 = dir.join("com.system76.CosmicTk").join("v1");
        fs::create_dir_all(&v1).expect("mkdir");
        assert_eq!(interface_font_family(&dir), None);
        fs::write(v1.join("interface_font"), include_str!("../../tests/fixtures/theme/cosmictk_interface_font"))
            .expect("write");
        assert_eq!(interface_font_family(&dir), Some("Inter".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    fn first_ink_x(pm: &Pixmap, y0: u32, y1: u32) -> Option<u32> {
        (0..pm.width()).find(|&x| (y0..y1.min(pm.height())).any(|y| alpha(pm, x, y) > 128))
    }

    #[test]
    fn wrapped_lines_start_at_same_column() {
        let s = "um dois tres quatro cinco seis sete oito nove dez onze doze treze quatorze quinze dezesseis";
        let mut t = text();
        let (laid, pm) = render(&mut t, s, BODY, 120.0, 3, 1);
        assert_eq!(laid.lines, 3);
        let lh = (BODY.px * BODY.line_height).round() as u32;
        let cols: Vec<u32> = (0..3).filter_map(|i| first_ink_x(&pm, i * lh, (i + 1) * lh)).collect();
        assert_eq!(cols.len(), 3);
        for c in &cols {
            assert!((*c as i32 - cols[0] as i32).abs() <= 1, "colunas {cols:?}");
        }
    }

    #[test]
    fn degenerate_style_does_not_panic() {
        let mut t = text();
        let a = t.layout("oi", TextStyle { px: 0.0, weight: 400, line_height: 1.3 }, 100.0, 1, 1);
        assert_eq!(a.lines, 1);
        let b = t.layout("oi", TextStyle { px: 12.0, weight: 400, line_height: 0.0 }, 100.0, 1, 1);
        assert_eq!(b.lines, 1);
    }

    #[test]
    fn long_token_without_spaces_is_ellipsized() {
        let mut t = text();
        let a = t.layout(&"a".repeat(200), TITLE, 280.0, 1, 1);
        assert_eq!(a.lines, 1);
        assert!(a.width <= 280.0, "largura {}", a.width);
        let b = t.layout(&"a".repeat(200), BODY, 280.0, 3, 1);
        assert!(b.lines <= 3, "linhas {}", b.lines);
    }
}

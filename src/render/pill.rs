//! Pílula OSD (porte de `logi-osd/src/osd/render.rs`): glifo ou ícone do tema, rótulo e um valor
//! (texto ou bolinhas), num fundo em pílula com borda e sombra iguais às do cartão.
//! Sem barra de tempo, sem botões, sem entrada: só desenho. O texto usa o `Text` do daemon.

use std::borrow::Cow;

use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, PixmapPaint, Transform};

use super::card::{Palette, Rect, SHADOW_PAD, palette};
use super::glyphs::{self, Glyph, glyph_for};
use super::shapes;
use super::text::{Laid, Text, TextStyle};
use super::theme::{CRITICAL, Rgba, Theme, WARNING};
use crate::model::{Level, Notification, Tom};

pub const HEIGHT: f32 = 38.0;
pub const MIN_W: f32 = 260.0;
pub const MAX_W: f32 = 420.0;
pub const PAD_X: f32 = 18.0;
pub const GAP: f32 = 12.0;
pub const ICON: f32 = 18.0;
pub const DOT: f32 = 9.0;
pub const DOT_GAP: f32 = 6.0;
pub const MARGIN_BOTTOM: i32 = 24;

/// Rótulo e valor: 13 px peso 600 (como a fonte SemiBold do logi-osd).
const STYLE: TextStyle = TextStyle { px: 13.0, weight: 600, line_height: 1.3 };
/// Teto do valor em texto, para um corpo enorme não comer o rótulo.
const VALUE_MAX_W: f32 = 180.0;
/// Largura de sondagem para medir o rótulo inteiro, sem truncar.
const LABEL_PROBE_W: f32 = 4000.0;
const DIM_ALPHA: f32 = 0.80;
const UNLIT_ALPHA: f32 = 0.18;
const SHADOW_SIGMA: f32 = 14.0;
const SHADOW_DY: f32 = 8.0;
const SHADOW_ALPHA: f32 = 0.45;

pub enum PillIcon<'a> { Drawn(Glyph), Themed(&'a Pixmap), None }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PillValue<'a> { None, Text(Cow<'a, str>), Dots { on: u8, total: u8 } }

pub struct PillView<'a> {
    pub icon: PillIcon<'a>,
    pub label: &'a str,
    pub value: PillValue<'a>,
    pub tom: Option<Tom>,
    pub level: Level,
}

/// Valor: pontos > corpo não vazio > `value`% > nada. Ícone: glifo desenhado (`logi-*`) >
/// ícone do tema já rasterizado (`themed`) > nenhum.
pub fn pill_view<'a>(n: &'a Notification, themed: Option<&'a Pixmap>) -> PillView<'a> {
    let value = if let Some((on, total)) = n.hints.pontos {
        PillValue::Dots { on, total }
    } else if !n.body.is_empty() {
        PillValue::Text(Cow::Borrowed(n.body.as_str()))
    } else if let Some(v) = n.hints.value {
        PillValue::Text(Cow::Owned(format!("{v}%")))
    } else {
        PillValue::None
    };
    let icon = match (glyph_for(&n.app_icon), themed) {
        (Some(g), _) => PillIcon::Drawn(g),
        (None, Some(pm)) => PillIcon::Themed(pm),
        (None, None) => PillIcon::None,
    };
    PillView { icon, label: &n.summary, value, tom: n.hints.tom, level: n.level() }
}

fn accent(theme: &Theme) -> Rgba {
    Rgba(theme.accent.0, theme.accent.1, theme.accent.2, 0xFF)
}

fn with_alpha(c: Rgba, factor: f32) -> Rgba {
    Rgba(c.0, c.1, c.2, (f32::from(c.3) * factor).round() as u8)
}

/// Cor do ícone: a do nível se `x-tom` for atenção/crítico, senão o destaque do tema.
pub fn icon_color(tom: Option<Tom>, theme: &Theme) -> Rgba {
    match tom {
        Some(Tom::Atencao) => WARNING,
        Some(Tom::Critico) => CRITICAL,
        _ => accent(theme),
    }
}

/// Cor do valor em texto: destaque (normal/ausente), texto a 80% (apagado), laranja ou vermelho.
fn value_color(tom: Option<Tom>, pal: &Palette, theme: &Theme) -> Rgba {
    match tom {
        Some(Tom::Apagado) => with_alpha(pal.text, DIM_ALPHA),
        other => icon_color(other, theme),
    }
}

fn icon_width(icon: &PillIcon) -> f32 {
    match icon {
        PillIcon::Drawn(g) => glyphs::width(*g),
        PillIcon::Themed(_) => ICON,
        PillIcon::None => 0.0,
    }
}

fn dots_width(total: u8) -> f32 {
    if total == 0 { 0.0 } else { f32::from(total) * DOT + f32::from(total - 1) * DOT_GAP }
}

/// Medidas lógicas e textos já compostos.
struct Geo {
    /// Largura do cartão (sem o enchimento da sombra).
    width: f32,
    label: Laid,
    value: Laid,
    dots_w: f32,
}

fn compose(text: &mut Text, v: &PillView, scale: u32) -> Geo {
    let (value, dots_w) = match &v.value {
        PillValue::None => (text.layout("", STYLE, 1.0, 1, scale), 0.0),
        PillValue::Text(s) => (text.layout(s, STYLE, VALUE_MAX_W, 1, scale), 0.0),
        PillValue::Dots { total, .. } => (text.layout("", STYLE, 1.0, 1, scale), dots_width(*total)),
    };
    let value_w = value.width.max(dots_w);
    let icon_w = icon_width(&v.icon);
    let fixed = PAD_X * 2.0 + if icon_w > 0.0 { icon_w + GAP } else { 0.0 } + if value_w > 0.0 { GAP + value_w } else { 0.0 };
    let max_label = (MAX_W - fixed).max(1.0);
    // Cabe inteiro: a pílula tem a largura dele (no mínimo MIN_W). Senão, trunca com reticências
    // e a pílula fica na largura máxima.
    let natural = text.layout(v.label, STYLE, LABEL_PROBE_W, 1, scale);
    let (label, width) = if natural.width <= max_label {
        let width = (fixed + natural.width).ceil().clamp(MIN_W, MAX_W);
        (natural, width)
    } else {
        (text.layout(v.label, STYLE, max_label, 1, scale), MAX_W)
    };
    Geo { width, label, value, dots_w }
}

fn surface_size(width: f32, scale: u32) -> (u32, u32) {
    let s = scale.max(1) as f32;
    (
        ((width + SHADOW_PAD.l + SHADOW_PAD.r) * s).ceil() as u32,
        ((HEIGHT + SHADOW_PAD.t + SHADOW_PAD.b) * s).ceil() as u32,
    )
}

/// Tamanho da superfície em px de dispositivo, com o enchimento da sombra.
pub fn measure(text: &mut Text, v: &PillView, scale: u32) -> (u32, u32) {
    surface_size(compose(text, v, scale).width, scale)
}

fn fill_circle(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, color: Rgba) {
    let mut pb = PathBuilder::new();
    pb.push_circle(cx, cy, r);
    let Some(path) = pb.finish() else { return };
    let mut paint = Paint::default();
    paint.set_color(shapes::skia_color(color));
    paint.anti_alias = true;
    pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
}

pub fn render(text: &mut Text, v: &PillView, theme: &Theme, scale: u32) -> Pixmap {
    let g = compose(text, v, scale);
    let s = scale.max(1);
    let sf = s as f32;
    let (sw, sh) = surface_size(g.width, s);
    let mut pm = Pixmap::new(sw.max(1), sh.max(1)).or_else(|| Pixmap::new(1, 1)).expect("pixmap 1x1 sempre existe");
    let pal = palette(theme, v.level);

    // Coordenadas lógicas com origem no canto do cartão dentro da superfície.
    let (ox, oy) = (SHADOW_PAD.l, SHADOW_PAD.t);
    let card = Rect { x: ox * sf, y: oy * sf, w: g.width * sf, h: HEIGHT * sf };
    let radius = HEIGHT * sf / 2.0;
    shapes::draw_shadow(&mut pm, card, radius, SHADOW_SIGMA * sf, SHADOW_DY * sf, SHADOW_ALPHA);
    shapes::fill_pill(&mut pm, card, pal.card_bg);
    let half = sf * 0.5;
    let inner = Rect { x: card.x + half, y: card.y + half, w: card.w - sf, h: card.h - sf };
    shapes::stroke_rounded_rect(&mut pm, inner, radius - half, sf, pal.border);

    let icon_w = icon_width(&v.icon);
    let icon_x = ox + PAD_X;
    match &v.icon {
        PillIcon::Drawn(glyph) => {
            let y = oy + (HEIGHT - glyphs::BOX_H) / 2.0;
            glyphs::draw(&mut pm, *glyph, icon_x * sf, y * sf, sf, icon_color(v.tom, theme));
        }
        PillIcon::Themed(src) if src.width() > 0 => {
            let y = oy + (HEIGHT - ICON) / 2.0;
            let k = ICON * sf / src.width() as f32;
            let t = Transform::from_row(k, 0.0, 0.0, k, (icon_x * sf).round(), (y * sf).round());
            pm.draw_pixmap(0, 0, src.as_ref(), &PixmapPaint::default(), t, None);
        }
        PillIcon::Themed(_) | PillIcon::None => {}
    }

    let label_x = icon_x + if icon_w > 0.0 { icon_w + GAP } else { 0.0 };
    text.draw(&mut pm, &g.label, label_x, oy + (HEIGHT - g.label.height) / 2.0, s, pal.text);

    let right = ox + g.width - PAD_X;
    match &v.value {
        PillValue::None => {}
        PillValue::Text(_) => {
            let color = value_color(v.tom, &pal, theme);
            text.draw(&mut pm, &g.value, right - g.value.width, oy + (HEIGHT - g.value.height) / 2.0, s, color);
        }
        PillValue::Dots { on, total } => {
            let left = right - g.dots_w;
            let unlit = with_alpha(pal.text, UNLIT_ALPHA);
            for i in 0..*total {
                let cx = left + f32::from(i) * (DOT + DOT_GAP) + DOT / 2.0;
                let fill = if i < *on { accent(theme) } else { unlit };
                fill_circle(&mut pm, cx * sf, (oy + HEIGHT / 2.0) * sf, DOT / 2.0 * sf, fill);
            }
        }
    }
    pm
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;
    use crate::model::{Hints, Level, Notification, Tom};
    use crate::render::card::{SHADOW_PAD, palette};
    use crate::render::glyphs::{Glyph, glyph_for};
    use crate::render::theme::{FALLBACK, WARNING};

    fn notif(icon: &str, summary: &str, body: &str, raw: Vec<(&str, crate::model::HintValue)>) -> Notification {
        let hints = Hints::parse(raw.into_iter().map(|(k, v)| (k.to_string(), v)).collect());
        Notification {
            id: 1, app_name: "logi-osd".into(), app_icon: icon.into(), summary: summary.into(), body: body.into(),
            actions: vec![], hints, expire_timeout: 1500,
        }
    }

    fn s(v: &str) -> crate::model::HintValue { crate::model::HintValue::Str(v.into()) }
    fn i(v: i64) -> crate::model::HintValue { crate::model::HintValue::Int(v) }

    fn text() -> Text { Text::new(None) }

    fn view<'a>(icon: PillIcon<'a>, label: &'a str, value: PillValue<'a>, tom: Option<Tom>) -> PillView<'a> {
        PillView { icon, label, value, tom, level: Level::Normal }
    }

    /// Pixel pré-multiplicado (r, g, b, a) em coordenadas do cartão (sem o enchimento da sombra).
    fn px(pm: &tiny_skia::Pixmap, scale: u32, x: f32, y: f32) -> [u8; 4] {
        let (dx, dy) = (((SHADOW_PAD.l + x) * scale as f32) as u32, ((SHADOW_PAD.t + y) * scale as f32) as u32);
        let p = pm.pixel(dx, dy).expect("pixel dentro do pixmap");
        [p.red(), p.green(), p.blue(), p.alpha()]
    }

    fn near(a: [u8; 4], b: [u8; 4], tol: i32) -> bool {
        a.iter().zip(b).all(|(x, y)| (i32::from(*x) - i32::from(y)).abs() <= tol)
    }

    fn card_w(t: &mut Text, v: &PillView, scale: u32) -> u32 {
        let (w, _) = measure(t, v, scale);
        w / scale - (SHADOW_PAD.l + SHADOW_PAD.r) as u32
    }

    #[test]
    fn width_is_at_least_260() {
        let mut t = text();
        let v = view(PillIcon::Drawn(Glyph::CapsLock), "Caps Lock", PillValue::Text(Cow::Borrowed("Ativado")), None);
        assert_eq!(card_w(&mut t, &v, 1), 260);
        let (w, h) = measure(&mut t, &v, 2);
        assert_eq!((w, h), ((260 + 40) * 2, (38 + 12 + 24) * 2));
    }

    #[test]
    fn long_label_ellipsized_within_420() {
        let mut t = text();
        let label = "Teclado Mecânico Sem Fio Ultra Super Mega Longo de Teste Para Truncar: bateria baixa";
        let v = view(PillIcon::Drawn(Glyph::Battery), label, PillValue::Text(Cow::Borrowed("10%")), Some(Tom::Atencao));
        assert_eq!(card_w(&mut t, &v, 1), 420);
        let pm = render(&mut t, &v, &FALLBACK, 1);
        assert_eq!(pm.width(), 420 + 40);
    }

    #[test]
    fn dots_lit_and_unlit() {
        let mut t = text();
        let v = view(PillIcon::Drawn(Glyph::KeyboardBacklight), "Teclado", PillValue::Dots { on: 3, total: 8 }, None);
        let w = card_w(&mut t, &v, 1) as f32;
        let pm = render(&mut t, &v, &FALLBACK, 1);
        let dots_w = 8.0 * DOT + 7.0 * DOT_GAP;
        let left = w - PAD_X - dots_w;
        let pal = palette(&FALLBACK, Level::Normal);
        let (a, tx, bg) = (FALLBACK.accent, pal.text, pal.card_bg);
        let bg_a = f32::from(bg.3) / 255.0;
        let unlit = |c: u8, b: u8| (f32::from(c) * 0.18 + f32::from(b) * bg_a * 0.82).round() as u8;
        for n in 0..8u32 {
            let x = left + n as f32 * (DOT + DOT_GAP) + DOT / 2.0;
            let got = px(&pm, 1, x, HEIGHT / 2.0);
            if n < 3 {
                assert!(near(got, [a.0, a.1, a.2, 255], 3), "bolinha {n} acesa: {got:?}");
            } else {
                let want_a = (0.18 * 255.0 + f32::from(bg.3) * 0.82).round() as u8;
                assert!(
                    near(got, [unlit(tx.0, bg.0), unlit(tx.1, bg.1), unlit(tx.2, bg.2), want_a], 3),
                    "bolinha {n} apagada: {got:?}"
                );
            }
        }
    }

    #[test]
    fn value_priority() {
        let both = notif("logi-backlight", "Teclado", "Ativado", vec![("x-pontos", i(3)), ("x-pontos-total", i(8)), ("value", i(42))]);
        assert_eq!(pill_view(&both, None).value, PillValue::Dots { on: 3, total: 8 });
        let body = notif("logi-caps-lock", "Caps Lock", "Ativado", vec![("value", i(42))]);
        assert_eq!(pill_view(&body, None).value, PillValue::Text(Cow::Borrowed("Ativado")));
        let pct = notif("logi-battery", "Bateria", "", vec![("value", i(42))]);
        assert_eq!(pill_view(&pct, None).value, PillValue::Text(Cow::Owned("42%".to_string())));
        let none = notif("logi-switch", "Canal 2", "", vec![]);
        assert_eq!(pill_view(&none, None).value, PillValue::None);
        assert_eq!(pill_view(&none, None).label, "Canal 2");
        assert!(matches!(pill_view(&none, None).icon, PillIcon::Drawn(Glyph::Switch)));
        assert_eq!(glyph_for("logi-switch"), Some(Glyph::Switch));
        let themed = notif("audio-volume-high-symbolic", "Volume", "", vec![]);
        assert!(matches!(pill_view(&themed, None).icon, PillIcon::None));
        let pm = tiny_skia::Pixmap::new(4, 4).unwrap();
        assert!(matches!(pill_view(&themed, Some(&pm)).icon, PillIcon::Themed(_)));
    }

    /// Maior canal vermelho (pré-multiplicado) na faixa horizontal do valor, alinhada à direita.
    fn brightest_red(pm: &tiny_skia::Pixmap, from: f32, to: f32) -> u8 {
        (from as u32..to as u32)
            .flat_map(|x| (4..34).map(move |y| (x, y)))
            .map(|(x, y)| px(pm, 1, x as f32, y as f32)[0])
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn apagado_dims_value() {
        let mut t = text();
        let normal = view(PillIcon::None, "Caps Lock", PillValue::Text(Cow::Borrowed("Ativado")), Some(Tom::Apagado));
        let w = card_w(&mut t, &normal, 1) as f32;
        let pm = render(&mut t, &normal, &FALLBACK, 1);
        let pal = palette(&FALLBACK, Level::Normal);
        let bg_a = f32::from(pal.card_bg.3) / 255.0;
        let want = f32::from(pal.text.0) * 0.8 + f32::from(pal.card_bg.0) * bg_a * 0.2;
        let got = f32::from(brightest_red(&pm, w - PAD_X - 50.0, w - PAD_X));
        assert!((got - want).abs() <= 12.0, "valor apagado: R={got}, esperado ≈ {want}");
        // Sem o tom, o valor usa o destaque (R do destaque, bem menor que o do texto a 80%).
        let plain = PillView { tom: None, ..normal };
        let pm2 = render(&mut t, &plain, &FALLBACK, 1);
        let acc = f32::from(brightest_red(&pm2, w - PAD_X - 50.0, w - PAD_X));
        assert!(acc < got - 40.0, "destaque R={acc} deveria ser menor que {got}");
    }

    #[test]
    fn atencao_icon_and_value_orange() {
        let mut t = text();
        let v = view(PillIcon::Drawn(Glyph::Battery), "G602", PillValue::Text(Cow::Borrowed("15%")), Some(Tom::Atencao));
        let w = card_w(&mut t, &v, 1) as f32;
        let pm = render(&mut t, &v, &FALLBACK, 1);
        let target = [WARNING.0, WARNING.1, WARNING.2];
        let best = |x0: f32, x1: f32| {
            (x0 as u32..x1 as u32)
                .flat_map(|x| (4..34).map(move |y| (x, y)))
                .map(|(x, y)| {
                    let p = px(&pm, 1, x as f32, y as f32);
                    (0..3).map(|k| (i32::from(p[k]) - i32::from(target[k])).abs()).max().unwrap_or(255)
                })
                .min()
                .unwrap_or(255)
        };
        let icon = best(PAD_X, PAD_X + ICON);
        let value = best(w - PAD_X - 30.0, w - PAD_X);
        assert!(icon <= 12, "ícone longe de #F0A35E (desvio {icon})");
        assert!(value <= 12, "valor longe de #F0A35E (desvio {value})");
    }

    #[test]
    fn pill_has_border_and_shadow() {
        let mut t = text();
        let v = view(PillIcon::Drawn(Glyph::CapsLock), "Caps Lock", PillValue::None, None);
        let pm = render(&mut t, &v, &FALLBACK, 1);
        let x = 240.0;
        assert!(px(&pm, 1, x, HEIGHT + 3.0)[3] > 0, "sem sombra logo abaixo da pílula");
        assert!(px(&pm, 1, x, -SHADOW_PAD.t + 0.5)[3] < 20, "a sombra deve sumir antes do topo da superfície");
        let border = px(&pm, 1, x, 0.0);
        let inside = px(&pm, 1, x, HEIGHT / 2.0);
        assert!(border[0] > inside[0], "borda {border:?} deveria ser mais clara que o fundo {inside:?}");
        // Raio = altura/2: o canto do retângulo fica fora da pílula.
        let corner = px(&pm, 1, 1.0, 1.0);
        assert!(corner[3] < 120, "o canto devia estar fora da pílula arredondada: {corner:?}");
    }

    #[test]
    fn render_size_scales() {
        let mut t = text();
        let v = view(PillIcon::Drawn(Glyph::CapsLock), "Caps Lock", PillValue::None, None);
        let pm = render(&mut t, &v, &FALLBACK, 2);
        assert_eq!((pm.width(), pm.height()), measure(&mut t, &v, 2));
    }

    /// Gera `docs/verificacao/t16-pilulas.png` (folha de amostras); `cargo test -- --ignored dump_sheet`.
    #[test]
    #[ignore]
    fn dump_sheet() {
        let mut t = text();
        let rows: Vec<PillView> = vec![
            view(PillIcon::Drawn(Glyph::CapsLock), "Caps Lock", PillValue::Text(Cow::Borrowed("Ativado")), None),
            view(PillIcon::Drawn(Glyph::KeyboardBacklight), "Teclado", PillValue::Dots { on: 3, total: 7 }, None),
            view(PillIcon::Drawn(Glyph::Battery), "G602: bateria baixa", PillValue::Text(Cow::Borrowed("15%")), Some(Tom::Atencao)),
            view(PillIcon::Drawn(Glyph::Charging), "MX Keys", PillValue::Text(Cow::Borrowed("Carregando")), Some(Tom::Apagado)),
            view(PillIcon::Drawn(Glyph::Switch), "Canal 2", PillValue::None, Some(Tom::Critico)),
        ];
        let pms: Vec<_> = rows.iter().map(|v| render(&mut t, v, &FALLBACK, 2)).collect();
        let w = pms.iter().map(|p| p.width()).max().unwrap_or(1);
        let h: u32 = pms.iter().map(|p| p.height()).sum();
        let mut sheet = tiny_skia::Pixmap::new(w, h).unwrap();
        sheet.fill(tiny_skia::Color::from_rgba8(0x70, 0x70, 0x70, 255));
        let mut y = 0;
        for p in &pms {
            sheet.draw_pixmap(0, y, p.as_ref(), &tiny_skia::PixmapPaint::default(), tiny_skia::Transform::identity(), None);
            y += p.height() as i32;
        }
        sheet.save_png(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/verificacao/t16-pilulas.png")).unwrap();
    }
}

//! Desenho do cartão de notificação em `tiny_skia::Pixmap`: geometria, paleta, regiões clicáveis.

use tiny_skia::Pixmap;

use super::shapes;
use super::text::{APP, BODY, BUTTON, Laid, TITLE, Text};
use super::theme::{CRITICAL, Rgba, Theme, WARNING};
use crate::icons::Icons;
use crate::model::{Action, Level};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect { pub x: f32, pub y: f32, pub w: f32, pub h: f32 }

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format { Card, Compact, Image }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hit { Close, Action(String), Default }

pub enum ActionButton {
    Text { key: String, label: String, primary: bool },
    Icon { key: String, label: String, icon: Pixmap },
}

pub struct CardView<'a> {
    pub format: Format,
    pub app: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub buttons: &'a [ActionButton],
    pub icon: Option<&'a Pixmap>,
    pub thumb: Option<&'a Pixmap>,
    pub dim_icon: bool,
    pub level: Level,
    pub redacted: bool,
}

pub const CARD_W: f32 = 340.0;
pub const PAD: f32 = 10.0;
pub const RADIUS: f32 = 16.0;
pub const ICON: f32 = 40.0;
pub const ROUND_BTN: f32 = 30.0;
pub const BTN_H: f32 = 26.0;
pub const BAR_H: f32 = 3.0;
pub const GAP: f32 = 8.0;

pub struct ShadowPad { pub l: f32, pub t: f32, pub r: f32, pub b: f32 }
pub const SHADOW_PAD: ShadowPad = ShadowPad { l: 20.0, t: 12.0, r: 20.0, b: 24.0 };

pub const AGORA: &str = "agora";
pub const NOVA_NOTIFICACAO: &str = "Nova notificação";

pub struct Palette {
    pub card_bg: Rgba, pub border: Rgba, pub text: Rgba, pub text_dim: Rgba, pub body: Rgba,
    pub button: Rgba, pub button_hover: Rgba, pub level: Rgba, pub on_level: Rgba,
}

const TEXT_DARK: Rgba = Rgba(0x0b, 0x1a, 0x24, 0xFF);
const TEXT_LIGHT: Rgba = Rgba(0xf2, 0xf2, 0xf5, 0xFF);
const MAX_BUTTONS: usize = 3;
const BUTTON_LABEL_MAX: f32 = 140.0;
const BUTTON_PAD: f32 = 14.0;
const BAR_SPACE: f32 = 8.0;
const ACTION_ICON: f32 = 16.0;
const COMPACT_ICON: f32 = 32.0;
const DIM_ALPHA: f32 = 0.45;
const SHADOW_SIGMA: f32 = 14.0;
const SHADOW_DY: f32 = 8.0;
const SHADOW_ALPHA: f32 = 0.45;

fn alpha_pct(c: Rgba, pct: f32) -> Rgba {
    Rgba(c.0, c.1, c.2, (pct * 255.0).round() as u8)
}

fn luminance(c: Rgba) -> f32 {
    (0.2126 * f32::from(c.0) + 0.7152 * f32::from(c.1) + 0.0722 * f32::from(c.2)) / 255.0
}

fn lighten(c: Rgba, amount: f32) -> Rgba {
    let mix = |v: u8| (f32::from(v) + (255.0 - f32::from(v)) * amount).round() as u8;
    Rgba(mix(c.0), mix(c.1), mix(c.2), c.3)
}

pub fn palette(theme: &Theme, level: Level) -> Palette {
    let level_color = match level {
        Level::Normal => Rgba(theme.accent.0, theme.accent.1, theme.accent.2, 0xFF),
        Level::Atencao => WARNING,
        Level::Critico => CRITICAL,
    };
    let on_level = if luminance(level_color) < 0.4 { TEXT_LIGHT } else { TEXT_DARK };
    let card_alpha = (0.94f32 * 255.0).round() as u8;
    if theme.dark {
        let white = Rgba(255, 255, 255, 255);
        Palette {
            card_bg: Rgba(0x1e, 0x1f, 0x24, card_alpha),
            border: alpha_pct(white, 0.08),
            text: TEXT_LIGHT,
            text_dim: alpha_pct(TEXT_LIGHT, 0.60),
            body: alpha_pct(TEXT_LIGHT, 0.85),
            button: alpha_pct(white, 0.06),
            button_hover: alpha_pct(white, 0.14),
            level: level_color,
            on_level,
        }
    } else {
        let (b, t) = (theme.background, theme.text);
        let black = Rgba(0, 0, 0, 255);
        let text = Rgba(t.0, t.1, t.2, 0xFF);
        Palette {
            card_bg: Rgba(b.0, b.1, b.2, card_alpha),
            border: alpha_pct(black, 0.08),
            text,
            text_dim: alpha_pct(text, 0.60),
            body: alpha_pct(text, 0.85),
            button: alpha_pct(black, 0.06),
            button_hover: alpha_pct(black, 0.14),
            level: level_color,
            on_level,
        }
    }
}

/// Botões de ação: sem `default`, no máximo 3; o 1º de texto é o principal. Com `action_icons`,
/// a chave vira ícone quando o tema o tem (senão cai em texto).
pub fn buttons(actions: &[Action], action_icons: bool, icons: &mut Icons, tint: Rgba) -> Vec<ActionButton> {
    let mut out = Vec::new();
    let mut primary_taken = false;
    for a in actions.iter().filter(|a| a.key != "default").take(MAX_BUTTONS) {
        let icon = if action_icons { icons.named(&a.key, ACTION_ICON as u32, tint) } else { None };
        if let Some(icon) = icon {
            out.push(ActionButton::Icon { key: a.key.clone(), label: a.label.clone(), icon });
        } else {
            out.push(ActionButton::Text { key: a.key.clone(), label: a.label.clone(), primary: !primary_taken });
            primary_taken = true;
        }
    }
    out
}

/// Botão já disposto (coordenadas da superfície, lógicas).
struct Placed { idx: usize, rect: Rect, label: Option<Laid> }

/// Layout completo: parte pública em `CardLayout`, textos dispostos em `LaidText`.
struct LaidText { app: Laid, title: Laid, body: Laid, app_xy: (f32, f32), title_xy: (f32, f32), body_xy: (f32, f32) }

pub struct CardLayout {
    pub card: Rect,
    pub surface: (u32, u32),
    pub regions: Vec<(Rect, Hit)>,
    pub bar: Rect,
    /// Retângulo do ícone (coordenadas da superfície, lógicas).
    pub icon: Rect,
    /// Imagem grande: (área de 320 de largura, miniatura centralizada nela).
    pub thumb: Option<(Rect, Rect)>,
    /// Título e corpo efetivos (já trocados pelo texto genérico no modo privado).
    pub title: String,
    pub body: String,
    texts: LaidText,
    placed: Vec<Placed>,
    close: Option<Rect>,
}

type Item = (usize, f32, Option<Laid>);

/// Larguras (e texto disposto) dos botões a colocar, no máximo `MAX_BUTTONS`.
fn button_items(text: &mut Text, buttons: &[ActionButton], s: u32) -> Vec<Item> {
    buttons
        .iter()
        .enumerate()
        .take(MAX_BUTTONS)
        .map(|(i, b)| match b {
            ActionButton::Text { label, .. } => {
                let laid = text.layout(label, BUTTON, BUTTON_LABEL_MAX, 1, s);
                (i, laid.width.ceil() + 2.0 * BUTTON_PAD, Some(laid))
            }
            ActionButton::Icon { .. } => (i, ROUND_BTN, None),
        })
        .collect()
}

fn items_total(items: &[Item]) -> f32 {
    items.iter().map(|it| it.1).sum::<f32>() + GAP * items.len().saturating_sub(1) as f32
}

/// Formato imagem só com miniatura e fora do modo privado; sem ela cai no cartão comum.
fn is_image(v: &CardView) -> bool {
    v.format == Format::Image && v.thumb.is_some() && !v.redacted
}

/// Menor largura que o bloco título/subtítulo da imagem grande pode ter antes de soltar botões.
const IMAGE_TEXT_MIN: f32 = 80.0;
const THUMB_RADIUS: f32 = 10.0;
const THUMB_BG_ALPHA: f32 = 0.35;

/// Imagem grande: miniatura em cima; embaixo título e subtítulo à esquerda, botões e × à direita.
fn layout_image(text: &mut Text, v: &CardView, thumb: &Pixmap, s: u32) -> CardLayout {
    let sf = s as f32;
    let (ox, oy) = (SHADOW_PAD.l, SHADOW_PAD.t);
    let area_w = CARD_W - 2.0 * PAD;
    let (tw, th) = ((thumb.width() as f32 / sf).min(area_w), thumb.height() as f32 / sf);
    let area = Rect { x: ox + PAD, y: oy + PAD, w: area_w, h: th };
    let thumb_r = Rect { x: area.x + (area_w - tw) / 2.0, y: area.y, w: tw, h: th };

    let mut items = button_items(text, v.buttons, s);
    let text_w = |items: &[Item]| {
        let used = ROUND_BTN + GAP + if items.is_empty() { 0.0 } else { items_total(items) + GAP };
        area_w - used
    };
    while items.len() > 1 && text_w(&items) < IMAGE_TEXT_MIN {
        items.pop();
    }
    let tw_avail = text_w(&items).max(1.0);
    let title_l = text.layout(v.title, TITLE, tw_avail, 1, s);
    let sub_l = text.layout(v.body, APP, tw_avail, 1, s);
    let sub_gap = if sub_l.lines > 0 { 1.0 } else { 0.0 };
    let block = title_l.height + sub_gap + sub_l.height;
    let row_y = PAD + th + GAP;
    let row_h = ceil_px(block.max(ROUND_BTN));
    let title_y = row_y + (row_h - block) / 2.0;
    let title_h = title_l.height;

    let close = Rect { x: ox + CARD_W - PAD - ROUND_BTN, y: oy + row_y + (row_h - ROUND_BTN) / 2.0, w: ROUND_BTN, h: ROUND_BTN };
    let mut placed = Vec::new();
    let mut x = close.x - GAP - items_total(&items);
    for (idx, w, label) in items {
        let h = if label.is_some() { BTN_H } else { ROUND_BTN };
        placed.push(Placed { idx, rect: Rect { x, y: oy + row_y + (row_h - h) / 2.0, w, h }, label });
        x += w + GAP;
    }

    let bar_y = row_y + row_h + BAR_SPACE;
    let card_h = bar_y + BAR_H + PAD;
    let card = Rect { x: ox, y: oy, w: CARD_W, h: card_h };
    let bar = Rect { x: ox + PAD, y: oy + bar_y, w: CARD_W - 2.0 * PAD, h: BAR_H };

    let mut regions = vec![(close, Hit::Close)];
    for p in &placed {
        if let Some(b) = v.buttons.get(p.idx) {
            let key = match b {
                ActionButton::Text { key, .. } | ActionButton::Icon { key, .. } => key.clone(),
            };
            regions.push((p.rect, Hit::Action(key)));
        }
    }
    regions.push((card, Hit::Default));

    let surface = (
        ((CARD_W + SHADOW_PAD.l + SHADOW_PAD.r) * sf).ceil() as u32,
        ((card_h + SHADOW_PAD.t + SHADOW_PAD.b) * sf).ceil() as u32,
    );
    let empty = text.layout("", APP, 1.0, 1, s);
    CardLayout {
        card,
        surface,
        regions,
        bar,
        icon: Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 },
        thumb: Some((area, thumb_r)),
        title: v.title.to_string(),
        body: v.body.to_string(),
        texts: LaidText {
            app: empty,
            title: title_l,
            body: sub_l,
            app_xy: (0.0, 0.0),
            title_xy: (ox + PAD, oy + title_y),
            body_xy: (ox + PAD, oy + title_y + title_h + sub_gap),
        },
        placed,
        close: Some(close),
    }
}

/// Arredonda para cima ignorando ruído de ponto flutuante, para as bordas caírem em pixels inteiros.
fn ceil_px(v: f32) -> f32 {
    (v - 0.01).ceil()
}

pub fn layout(text: &mut Text, v: &CardView, scale: u32) -> CardLayout {
    let s = scale.max(1);
    if let (true, Some(thumb)) = (is_image(v), v.thumb) {
        return layout_image(text, v, thumb, s);
    }
    let compact = v.format == Format::Compact;
    let (ox, oy) = (SHADOW_PAD.l, SHADOW_PAD.t);
    let (title, body) = if v.redacted { (v.app, NOVA_NOTIFICACAO) } else { (v.title, v.body) };
    let icon_size = if compact { COMPACT_ICON } else { ICON };
    let text_x = PAD + icon_size + PAD;
    let right = if compact { CARD_W - PAD } else { CARD_W - PAD - ROUND_BTN - GAP };
    let text_w = right - text_x;

    let empty = |text: &mut Text| text.layout("", APP, 1.0, 1, s);
    let (app, title_l, body_l, content_bottom, icon_y, text_y0) = if compact {
        let title_l = text.layout(title, TITLE, text_w, 1, s);
        let sub = text.layout(body, APP, text_w, 1, s);
        let sub_gap = if sub.lines > 0 { 1.0 } else { 0.0 };
        let block = title_l.height + sub_gap + sub.height;
        let content_h = ceil_px(block.max(icon_size));
        let icon_y = PAD + (content_h - icon_size) / 2.0;
        let y0 = PAD + (content_h - block) / 2.0;
        (empty(text), title_l, sub, PAD + content_h, icon_y, y0)
    } else {
        let app_line = format!("{} · {AGORA}", v.app);
        let app = text.layout(&app_line, APP, text_w, 1, s);
        let title_l = text.layout(title, TITLE, text_w, 1, s);
        let body_l = text.layout(body, BODY, text_w, 3, s);
        let body_gap = if body_l.lines > 0 { 2.0 } else { 0.0 };
        let bottom = PAD + app.height + title_l.height + body_gap + body_l.height;
        let content_bottom = ceil_px(bottom.max(PAD + ICON).max(PAD + ROUND_BTN));
        (app, title_l, body_l, content_bottom, PAD, PAD)
    };
    let (app_h, title_h) = (app.height, title_l.height);
    let (app_xy, title_xy, body_xy) = if compact {
        ((0.0, 0.0), (text_x, text_y0), (text_x, text_y0 + title_h + 1.0))
    } else {
        ((text_x, text_y0), (text_x, text_y0 + app_h), (text_x, text_y0 + app_h + title_h + 2.0))
    };

    // Botões (só no cartão normal, nunca no privado).
    let mut placed: Vec<Placed> = Vec::new();
    let mut bottom = content_bottom;
    if !compact && !v.redacted && !v.buttons.is_empty() {
        let has_icon = v.buttons.iter().any(|b| matches!(b, ActionButton::Icon { .. }));
        let row_h = if has_icon { ROUND_BTN } else { BTN_H };
        let row_y = content_bottom + GAP;
        let mut items = button_items(text, v.buttons, s);
        let avail = CARD_W - 2.0 * PAD;
        while items.len() > 1 && items_total(&items) > avail {
            items.pop();
        }
        let mut x = ox + CARD_W - PAD - items_total(&items);
        for (idx, w, label) in items {
            let h = if label.is_some() { BTN_H } else { ROUND_BTN };
            placed.push(Placed { idx, rect: Rect { x, y: oy + row_y + (row_h - h) / 2.0, w, h }, label });
            x += w + GAP;
        }
        bottom = row_y + row_h;
    }

    let bar_y = bottom + BAR_SPACE;
    let card_h = bar_y + BAR_H + PAD;
    let card = Rect { x: ox, y: oy, w: CARD_W, h: card_h };
    let bar = Rect { x: ox + PAD, y: oy + bar_y, w: CARD_W - 2.0 * PAD, h: BAR_H };
    let icon = Rect { x: ox + PAD, y: oy + icon_y, w: icon_size, h: icon_size };

    let mut regions = Vec::new();
    let mut close = None;
    if !compact {
        let r = Rect { x: ox + CARD_W - PAD - ROUND_BTN, y: oy + PAD, w: ROUND_BTN, h: ROUND_BTN };
        regions.push((r, Hit::Close));
        close = Some(r);
        for p in &placed {
            if let Some(b) = v.buttons.get(p.idx) {
                let key = match b {
                    ActionButton::Text { key, .. } | ActionButton::Icon { key, .. } => key.clone(),
                };
                regions.push((p.rect, Hit::Action(key)));
            }
        }
    }
    regions.push((card, Hit::Default));

    let surface = (
        ((CARD_W + SHADOW_PAD.l + SHADOW_PAD.r) * s as f32).ceil() as u32,
        ((card_h + SHADOW_PAD.t + SHADOW_PAD.b) * s as f32).ceil() as u32,
    );
    let off = |(x, y): (f32, f32)| (ox + x, oy + y);
    CardLayout {
        card,
        surface,
        regions,
        bar,
        icon,
        thumb: None,
        title: title.to_string(),
        body: body.to_string(),
        texts: LaidText {
            app,
            title: title_l,
            body: body_l,
            app_xy: off(app_xy),
            title_xy: off(title_xy),
            body_xy: off(body_xy),
        },
        placed,
        close,
    }
}

fn scaled(r: Rect, s: f32) -> Rect {
    Rect { x: r.x * s, y: r.y * s, w: r.w * s, h: r.h * s }
}

/// Desenha `src` com o canto superior esquerdo em (`x`, `y`) e `target` px de largura (device).
fn blit(pm: &mut Pixmap, src: &Pixmap, x: f32, y: f32, target: f32, opacity: f32) {
    if src.width() == 0 {
        return;
    }
    let k = target / src.width() as f32;
    let paint = tiny_skia::PixmapPaint { opacity, quality: tiny_skia::FilterQuality::Bilinear, ..Default::default() };
    pm.draw_pixmap(0, 0, src.as_ref(), &paint, tiny_skia::Transform::from_row(k, 0.0, 0.0, k, x, y), None);
}

/// Fundo preto a 35% na área toda e a miniatura recortada em cantos de 10 (coordenadas de dispositivo).
fn draw_thumb(pm: &mut Pixmap, src: &Pixmap, area: Rect, thumb: Rect, sf: f32) {
    shapes::fill_rounded_rect(pm, area, THUMB_RADIUS * sf, Rgba(0, 0, 0, (THUMB_BG_ALPHA * 255.0).round() as u8));
    if src.width() == 0 {
        return;
    }
    let mask = shapes::rounded_rect_path(thumb, THUMB_RADIUS * sf).and_then(|path| {
        let mut m = tiny_skia::Mask::new(pm.width(), pm.height())?;
        m.fill_path(&path, tiny_skia::FillRule::Winding, true, tiny_skia::Transform::identity());
        Some(m)
    });
    let k = thumb.w / src.width() as f32;
    let paint = tiny_skia::PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..Default::default() };
    pm.draw_pixmap(0, 0, src.as_ref(), &paint, tiny_skia::Transform::from_row(k, 0.0, 0.0, k, thumb.x, thumb.y), mask.as_ref());
}

pub fn render(text: &mut Text, v: &CardView, l: &CardLayout, theme: &Theme, hover: Option<&Hit>, scale: u32) -> Pixmap {
    let s = scale.max(1);
    let sf = s as f32;
    let pal = palette(theme, v.level);
    let mut pm = Pixmap::new(l.surface.0.max(1), l.surface.1.max(1))
        .or_else(|| Pixmap::new(1, 1))
        .expect("pixmap 1x1 sempre existe");

    let card = scaled(l.card, sf);
    shapes::draw_shadow(&mut pm, card, RADIUS * sf, SHADOW_SIGMA * sf, SHADOW_DY * sf, SHADOW_ALPHA);
    shapes::fill_rounded_rect(&mut pm, card, RADIUS * sf, pal.card_bg);
    let half = sf * 0.5;
    let inner = Rect { x: card.x + half, y: card.y + half, w: card.w - sf, h: card.h - sf };
    shapes::stroke_rounded_rect(&mut pm, inner, RADIUS * sf - half, sf, pal.border);

    if let (Some((area, thumb_r)), Some(src)) = (l.thumb, v.thumb) {
        draw_thumb(&mut pm, src, scaled(area, sf), scaled(thumb_r, sf), sf);
    } else {
        // Ícone (ou um marcador discreto quando o app não tem ícone).
        let icon = scaled(l.icon, sf);
        match v.icon {
            Some(src) => blit(&mut pm, src, icon.x, icon.y, icon.w, if v.dim_icon { DIM_ALPHA } else { 1.0 }),
            None => shapes::fill_rounded_rect(&mut pm, icon, 10.0 * sf, pal.button),
        }
    }

    let t = &l.texts;
    if l.thumb.is_some() {
        text.draw(&mut pm, &t.title, t.title_xy.0, t.title_xy.1, s, pal.text);
        text.draw(&mut pm, &t.body, t.body_xy.0, t.body_xy.1, s, pal.text_dim);
    } else if v.format != Format::Compact {
        text.draw(&mut pm, &t.app, t.app_xy.0, t.app_xy.1, s, pal.text_dim);
        text.draw(&mut pm, &t.title, t.title_xy.0, t.title_xy.1, s, pal.text);
        text.draw(&mut pm, &t.body, t.body_xy.0, t.body_xy.1, s, pal.body);
    } else {
        text.draw(&mut pm, &t.title, t.title_xy.0, t.title_xy.1, s, pal.text);
        text.draw(&mut pm, &t.body, t.body_xy.0, t.body_xy.1, s, pal.text_dim);
    }

    if let Some(close) = l.close {
        let r = scaled(close, sf);
        let bg = if hover == Some(&Hit::Close) { pal.button_hover } else { pal.button };
        shapes::fill_pill(&mut pm, r, bg);
        shapes::draw_cross(&mut pm, r.x + r.w / 2.0, r.y + r.h / 2.0, 4.5 * sf, 1.5 * sf, pal.text);
    }

    for p in &l.placed {
        let Some(b) = v.buttons.get(p.idx) else { continue };
        let r = scaled(p.rect, sf);
        match b {
            ActionButton::Text { key, primary, .. } => {
                let hot = hover == Some(&Hit::Action(key.clone()));
                let (bg, fg) = match (primary, hot) {
                    (true, false) => (pal.level, pal.on_level),
                    (true, true) => (lighten(pal.level, 0.25), pal.on_level),
                    (false, false) => (pal.button, pal.text),
                    (false, true) => (pal.button_hover, pal.text),
                };
                shapes::fill_pill(&mut pm, r, bg);
                if let Some(label) = &p.label {
                    let x = p.rect.x + (p.rect.w - label.width) / 2.0;
                    let y = p.rect.y + (p.rect.h - label.height) / 2.0;
                    text.draw(&mut pm, label, x, y, s, fg);
                }
            }
            ActionButton::Icon { key, icon, .. } => {
                let hot = hover == Some(&Hit::Action(key.clone()));
                shapes::fill_pill(&mut pm, r, if hot { pal.button_hover } else { pal.button });
                let size = ACTION_ICON * sf;
                blit(&mut pm, icon, r.x + (r.w - size) / 2.0, r.y + (r.h - size) / 2.0, size, 1.0);
            }
        }
    }
    pm
}

/// Barra de tempo: trilho (branco/preto 8%) e progresso na cor do nível. Pinte sobre uma cópia
/// do `render` (o trilho é translúcido: pintar duas vezes sobre o mesmo pixmap o escurece).
pub fn draw_bar(pm: &mut Pixmap, l: &CardLayout, theme: &Theme, level: Level, fraction: f32, scale: u32) {
    let sf = scale.max(1) as f32;
    let pal = palette(theme, level);
    let bar = scaled(l.bar, sf);
    let radius = 2.0 * sf;
    let trough = if theme.dark { Rgba(255, 255, 255, 20) } else { Rgba(0, 0, 0, 20) };
    shapes::fill_rounded_rect(pm, bar, radius, trough);
    let fraction = if fraction.is_nan() { 0.0 } else { fraction.clamp(0.0, 1.0) };
    if fraction > 0.0 {
        shapes::fill_rounded_rect(pm, Rect { w: bar.w * fraction, ..bar }, radius, pal.level);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::icons::IconRoots;
    use crate::render::theme::{parse_hex, top_level_field};

    fn theme(dark: bool) -> Theme {
        let (bg, accent) = if dark {
            (include_str!("../../tests/fixtures/theme/dark_background"), include_str!("../../tests/fixtures/theme/dark_accent"))
        } else {
            (include_str!("../../tests/fixtures/theme/light_background"), include_str!("../../tests/fixtures/theme/light_accent"))
        };
        let get = |src: &str, key: &str| parse_hex(top_level_field(src, key).unwrap()).unwrap();
        Theme { dark, background: get(bg, "base"), text: get(bg, "on"), accent: get(accent, "base") }
    }

    fn act(key: &str, label: &str) -> Action { Action { key: key.into(), label: label.into() } }

    fn icons() -> Icons { Icons::new(IconRoots { app_dirs: vec![], icon_dirs: vec![] }, "x".into()) }

    fn btns(keys: &[&str]) -> Vec<ActionButton> {
        let actions: Vec<Action> = keys.iter().map(|k| act(k, k)).collect();
        buttons(&actions, false, &mut icons(), Rgba(255, 255, 255, 255))
    }

    fn view<'a>(body: &'a str, bs: &'a [ActionButton]) -> CardView<'a> {
        CardView {
            format: Format::Card, app: "Firefox", title: "Título", body, buttons: bs, icon: None, thumb: None,
            dim_icon: false, level: Level::Normal, redacted: false,
        }
    }

    /// RGBA não pré-multiplicado.
    fn px(pm: &Pixmap, x: f32, y: f32) -> [u8; 4] {
        let c = pm.pixel(x as u32, y as u32).unwrap().demultiply();
        [c.red(), c.green(), c.blue(), c.alpha()]
    }

    fn near(a: u8, b: u8, tol: i32) -> bool { (i32::from(a) - i32::from(b)).abs() <= tol }

    fn btn_rect(l: &CardLayout, key: &str) -> Rect {
        l.regions.iter().find(|(_, h)| *h == Hit::Action(key.into())).map(|(r, _)| *r).unwrap()
    }

    fn solid(px: u32, c: [u8; 4]) -> Pixmap {
        let mut pm = Pixmap::new(px, px).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(c[0], c[1], c[2], c[3]));
        pm
    }

    #[test]
    fn card_width_340_and_surface_adds_pad() {
        let mut t = Text::new(None);
        let l = layout(&mut t, &view("corpo", &[]), 1);
        assert_eq!(l.card.w, 340.0);
        assert_eq!(l.surface.0, 380);
        assert_eq!((l.card.x, l.card.y), (20.0, 12.0));
        assert_eq!(l.surface.1 as f32, l.card.h + 12.0 + 24.0);
    }

    #[test]
    fn card_height_grows_with_body_lines() {
        let mut t = Text::new(None);
        let one = layout(&mut t, &view("curto", &[]), 1);
        let long = "palavra comprida de teste ".repeat(40);
        let three = layout(&mut t, &view(&long, &[]), 1);
        assert!(three.card.h > one.card.h, "{} !> {}", three.card.h, one.card.h);
    }

    #[test]
    fn outside_rounded_corner_has_only_shadow() {
        let mut t = Text::new(None);
        let v = view("corpo", &[]);
        let l = layout(&mut t, &v, 1);
        let pm = render(&mut t, &v, &l, &theme(true), None, 1);
        let corner = px(&pm, l.card.x + 1.0, l.card.y + 1.0);
        let center = px(&pm, l.card.x + 170.0, l.bar.y - 4.0);
        assert!(f32::from(corner[3]) < 0.5 * f32::from(center[3]), "{corner:?} vs {center:?}");
    }

    #[test]
    fn center_has_dark_background() {
        let mut t = Text::new(None);
        let v = view("corpo", &[]);
        let l = layout(&mut t, &v, 1);
        let pm = render(&mut t, &v, &l, &theme(true), None, 1);
        let c = px(&pm, l.card.x + 170.0, l.bar.y - 4.0);
        assert!(near(c[0], 0x1e, 3) && near(c[1], 0x1f, 3) && near(c[2], 0x24, 3), "{c:?}");
        assert!(near(c[3], 240, 3), "alfa {}", c[3]);
    }

    #[test]
    fn primary_button_uses_level_color() {
        let mut t = Text::new(None);
        let th = theme(true);
        let bs = btns(&["reply"]);
        for (level, want) in [
            (Level::Normal, th.accent),
            (Level::Atencao, WARNING),
            (Level::Critico, CRITICAL),
        ] {
            let v = CardView { level, ..view("corpo", &bs) };
            let l = layout(&mut t, &v, 1);
            let pm = render(&mut t, &v, &l, &th, None, 1);
            let r = btn_rect(&l, "reply");
            let c = px(&pm, r.x + 7.0, r.y + r.h / 2.0);
            assert!(near(c[0], want.0, 2) && near(c[1], want.1, 2) && near(c[2], want.2, 2), "{level:?}: {c:?}");
        }
    }

    #[test]
    fn bar_fraction_proportional() {
        let mut t = Text::new(None);
        let th = theme(true);
        let v = view("corpo", &[]);
        let l = layout(&mut t, &v, 1);
        let mut pm = render(&mut t, &v, &l, &th, None, 1);
        draw_bar(&mut pm, &l, &th, Level::Normal, 0.5, 1);
        let y = l.bar.y + 1.5;
        let lit = (0..pm.width())
            .filter(|&x| {
                let c = px(&pm, x as f32, y);
                near(c[0], th.accent.0, 12) && near(c[1], th.accent.1, 12) && near(c[2], th.accent.2, 12) && c[3] > 200
            })
            .count() as i32;
        assert!((lit - 160).abs() <= 2, "acesos = {lit}");
        assert_eq!(l.bar.w, 320.0);
        assert_eq!(l.bar.h, 3.0);
    }

    #[test]
    fn regions_close_buttons_default() {
        let mut t = Text::new(None);
        let bs = btns(&["default", "reply"]);
        let l = layout(&mut t, &view("corpo", &bs), 1);
        let hits: Vec<&Hit> = l.regions.iter().map(|(_, h)| h).collect();
        assert_eq!(hits, [&Hit::Close, &Hit::Action("reply".into()), &Hit::Default]);
        let close = l.regions[0].0;
        assert_eq!((close.x, close.y, close.w, close.h), (20.0 + 340.0 - 10.0 - 30.0, 12.0 + 10.0, 30.0, 30.0));
        assert_eq!(l.regions[2].0, l.card);
    }

    #[test]
    fn at_most_three_buttons() {
        let mut t = Text::new(None);
        let bs = btns(&["a", "b", "c", "d", "e"]);
        assert_eq!(bs.len(), 3);
        let l = layout(&mut t, &view("corpo", &bs), 1);
        let n = l.regions.iter().filter(|(_, h)| matches!(h, Hit::Action(_))).count();
        assert_eq!(n, 3);
    }

    #[test]
    fn buttons_skip_default_and_mark_first_text_primary() {
        let bs = btns(&["default", "x", "y"]);
        assert_eq!(bs.len(), 2);
        assert!(matches!(&bs[0], ActionButton::Text { key, primary: true, .. } if key == "x"));
        assert!(matches!(&bs[1], ActionButton::Text { key, primary: false, .. } if key == "y"));
    }

    #[test]
    fn redacted_has_no_buttons_and_generic_text() {
        let mut t = Text::new(None);
        let bs = btns(&["reply"]);
        let v = CardView { redacted: true, ..view("segredo", &bs) };
        let l = layout(&mut t, &v, 1);
        assert!(!l.regions.iter().any(|(_, h)| matches!(h, Hit::Action(_))));
        assert!(l.regions.iter().any(|(_, h)| *h == Hit::Close));
        assert_eq!(l.title, "Firefox");
        assert_eq!(l.body, NOVA_NOTIFICACAO);
    }

    #[test]
    fn compact_has_no_close_and_dims_icon() {
        let mut t = Text::new(None);
        let th = theme(true);
        let red = solid(32, [255, 0, 0, 255]);
        let mk = |dim| CardView {
            format: Format::Compact, icon: Some(&red), dim_icon: dim, ..view("Armazenamento USB", &[])
        };
        let (bright, dim) = (mk(false), mk(true));
        let l = layout(&mut t, &bright, 1);
        assert!(!l.regions.iter().any(|(_, h)| *h == Hit::Close));
        assert_eq!(l.regions.len(), 1);
        assert_eq!(l.regions[0].1, Hit::Default);
        let (cx, cy) = (l.icon.x + 16.0, l.icon.y + 16.0);
        let full = px(&render(&mut t, &bright, &l, &th, None, 1), cx, cy);
        let faded = px(&render(&mut t, &dim, &l, &th, None, 1), cx, cy);
        assert_eq!(full[0], 255);
        // Sobre o fundo (r≈0x1e), 45% de alfa deixa o vermelho em 0,45*255 + 0,55*0x1e.
        let want = 0.45 * 255.0 + 0.55 * 30.0;
        assert!((f32::from(faded[0]) - want).abs() <= 8.0, "vermelho apagado = {}", faded[0]);
    }

    #[test]
    fn scale_two_doubles_surface() {
        let mut t = Text::new(None);
        let bs = btns(&["reply"]);
        let v = view("corpo", &bs);
        let one = layout(&mut t, &v, 1);
        let two = layout(&mut t, &v, 2);
        assert_eq!(two.surface, (760, 2 * one.surface.1));
        let pm = render(&mut t, &v, &two, &theme(true), None, 2);
        assert_eq!((pm.width(), pm.height()), two.surface);
    }

    #[test]
    fn hover_lightens_button() {
        let mut t = Text::new(None);
        let th = theme(true);
        let bs = btns(&["default", "reply"]);
        let v = view("corpo", &bs);
        let l = layout(&mut t, &v, 1);
        let r = btn_rect(&l, "reply");
        let mut at = |hover: Option<&Hit>| {
            let pm = render(&mut t, &v, &l, &th, hover, 1);
            px(&pm, r.x + 7.0, r.y + r.h / 2.0)
        };
        let (off, on) = (at(None), at(Some(&Hit::Action("reply".into()))));
        let sum = |c: [u8; 4]| u32::from(c[0]) + u32::from(c[1]) + u32::from(c[2]);
        assert!(sum(on) > sum(off), "{on:?} !> {off:?}");
    }

    #[test]
    fn hover_lightens_secondary_button_and_close() {
        let mut t = Text::new(None);
        let th = theme(true);
        let bs = btns(&["a", "b"]);
        let v = view("corpo", &bs);
        let l = layout(&mut t, &v, 1);
        let r = btn_rect(&l, "b");
        let mut at = |hover: Option<&Hit>| {
            let pm = render(&mut t, &v, &l, &th, hover, 1);
            (px(&pm, r.x + 7.0, r.y + r.h / 2.0), px(&pm, l.regions[0].0.x + 4.0, l.regions[0].0.y + 15.0))
        };
        let (off, on) = (at(None), at(Some(&Hit::Action("b".into()))));
        assert!(on.0[0] > off.0[0]);
        let (_, close_on) = at(Some(&Hit::Close));
        assert!(close_on[0] > off.1[0]);
    }

    #[test]
    fn light_theme_uses_theme_background() {
        let mut t = Text::new(None);
        let th = theme(false);
        let v = view("corpo", &[]);
        let l = layout(&mut t, &v, 1);
        let pm = render(&mut t, &v, &l, &th, None, 1);
        let c = px(&pm, l.card.x + 170.0, l.bar.y - 4.0);
        let want = th.background;
        assert!(near(c[0], want.0, 3) && near(c[1], want.1, 3) && near(c[2], want.2, 3), "{c:?} vs {want:?}");
    }

    #[test]
    fn zero_scale_is_treated_as_one() {
        let mut t = Text::new(None);
        let l = layout(&mut t, &view("corpo", &[]), 0);
        assert_eq!(l.surface.0, 380);
    }

    /// Gera `docs/verificacao/cartoes/imagem*.png` com o print de exemplo (800×450).
    #[test]
    #[ignore]
    fn dump_image_pngs() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let dir = root.join("docs/verificacao/cartoes");
        std::fs::create_dir_all(&dir).unwrap();
        let src = crate::images::decode_file(&root.join("tests/fixtures/images/print.png")).unwrap();
        let fx = root.join("tests/fixtures");
        let mut icons = Icons::new(IconRoots { app_dirs: vec![], icon_dirs: vec![fx.join("icons")] }, "Teste".into());
        let actions = vec![act("default", "Editar"), act("editar", "Editar"), act("algo-symbolic", "Abrir pasta")];
        let mut t = Text::new(None);
        for (suffix, dark, scale) in [("", true, 1u32), ("-claro", false, 1), ("@2x", true, 2)] {
            let thumb = crate::images::fit(&src, (320 * scale, 180 * scale));
            let bs = buttons(&actions, true, &mut icons, Rgba(255, 255, 255, 255));
            let v = CardView {
                format: Format::Image, app: "Captura de tela", title: "Captura copiada",
                body: "800×450 · salvo em Screenshots", buttons: &bs, icon: None, thumb: Some(&thumb),
                dim_icon: false, level: Level::Normal, redacted: false,
            };
            let th = theme(dark);
            let l = layout(&mut t, &v, scale);
            let mut pm = render(&mut t, &v, &l, &th, None, scale);
            draw_bar(&mut pm, &l, &th, Level::Normal, 0.62, scale);
            let mut bg = Pixmap::new(pm.width(), pm.height()).unwrap();
            bg.fill(tiny_skia::Color::from_rgba8(0x5a, 0x6b, 0x8c, 255));
            bg.draw_pixmap(0, 0, pm.as_ref(), &tiny_skia::PixmapPaint::default(), tiny_skia::Transform::identity(), None);
            std::fs::write(dir.join(format!("imagem{suffix}.png")), bg.encode_png().unwrap()).unwrap();
        }
    }

    /// Gera `docs/verificacao/cartoes/*.png` sobre um fundo cinza-azulado, para olhar a sombra.
    #[test]
    #[ignore]
    fn dump_pngs() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/verificacao/cartoes");
        std::fs::create_dir_all(&dir).unwrap();
        let mut t = Text::new(None);
        let mut ic = Pixmap::new(40, 40).unwrap();
        ic.fill(tiny_skia::Color::from_rgba8(0xE9, 0x5A, 0x2A, 255));
        let usb = solid(32, [0x3d, 0xae, 0xe9, 255]);
        let bs = btns(&["default", "reply", "archive"]);
        let body = "Mensagem de exemplo com texto longo o bastante para quebrar em várias linhas e mostrar as reticências no fim do terceiro trecho do corpo.";
        let cases: [(&str, Level, bool, Format, bool); 6] = [
            ("normal", Level::Normal, false, Format::Card, false),
            ("atencao", Level::Atencao, false, Format::Card, false),
            ("critico", Level::Critico, false, Format::Card, false),
            ("compacto", Level::Normal, false, Format::Compact, false),
            ("privado", Level::Normal, true, Format::Card, false),
            ("compacto-apagado", Level::Normal, false, Format::Compact, true),
        ];
        for (name, level, redacted, format, dim) in cases {
            for (suffix, dark, scale) in [("", true, 1u32), ("-claro", false, 1), ("@2x", true, 2)] {
                if suffix != "" && name != "normal" && name != "critico" {
                    continue;
                }
                let th = theme(dark);
                let compact = format == Format::Compact;
                let v = CardView {
                    format, app: if compact { "Dispositivos USB" } else { "Firefox" },
                    title: if compact { "Armazenamento USB conectado" } else { "Nova mensagem de Maria" },
                    body: if compact { "SanDisk Ultra 64 GB" } else { body },
                    buttons: if compact { &[] } else { &bs },
                    icon: Some(if compact { &usb } else { &ic }), thumb: None, dim_icon: dim, level, redacted,
                };
                let l = layout(&mut t, &v, scale);
                let mut pm = render(&mut t, &v, &l, &th, None, scale);
                draw_bar(&mut pm, &l, &th, level, 0.62, scale);
                let mut bg = Pixmap::new(pm.width(), pm.height()).unwrap();
                bg.fill(tiny_skia::Color::from_rgba8(0x5a, 0x6b, 0x8c, 255));
                bg.draw_pixmap(0, 0, pm.as_ref(), &tiny_skia::PixmapPaint::default(), tiny_skia::Transform::identity(), None);
                let path = dir.join(format!("{name}{suffix}.png"));
                std::fs::write(&path, bg.encode_png().unwrap()).unwrap();
            }
        }
    }

    fn image_view<'a>(bs: &'a [ActionButton], thumb: &'a Pixmap) -> CardView<'a> {
        CardView { format: Format::Image, thumb: Some(thumb), ..view("subtítulo", bs) }
    }

    fn solid_wh(w: u32, h: u32, c: [u8; 4]) -> Pixmap {
        let mut pm = Pixmap::new(w, h).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(c[0], c[1], c[2], c[3]));
        pm
    }

    #[test]
    fn image_card_layout() {
        let mut t = Text::new(None);
        let thumb = solid_wh(320, 180, [255, 0, 0, 255]);
        let bs = btns(&["editar", "pasta"]);
        let v = image_view(&bs, &thumb);
        let l = layout(&mut t, &v, 1);
        assert_eq!(l.card.w, 340.0);
        assert_eq!(l.surface.0, 380);
        let (area, th) = l.thumb.expect("miniatura");
        assert_eq!((area.w, th.w, th.h), (320.0, 320.0, 180.0));
        // A região Default cobre a miniatura; o × é a última região à direita, depois dos botões.
        let center = (th.x + th.w / 2.0, th.y + th.h / 2.0);
        assert_eq!(crate::presenter::hit(&l, center.0, center.1), Some(Hit::Default));
        let close = l.regions.iter().find(|(_, h)| *h == Hit::Close).map(|(r, _)| *r).unwrap();
        assert_eq!(close.x + close.w, l.card.x + 340.0 - PAD);
        assert_eq!((close.w, close.h), (30.0, 30.0));
        for key in ["editar", "pasta"] {
            let b = btn_rect(&l, key);
            assert!(b.x + b.w < close.x, "{key} {b:?} !< {close:?}");
            assert!(b.y >= l.card.y + PAD + 180.0, "botão abaixo da miniatura");
        }
        assert!(btn_rect(&l, "editar").x < btn_rect(&l, "pasta").x, "ordem das ações");
        // Título e subtítulo ficam à esquerda dos botões.
        assert!(l.bar.y > close.y + close.h);
        // A linha de baixo tem no mínimo a altura do botão redondo.
        assert!(l.card.h >= 10.0 + 180.0 + 8.0 + 30.0 + 8.0 + 3.0 + 10.0, "{}", l.card.h);
    }

    #[test]
    fn image_thumb_is_centered_and_rounded() {
        let mut t = Text::new(None);
        let th = theme(true);
        let thumb = solid_wh(90, 180, [255, 0, 0, 255]);
        let v = image_view(&[], &thumb);
        let l = layout(&mut t, &v, 1);
        let (area, r) = l.thumb.unwrap();
        assert_eq!((r.x - area.x, area.x + area.w - (r.x + r.w)), (115.0, 115.0));
        let pm = render(&mut t, &v, &l, &th, None, 1);
        let c = px(&pm, r.x + r.w / 2.0, r.y + r.h / 2.0);
        assert!(c[0] > 240 && c[1] < 10 && c[2] < 10, "{c:?}");
        // Canto recortado: não é vermelho; fundo preto 35% nas laterais da área.
        let corner = px(&pm, r.x + 0.5, r.y + 0.5);
        assert!(corner[0] < 200, "{corner:?}");
        let side = px(&pm, area.x + 20.0, area.y + 90.0);
        assert!(side[0] < 60 && side[3] > 200, "{side:?}");
    }

    #[test]
    fn image_without_thumb_or_redacted_falls_back_to_card() {
        let mut t = Text::new(None);
        let thumb = solid_wh(320, 180, [255, 0, 0, 255]);
        let none = CardView { thumb: None, ..image_view(&[], &thumb) };
        assert!(layout(&mut t, &none, 1).thumb.is_none());
        let red = CardView { redacted: true, ..image_view(&[], &thumb) };
        assert!(layout(&mut t, &red, 1).thumb.is_none());
    }

    #[test]
    fn action_icons_make_round_button() {
        let fx = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut icons = Icons::new(IconRoots { app_dirs: vec![], icon_dirs: vec![fx.join("icons")] }, "Teste".into());
        let actions = vec![act("editar", "Editar"), act("algo-symbolic", "Abrir pasta")];
        let on = buttons(&actions, true, &mut icons, Rgba(255, 255, 255, 255));
        assert!(matches!(&on[0], ActionButton::Text { key, primary: true, .. } if key == "editar"));
        assert!(matches!(&on[1], ActionButton::Icon { key, label, .. } if key == "algo-symbolic" && label == "Abrir pasta"));
        let off = buttons(&actions, false, &mut icons, Rgba(255, 255, 255, 255));
        assert!(off.iter().all(|b| matches!(b, ActionButton::Text { .. })));
        // O botão de ícone é redondo (30×30) no layout.
        let mut t = Text::new(None);
        let l = layout(&mut t, &view("corpo", &on), 1);
        let r = btn_rect(&l, "algo-symbolic");
        assert_eq!((r.w, r.h), (30.0, 30.0));
    }
}

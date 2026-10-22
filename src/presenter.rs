//! Regras puras da tela: margens da pilha, próximo despertar do timer único, alfa global e
//! ponteiro (região sob o cursor, clique, mãozinha).

use std::time::Instant;

use tiny_skia::Pixmap;

use crate::render::card::{CardLayout, Format, Hit};
use crate::timing::{Countdown, FADE_IN, MIN_FRAME, next_bar_step};

/// Margem inferior de cada superfície, mais nova primeiro (índice 0 = embaixo).
/// margem_i = base − pad_b + Σ_{j<i}(card_heights[j] + gap)  (alturas do cartão, sem o enchimento de sombra)
pub fn stack_margins(card_heights: &[u32], base: i32, gap: i32, pad_b: i32) -> Vec<i32> {
    let mut margin = base - pad_b;
    card_heights
        .iter()
        .map(|&h| {
            let this = margin;
            margin = margin.saturating_add(i32::try_from(h).unwrap_or(i32::MAX)).saturating_add(gap);
            this
        })
        .collect()
}

/// Tempo de um cartão na tela.
#[derive(Debug, Clone)]
pub struct CardTiming {
    pub countdown: Countdown,
    /// Largura da barra em px de dispositivo (0 = sem barra).
    pub bar_device_px: u32,
    pub shown_at: Instant,
}

/// Próximo despertar: menor entre prazos, passos de barra e fim de fade; None sem cartão/pílula.
/// Durante o fade, um quadro a cada `MIN_FRAME`, sem passar do fim do fade.
pub fn next_wake(cards: &[CardTiming], now: Instant) -> Option<Instant> {
    cards.iter().filter_map(|c| card_wake(c, now)).min()
}

fn card_wake(c: &CardTiming, now: Instant) -> Option<Instant> {
    let fade_end = c.shown_at + FADE_IN;
    let fade = (now < fade_end).then(|| (now + MIN_FRAME).min(fade_end));
    [c.countdown.deadline(), next_bar_step(&c.countdown, c.bar_device_px, now), fade].into_iter().flatten().min()
}

/// Multiplica o pixmap (RGBA pré-multiplicado) por um alfa global.
pub fn apply_alpha(pm: &mut Pixmap, alpha: f32) {
    let alpha = if alpha.is_nan() { 0.0 } else { alpha.clamp(0.0, 1.0) };
    if alpha >= 1.0 {
        return;
    }
    let k = (alpha * 256.0).round() as u32;
    for b in pm.data_mut() {
        *b = ((u32::from(*b) * k + 128) >> 8) as u8;
    }
}

/// Região sob (`x`, `y`) (coordenadas da superfície, lógicas); a primeira que contém o ponto ganha.
pub fn hit(l: &CardLayout, x: f32, y: f32) -> Option<Hit> {
    l.regions.iter().find(|(r, _)| r.contains(x, y)).map(|(_, h)| h.clone())
}

/// Mãozinha sobre o que clica: ×, botões e, fora do cartão normal, o corpo (`Default`).
pub fn wants_hand(format: Format, h: Option<&Hit>) -> bool {
    match h {
        Some(Hit::Close | Hit::Action(_)) => true,
        Some(Hit::Default) => format != Format::Card,
        None => false,
    }
}

/// Clique = press e release sobre o mesmo `Hit`.
#[derive(Debug, Default)]
pub struct ClickTracker {
    pressed: Option<Hit>,
}

impl ClickTracker {
    pub fn press(&mut self, h: Option<Hit>) {
        self.pressed = h;
    }

    pub fn release(&mut self, h: Option<Hit>) -> Option<Hit> {
        let pressed = self.pressed.take()?;
        (h.as_ref() == Some(&pressed)).then_some(pressed)
    }

    pub fn cancel(&mut self) {
        self.pressed = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Cartão já fora do fade (mostrado há 1 s).
    fn card(total: Duration, bar_px: u32, now: Instant) -> CardTiming {
        CardTiming { countdown: Countdown::start(total, now), bar_device_px: bar_px, shown_at: now - ms(1000) }
    }

    #[test]
    fn stack_margins_newest_at_bottom() {
        assert_eq!(stack_margins(&[100, 80], 24, 8, 24), vec![0, 108]);
        assert_eq!(stack_margins(&[], 24, 8, 24), Vec::<i32>::new());
    }

    #[test]
    fn removing_card_recomputes_margins() {
        assert_eq!(stack_margins(&[100, 80, 90], 24, 8, 24), vec![0, 108, 196]);
        assert_eq!(stack_margins(&[100, 90], 24, 8, 24), vec![0, 108]);
    }

    #[test]
    fn next_wake_is_min_of_deadline_and_bar_step() {
        let now = Instant::now();
        // Barra de 320 px em 6 s: um passo a cada 18,75 ms, antes do prazo.
        let with_bar = card(ms(6000), 320, now);
        let step = next_bar_step(&with_bar.countdown, 320, now).unwrap();
        assert_eq!(next_wake(std::slice::from_ref(&with_bar), now), Some(step));
        // Sem barra: só o prazo conta.
        let no_bar = card(ms(500), 0, now);
        assert_eq!(next_wake(std::slice::from_ref(&no_bar), now), Some(now + ms(500)));
        // Barra de 1 px em 60 s: o passo é o próprio prazo; o prazo menor de outro cartão ganha.
        let slow = card(ms(60_000), 1, now);
        assert_eq!(next_wake(&[slow.clone(), no_bar], now), Some(now + ms(500)));
        assert_eq!(next_wake(&[slow], now), Some(now + ms(60_000)));
    }

    #[test]
    fn paused_card_contributes_no_wake() {
        let now = Instant::now();
        let mut c = card(ms(6000), 320, now);
        c.countdown.pause(now);
        assert_eq!(next_wake(&[c.clone()], now), None);
        // Ao lado de um cartão rodando, só o que roda conta.
        let other = card(ms(500), 0, now);
        assert_eq!(next_wake(&[c, other], now), Some(now + ms(500)));
    }

    #[test]
    fn fade_end_wakes_even_with_long_bar_step() {
        let now = Instant::now();
        // Acabou de aparecer: 60 s numa barra de 1 px (passo distante), mas o fade precisa de quadros.
        let fresh = CardTiming { countdown: Countdown::start(ms(60_000), now), bar_device_px: 1, shown_at: now };
        let wake = next_wake(std::slice::from_ref(&fresh), now).unwrap();
        assert!(wake <= now + FADE_IN, "{:?} depois do fim do fade", wake - now);
        assert_eq!(wake, now + MIN_FRAME);
        // Perto do fim do fade, o despertar é o próprio fim (não passa dele).
        let late = now + FADE_IN - ms(5);
        assert_eq!(next_wake(std::slice::from_ref(&fresh), late), Some(now + FADE_IN));
        // Pausado durante o fade: o fade ainda acorda.
        let mut paused = fresh.clone();
        paused.countdown.pause(now);
        assert_eq!(next_wake(&[paused.clone()], now), Some(now + MIN_FRAME));
        // Depois do fade, o pausado não acorda mais.
        assert_eq!(next_wake(&[paused], now + FADE_IN), None);
    }

    fn layout_with(keys: &[&str]) -> CardLayout {
        use crate::model::Level;
        use crate::render::card::{self, ActionButton, CardView, Format};
        use crate::render::text::Text;
        let buttons: Vec<ActionButton> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| ActionButton::Text { key: (*k).into(), label: (*k).into(), primary: i == 0 })
            .collect();
        let view = CardView {
            format: Format::Card,
            app: "App",
            title: "Título",
            body: "corpo",
            buttons: &buttons,
            icon: None,
            thumb: None,
            dim_icon: false,
            level: Level::Normal,
            redacted: false,
        };
        card::layout(&mut Text::new(None), &view, 1)
    }

    fn center(l: &CardLayout, want: &Hit) -> (f32, f32) {
        let r = l.regions.iter().find(|(_, h)| h == want).map(|(r, _)| *r).unwrap();
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    #[test]
    fn hit_maps_points_to_regions() {
        let l = layout_with(&["reply"]);
        let (x, y) = center(&l, &Hit::Close);
        assert_eq!(hit(&l, x, y), Some(Hit::Close));
        let (x, y) = center(&l, &Hit::Action("reply".into()));
        assert_eq!(hit(&l, x, y), Some(Hit::Action("reply".into())));
        // Meio do texto: à direita do ícone, na altura do título.
        let text = (l.icon.x + l.icon.w + 40.0, l.card.y + 25.0);
        assert_eq!(hit(&l, text.0, text.1), Some(Hit::Default));
        // Sombra: fora do cartão, dentro da superfície.
        assert_eq!(hit(&l, 5.0, 5.0), None);
        assert_eq!(hit(&l, l.card.x + l.card.w + 5.0, l.card.y + 20.0), None);
    }

    #[test]
    fn click_needs_press_and_release_same_hit() {
        let mut c = ClickTracker::default();
        c.press(Some(Hit::Close));
        assert_eq!(c.release(Some(Hit::Close)), Some(Hit::Close));
        c.press(Some(Hit::Close));
        assert_eq!(c.release(Some(Hit::Default)), None);
        // Um release sozinho (sem press) não clica; o press não sobrevive ao release.
        assert_eq!(c.release(Some(Hit::Close)), None);
        c.press(Some(Hit::Action("b".into())));
        assert_eq!(c.release(Some(Hit::Action("b".into()))), Some(Hit::Action("b".into())));
        assert_eq!(c.release(Some(Hit::Action("b".into()))), None);
        // Press na sombra não clica.
        c.press(None);
        assert_eq!(c.release(None), None);
    }

    #[test]
    fn leave_cancels_click() {
        let mut c = ClickTracker::default();
        c.press(Some(Hit::Close));
        c.cancel();
        assert_eq!(c.release(Some(Hit::Close)), None);
    }

    #[test]
    fn hand_cursor_only_over_clickable() {
        let b = Hit::Action("b".into());
        assert!(wants_hand(Format::Card, Some(&Hit::Close)));
        assert!(wants_hand(Format::Card, Some(&b)));
        assert!(!wants_hand(Format::Card, Some(&Hit::Default)));
        assert!(wants_hand(Format::Image, Some(&Hit::Default)));
        assert!(wants_hand(Format::Compact, Some(&Hit::Default)));
        for f in [Format::Card, Format::Image, Format::Compact] {
            assert!(!wants_hand(f, None));
        }
    }

    #[test]
    fn no_cards_no_wake() {
        assert_eq!(next_wake(&[], Instant::now()), None);
    }

    #[test]
    fn apply_alpha_scales_premultiplied_channels() {
        let mut pm = Pixmap::new(2, 1).unwrap();
        pm.data_mut().copy_from_slice(&[200, 100, 50, 200, 255, 255, 255, 255]);
        apply_alpha(&mut pm, 0.5);
        assert_eq!(pm.data(), &[100, 50, 25, 100, 128, 128, 128, 128]);
        apply_alpha(&mut pm, 0.0);
        assert!(pm.data().iter().all(|&b| b == 0));
        let mut full = Pixmap::new(1, 1).unwrap();
        full.data_mut().copy_from_slice(&[10, 20, 30, 40]);
        apply_alpha(&mut full, 1.0);
        assert_eq!(full.data(), &[10, 20, 30, 40]);
    }
}

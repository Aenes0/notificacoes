use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::{Duration, Instant};

use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::{LoopHandle, RegistrationToken};
use smithay_client_toolkit::shell::wlr_layer::Anchor;
use tiny_skia::Pixmap;

use crate::config::{self, Config};
use crate::icons::{IconRoots, Icons, icon_theme_name};
use crate::images::{self, THUMB_MAX};
use crate::model::{ImageHint, RawImage, Level, Notification, Tom};
use crate::presenter::{CardTiming, ClickTracker, apply_alpha, hit, next_wake, stack_margins, wants_hand};
use crate::privacy::{self, Exposure, ScreenState};
use crate::render::card::{self, CardLayout, CardView, Format, Hit, SHADOW_PAD};
use crate::render::glyphs::glyph_for;
use crate::render::pill;
use crate::render::text::{Text, interface_font_family};
use crate::render::theme::{self, Theme};
use crate::store::Store;
use crate::timing::{Countdown, fade_alpha};
use crate::wayland::{SurfaceKey, Ui};
use crate::{PointerEv, Screen, State};

/// Margem da pilha até a base e a direita da tela, e entre cartões (px lógicos).
const STACK_BASE: i32 = 24;
const STACK_RIGHT: i32 = 24;
const STACK_GAP: i32 = 8;
/// Quanto um cartão de app novo espera o primeiro `done` da inscrição de janelas antes de aparecer.
const PRIVACY_WAIT: Duration = Duration::from_millis(100);

/// Um cartão na tela. O `static_pm` não tem a barra: cada quadro é uma cópia dele + `draw_bar`.
pub struct CardState {
    pub id: u32,
    pub layout: CardLayout,
    pub static_pm: Pixmap,
    pub timing: CardTiming,
    pub hover: Option<Hit>,
    pub scale: u32,
    format: Format,
    level: Level,
    /// Tema do último `Show`/`Update` deste cartão.
    theme: Theme,
    /// Margem inferior aplicada à superfície.
    margin_b: i32,
    /// Último quadro anexado: (px de barra visíveis, alfa 0..=255).
    drawn: Option<(u32, u8)>,
    /// Cartão de app (`Notification::is_private`) e se está desenhado redigido.
    private: bool,
    redacted: bool,
}

/// A pílula na tela: sem barra, sem fade, só o prazo. O quadro é refeito quando o aviso muda
/// ou a escala da superfície muda.
struct PillState {
    id: u32,
    countdown: Countdown,
    theme: Theme,
    scale: u32,
}

/// Cartões em layer surfaces, um `Timer` calloop para prazos, passos da barra e fade.
pub struct WaylandScreen {
    pub(crate) ui: Ui,
    text: Text,
    icons: Icons,
    cosmic_dir: PathBuf,
    /// Mais antigo primeiro (a ordem do `Store::stack`).
    cards: Vec<CardState>,
    /// A pílula, se houver; fica à parte da pilha de cartões.
    pill: Option<PillState>,
    handle: LoopHandle<'static, State>,
    /// O timer registrado e o instante dele.
    timer: Option<(RegistrationToken, Instant)>,
    /// Imagem decodificada de cada notificação (reduzida a `STORED_MAX`), para refazer o desenho sem reler o arquivo.
    images: ImageCache,
    /// Caches já soltos desde que a tela esvaziou.
    released: bool,
    /// Cartão sob o ponteiro e a posição lógica nele.
    pointer: Option<(u32, f32, f32)>,
    clicks: ClickTracker,
    /// Serial do último press do botão esquerdo (vai no pedido de token).
    press_serial: Option<u32>,
    /// Forma pedida desde o último `Enter` (true = mãozinha).
    hand: Option<bool>,
    /// Cartões de app à espera do primeiro `done` da inscrição de janelas: (id, tempo de tela).
    deferred: Vec<(u32, Duration)>,
    /// Timer do prazo da espera; só existe com cartão esperando.
    privacy_timer: Option<RegistrationToken>,
    /// O prazo venceu sem `done` nesta inscrição: os próximos não esperam (saem redigidos).
    privacy_timed_out: bool,
}

impl WaylandScreen {
    pub(crate) fn new(ui: Ui, cfg: &Config, handle: LoopHandle<'static, State>) -> WaylandScreen {
        let cosmic_dir = config::config_home().join("cosmic");
        let family = cfg.font.clone().or_else(|| interface_font_family(&cosmic_dir));
        WaylandScreen {
            ui,
            text: Text::new(family.as_deref()),
            icons: Icons::new(IconRoots::from_env(), icon_theme_name(&cosmic_dir)),
            cosmic_dir,
            cards: Vec::new(),
            pill: None,
            images: ImageCache::default(),
            handle,
            timer: None,
            released: true,
            pointer: None,
            clicks: ClickTracker::default(),
            press_serial: None,
            hand: None,
            deferred: Vec::new(),
            privacy_timer: None,
            privacy_timed_out: false,
        }
    }

    pub(crate) fn show(&mut self, id: u32, total: Duration, now: Instant, store: &Store) {
        // O Store pode mandar `Hide` e `Show` do mesmo id; um `Show` repetido também começa do zero.
        // A imagem decodificada sobrevive ao `hide` se a notificação continuar com a mesma imagem.
        let kept = self.images.take(id);
        self.hide(id);
        if store.pill() == Some(id) {
            if let Some(n) = store.get(id) {
                self.show_pill(n, total, now);
            }
            return;
        }
        let Some(n) = store.get(id) else { return };
        self.images.restore(id, kept);
        self.images.load(n);
        let private = n.is_private();
        if private {
            self.ui.watch_toplevels(true);
            if self.must_wait() && self.defer(id, total) {
                return;
            }
        }
        let redacted = self.redacted(n);
        let theme = theme::load(&self.cosmic_dir);
        let key = SurfaceKey::Card(id);
        let scale = self.ui.scale(key);
        let (layout, static_pm, _) = self.build(n, &theme, scale, None, redacted);
        let margin_b = STACK_BASE - SHADOW_PAD.b as i32;
        self.ui.open(
            key,
            Anchor::BOTTOM | Anchor::RIGHT,
            logical(layout.surface, scale),
            (0, right_margin(), margin_b, 0),
            Some(layout.card),
            true,
        );
        let timing =
            CardTiming { countdown: Countdown::start(total, now), bar_device_px: bar_px(&layout, scale), shown_at: now };
        self.cards.push(CardState {
            id,
            layout,
            static_pm,
            timing,
            hover: None,
            scale,
            format: self.format_of(n),
            level: n.level(),
            theme,
            margin_b,
            drawn: None,
            private,
            redacted,
        });
        self.released = false;
        self.restack();
        if let Some(c) = self.cards.last_mut() {
            draw_card(&mut self.ui, c, now, true);
        }
    }

    /// Conteúdo trocado: estático refeito com o tema atual e a contagem recomeça.
    pub(crate) fn update(&mut self, id: u32, total: Duration, now: Instant, store: &Store) {
        if self.pill.as_ref().is_some_and(|p| p.id == id) {
            if let Some(n) = store.get(id) {
                self.update_pill(n, total, now);
            }
            return;
        }
        if !self.cards.iter().any(|c| c.id == id) {
            self.show(id, total, now, store);
            return;
        }
        if let Some(n) = store.get(id) {
            self.images.load(n);
        }
        let theme = theme::load(&self.cosmic_dir);
        self.rebuild(id, store, theme);
        if let Some(c) = self.cards.iter_mut().find(|c| c.id == id) {
            c.timing.countdown.restart(total, now);
            draw_card(&mut self.ui, c, now, true);
        }
    }

    /// Id da pílula na tela, se houver.
    pub(crate) fn pill_id(&self) -> Option<u32> {
        self.pill.as_ref().map(|p| p.id)
    }

    /// Quadro da pílula: glifo desenhado (`logi-*`) ou ícone do tema pelo `app_icon`.
    fn render_pill(&mut self, n: &Notification, theme: &Theme, scale: u32) -> Pixmap {
        let themed = if glyph_for(&n.app_icon).is_none() {
            let px = (pill::ICON * scale.max(1) as f32).round() as u32;
            self.icons.named(&n.app_icon, px, pill::icon_color(n.hints.tom, theme))
        } else {
            None
        };
        pill::render(&mut self.text, &pill::pill_view(n, themed.as_ref()), theme, scale)
    }

    /// Abre a superfície da pílula (centro inferior, sem entrada) e desenha. Sempre nova: o
    /// `hide` que precede o `show` já fechou a anterior.
    fn show_pill(&mut self, n: &Notification, total: Duration, now: Instant) {
        let theme = theme::load(&self.cosmic_dir);
        let key = SurfaceKey::Pill;
        let scale = self.ui.scale(key);
        self.ui.mark_pill(now);
        let pm = self.render_pill(n, &theme, scale);
        self.ui.open(
            key,
            Anchor::BOTTOM,
            logical((pm.width(), pm.height()), scale),
            (0, 0, pill::MARGIN_BOTTOM - SHADOW_PAD.b as i32, 0),
            None,
            false,
        );
        self.ui.present(key, &pm, scale, None);
        self.pill = Some(PillState { id: n.id, countdown: Countdown::start(total, now), theme, scale });
        self.released = false;
    }

    /// Aviso trocado na mesma pílula: refaz o quadro com o tema atual e a contagem recomeça.
    fn update_pill(&mut self, n: &Notification, total: Duration, now: Instant) {
        let theme = theme::load(&self.cosmic_dir);
        let scale = self.ui.scale(SurfaceKey::Pill);
        self.ui.mark_pill(now);
        let pm = self.render_pill(n, &theme, scale);
        self.ui.present(SurfaceKey::Pill, &pm, scale, None);
        if let Some(p) = &mut self.pill {
            p.countdown.restart(total, now);
            p.theme = theme;
            p.scale = scale;
        }
    }

    /// A superfície da pílula recebeu outra escala: redesenha (mesmo tema, mesma contagem).
    pub(crate) fn rescale_pill(&mut self, store: &Store) {
        let Some((id, theme, old)) = self.pill.as_ref().map(|p| (p.id, p.theme, p.scale)) else { return };
        let scale = self.ui.scale(SurfaceKey::Pill);
        let Some(n) = store.get(id) else { return };
        if scale == old {
            return;
        }
        let pm = self.render_pill(n, &theme, scale);
        self.ui.present(SurfaceKey::Pill, &pm, scale, None);
        if let Some(p) = &mut self.pill {
            p.scale = scale;
        }
    }

    /// A superfície recebeu outra escala: redesenha nela (mesmo tema, mesma contagem).
    pub(crate) fn rescale(&mut self, id: u32, store: &Store, now: Instant) {
        let Some(c) = self.cards.iter().find(|c| c.id == id) else { return };
        if c.scale == self.ui.scale(SurfaceKey::Card(id)) {
            return;
        }
        let theme = c.theme;
        self.rebuild(id, store, theme);
        if let Some(c) = self.cards.iter_mut().find(|c| c.id == id) {
            draw_card(&mut self.ui, c, now, true);
        }
    }

    /// Refaz layout e estático na escala atual da superfície; acerta entrada e margens.
    fn rebuild(&mut self, id: u32, store: &Store, theme: Theme) {
        let Some(n) = store.get(id) else { return };
        let private = n.is_private();
        if private {
            // Um `Update` pode transformar um aviso em cartão de app.
            self.ui.watch_toplevels(true);
        }
        let redacted = self.redacted(n);
        let key = SurfaceKey::Card(id);
        let scale = self.ui.scale(key);
        let (layout, static_pm, hover) = self.build(n, &theme, scale, self.pointer_on(id), redacted);
        let Some(c) = self.cards.iter_mut().find(|c| c.id == id) else { return };
        c.private = private;
        c.redacted = redacted;
        self.ui.set_input(key, Some(layout.card));
        c.hover = hover;
        c.format = format_of_decoded(n, self.images.get(id));
        c.timing.bar_device_px = bar_px(&layout, scale);
        c.layout = layout;
        c.static_pm = static_pm;
        c.scale = scale;
        c.level = n.level();
        c.theme = theme;
        c.drawn = None;
        self.restack();
    }

    pub(crate) fn hide(&mut self, id: u32) {
        if self.pill.as_ref().is_some_and(|p| p.id == id) {
            self.pill = None;
            self.ui.close(SurfaceKey::Pill);
        }
        self.deferred.retain(|(d, _)| *d != id);
        if self.pointer.is_some_and(|(p, ..)| p == id) {
            // A superfície some sem `Leave`.
            self.pointer = None;
            self.clicks.cancel();
        }
        self.images.remove(id);
        let before = self.cards.len();
        self.cards.retain(|c| c.id != id);
        if self.cards.len() != before {
            self.ui.close(SurfaceKey::Card(id));
            self.restack();
        }
    }

    /// Layout, desenho estático (sem barra) e o `Hit` sob o ponteiro em `pointer` (realçado).
    /// `redacted`: só ícone do app, nome do app e "Nova notificação" (modo privado).
    fn build(
        &mut self,
        n: &Notification,
        theme: &Theme,
        scale: u32,
        pointer: Option<(f32, f32)>,
        redacted: bool,
    ) -> (CardLayout, Pixmap, Option<Hit>) {
        let decoded = self.images.get(n.id);
        let format = format_of_decoded(n, decoded);
        let thumb = if format == Format::Image {
            decoded.map(|p| images::fit(p, (THUMB_MAX.0 * scale.max(1), THUMB_MAX.1 * scale.max(1))))
        } else {
            None
        };
        let level = n.level();
        let pal = card::palette(theme, level);
        let buttons = if matches!(format, Format::Card | Format::Image) {
            card::buttons(&n.actions, n.hints.action_icons, &mut self.icons, pal.text)
        } else {
            Vec::new()
        };
        let view = CardView {
            format,
            app: n.display_app(),
            title: &n.summary,
            body: &n.body,
            buttons: &buttons,
            icon: None,
            thumb: thumb.as_ref(),
            dim_icon: n.hints.tom == Some(Tom::Apagado),
            level,
            redacted,
        };
        let layout = card::layout(&mut self.text, &view, scale);
        let icon_px = (layout.icon.w * scale.max(1) as f32).round() as u32;
        // Sem miniatura, uma imagem pequena (ou a de um cartão compacto) faz o papel do ícone do app.
        // Redigido, só o ícone do próprio app (a imagem pode ser a foto de quem mandou).
        let icon = match decoded.filter(|_| format != Format::Image && !redacted) {
            Some(img) => Some(images::fit(img, (icon_px, icon_px))),
            None if redacted => self.icons.app_icon_redacted(n, icon_px, pal.level),
            None => self.icons.app_icon(n, icon_px, pal.level),
        };
        let view = CardView { icon: icon.as_ref(), ..view };
        let hover = pointer.and_then(|(x, y)| hit(&layout, x, y));
        let pm = card::render(&mut self.text, &view, &layout, theme, hover.as_ref(), scale);
        (layout, pm, hover)
    }

    fn format_of(&self, n: &Notification) -> Format {
        format_of_decoded(n, self.images.get(n.id))
    }

    /// Posição do ponteiro, se estiver sobre o cartão `id`.
    fn pointer_on(&self, id: u32) -> Option<(f32, f32)> {
        self.pointer.filter(|(p, ..)| *p == id).map(|(_, x, y)| (x, y))
    }

    /// Ponteiro no cartão `id`: pausa/retoma, realce, cursor. Devolve o clique completo
    /// (`Hit` e serial do press), se houve.
    pub(crate) fn on_pointer(&mut self, id: u32, ev: PointerEv, now: Instant, store: &Store) -> Option<(Hit, Option<u32>)> {
        match ev {
            PointerEv::Enter(x, y, _) => {
                if let Some((old, ..)) = self.pointer.filter(|(p, ..)| *p != id) {
                    // Sem `Leave` do anterior: retoma ele aqui.
                    self.pointer_left(old, now, store);
                }
                self.clicks.cancel();
                self.hand = None;
                self.pointer = Some((id, x, y));
                if let Some(c) = self.cards.iter_mut().find(|c| c.id == id) {
                    c.timing.countdown.pause(now);
                }
                self.hover(id, now, store);
            }
            PointerEv::Motion(x, y) => {
                if self.pointer_on(id).is_some() {
                    self.pointer = Some((id, x, y));
                    self.hover(id, now, store);
                }
            }
            PointerEv::Leave => self.pointer_left(id, now, store),
            PointerEv::Press(serial) => {
                self.press_serial = Some(serial);
                let h = self.hit_under(id);
                self.clicks.press(h);
            }
            PointerEv::Release => {
                let h = self.hit_under(id);
                return self.clicks.release(h).map(|h| (h, self.press_serial));
            }
        }
        None
    }

    fn pointer_left(&mut self, id: u32, now: Instant, store: &Store) {
        if self.pointer_on(id).is_some() {
            self.pointer = None;
            self.clicks.cancel();
        }
        if let Some(c) = self.cards.iter_mut().find(|c| c.id == id) {
            c.timing.countdown.resume(now);
        }
        self.hover(id, now, store);
    }

    fn hit_under(&self, id: u32) -> Option<Hit> {
        let (x, y) = self.pointer_on(id)?;
        self.cards.iter().find(|c| c.id == id).and_then(|c| hit(&c.layout, x, y))
    }

    /// Acerta cursor e realce com o `Hit` sob o ponteiro; só redesenha se o `Hit` mudou.
    fn hover(&mut self, id: u32, now: Instant, store: &Store) {
        let Some(c) = self.cards.iter().find(|c| c.id == id) else { return };
        let pointer = self.pointer_on(id);
        let h = pointer.and_then(|(x, y)| hit(&c.layout, x, y));
        if pointer.is_some() {
            let hand = wants_hand(c.format, h.as_ref());
            if self.hand != Some(hand) {
                self.ui.set_cursor(hand);
                self.hand = Some(hand);
            }
        }
        if c.hover == h {
            return;
        }
        let (theme, scale, redacted) = (c.theme, c.scale, c.redacted);
        let Some(n) = store.get(id) else { return };
        let (layout, static_pm, hover) = self.build(n, &theme, scale, pointer, redacted);
        if let Some(c) = self.cards.iter_mut().find(|c| c.id == id) {
            c.layout = layout;
            c.static_pm = static_pm;
            c.hover = hover;
            // O estático mudou: o dano é o cartão todo, não só a barra.
            c.drawn = None;
            draw_card(&mut self.ui, c, now, true);
        }
    }

    /// Margens de todos, mais novo embaixo; só mexe em quem mudou.
    fn restack(&mut self) {
        let heights: Vec<u32> = self.cards.iter().rev().map(|c| c.layout.card.h.round().max(0.0) as u32).collect();
        let margins = stack_margins(&heights, STACK_BASE, STACK_GAP, SHADOW_PAD.b as i32);
        for (c, m) in self.cards.iter_mut().rev().zip(margins) {
            if c.margin_b != m {
                self.ui.set_margin(SurfaceKey::Card(c.id), (0, right_margin(), m, 0));
                c.margin_b = m;
            }
        }
    }

    /// O timer venceu: tira os vencidos (devolvidos para o Store) e redesenha o que mudou.
    pub(crate) fn on_timer(&mut self, now: Instant) -> Vec<u32> {
        self.timer = None;
        let mut due: Vec<u32> = self
            .cards
            .iter()
            .filter(|c| c.timing.countdown.deadline().is_some_and(|d| d <= now))
            .map(|c| c.id)
            .collect();
        if let Some(p) = &self.pill
            && p.countdown.deadline().is_some_and(|d| d <= now)
        {
            due.push(p.id);
        }
        for id in &due {
            self.hide(*id);
        }
        for c in &mut self.cards {
            draw_card(&mut self.ui, c, now, false);
        }
        due
    }

    /// Exatamente um timer no próximo despertar, ou nenhum. Tela vazia solta os caches.
    pub(crate) fn rearm(&mut self) {
        self.sync_toplevels();
        if self.ui.is_empty() && !self.released {
            self.text.release_caches();
            self.icons.clear();
            // SAFETY: só devolve ao sistema páginas livres do malloc; sem efeito em memória viva.
            unsafe {
                libc::malloc_trim(0);
            }
            self.released = true;
        }
        let now = Instant::now();
        let timings: Vec<CardTiming> = self.cards.iter().map(|c| c.timing.clone()).collect();
        let pill = self.pill.as_ref().and_then(|p| p.countdown.deadline());
        let next = [next_wake(&timings, now), pill].into_iter().flatten().min();
        if next == self.timer.map(|(_, at)| at) {
            return;
        }
        if let Some((token, _)) = self.timer.take() {
            self.handle.remove(token);
        }
        let Some(at) = next else { return };
        match self.handle.insert_source(Timer::from_deadline(at), |_, _, state: &mut State| {
            state.on_screen_timer();
            TimeoutAction::Drop
        }) {
            Ok(token) => self.timer = Some((token, at)),
            Err(e) => eprintln!("notificacoes: o timer da tela não registrou: {}", e.error),
        }
    }
}

impl WaylandScreen {
    /// Exposição de `n` com a tela como está agora.
    fn redacted(&self, n: &Notification) -> bool {
        let screen = ScreenState { fullscreen: self.ui.fullscreen_focused() };
        privacy::exposure(n, &screen) == Exposure::Redacted
    }

    /// A inscrição de janelas acabou de começar e ainda não deu o primeiro valor (nem venceu o prazo).
    fn must_wait(&self) -> bool {
        !self.privacy_timed_out && self.ui.toplevels_pending()
    }

    /// Guarda o cartão até o primeiro `done` ou o prazo. `false` = não deu para esperar (sem timer).
    fn defer(&mut self, id: u32, total: Duration) -> bool {
        if self.privacy_timer.is_none() {
            match self.handle.insert_source(Timer::from_duration(PRIVACY_WAIT), |_, _, state: &mut State| {
                state.on_privacy_deadline();
                TimeoutAction::Drop
            }) {
                Ok(token) => self.privacy_timer = Some(token),
                Err(e) => {
                    eprintln!("notificacoes: o timer da privacidade não registrou: {}; o cartão sai redigido", e.error);
                    self.privacy_timed_out = true;
                    return false;
                }
            }
        }
        self.deferred.retain(|(d, _)| *d != id);
        self.deferred.push((id, total));
        true
    }

    /// Sem cartão de app na tela (nem esperando), a inscrição de janelas cai (zero acordadas).
    fn sync_toplevels(&mut self) {
        if !self.deferred.is_empty() || self.cards.iter().any(|c| c.private) {
            return;
        }
        self.ui.watch_toplevels(false);
        self.privacy_timed_out = false;
        if let Some(token) = self.privacy_timer.take() {
            self.handle.remove(token);
        }
    }

    /// O estado da tela mudou (ou a espera acabou): mostra os que esperavam, na ordem da pilha, e
    /// redesenha os cartões de app cuja exposição mudou (mesma contagem).
    fn refresh_privacy(&mut self, now: Instant, store: &Store) {
        if !self.must_wait() && !self.deferred.is_empty() {
            for (id, total) in std::mem::take(&mut self.deferred) {
                self.show(id, total, now, store);
            }
            let stack = store.stack();
            self.cards.sort_by_key(|c| stack.iter().position(|s| *s == c.id).unwrap_or(usize::MAX));
            self.restack();
        }
        let changed: Vec<(u32, Theme)> = self
            .cards
            .iter()
            .filter(|c| c.private && store.get(c.id).is_some_and(|n| self.redacted(n) != c.redacted))
            .map(|c| (c.id, c.theme))
            .collect();
        for (id, theme) in changed {
            self.rebuild(id, store, theme);
            if let Some(c) = self.cards.iter_mut().find(|c| c.id == id) {
                draw_card(&mut self.ui, c, now, true);
            }
        }
    }
}

impl State {
    /// A tela cheia em foco mudou ou ficou conhecida (eventos do `ToplevelWatch`).
    pub(crate) fn on_fullscreen_changed(&mut self) {
        let Screen::Wayland(w) = &mut self.screen else { return };
        w.refresh_privacy(Instant::now(), &self.store);
        w.rearm();
    }

    /// Venceu a espera do primeiro `done`: os cartões que esperavam saem (redigidos, se ainda sem valor).
    fn on_privacy_deadline(&mut self) {
        let Screen::Wayland(w) = &mut self.screen else { return };
        w.privacy_timer = None;
        w.privacy_timed_out = true;
        w.refresh_privacy(Instant::now(), &self.store);
        w.rearm();
    }
}

/// De onde veio a imagem guardada: se a dica da notificação não mudou, não se decodifica de novo.
#[derive(Debug, Clone, PartialEq)]
enum ImageKey {
    Data(RawImage),
    /// Caminho, tamanho e data de modificação (pega "mesmo caminho, arquivo regravado").
    File(PathBuf, u64, Option<SystemTime>),
}

fn key_of(hint: &ImageHint) -> Option<ImageKey> {
    match hint {
        ImageHint::Data(raw) => Some(ImageKey::Data(raw.clone())),
        ImageHint::File(path) => {
            let meta = std::fs::metadata(path).ok()?;
            Some(ImageKey::File(path.clone(), meta.len(), meta.modified().ok()))
        }
        ImageHint::IconName(_) => None,
    }
}

/// Uma imagem decodificada (reduzida a `STORED_MAX`) por cartão visível, com a chave que a gerou.
#[derive(Default)]
struct ImageCache {
    entries: HashMap<u32, (ImageKey, Pixmap)>,
    #[cfg(test)]
    decodes: u32,
}

impl ImageCache {
    fn get(&self, id: u32) -> Option<&Pixmap> {
        self.entries.get(&id).map(|(_, pm)| pm)
    }

    fn remove(&mut self, id: u32) {
        self.entries.remove(&id);
    }

    fn take(&mut self, id: u32) -> Option<(ImageKey, Pixmap)> {
        self.entries.remove(&id)
    }

    fn restore(&mut self, id: u32, entry: Option<(ImageKey, Pixmap)>) {
        if let Some(entry) = entry {
            self.entries.insert(id, entry);
        }
    }

    /// Decodifica a imagem da notificação (dado cru ou arquivo), a não ser que a guardada seja da
    /// mesma dica. Falha vira cartão comum, com uma linha no log.
    fn load(&mut self, n: &Notification) {
        let Some(hint) = &n.hints.image else {
            self.entries.remove(&n.id);
            return;
        };
        let key = key_of(hint);
        if let (Some(key), Some((old, _))) = (&key, self.entries.get(&n.id))
            && key == old
        {
            return;
        }
        self.entries.remove(&n.id);
        let Some(key) = key.or_else(|| match hint {
            // Arquivo sem metadados: tenta decodificar mesmo assim (o erro vai para o log).
            ImageHint::File(path) => Some(ImageKey::File(path.clone(), 0, None)),
            _ => None,
        }) else {
            return;
        };
        #[cfg(test)]
        {
            self.decodes += 1;
        }
        let decoded = match hint {
            ImageHint::Data(raw) => images::decode_raw(raw),
            ImageHint::File(path) => images::decode_file(path),
            ImageHint::IconName(_) => return,
        };
        match decoded {
            Ok(pm) => {
                self.entries.insert(n.id, (key, images::fit(&pm, STORED_MAX)));
            }
            Err(e) => eprintln!("notificacoes: imagem da notificação {} descartada: {e}", n.id),
        }
    }
}

/// Maior imagem guardada: cobre a miniatura até escala 3 (320×180 · 3).
const STORED_MAX: (u32, u32) = (THUMB_MAX.0 * 3, THUMB_MAX.1 * 3);

fn format_of_decoded(n: &Notification, decoded: Option<&Pixmap>) -> Format {
    images::choose_format(n, decoded)
}

fn right_margin() -> i32 {
    STACK_RIGHT - SHADOW_PAD.r as i32
}

/// Tamanho lógico de uma superfície em px de dispositivo.
fn logical(surface: (u32, u32), scale: u32) -> (u32, u32) {
    let s = scale.max(1);
    (surface.0.div_ceil(s), surface.1.div_ceil(s))
}

fn bar_px(layout: &CardLayout, scale: u32) -> u32 {
    (layout.bar.w * scale.max(1) as f32).round().max(0.0) as u32
}

/// Fração da barra arredondada para px de dispositivo inteiros (a borda não fica borrada) e
/// quantos px aparecem.
fn bar_fill(c: &CardState, now: Instant) -> (f32, u32) {
    let px = c.timing.bar_device_px;
    let fraction = c.timing.countdown.fraction(now);
    if px == 0 {
        return (fraction, 0);
    }
    // A folga absorve o erro do f32 no instante exato do passo.
    let visible = (fraction * px as f32 - 1e-3).ceil().clamp(0.0, px as f32) as u32;
    (visible as f32 / px as f32, visible)
}

/// Cópia do estático + barra (+ alfa do fade). `force` desenha mesmo sem mudança visível.
fn draw_card(ui: &mut Ui, c: &mut CardState, now: Instant, force: bool) {
    let (fraction, visible) = bar_fill(c, now);
    let alpha = fade_alpha(now.saturating_duration_since(c.timing.shown_at));
    let a8 = (alpha * 255.0).round() as u8;
    if !force && c.drawn == Some((visible, a8)) {
        return;
    }
    let mut pm = c.static_pm.clone();
    card::draw_bar(&mut pm, &c.layout, &c.theme, c.level, fraction, c.scale);
    if a8 < u8::MAX {
        apply_alpha(&mut pm, alpha);
    }
    // Fora do fade, só a barra muda entre um quadro e outro.
    let only_bar = a8 == u8::MAX && c.drawn.is_some_and(|(_, a)| a == u8::MAX);
    ui.present(SurfaceKey::Card(c.id), &pm, c.scale, only_bar.then_some(c.layout.bar));
    c.drawn = Some((visible, a8));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Hints;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/images").join(name)
    }

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("notificacoes-screen-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn with_image(id: u32, image: ImageHint) -> Notification {
        Notification {
            id,
            app_name: "app".into(),
            app_icon: String::new(),
            summary: String::new(),
            body: String::new(),
            actions: vec![],
            hints: Hints { image: Some(image), ..Hints::default() },
            expire_timeout: -1,
        }
    }

    #[test]
    fn same_image_path_decodes_once_across_updates() {
        let mut cache = ImageCache::default();
        let n = with_image(1, ImageHint::File(fixture("print.png")));
        cache.load(&n);
        cache.load(&n);
        cache.load(&n);
        assert_eq!(cache.decodes, 1);
        assert!(cache.get(1).is_some());
    }

    #[test]
    fn show_after_hide_keeps_entry_when_image_is_the_same() {
        let mut cache = ImageCache::default();
        let n = with_image(1, ImageHint::File(fixture("print.png")));
        cache.load(&n);
        let kept = cache.take(1);
        cache.remove(1); // o `hide`
        cache.restore(1, kept);
        cache.load(&n);
        assert_eq!(cache.decodes, 1);
        assert!(cache.get(1).is_some());
    }

    #[test]
    fn rewritten_file_is_decoded_again() {
        let dir = temp("regravado");
        let path = dir.join("capa.png");
        std::fs::copy(fixture("print.png"), &path).unwrap();
        let mut cache = ImageCache::default();
        let n = with_image(1, ImageHint::File(path.clone()));
        cache.load(&n);
        // Outro conteúdo (outro tamanho) no mesmo caminho.
        std::fs::copy(fixture("avatar.png"), &path).unwrap();
        cache.load(&n);
        assert_eq!(cache.decodes, 2);
        assert_eq!(cache.get(1).map(|p| (p.width(), p.height())), Some((64, 64)));
        // Mesmo tamanho, só a data mudou.
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(SystemTime::now() + Duration::from_secs(5)).unwrap();
        cache.load(&n);
        assert_eq!(cache.decodes, 3);
    }

    #[test]
    fn different_path_or_raw_data_decodes_again_and_equal_raw_does_not() {
        let mut cache = ImageCache::default();
        cache.load(&with_image(1, ImageHint::File(fixture("print.png"))));
        cache.load(&with_image(1, ImageHint::File(fixture("foto.jpg"))));
        assert_eq!(cache.decodes, 2);
        let raw = RawImage { width: 2, height: 2, rowstride: 8, has_alpha: false, bits_per_sample: 8, channels: 4, data: vec![9; 16] };
        let n = with_image(2, ImageHint::Data(raw));
        cache.load(&n);
        cache.load(&n);
        assert_eq!(cache.decodes, 3);
    }

    #[test]
    fn hide_drops_the_entry() {
        let mut cache = ImageCache::default();
        cache.load(&with_image(1, ImageHint::File(fixture("print.png"))));
        cache.remove(1);
        assert!(cache.get(1).is_none());
    }
}

//! Tela cheia em foco, pelo compositor: `ext_foreign_toplevel_list_v1` lista as janelas e
//! `zcosmic_toplevel_info_v1` (v2..=3, `get_cosmic_toplevel`) dá os estados de cada uma.
//!
//! A inscrição só existe enquanto houver cartão de app (privado) na tela: o cosmic-comp manda
//! `done` a todas as instâncias a cada mudança de qualquer janela (`toplevel_info.rs`,
//! `last_dirty`), o que acordaria o daemon parado. `stop` desfaz tudo (`stop` nos dois globais,
//! `destroy` nos handles; a lista é destruída no `finished`). Cada inscrição tem uma geração:
//! eventos de uma inscrição já parada são descartados (e os handles novos dela, destruídos).
//!
//! O valor só muda no `done` do `zcosmic_toplevel_info_v1` (estados atômicos), no fechamento de
//! uma janela, ou no fim da enumeração inicial (`wl_display.sync` depois do bind) quando não há
//! janela nenhuma: sem janela o cosmic-comp não manda `done`.

use cosmic_protocols::toplevel_info::v1::client::{
    zcosmic_toplevel_handle_v1::{self, ZcosmicToplevelHandleV1},
    zcosmic_toplevel_info_v1::{self, ZcosmicToplevelInfoV1},
};
use smithay_client_toolkit::reexports::client::backend::ObjectId;
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::wl_callback::{self, WlCallback};
use smithay_client_toolkit::reexports::client::protocol::wl_display::WlDisplay;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};
use smithay_client_toolkit::reexports::protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use smithay_client_toolkit::registry::RegistryState;

use super::Ui;
use crate::State;

const LIST: &str = "ext_foreign_toplevel_list_v1";
const INFO: &str = "zcosmic_toplevel_info_v1";
/// `get_cosmic_toplevel` e o `done` global vêm da v2.
const INFO_MIN: u32 = 2;
const INFO_MAX: u32 = 3;

/// Estados de janela agregados (puro). `K` identifica a janela (o `ObjectId` do handle da lista).
#[derive(Debug)]
pub struct Windows<K> {
    /// (janela, último (ativada, tela cheia) recebido).
    items: Vec<(K, Option<(bool, bool)>)>,
    /// O valor do último `done` (ou fechamento/enumeração); `None` = ainda desconhecido.
    value: Option<bool>,
}

impl<K: PartialEq> Windows<K> {
    pub fn new() -> Windows<K> {
        Windows { items: Vec::new(), value: None }
    }

    pub fn add(&mut self, key: K) {
        if !self.items.iter().any(|(k, _)| *k == key) {
            self.items.push((key, None));
        }
    }

    /// Guarda o estado; só vale no próximo `done`. Janela que não está na lista é ignorada.
    pub fn set_state(&mut self, key: &K, activated: bool, fullscreen: bool) {
        if let Some((_, st)) = self.items.iter_mut().find(|(k, _)| k == key) {
            *st = Some((activated, fullscreen));
        }
    }

    /// A janela fechou: sai já (se o valor já era conhecido, é refeito sem ela).
    pub fn remove(&mut self, key: &K) {
        self.items.retain(|(k, _)| k != key);
        if self.value.is_some() {
            self.value = Some(self.any_focused_fullscreen());
        }
    }

    /// Fim de um lote de estados. Antes do primeiro valor, toda janela precisa ter mandado o seu
    /// (senão continua desconhecido: falha fechada).
    pub fn done(&mut self) {
        if self.value.is_none() && self.items.iter().any(|(_, st)| st.is_none()) {
            return;
        }
        self.value = Some(self.any_focused_fullscreen());
    }

    /// A enumeração inicial acabou. Sem janela nenhuma não virá `done`: não há tela cheia.
    pub fn enumerated(&mut self) {
        if self.value.is_none() && self.items.is_empty() {
            self.value = Some(false);
        }
    }

    pub fn value(&self) -> Option<bool> {
        self.value
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.value = None;
    }

    fn any_focused_fullscreen(&self) -> bool {
        self.items.iter().any(|(_, st)| *st == Some((true, true)))
    }
}

/// Os objetos de uma inscrição ativa.
struct Subscription {
    list: ExtForeignToplevelListV1,
    info: ZcosmicToplevelInfoV1,
    /// Cada janela: o handle da lista e a extensão cosmic dele.
    handles: Vec<(ExtForeignToplevelHandleV1, ZcosmicToplevelHandleV1)>,
}

/// Dado do `wl_display.sync` que marca o fim da enumeração inicial (a geração da inscrição).
pub struct SyncData(u32);

pub struct ToplevelWatch {
    qh: QueueHandle<State>,
    display: WlDisplay,
    /// Os dois globais existem (na versão certa).
    available: bool,
    generation: u32,
    sub: Option<Subscription>,
    windows: Windows<ObjectId>,
}

impl ToplevelWatch {
    /// Sem inscrição: só anota se os globais existem (e loga uma vez se não).
    pub fn new(globals: &GlobalList, qh: QueueHandle<State>, display: WlDisplay) -> ToplevelWatch {
        let available = globals.contents().with_list(|list| {
            let has = |name: &str, min: u32| list.iter().any(|g| g.interface == name && g.version >= min);
            has(LIST, 1) && has(INFO, INFO_MIN)
        });
        if !available {
            eprintln!("notificacoes: sem {LIST} ou {INFO} v{INFO_MIN}+; a tela cheia não é detectada (cartões inteiros)");
        }
        ToplevelWatch { qh, display, available, generation: 0, sub: None, windows: Windows::new() }
    }

    /// Inscreve (se ainda não está e os globais existem). O valor fica `None` até o primeiro `done`.
    pub fn start(&mut self, registry: &RegistryState) {
        if self.sub.is_some() || !self.available {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let info = registry.bind_one::<ZcosmicToplevelInfoV1, State, u32>(&self.qh, INFO_MIN..=INFO_MAX, generation);
        let list = registry.bind_one::<ExtForeignToplevelListV1, State, u32>(&self.qh, 1..=1, generation);
        let (info, list) = match (info, list) {
            (Ok(info), Ok(list)) => (info, list),
            (info, list) => {
                eprintln!("notificacoes: {LIST}/{INFO} sumiram; a tela cheia deixa de ser detectada (cartões inteiros)");
                if let Ok(info) = info {
                    info.stop();
                }
                if let Ok(list) = list {
                    list.stop();
                }
                self.available = false;
                return;
            }
        };
        self.windows.clear();
        self.display.sync(&self.qh, SyncData(generation));
        self.sub = Some(Subscription { list, info, handles: Vec::new() });
    }

    /// Desinscreve: destrói os handles e pede `stop` aos dois globais.
    pub fn stop(&mut self) {
        let Some(sub) = self.sub.take() else { return };
        for (foreign, cosmic) in sub.handles {
            cosmic.destroy();
            foreign.destroy();
        }
        sub.info.stop();
        sub.list.stop();
        self.windows.clear();
    }

    pub fn is_active(&self) -> bool {
        self.sub.is_some()
    }

    /// `Some(false)` sem os globais; `None` até o primeiro `done` (ou sem inscrição); depois,
    /// se alguma janela está ativada e em tela cheia.
    pub fn fullscreen_focused(&self) -> Option<bool> {
        if !self.available {
            return Some(false);
        }
        self.sub.as_ref().and_then(|_| self.windows.value())
    }

    pub fn available(&self) -> bool {
        self.available
    }

    fn current(&self, generation: u32) -> bool {
        self.sub.is_some() && generation == self.generation
    }
}

impl Ui {
    /// Liga/desliga a inscrição de janelas.
    pub fn watch_toplevels(&mut self, on: bool) {
        if on {
            self.toplevels.start(&self.registry);
        } else {
            self.toplevels.stop();
        }
    }

    /// Tela cheia em foco, como o `ToplevelWatch` a conhece agora.
    pub fn fullscreen_focused(&self) -> Option<bool> {
        self.toplevels.fullscreen_focused()
    }

    /// Inscrita, com os globais, e ainda sem o primeiro valor.
    pub fn toplevels_pending(&self) -> bool {
        self.toplevels.available() && self.toplevels.is_active() && self.toplevels.fullscreen_focused().is_none()
    }
}

/// Aplica `f` ao vigia de janelas e avisa o estado se o valor mudou.
fn update(state: &mut State, f: impl FnOnce(&mut ToplevelWatch)) {
    let Some(ui) = state.ui_mut() else { return };
    let before = ui.toplevels.fullscreen_focused();
    f(&mut ui.toplevels);
    if ui.toplevels.fullscreen_focused() != before {
        state.on_fullscreen_changed();
    }
}

impl Dispatch<ExtForeignToplevelListV1, u32> for State {
    fn event(
        state: &mut State,
        list: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        generation: &u32,
        _: &Connection,
        qh: &QueueHandle<State>,
    ) {
        match event {
            ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } => {
                let Some(watch) = state.ui_mut().map(|ui| &mut ui.toplevels).filter(|w| w.current(*generation)) else {
                    toplevel.destroy();
                    return;
                };
                let Some(sub) = watch.sub.as_mut() else {
                    toplevel.destroy();
                    return;
                };
                let id = toplevel.id();
                let cosmic = sub.info.get_cosmic_toplevel(&toplevel, qh, Some(id.clone()));
                sub.handles.push((toplevel, cosmic));
                watch.windows.add(id);
            }
            // Depois do nosso `stop`: agora a lista pode ser destruída.
            ext_foreign_toplevel_list_v1::Event::Finished => list.destroy(),
            _ => {}
        }
    }

    event_created_child!(State, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut State,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if !matches!(event, ext_foreign_toplevel_handle_v1::Event::Closed) {
            return;
        }
        let mut found = false;
        update(state, |watch| {
            let Some(sub) = watch.sub.as_mut() else { return };
            let Some(at) = sub.handles.iter().position(|(f, _)| f == handle) else { return };
            let (foreign, cosmic) = sub.handles.remove(at);
            watch.windows.remove(&foreign.id());
            cosmic.destroy();
            foreign.destroy();
            found = true;
        });
        if !found {
            handle.destroy();
        }
    }
}

impl Dispatch<ZcosmicToplevelInfoV1, u32> for State {
    fn event(
        state: &mut State,
        _: &ZcosmicToplevelInfoV1,
        event: zcosmic_toplevel_info_v1::Event,
        generation: &u32,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let zcosmic_toplevel_info_v1::Event::Done = event {
            update(state, |watch| {
                if watch.current(*generation) {
                    watch.windows.done();
                }
            });
        }
    }

    // Só a v1 manda `toplevel` (nós ligamos v2+); se vier, o handle nasce sem janela e é ignorado.
    event_created_child!(State, ZcosmicToplevelInfoV1, [
        zcosmic_toplevel_info_v1::EVT_TOPLEVEL_OPCODE => (ZcosmicToplevelHandleV1, None),
    ]);
}

impl Dispatch<ZcosmicToplevelHandleV1, Option<ObjectId>> for State {
    fn event(
        state: &mut State,
        _: &ZcosmicToplevelHandleV1,
        event: zcosmic_toplevel_handle_v1::Event,
        key: &Option<ObjectId>,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let (zcosmic_toplevel_handle_v1::Event::State { state: raw }, Some(key)) = (event, key) else { return };
        let (activated, fullscreen) = parse_states(&raw);
        if let Some(ui) = state.ui_mut() {
            ui.toplevels.windows.set_state(key, activated, fullscreen);
        }
    }
}

impl Dispatch<WlCallback, SyncData> for State {
    fn event(
        state: &mut State,
        _: &WlCallback,
        event: wl_callback::Event,
        data: &SyncData,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            update(state, |watch| {
                if watch.current(data.0) {
                    watch.windows.enumerated();
                }
            });
        }
    }
}

/// O array `state` (u32 em ordem nativa) → (ativada, tela cheia). Sobra que não fecha um u32 é ignorada.
fn parse_states(raw: &[u8]) -> (bool, bool) {
    let activated = zcosmic_toplevel_handle_v1::State::Activated as u32;
    let fullscreen = zcosmic_toplevel_handle_v1::State::Fullscreen as u32;
    let mut out = (false, false);
    for chunk in raw.chunks_exact(4) {
        let mut word = [0u8; 4];
        word.copy_from_slice(chunk);
        let v = u32::from_ne_bytes(word);
        if v == activated {
            out.0 = true;
        } else if v == fullscreen {
            out.1 = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullscreen_needs_activated_and_fullscreen_same_window() {
        let mut w = Windows::new();
        w.add('a');
        w.add('b');
        // A em tela cheia sem foco, B com foco em janela: não conta.
        w.set_state(&'a', false, true);
        w.set_state(&'b', true, false);
        assert_eq!(w.value(), None, "antes do done nada vale");
        w.done();
        assert_eq!(w.value(), Some(false));
        // A ganha o foco em tela cheia.
        w.set_state(&'a', true, true);
        w.set_state(&'b', false, false);
        assert_eq!(w.value(), Some(false), "só no done");
        w.done();
        assert_eq!(w.value(), Some(true));
    }

    #[test]
    fn closed_window_removed_from_state() {
        let mut w = Windows::new();
        w.add(1);
        w.add(2);
        w.set_state(&1, true, true);
        w.set_state(&2, false, false);
        w.done();
        assert_eq!(w.value(), Some(true));
        w.remove(&1);
        assert_eq!(w.value(), Some(false), "a janela fechada sai na hora");
        // Estado atrasado de uma janela que já saiu não volta.
        w.set_state(&1, true, true);
        w.done();
        assert_eq!(w.value(), Some(false));
    }

    #[test]
    fn done_before_every_window_has_state_stays_unknown() {
        let mut w = Windows::new();
        w.add(1);
        w.add(2);
        w.set_state(&1, false, false);
        w.done();
        assert_eq!(w.value(), None, "a janela 2 ainda não mandou estado: falha fechada");
        w.set_state(&2, true, true);
        w.done();
        assert_eq!(w.value(), Some(true));
    }

    #[test]
    fn empty_list_after_enumeration_is_not_fullscreen() {
        let mut w: Windows<u32> = Windows::new();
        assert_eq!(w.value(), None);
        w.enumerated();
        assert_eq!(w.value(), Some(false));
        let mut w = Windows::new();
        w.add(7);
        w.enumerated();
        assert_eq!(w.value(), None, "com janela, espera o estado dela");
    }

    #[test]
    fn states_array_parsed_natively_and_odd_tail_ignored() {
        let mut raw = Vec::new();
        for v in [0u32, 2, 3] {
            raw.extend_from_slice(&v.to_ne_bytes());
        }
        assert_eq!(parse_states(&raw), (true, true));
        assert_eq!(parse_states(&3u32.to_ne_bytes()), (false, true));
        assert_eq!(parse_states(&[2, 0]), (false, false));
        assert_eq!(parse_states(&[]), (false, false));
    }
}

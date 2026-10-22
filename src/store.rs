//! Núcleo puro do protocolo: ids, pilha visível, lugar da pílula, lista oculta e sinais.
//! Sem I/O e sem tempo: quem chama decide quando algo expira.
//!
//! Regra de fechamento: expirar ou sair da pilha não emite sinal (a notificação vai para a lista
//! oculta); só × (motivo 2), ação (motivo 2) e `CloseNotification` (motivo 3) emitem
//! `NotificationClosed`. Avisos transient (toda pílula inclusive) emitem motivo 1 ao sair da tela
//! e nunca entram na lista oculta.

use std::collections::VecDeque;

use crate::model::{Action, Hints, Notification};

/// Motivo do sinal `NotificationClosed` (valores da spec 1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    Expired = 1,
    Dismissed = 2,
    Closed = 3,
}

/// O que quem chama precisa fazer: mexer na tela (`Show`/`Update`/`Hide`) ou emitir sinais.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Show(u32),
    Update(u32),
    Hide(u32),
    Closed(u32, CloseReason),
    ActivationToken(u32, String),
    ActionInvoked(u32, String),
}

/// Uma chamada `Notify` já decodificada, ainda sem id.
#[derive(Debug, Clone, PartialEq)]
pub struct NewNotification {
    pub app_name: String,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub actions: Vec<Action>,
    pub hints: Hints,
    pub expire_timeout: i32,
}

impl NewNotification {
    fn into_notification(self, id: u32) -> Notification {
        Notification {
            id,
            app_name: self.app_name,
            app_icon: self.app_icon,
            summary: self.summary,
            body: self.body,
            actions: self.actions,
            hints: self.hints,
            expire_timeout: self.expire_timeout,
        }
    }
}

/// Tamanho máximo da lista oculta; a mais antiga sai primeiro, sem sinal.
pub const HIDDEN_MAX: usize = 200;

/// Onde uma notificação está.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    Stack(usize),
    Pill,
    Hidden(usize),
}

pub struct Store {
    next_id: u32,
    /// Cartões visíveis, mais antigo primeiro.
    stack: Vec<Notification>,
    /// Lugar próprio da pílula, fora do limite da pilha.
    pill: Option<Notification>,
    /// Lista oculta, mais antiga primeiro, sem dados de imagem.
    hidden: VecDeque<Notification>,
    max_visible: usize,
}

impl Store {
    /// `max_visible` 0 vale 1: um cartão novo sempre aparece.
    pub fn new(max_visible: usize) -> Store {
        Store {
            next_id: 1,
            stack: Vec::new(),
            pill: None,
            hidden: VecDeque::new(),
            max_visible: max_visible.max(1),
        }
    }

    #[cfg(test)]
    pub fn with_next_id(max_visible: usize, next: u32) -> Store {
        let mut s = Store::new(max_visible);
        s.next_id = next;
        s
    }

    /// Muda o limite da pilha; as mais antigas que sobrarem saem como se a pilha enchesse.
    pub fn set_max_visible(&mut self, n: usize) -> Vec<Effect> {
        self.max_visible = n.max(1);
        let mut effects = Vec::new();
        while self.stack.len() > self.max_visible {
            self.evict_oldest(&mut effects);
        }
        effects
    }

    /// Aceita uma notificação. Sempre devolve um id válido (nunca 0).
    pub fn notify(&mut self, n: NewNotification, replaces_id: u32, dnd: bool) -> (u32, Vec<Effect>) {
        let target = if replaces_id != 0 {
            Some(replaces_id)
        } else {
            n.hints.sync_key.as_deref().and_then(|k| self.visible_with_sync_key(k))
        };
        let Some(id) = target else {
            let id = self.alloc_id();
            return (id, self.place(n.into_notification(id), dnd));
        };
        let new = n.into_notification(id);
        let effects = match self.find(id) {
            Some(Place::Stack(i)) if !new.is_pill() => self.update_at(Place::Stack(i), new),
            Some(Place::Pill) if new.is_pill() => self.update_at(Place::Pill, new),
            Some(place @ (Place::Stack(_) | Place::Pill)) => {
                // Mudou de tipo (cartão ↔ pílula): sai do lugar antigo sem sinal e entra no novo.
                self.take(place);
                let mut effects = vec![Effect::Hide(id)];
                effects.extend(self.place(new, dnd));
                effects
            }
            Some(place @ Place::Hidden(_)) => {
                self.take(place);
                self.place(new, dnd)
            }
            None => self.place(new, dnd),
        };
        (id, effects)
    }

    /// O tempo de tela acabou: sai da tela sem sinal (transient: motivo 1 e some).
    pub fn expire(&mut self, id: u32) -> Vec<Effect> {
        let mut effects = Vec::new();
        match self.find(id) {
            Some(place @ (Place::Stack(_) | Place::Pill)) => {
                if let Some(n) = self.take(place) {
                    effects.push(Effect::Hide(id));
                    self.retire(n, &mut effects);
                }
            }
            Some(Place::Hidden(_)) | None => {}
        }
        effects
    }

    /// × do usuário: motivo 2.
    pub fn dismiss(&mut self, id: u32) -> Vec<Effect> {
        self.remove_with_reason(id, CloseReason::Dismissed)
    }

    /// `CloseNotification`: motivo 3.
    pub fn close(&mut self, id: u32) -> Vec<Effect> {
        self.remove_with_reason(id, CloseReason::Closed)
    }

    /// Clique ou botão de ação. Chave inexistente → nada; `"default"` sem ação default → dispensar.
    pub fn invoke(&mut self, id: u32, key: &str, token: Option<String>) -> Vec<Effect> {
        let Some(place) = self.find(id) else {
            return Vec::new();
        };
        let has_key = self.at(place).is_some_and(|n| n.actions.iter().any(|a| a.key == key));
        if !has_key {
            return if key == "default" { self.dismiss(id) } else { Vec::new() };
        }
        let mut effects = Vec::new();
        if let Some(t) = token {
            effects.push(Effect::ActivationToken(id, t));
        }
        effects.push(Effect::ActionInvoked(id, key.to_owned()));
        if !matches!(place, Place::Hidden(_)) {
            effects.push(Effect::Hide(id));
        }
        self.take(place);
        effects.push(Effect::Closed(id, CloseReason::Dismissed));
        effects
    }

    /// Tira tudo da tela (ex.: Central aberta): cartões vão para a oculta sem sinal; pílula fecha com motivo 1.
    pub fn hide_all(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        for n in std::mem::take(&mut self.stack) {
            effects.push(Effect::Hide(n.id));
            self.retire(n, &mut effects);
        }
        if let Some(n) = self.pill.take() {
            effects.push(Effect::Hide(n.id));
            self.retire(n, &mut effects);
        }
        effects
    }

    /// Visível, pílula ou oculta.
    pub fn get(&self, id: u32) -> Option<&Notification> {
        self.find(id).and_then(|p| self.at(p))
    }

    /// Ids visíveis, mais antiga primeiro.
    pub fn stack(&self) -> Vec<u32> {
        self.stack.iter().map(|n| n.id).collect()
    }

    pub fn pill(&self) -> Option<u32> {
        self.pill.as_ref().map(|n| n.id)
    }

    /// Id do cartão visível ou da pílula com essa chave de sincronia.
    pub fn visible_with_sync_key(&self, key: &str) -> Option<u32> {
        self.stack
            .iter()
            .chain(self.pill.iter())
            .find(|n| n.hints.sync_key.as_deref() == Some(key))
            .map(|n| n.id)
    }

    // --- internos ---

    /// Próximo id livre: nunca 0, volta para 1 depois de `u32::MAX`, pula ids em uso
    /// (um `replaces_id` desconhecido pode ter ocupado um id adiante).
    fn alloc_id(&mut self) -> u32 {
        loop {
            let id = self.next_id;
            self.next_id = if id == u32::MAX { 1 } else { id + 1 };
            if id != 0 && self.find(id).is_none() {
                return id;
            }
        }
    }

    fn find(&self, id: u32) -> Option<Place> {
        if let Some(i) = self.stack.iter().position(|n| n.id == id) {
            return Some(Place::Stack(i));
        }
        if self.pill.as_ref().is_some_and(|n| n.id == id) {
            return Some(Place::Pill);
        }
        self.hidden.iter().position(|n| n.id == id).map(Place::Hidden)
    }

    fn at(&self, place: Place) -> Option<&Notification> {
        match place {
            Place::Stack(i) => self.stack.get(i),
            Place::Pill => self.pill.as_ref(),
            Place::Hidden(i) => self.hidden.get(i),
        }
    }

    fn take(&mut self, place: Place) -> Option<Notification> {
        match place {
            Place::Stack(i) => (i < self.stack.len()).then(|| self.stack.remove(i)),
            Place::Pill => self.pill.take(),
            Place::Hidden(i) => self.hidden.remove(i),
        }
    }

    fn update_at(&mut self, place: Place, new: Notification) -> Vec<Effect> {
        let id = new.id;
        let slot = match place {
            Place::Stack(i) => self.stack.get_mut(i),
            Place::Pill => self.pill.as_mut(),
            Place::Hidden(i) => self.hidden.get_mut(i),
        };
        match slot {
            Some(old) => {
                *old = new;
                vec![Effect::Update(id)]
            }
            None => self.place(new, false),
        }
    }

    /// Põe uma notificação nova (id já decidido, fora do store) no lugar certo.
    fn place(&mut self, n: Notification, dnd: bool) -> Vec<Effect> {
        let mut effects = Vec::new();
        let id = n.id;
        if dnd {
            self.retire(n, &mut effects);
            return effects;
        }
        if n.is_pill() {
            if let Some(old) = self.pill.take() {
                effects.push(Effect::Hide(old.id));
                self.retire(old, &mut effects);
            }
            self.pill = Some(n);
        } else {
            while self.stack.len() >= self.max_visible {
                self.evict_oldest(&mut effects);
            }
            self.stack.push(n);
        }
        effects.push(Effect::Show(id));
        effects
    }

    fn evict_oldest(&mut self, effects: &mut Vec<Effect>) {
        if self.stack.is_empty() {
            return;
        }
        let old = self.stack.remove(0);
        effects.push(Effect::Hide(old.id));
        self.retire(old, effects);
    }

    /// Saiu da tela sem ação do usuário: transient fecha com motivo 1; o resto vai para a oculta.
    fn retire(&mut self, mut n: Notification, effects: &mut Vec<Effect>) {
        if n.is_transient() {
            effects.push(Effect::Closed(n.id, CloseReason::Expired));
            return;
        }
        n.hints.image = None;
        self.hidden.push_back(n);
        while self.hidden.len() > HIDDEN_MAX {
            self.hidden.pop_front();
        }
    }

    fn remove_with_reason(&mut self, id: u32, reason: CloseReason) -> Vec<Effect> {
        let Some(place) = self.find(id) else {
            return Vec::new();
        };
        let mut effects = Vec::new();
        if !matches!(place, Place::Hidden(_)) {
            effects.push(Effect::Hide(id));
        }
        self.take(place);
        effects.push(Effect::Closed(id, reason));
        effects
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Action, Estilo, Hints, ImageHint, RawImage};

    fn nn(summary: &str) -> NewNotification {
        NewNotification {
            app_name: "app".into(),
            app_icon: String::new(),
            summary: summary.into(),
            body: String::new(),
            actions: Vec::new(),
            hints: Hints::default(),
            expire_timeout: -1,
        }
    }

    fn with_actions(summary: &str, keys: &[&str]) -> NewNotification {
        let mut n = nn(summary);
        n.actions = keys.iter().map(|k| Action { key: (*k).into(), label: format!("rótulo {k}") }).collect();
        n
    }

    fn transient(summary: &str) -> NewNotification {
        let mut n = nn(summary);
        n.hints.transient = true;
        n
    }

    fn pill(summary: &str, key: Option<&str>) -> NewNotification {
        let mut n = nn(summary);
        n.hints.estilo = Some(Estilo::Osd);
        n.hints.sync_key = key.map(Into::into);
        n
    }

    fn synced(summary: &str, key: &str) -> NewNotification {
        let mut n = nn(summary);
        n.hints.sync_key = Some(key.into());
        n
    }

    fn with_image(summary: &str) -> NewNotification {
        let mut n = nn(summary);
        n.hints.image = Some(ImageHint::Data(RawImage {
            width: 1, height: 1, rowstride: 4, has_alpha: true, bits_per_sample: 8, channels: 4,
            data: vec![1, 2, 3, 4],
        }));
        n
    }

    fn has_closed(effects: &[Effect]) -> bool {
        effects.iter().any(|e| matches!(e, Effect::Closed(..)))
    }

    #[test]
    fn ids_start_at_one_and_increase() {
        let mut s = Store::new(4);
        assert_eq!(s.notify(nn("a"), 0, false), (1, vec![Effect::Show(1)]));
        assert_eq!(s.notify(nn("b"), 0, false), (2, vec![Effect::Show(2)]));
        assert_eq!(s.notify(nn("c"), 0, false), (3, vec![Effect::Show(3)]));
        assert_eq!(s.stack(), vec![1, 2, 3]);
    }

    #[test]
    fn ids_wrap_skipping_zero() {
        let mut s = Store::with_next_id(4, u32::MAX);
        assert_eq!(s.notify(nn("a"), 0, false).0, u32::MAX);
        assert_eq!(s.notify(nn("b"), 0, false).0, 1);
        assert_eq!(s.stack(), vec![u32::MAX, 1]);
    }

    #[test]
    fn replaces_visible_keeps_id_and_updates() {
        let mut s = Store::new(4);
        s.notify(nn("velho"), 0, false);
        s.notify(nn("outro"), 0, false);
        assert_eq!(s.notify(nn("novo"), 1, false), (1, vec![Effect::Update(1)]));
        assert_eq!(s.get(1).map(|n| n.summary.as_str()), Some("novo"));
        assert_eq!(s.stack(), vec![1, 2]);
    }

    #[test]
    fn replaces_hidden_shows_again_same_id() {
        let mut s = Store::new(4);
        s.notify(nn("velho"), 0, false);
        assert_eq!(s.expire(1), vec![Effect::Hide(1)]);
        assert_eq!(s.notify(nn("novo"), 1, false), (1, vec![Effect::Show(1)]));
        assert_eq!(s.stack(), vec![1]);
        assert_eq!(s.get(1).map(|n| n.summary.as_str()), Some("novo"));
        // Não ficou cópia na lista oculta: dispensar some com ela de vez.
        assert_eq!(s.dismiss(1), vec![Effect::Hide(1), Effect::Closed(1, CloseReason::Dismissed)]);
        assert!(s.get(1).is_none());
    }

    #[test]
    fn replaces_unknown_id_creates_with_that_id() {
        let mut s = Store::new(4);
        assert_eq!(s.notify(nn("a"), 77, false), (77, vec![Effect::Show(77)]));
        assert_eq!(s.get(77).map(|n| n.id), Some(77));
        assert_eq!(s.notify(nn("b"), 0, false).0, 1);
    }

    #[test]
    fn sync_key_replaces_visible_in_place() {
        let mut s = Store::new(4);
        s.notify(synced("vol 10", "vol"), 0, false);
        s.notify(nn("outro"), 0, false);
        assert_eq!(s.notify(synced("vol 20", "vol"), 0, false), (1, vec![Effect::Update(1)]));
        assert_eq!(s.stack(), vec![1, 2]);
        assert_eq!(s.get(1).map(|n| n.summary.as_str()), Some("vol 20"));
        assert_eq!(s.visible_with_sync_key("vol"), Some(1));
        assert_eq!(s.visible_with_sync_key("bri"), None);
    }

    #[test]
    fn fifth_card_pushes_oldest_to_hidden_without_signal() {
        let mut s = Store::new(4);
        for i in 1..=4 {
            s.notify(nn(&format!("n{i}")), 0, false);
        }
        let (id, effects) = s.notify(nn("n5"), 0, false);
        assert_eq!(id, 5);
        assert_eq!(effects, vec![Effect::Hide(1), Effect::Show(5)]);
        assert!(!has_closed(&effects));
        assert!(s.get(1).is_some());
        assert_eq!(s.stack(), vec![2, 3, 4, 5]);
    }

    #[test]
    fn burst_keeps_four_visible_and_all_ids_unique() {
        let mut s = Store::new(4);
        let mut ids = Vec::new();
        for i in 0..50 {
            let (id, effects) = s.notify(nn(&format!("n{i}")), 0, false);
            assert!(!has_closed(&effects));
            ids.push(id);
        }
        assert_eq!(s.stack().len(), 4);
        assert_eq!(ids, (1..=50).collect::<Vec<u32>>());
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 50);
        assert!(s.get(1).is_some());
        assert!(ids.iter().all(|id| s.get(*id).is_some()));
    }

    #[test]
    fn expire_hides_without_signal_and_stays_invokable() {
        let mut s = Store::new(4);
        s.notify(with_actions("a", &["default"]), 0, false);
        assert_eq!(s.expire(1), vec![Effect::Hide(1)]);
        assert!(s.stack().is_empty());
        assert!(s.get(1).is_some());
        assert_eq!(
            s.invoke(1, "default", None),
            vec![Effect::ActionInvoked(1, "default".into()), Effect::Closed(1, CloseReason::Dismissed)]
        );
        assert!(s.get(1).is_none());
    }

    #[test]
    fn transient_expire_emits_reason_1_and_forgets() {
        let mut s = Store::new(4);
        s.notify(transient("a"), 0, false);
        assert_eq!(s.expire(1), vec![Effect::Hide(1), Effect::Closed(1, CloseReason::Expired)]);
        assert!(s.get(1).is_none());
        assert_eq!(s.expire(1), vec![]);
    }

    #[test]
    fn dismiss_visible_and_hidden_reason_2() {
        let mut s = Store::new(4);
        s.notify(nn("a"), 0, false);
        s.notify(nn("b"), 0, false);
        s.expire(1);
        assert_eq!(s.dismiss(2), vec![Effect::Hide(2), Effect::Closed(2, CloseReason::Dismissed)]);
        assert_eq!(s.dismiss(1), vec![Effect::Closed(1, CloseReason::Dismissed)]);
        assert!(s.get(1).is_none() && s.get(2).is_none());
        assert_eq!(s.dismiss(1), vec![]);
    }

    #[test]
    fn close_reason_3_visible_and_hidden_unknown_silent() {
        let mut s = Store::new(4);
        s.notify(nn("a"), 0, false);
        s.notify(nn("b"), 0, false);
        s.expire(1);
        assert_eq!(s.close(2), vec![Effect::Hide(2), Effect::Closed(2, CloseReason::Closed)]);
        assert_eq!(s.close(1), vec![Effect::Closed(1, CloseReason::Closed)]);
        assert!(s.get(1).is_none() && s.get(2).is_none());
        assert_eq!(s.close(99), vec![]);
    }

    #[test]
    fn invoke_with_token_emits_token_first() {
        let mut s = Store::new(4);
        s.notify(with_actions("a", &["default", "reply"]), 0, false);
        assert_eq!(
            s.invoke(1, "reply", Some("t".into())),
            vec![
                Effect::ActivationToken(1, "t".into()),
                Effect::ActionInvoked(1, "reply".into()),
                Effect::Hide(1),
                Effect::Closed(1, CloseReason::Dismissed),
            ]
        );
        assert!(s.get(1).is_none());
        assert!(s.stack().is_empty());
    }

    #[test]
    fn invoke_unknown_action_is_ignored() {
        let mut s = Store::new(4);
        s.notify(with_actions("a", &["reply"]), 0, false);
        assert_eq!(s.invoke(1, "nao-existe", None), vec![]);
        assert_eq!(s.stack(), vec![1]);
    }

    #[test]
    fn invoke_unknown_id_is_silent() {
        let mut s = Store::new(4);
        s.notify(with_actions("a", &["default"]), 0, false);
        assert_eq!(s.invoke(42, "default", Some("t".into())), vec![]);
        assert_eq!(s.stack(), vec![1]);
    }

    #[test]
    fn default_without_default_action_dismisses() {
        let mut s = Store::new(4);
        s.notify(with_actions("a", &["reply"]), 0, false);
        assert_eq!(
            s.invoke(1, "default", Some("t".into())),
            vec![Effect::Hide(1), Effect::Closed(1, CloseReason::Dismissed)]
        );
        assert!(s.get(1).is_none());
        // Também na lista oculta.
        s.notify(nn("b"), 0, false);
        s.expire(2);
        assert_eq!(s.invoke(2, "default", None), vec![Effect::Closed(2, CloseReason::Dismissed)]);
        assert!(s.get(2).is_none());
    }

    #[test]
    fn hidden_list_caps_at_200_and_drops_images() {
        let mut s = Store::new(4);
        for i in 1..=205u32 {
            let (id, _) = s.notify(with_image(&format!("n{i}")), 0, false);
            assert_eq!(id, i);
            assert_eq!(s.expire(id), vec![Effect::Hide(id)]);
        }
        assert_eq!(HIDDEN_MAX, 200);
        for id in 1..=5 {
            assert!(s.get(id).is_none(), "id {id} deveria ter saído da lista oculta");
        }
        for id in 6..=205 {
            let n = s.get(id);
            assert!(n.is_some(), "id {id} deveria estar na lista oculta");
            assert_eq!(n.and_then(|n| n.hints.image.as_ref()), None);
        }
    }

    #[test]
    fn dnd_accepts_without_showing() {
        let mut s = Store::new(4);
        assert_eq!(s.notify(with_actions("a", &["default"]), 0, true), (1, vec![]));
        assert!(s.stack().is_empty());
        assert!(s.get(1).is_some());
        assert_eq!(
            s.invoke(1, "default", None),
            vec![Effect::ActionInvoked(1, "default".into()), Effect::Closed(1, CloseReason::Dismissed)]
        );
        assert!(s.get(1).is_none());
    }

    #[test]
    fn dnd_transient_closes_immediately() {
        let mut s = Store::new(4);
        assert_eq!(s.notify(transient("a"), 0, true), (1, vec![Effect::Closed(1, CloseReason::Expired)]));
        assert!(s.get(1).is_none());
        assert_eq!(s.notify(pill("vol", Some("vol")), 0, true), (2, vec![Effect::Closed(2, CloseReason::Expired)]));
        assert_eq!(s.pill(), None);
    }

    #[test]
    fn hide_all_moves_visible_to_hidden() {
        let mut s = Store::new(4);
        s.notify(nn("a"), 0, false);
        s.notify(nn("b"), 0, false);
        let effects = s.hide_all();
        assert_eq!(effects, vec![Effect::Hide(1), Effect::Hide(2)]);
        assert!(s.stack().is_empty());
        assert!(s.get(1).is_some() && s.get(2).is_some());

        s.notify(nn("c"), 0, false);
        s.notify(pill("vol", None), 0, false);
        assert_eq!(s.hide_all(), vec![Effect::Hide(3), Effect::Hide(4), Effect::Closed(4, CloseReason::Expired)]);
        assert_eq!(s.pill(), None);
        assert!(s.get(4).is_none());
        assert!(s.get(3).is_some());
        assert_eq!(s.hide_all(), vec![]);
    }

    #[test]
    fn pill_slot_is_separate_and_replaced() {
        let mut s = Store::new(4);
        for i in 1..=4 {
            s.notify(nn(&format!("n{i}")), 0, false);
        }
        assert_eq!(s.notify(pill("vol", None), 0, false), (5, vec![Effect::Show(5)]));
        assert_eq!(s.stack(), vec![1, 2, 3, 4]);
        assert_eq!(s.pill(), Some(5));
        assert_eq!(
            s.notify(pill("bri", None), 0, false),
            (6, vec![Effect::Hide(5), Effect::Closed(5, CloseReason::Expired), Effect::Show(6)])
        );
        assert_eq!(s.pill(), Some(6));
        assert!(s.get(5).is_none());
        assert_eq!(s.stack(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn pill_same_sync_key_updates_same_id() {
        let mut s = Store::new(4);
        assert_eq!(s.notify(pill("vol 10", Some("vol")), 0, false), (1, vec![Effect::Show(1)]));
        assert_eq!(s.notify(pill("vol 20", Some("vol")), 0, false), (1, vec![Effect::Update(1)]));
        assert_eq!(s.pill(), Some(1));
        assert_eq!(s.get(1).map(|n| n.summary.as_str()), Some("vol 20"));
        assert_eq!(s.visible_with_sync_key("vol"), Some(1));
    }

    #[test]
    fn set_max_visible_shrinks_stack() {
        let mut s = Store::new(4);
        for i in 1..=4 {
            s.notify(nn(&format!("n{i}")), 0, false);
        }
        assert_eq!(s.set_max_visible(2), vec![Effect::Hide(1), Effect::Hide(2)]);
        assert_eq!(s.stack(), vec![3, 4]);
        assert!(s.get(1).is_some() && s.get(2).is_some());
        assert_eq!(s.notify(nn("n5"), 0, false), (5, vec![Effect::Hide(3), Effect::Show(5)]));
    }

    // Extras: regras do brief que os 23 acima não fixam.

    #[test]
    fn fresh_ids_skip_ids_in_use() {
        let mut s = Store::new(4);
        assert_eq!(s.notify(nn("a"), 2, false).0, 2);
        assert_eq!(s.notify(nn("b"), 0, false).0, 1);
        assert_eq!(s.notify(nn("c"), 0, false).0, 3);
        assert_eq!(s.stack(), vec![2, 1, 3]);
    }

    #[test]
    fn replacing_card_with_pill_moves_it_to_pill_slot() {
        let mut s = Store::new(4);
        s.notify(nn("cartão"), 0, false);
        assert_eq!(s.notify(pill("vol", None), 1, false), (1, vec![Effect::Hide(1), Effect::Show(1)]));
        assert!(s.stack().is_empty());
        assert_eq!(s.pill(), Some(1));
        assert_eq!(s.get(1).map(|n| n.summary.as_str()), Some("vol"));
    }

    #[test]
    fn full_stack_with_transient_oldest_closes_it() {
        let mut s = Store::new(1);
        s.notify(transient("a"), 0, false);
        assert_eq!(
            s.notify(nn("b"), 0, false),
            (2, vec![Effect::Hide(1), Effect::Closed(1, CloseReason::Expired), Effect::Show(2)])
        );
        assert!(s.get(1).is_none());
    }

    #[test]
    fn max_visible_zero_is_treated_as_one() {
        let mut s = Store::new(0);
        assert_eq!(s.notify(nn("a"), 0, false), (1, vec![Effect::Show(1)]));
        assert_eq!(s.notify(nn("b"), 0, false), (2, vec![Effect::Hide(1), Effect::Show(2)]));
        assert_eq!(s.stack(), vec![2]);
    }
}

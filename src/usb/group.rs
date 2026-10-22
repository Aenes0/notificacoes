//! Agrupamento dos avisos de USB: eventos próximos viram um cartão só; um evento com o cartão de
//! grupo ainda na tela soma aos nomes dele. Textos, ícone e a chamada `Notify`. Pura.

use std::time::{Duration, Instant};

use crate::dbus::server::NotifyArgs;
use crate::model::{HintValue, UsbKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Announcement {
    pub change: Change,
    pub names: Vec<String>,
    pub kinds: Vec<UsbKind>,
}

/// Um evento até `GROUP_WINDOW` depois do anterior entra no mesmo cartão.
pub const GROUP_WINDOW: Duration = Duration::from_millis(1500);
/// Teto da janela, contado do primeiro evento: um dispositivo que pisca não adia o aviso para sempre.
pub const GROUP_MAX: Duration = Duration::from_millis(6000);

/// Mais de tantos nomes: o corpo resume (`"A, B… e mais N"`).
const LIST_MAX: usize = 5;
const APP_NAME: &str = "Dispositivos USB";
const MIXED_ICON: &str = "media-removable-symbolic";

/// Prazo de um grupo aberto em `open` cujo último evento foi em `last`.
pub fn window_end(open: Instant, last: Instant) -> Instant {
    (last + GROUP_WINDOW).min(open + GROUP_MAX)
}

const CHANGES: [Change; 2] = [Change::Added, Change::Removed];

fn slot(c: Change) -> usize {
    match c {
        Change::Added => 0,
        Change::Removed => 1,
    }
}

/// Eventos de uma mudança esperando o prazo.
struct Batch {
    open: Instant,
    last: Instant,
    names: Vec<String>,
    kinds: Vec<UsbKind>,
}

pub struct Grouper {
    pending: [Option<Batch>; 2],
    /// Nomes e tipos do último cartão de cada mudança (para somar enquanto ele está na tela).
    shown: [(Vec<String>, Vec<UsbKind>); 2],
}

impl Grouper {
    pub fn new() -> Grouper {
        Grouper { pending: [None, None], shown: [(Vec::new(), Vec::new()), (Vec::new(), Vec::new())] }
    }

    /// Junta o evento ao grupo da mudança (abre um se não houver) e devolve o prazo desse grupo.
    pub fn push(&mut self, change: Change, name: String, kind: UsbKind, now: Instant) -> Instant {
        let batch = self.pending[slot(change)].get_or_insert_with(|| Batch { open: now, last: now, names: Vec::new(), kinds: Vec::new() });
        batch.last = batch.last.max(now);
        batch.names.push(name);
        batch.kinds.push(kind);
        window_end(batch.open, batch.last)
    }

    /// Prazo mais próximo entre os grupos abertos; `None` = nada pendente (sem timer).
    pub fn deadline(&self) -> Option<Instant> {
        self.pending.iter().flatten().map(|b| window_end(b.open, b.last)).min()
    }

    /// Fecha os grupos vencidos. `visible(change)`: o cartão de grupo dessa mudança ainda está na
    /// tela → soma aos nomes dele.
    pub fn flush(&mut self, now: Instant, visible: impl Fn(Change) -> bool) -> Vec<Announcement> {
        let mut out = Vec::new();
        for change in CHANGES {
            let i = slot(change);
            if !self.pending[i].as_ref().is_some_and(|b| window_end(b.open, b.last) <= now) {
                continue;
            }
            let Some(batch) = self.pending[i].take() else { continue };
            let (mut names, mut kinds) = if visible(change) { std::mem::take(&mut self.shown[i]) } else { (Vec::new(), Vec::new()) };
            names.extend(batch.names);
            kinds.extend(batch.kinds);
            self.shown[i] = (names.clone(), kinds.clone());
            out.push(Announcement { change, names, kinds });
        }
        out
    }
}

fn kind_label(k: UsbKind) -> &'static str {
    match k {
        UsbKind::Controle => "Controle",
        UsbKind::Armazenamento => "Armazenamento USB",
        UsbKind::Celular => "Celular",
        UsbKind::Audio => "Dispositivo de áudio",
        UsbKind::Teclado => "Teclado",
        UsbKind::Mouse => "Mouse",
        UsbKind::Generico => "Dispositivo USB",
    }
}

fn kind_icon(k: UsbKind) -> &'static str {
    match k {
        UsbKind::Controle => "input-gaming-symbolic",
        UsbKind::Armazenamento => "drive-removable-media-symbolic",
        UsbKind::Celular => "phone-symbolic",
        UsbKind::Audio => "audio-headphones-symbolic",
        UsbKind::Teclado => "input-keyboard-symbolic",
        UsbKind::Mouse => "input-mouse-symbolic",
        UsbKind::Generico => MIXED_ICON,
    }
}

/// O tipo comum a todos, se houver um só.
fn common_kind(a: &Announcement) -> Option<UsbKind> {
    let first = *a.kinds.first()?;
    a.kinds.iter().all(|k| *k == first).then_some(first)
}

pub fn title(a: &Announcement) -> String {
    let verb = match a.change {
        Change::Added => "conectado",
        Change::Removed => "desconectado",
    };
    match (a.names.len(), a.kinds.first()) {
        (0 | 1, Some(kind)) => format!("{} {verb}", kind_label(*kind)),
        (0 | 1, None) => format!("{} {verb}", kind_label(UsbKind::Generico)),
        (n, _) => format!("{n} dispositivos {verb}s"),
    }
}

pub fn body(a: &Announcement) -> String {
    match a.names.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [first, second, ..] if a.names.len() > LIST_MAX => format!("{first}, {second}… e mais {}", a.names.len() - 2),
        [init @ .., last] => format!("{} e {last}", init.join(", ")),
    }
}

pub fn icon_name(a: &Announcement) -> &'static str {
    common_kind(a).map_or(MIXED_ICON, kind_icon)
}

pub fn sync_key(c: Change) -> &'static str {
    match c {
        Change::Added => "notificacoes-usb-conectado",
        Change::Removed => "notificacoes-usb-desconectado",
    }
}

/// Chamada `Notify` do aviso: compacta, com a categoria (som) e a chave de sincronia (o cartão de
/// grupo visível é substituído no lugar); remoções em tom apagado.
pub fn to_notify(a: &Announcement) -> NotifyArgs {
    let s = |v: &str| HintValue::Str(v.to_owned());
    let category = match a.change {
        Change::Added => "device.added",
        Change::Removed => "device.removed",
    };
    let mut hints = vec![
        ("x-estilo".to_owned(), s("compacto")),
        ("category".to_owned(), s(category)),
        ("synchronous".to_owned(), s(sync_key(a.change))),
    ];
    if a.change == Change::Removed {
        hints.push(("x-tom".to_owned(), s("apagado")));
    }
    NotifyArgs {
        app_name: APP_NAME.to_owned(),
        replaces_id: 0,
        app_icon: icon_name(a).to_owned(),
        summary: title(a),
        body: body(a),
        hints,
        expire_timeout: -1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ann(change: Change, items: &[(&str, UsbKind)]) -> Announcement {
        Announcement {
            change,
            names: items.iter().map(|(n, _)| (*n).to_owned()).collect(),
            kinds: items.iter().map(|(_, k)| *k).collect(),
        }
    }

    fn names(n: usize) -> Vec<(String, UsbKind)> {
        (0..n).map(|i| (char::from(b'A' + i as u8).to_string(), UsbKind::Generico)).collect()
    }

    fn ann_n(change: Change, n: usize) -> Announcement {
        let items = names(n);
        let refs: Vec<(&str, UsbKind)> = items.iter().map(|(s, k)| (s.as_str(), *k)).collect();
        ann(change, &refs)
    }

    const NOT_VISIBLE: fn(Change) -> bool = |_| false;

    #[test]
    fn single_device_title_by_kind() {
        let a = ann(Change::Added, &[("8BitDo Pro 2", UsbKind::Controle)]);
        assert_eq!(title(&a), "Controle conectado");
        assert_eq!(body(&a), "8BitDo Pro 2");
        assert_eq!(icon_name(&a), "input-gaming-symbolic");
        let cases = [
            (UsbKind::Armazenamento, "Armazenamento USB conectado", "drive-removable-media-symbolic"),
            (UsbKind::Celular, "Celular conectado", "phone-symbolic"),
            (UsbKind::Audio, "Dispositivo de áudio conectado", "audio-headphones-symbolic"),
            (UsbKind::Teclado, "Teclado conectado", "input-keyboard-symbolic"),
            (UsbKind::Mouse, "Mouse conectado", "input-mouse-symbolic"),
            (UsbKind::Generico, "Dispositivo USB conectado", "media-removable-symbolic"),
        ];
        for (kind, t, icon) in cases {
            let a = ann(Change::Added, &[("X", kind)]);
            assert_eq!(title(&a), t);
            assert_eq!(icon_name(&a), icon);
        }
    }

    #[test]
    fn removed_single() {
        let a = ann(Change::Removed, &[("SanDisk Ultra", UsbKind::Armazenamento)]);
        assert_eq!(title(&a), "Armazenamento USB desconectado");
        assert_eq!(body(&a), "SanDisk Ultra");
        assert_eq!(icon_name(&a), "drive-removable-media-symbolic");
    }

    #[test]
    fn three_devices_list_names() {
        let a = ann_n(Change::Added, 3);
        assert_eq!(title(&a), "3 dispositivos conectados");
        assert_eq!(body(&a), "A, B e C");
        assert_eq!(title(&ann_n(Change::Removed, 3)), "3 dispositivos desconectados");
    }

    #[test]
    fn two_devices_use_e() {
        let a = ann_n(Change::Added, 2);
        assert_eq!(title(&a), "2 dispositivos conectados");
        assert_eq!(body(&a), "A e B");
    }

    #[test]
    fn eight_devices_two_names_and_count() {
        let a = ann_n(Change::Added, 8);
        assert_eq!(title(&a), "8 dispositivos conectados");
        assert_eq!(body(&a), "A, B… e mais 6");
        // Limite: 5 ainda lista todos; 6 já resume.
        assert_eq!(body(&ann_n(Change::Added, 5)), "A, B, C, D e E");
        assert_eq!(body(&ann_n(Change::Added, 6)), "A, B… e mais 4");
    }

    #[test]
    fn mixed_kinds_use_generic_icon() {
        let a = ann(Change::Added, &[("Pad", UsbKind::Controle), ("Pen", UsbKind::Armazenamento)]);
        assert_eq!(icon_name(&a), "media-removable-symbolic");
    }

    #[test]
    fn same_kind_group_uses_kind_icon() {
        let a = ann(Change::Added, &[("Pad 1", UsbKind::Controle), ("Pad 2", UsbKind::Controle)]);
        assert_eq!(icon_name(&a), "input-gaming-symbolic");
        assert_eq!(title(&a), "2 dispositivos conectados");
    }

    #[test]
    fn events_within_window_become_one() {
        let t = Instant::now();
        let mut g = Grouper::new();
        g.push(Change::Added, "A".into(), UsbKind::Controle, t);
        g.push(Change::Added, "B".into(), UsbKind::Mouse, t + Duration::from_millis(500));
        let due = g.push(Change::Added, "C".into(), UsbKind::Teclado, t + Duration::from_millis(1400));
        assert_eq!(due, t + Duration::from_millis(2900), "a janela conta do último evento");
        assert!(g.flush(t + Duration::from_millis(2000), NOT_VISIBLE).is_empty(), "antes do prazo nada sai");
        let out = g.flush(t + Duration::from_millis(2900), NOT_VISIBLE);
        assert_eq!(out, vec![ann(Change::Added, &[("A", UsbKind::Controle), ("B", UsbKind::Mouse), ("C", UsbKind::Teclado)])]);
        assert!(g.flush(t + Duration::from_secs(10), NOT_VISIBLE).is_empty(), "já saiu");
        assert_eq!(g.deadline(), None, "sem pendentes não há prazo (nem timer)");
    }

    #[test]
    fn window_has_a_ceiling() {
        // Um dispositivo que pisca sem parar não adia o aviso para sempre.
        let t = Instant::now();
        let mut g = Grouper::new();
        let mut due = t;
        for i in 0..20u64 {
            due = g.push(Change::Added, format!("D{i}"), UsbKind::Generico, t + Duration::from_millis(i * 1000));
        }
        assert!(due <= t + GROUP_MAX);
        assert_eq!(g.flush(t + GROUP_MAX, NOT_VISIBLE).len(), 1);
    }

    #[test]
    fn added_and_removed_are_separate_groups() {
        let t = Instant::now();
        let mut g = Grouper::new();
        g.push(Change::Added, "A".into(), UsbKind::Controle, t);
        g.push(Change::Removed, "B".into(), UsbKind::Armazenamento, t + Duration::from_millis(100));
        g.push(Change::Added, "C".into(), UsbKind::Controle, t + Duration::from_millis(200));
        assert_eq!(g.deadline(), Some(t + Duration::from_millis(1600)), "o prazo mais próximo (remoção)");
        let out = g.flush(t + Duration::from_millis(1700), NOT_VISIBLE);
        assert_eq!(out, vec![
            ann(Change::Added, &[("A", UsbKind::Controle), ("C", UsbKind::Controle)]),
            ann(Change::Removed, &[("B", UsbKind::Armazenamento)]),
        ]);
    }

    #[test]
    fn each_change_flushes_at_its_own_deadline() {
        let t = Instant::now();
        let mut g = Grouper::new();
        g.push(Change::Removed, "B".into(), UsbKind::Mouse, t);
        g.push(Change::Added, "A".into(), UsbKind::Mouse, t + Duration::from_millis(1000));
        let out = g.flush(t + Duration::from_millis(1500), NOT_VISIBLE);
        assert_eq!(out, vec![ann(Change::Removed, &[("B", UsbKind::Mouse)])]);
        assert_eq!(g.deadline(), Some(t + Duration::from_millis(2500)));
    }

    #[test]
    fn event_while_group_visible_merges() {
        let t = Instant::now();
        let mut g = Grouper::new();
        g.push(Change::Added, "A".into(), UsbKind::Controle, t);
        g.push(Change::Added, "B".into(), UsbKind::Mouse, t);
        assert_eq!(g.flush(t + GROUP_WINDOW, NOT_VISIBLE).len(), 1);
        let later = t + Duration::from_secs(3);
        g.push(Change::Added, "C".into(), UsbKind::Teclado, later);
        let out = g.flush(later + GROUP_WINDOW, |c| c == Change::Added);
        let merged = ann(Change::Added, &[("A", UsbKind::Controle), ("B", UsbKind::Mouse), ("C", UsbKind::Teclado)]);
        assert_eq!(out, vec![merged.clone()]);
        assert_eq!(title(&merged), "3 dispositivos conectados");
        // E de novo: o cartão visível agora tem os 3.
        let later = later + Duration::from_secs(3);
        g.push(Change::Added, "D".into(), UsbKind::Mouse, later);
        let out = g.flush(later + GROUP_WINDOW, |c| c == Change::Added);
        assert_eq!(out[0].names, vec!["A", "B", "C", "D"]);
        // O cartão de remoção visível não mistura com conexões.
        g.push(Change::Removed, "Z".into(), UsbKind::Mouse, later);
        let out = g.flush(later + GROUP_WINDOW, |c| c == Change::Added);
        assert_eq!(out, vec![ann(Change::Removed, &[("Z", UsbKind::Mouse)])]);
    }

    #[test]
    fn group_not_visible_starts_fresh() {
        let t = Instant::now();
        let mut g = Grouper::new();
        g.push(Change::Added, "A".into(), UsbKind::Controle, t);
        g.flush(t + GROUP_WINDOW, NOT_VISIBLE);
        let later = t + Duration::from_secs(20);
        g.push(Change::Added, "B".into(), UsbKind::Mouse, later);
        let out = g.flush(later + GROUP_WINDOW, NOT_VISIBLE);
        assert_eq!(out, vec![ann(Change::Added, &[("B", UsbKind::Mouse)])]);
        // Mesmo visível depois, o que vale é o último cartão (só B).
        g.push(Change::Added, "C".into(), UsbKind::Mouse, later);
        let out = g.flush(later + GROUP_WINDOW, |_| true);
        assert_eq!(out[0].names, vec!["B", "C"]);
    }

    fn hint<'a>(n: &'a NotifyArgs, key: &str) -> Option<&'a HintValue> {
        n.hints.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    #[test]
    fn removed_notify_is_dimmed_and_compact() {
        let s = |v: &str| Some(HintValue::Str(v.to_owned()));
        let a = ann(Change::Removed, &[("SanDisk Ultra", UsbKind::Armazenamento)]);
        let n = to_notify(&a);
        assert_eq!(n.app_name, "Dispositivos USB");
        assert_eq!(n.app_icon, "drive-removable-media-symbolic");
        assert_eq!(n.summary, "Armazenamento USB desconectado");
        assert_eq!(n.body, "SanDisk Ultra");
        assert_eq!(n.replaces_id, 0);
        assert_eq!(n.expire_timeout, -1);
        assert_eq!(hint(&n, "x-estilo").cloned(), s("compacto"));
        assert_eq!(hint(&n, "x-tom").cloned(), s("apagado"));
        assert_eq!(hint(&n, "category").cloned(), s("device.removed"));
        assert_eq!(hint(&n, "synchronous").cloned(), s("notificacoes-usb-desconectado"));

        let added = to_notify(&ann(Change::Added, &[("Pad", UsbKind::Controle)]));
        assert_eq!(hint(&added, "x-tom"), None, "conexão não é apagada");
        assert_eq!(hint(&added, "category").cloned(), s("device.added"));
        assert_eq!(hint(&added, "synchronous").cloned(), s("notificacoes-usb-conectado"));
        assert_eq!(hint(&added, "x-estilo").cloned(), s("compacto"));
        assert_eq!(sync_key(Change::Added), "notificacoes-usb-conectado");
        assert_eq!(sync_key(Change::Removed), "notificacoes-usb-desconectado");
    }

    /// As dicas passam pelo parser do daemon como compacto, apagado e com chave de sincronia.
    #[test]
    fn notify_hints_parse_back() {
        use crate::model::{Estilo, Hints, Tom};
        let n = to_notify(&ann(Change::Removed, &[("X", UsbKind::Mouse)]));
        let h = Hints::parse(n.hints);
        assert_eq!(h.estilo, Some(Estilo::Compacto));
        assert_eq!(h.tom, Some(Tom::Apagado));
        assert_eq!(h.category.as_deref(), Some("device.removed"));
        assert_eq!(h.sync_key.as_deref(), Some("notificacoes-usb-desconectado"));
    }
}

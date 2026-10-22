//! Aviso de USB embutido: monitor udev (`usb_device` + `input`) no calloop, mapa dos conhecidos,
//! agrupamento e silêncio depois da suspensão.

pub mod classify;
pub mod group;

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::time::{Duration, Instant};

use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken};

use crate::State;
use crate::config::Config;
use crate::dbus::server::notify_call;
use crate::model::{Hints, UsbKind};
use crate::store::NewNotification;
use classify::{InputHints, UsbProps, display_name, is_hub};
use group::{Announcement, Change, Grouper, window_end};

/// Depois de acordar, nada é anunciado por este tempo (o kernel refaz a enumeração).
pub const RESUME_QUIET: Duration = Duration::from_secs(5);

/// O que interessa de um evento do udev, já lido do `udev::Device`.
#[derive(Debug, Clone, PartialEq)]
pub enum UdevEvent {
    UsbAdd(UsbProps),
    UsbRemove(String),
    /// Um nó `input` apareceu (devpath dele, dicas `ID_INPUT_*`).
    Input(String, InputHints),
}

/// Um `usb_device` presente. O tipo é recalculado das propriedades e das dicas de input, que
/// podem chegar depois do `add`.
struct Known {
    name: String,
    props: UsbProps,
    hints: InputHints,
}

impl Known {
    fn kind(&self) -> UsbKind {
        classify::kind(&self.props, self.hints)
    }
}

/// `child` está dentro do dispositivo `parent` (fronteira de componente: `1-1.1` não contém `1-1.10`).
fn is_under(child: &str, parent: &str) -> bool {
    child.strip_prefix(parent).is_some_and(|rest| rest.starts_with('/'))
}

/// Estado dos avisos de USB. O socket do monitor fica na fonte do calloop; aqui, só o mapa dos
/// conhecidos e o que espera o prazo do grupo.
pub struct Usb {
    /// Por devpath: todo `usb_device` presente que não é hub, anunciado ou não.
    known: HashMap<String, Known>,
    sleeping: bool,
    /// Fim do silêncio depois de acordar.
    quiet_until: Option<Instant>,
    grouper: Grouper,
    /// Conexões a anunciar esperando as dicas de input dos filhos (devpath, quando chegou), na
    /// ordem. Entram no `Grouper` só no prazo, já com o tipo final.
    adds: Vec<(String, Instant)>,
    /// Timer do prazo mais próximo; só existe com algo pendente.
    timer: Option<(RegistrationToken, Instant)>,
}

impl Usb {
    /// Liga o monitor (`usb_device` e `input`) **antes** de enumerar, para nada se perder entre os
    /// dois; os presentes entram no mapa em silêncio (só nome e tipo).
    pub fn start(handle: &LoopHandle<'static, State>) -> io::Result<Usb> {
        let socket = udev::MonitorBuilder::new()?
            .match_subsystem_devtype("usb", "usb_device")?
            .match_subsystem("input")?
            .listen()?;
        let usb = Usb::with_present(present_devices(), present_inputs());
        handle
            .insert_source(Generic::new(socket, Interest::READ, Mode::Level), |_, socket, state: &mut State| {
                let events: Vec<UdevEvent> = socket.iter().filter_map(|e| read_event(&e)).collect();
                state.on_udev(events);
                Ok(PostAction::Continue)
            })
            .map_err(|e| io::Error::other(format!("monitor udev no laço: {}", e.error)))?;
        Ok(usb)
    }

    /// Estado com os dispositivos já presentes (sem anunciar) e os nós `input` deles.
    fn with_present(devices: Vec<UsbProps>, inputs: Vec<(String, InputHints)>) -> Usb {
        let mut usb = Usb {
            known: HashMap::new(),
            sleeping: false,
            quiet_until: None,
            grouper: Grouper::new(),
            adds: Vec::new(),
            timer: None,
        };
        for props in devices.into_iter().filter(|p| !is_hub(p)) {
            let name = display_name(&props);
            usb.known.insert(props.devpath.clone(), Known { name, props, hints: InputHints::default() });
        }
        for (path, hints) in inputs {
            usb.add_hints(&path, hints);
        }
        usb
    }

    /// `LogindEvent::Sleep`: dormindo, e até `RESUME_QUIET` depois de acordar, nada é anunciado
    /// (só o mapa é atualizado). Comparação na hora de cada evento, sem timer.
    pub fn on_sleep(&mut self, sleeping: bool, now: Instant) {
        self.sleeping = sleeping;
        if sleeping {
            // Conexão que ainda esperava as dicas de input não pode ser anunciada ao acordar.
            self.adds.clear();
        } else {
            self.quiet_until = Some(now + RESUME_QUIET);
        }
    }

    fn may_announce(&self, now: Instant, cfg: &Config) -> bool {
        cfg.usb && !self.sleeping && self.quiet_until.is_none_or(|q| now >= q)
    }

    fn add_hints(&mut self, input_path: &str, h: InputHints) {
        if let Some(k) = self.known.iter_mut().find(|(dev, _)| is_under(input_path, dev)).map(|(_, k)| k) {
            k.hints.joystick |= h.joystick;
            k.hints.keyboard |= h.keyboard;
            k.hints.mouse |= h.mouse;
        }
    }

    fn on_event(&mut self, ev: UdevEvent, now: Instant, cfg: &Config) {
        match ev {
            UdevEvent::UsbAdd(props) => {
                if is_hub(&props) {
                    return;
                }
                let devpath = props.devpath.clone();
                let name = display_name(&props);
                self.known.insert(devpath.clone(), Known { name, props, hints: InputHints::default() });
                self.adds.retain(|(d, _)| *d != devpath);
                if self.may_announce(now, cfg) {
                    self.adds.push((devpath, now));
                }
            }
            UdevEvent::UsbRemove(devpath) => {
                let Some(k) = self.known.remove(&devpath) else { return };
                let kind = k.kind();
                // Saiu antes do prazo da conexão: a conexão sai também, com o que se sabia.
                if let Some(at) = self.adds.iter().position(|(d, _)| *d == devpath).map(|i| self.adds.remove(i).1)
                    && cfg.usb_kinds.contains(&kind)
                {
                    self.grouper.push(Change::Added, k.name.clone(), kind, at);
                }
                if self.may_announce(now, cfg) && cfg.usb_kinds.contains(&kind) {
                    self.grouper.push(Change::Removed, k.name, kind, now);
                }
            }
            UdevEvent::Input(path, hints) => self.add_hints(&path, hints),
        }
    }

    fn adds_deadline(&self) -> Option<Instant> {
        let (first, last) = (self.adds.first()?, self.adds.last()?);
        Some(window_end(first.1, last.1))
    }

    /// Prazo mais próximo; `None` = nada pendente.
    fn deadline(&self) -> Option<Instant> {
        match (self.adds_deadline(), self.grouper.deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Os avisos vencidos em `now`. `visible(change)`: o cartão de grupo dessa mudança está na tela.
    fn flush(&mut self, now: Instant, cfg: &Config, visible: impl Fn(Change) -> bool) -> Vec<Announcement> {
        if self.adds_deadline().is_some_and(|due| due <= now) {
            for (devpath, at) in std::mem::take(&mut self.adds) {
                let Some(k) = self.known.get(&devpath) else { continue };
                let kind = k.kind();
                if cfg.usb_kinds.contains(&kind) {
                    self.grouper.push(Change::Added, k.name.clone(), kind, at);
                }
            }
        }
        self.grouper.flush(now, visible)
    }
}

impl State {
    /// Eventos do monitor udev.
    fn on_udev(&mut self, events: Vec<UdevEvent>) {
        let now = Instant::now();
        let Some(usb) = self.usb.as_mut() else { return };
        for ev in events {
            usb.on_event(ev, now, &self.cfg);
        }
        self.rearm_usb();
    }

    /// O prazo do grupo venceu: anuncia o que estiver pronto.
    fn on_usb_timer(&mut self) {
        let now = Instant::now();
        let store = &self.store;
        let Some(usb) = self.usb.as_mut() else { return };
        let out = usb.flush(now, &self.cfg, |c| store.visible_with_sync_key(group::sync_key(c)).is_some());
        for a in &out {
            self.announce(a);
        }
        self.rearm_usb();
    }

    /// `Notify` ao próprio nome, para a Central (que só vê o barramento) registrar no histórico. Não
    /// espera a resposta aqui (bloquearia: quem atende somos nós); a chamada chega pelo laço como
    /// qualquer outra, e a resposta, depois, é ignorada pelo `drain`. Sem barramento, direto no Store.
    fn announce(&mut self, a: &Announcement) {
        let args = group::to_notify(a);
        if let Some(bus) = &self.bus {
            bus.send(notify_call(&args));
            return;
        }
        let new = NewNotification {
            app_name: args.app_name,
            app_icon: args.app_icon,
            summary: args.summary,
            body: args.body,
            actions: Vec::new(),
            hints: Hints::parse(args.hints),
            expire_timeout: args.expire_timeout,
        };
        let dnd = self.current_dnd();
        let (_, effects) = self.store.notify(new, 0, dnd);
        self.apply_effects(effects);
    }

    /// Exatamente um timer no prazo mais próximo, ou nenhum sem pendentes.
    fn rearm_usb(&mut self) {
        let Some(usb) = self.usb.as_mut() else { return };
        let next = usb.deadline();
        if next == usb.timer.map(|(_, at)| at) {
            return;
        }
        if let Some((token, _)) = usb.timer.take() {
            self.handle.remove(token);
        }
        let Some(at) = next else { return };
        match self.handle.insert_source(Timer::from_deadline(at), |_, _, state: &mut State| {
            // Este timer cai com o `Drop`: não pode ser removido de novo no rearme.
            if let Some(usb) = state.usb.as_mut() {
                usb.timer = None;
            }
            state.on_usb_timer();
            TimeoutAction::Drop
        }) {
            Ok(token) => usb.timer = Some((token, at)),
            Err(e) => eprintln!("notificacoes: o timer dos avisos de USB não registrou: {}", e.error),
        }
    }
}

// --- udev ---

fn text(v: Option<&OsStr>) -> Option<String> {
    v.map(|s| s.to_string_lossy().into_owned())
}

fn hex16(v: Option<String>) -> Option<u16> {
    u16::from_str_radix(v?.trim(), 16).ok()
}

/// `ID_INPUT_*`/`ID_MTP_DEVICE` valem "1"; `ID_MEDIA_PLAYER` pode ser "1" ou o nome de um perfil.
fn flag(dev: &udev::Device, key: &str) -> bool {
    dev.property_value(key).and_then(OsStr::to_str).is_some_and(|v| v == "1")
}

fn is_usb_device(dev: &udev::Device) -> bool {
    dev.devtype().is_some_and(|t| t == "usb_device")
}

fn props_of(dev: &udev::Device) -> UsbProps {
    let prop = |k: &str| text(dev.property_value(k));
    let attr = |k: &str| text(dev.attribute_value(k));
    let vid = hex16(prop("ID_VENDOR_ID").or_else(|| attr("idVendor")));
    let pid = hex16(prop("ID_MODEL_ID").or_else(|| attr("idProduct")));
    UsbProps {
        devpath: dev.devpath().to_string_lossy().into_owned(),
        device_class: attr("bDeviceClass"),
        interfaces: prop("ID_USB_INTERFACES"),
        model_db: prop("ID_MODEL_FROM_DATABASE"),
        vendor_db: prop("ID_VENDOR_FROM_DATABASE"),
        product: attr("product"),
        manufacturer: attr("manufacturer"),
        vid_pid: vid.zip(pid),
        mtp: flag(dev, "ID_MTP_DEVICE"),
        media_player: dev.property_value("ID_MEDIA_PLAYER").is_some_and(|v| !v.is_empty() && v != "0"),
    }
}

fn hints_of(dev: &udev::Device) -> InputHints {
    InputHints {
        joystick: flag(dev, "ID_INPUT_JOYSTICK"),
        keyboard: flag(dev, "ID_INPUT_KEYBOARD"),
        mouse: flag(dev, "ID_INPUT_MOUSE"),
    }
}

fn read_event(e: &udev::Event) -> Option<UdevEvent> {
    let subsystem = e.subsystem()?;
    match e.event_type() {
        udev::EventType::Add if subsystem == "usb" && is_usb_device(e) => Some(UdevEvent::UsbAdd(props_of(e))),
        udev::EventType::Remove if subsystem == "usb" && is_usb_device(e) => {
            Some(UdevEvent::UsbRemove(e.devpath().to_string_lossy().into_owned()))
        }
        udev::EventType::Add if subsystem == "input" => {
            let hints = hints_of(e);
            (hints != InputHints::default()).then(|| UdevEvent::Input(e.devpath().to_string_lossy().into_owned(), hints))
        }
        _ => None,
    }
}

/// Enumera um subsistema; falha só loga (o mapa começa sem esses).
fn scan(subsystem: &str) -> Vec<udev::Device> {
    let devices = udev::Enumerator::new().and_then(|mut en| {
        en.match_subsystem(subsystem)?;
        Ok(en.scan_devices()?.collect::<Vec<_>>())
    });
    devices.unwrap_or_else(|e| {
        eprintln!("notificacoes: enumeração udev de {subsystem} falhou ({e}); os já plugados ficam sem nome");
        Vec::new()
    })
}

fn present_devices() -> Vec<UsbProps> {
    scan("usb").iter().filter(|d| is_usb_device(d)).map(props_of).collect()
}

fn present_inputs() -> Vec<(String, InputHints)> {
    scan("input")
        .iter()
        .map(|d| (d.devpath().to_string_lossy().into_owned(), hints_of(d)))
        .filter(|(_, h)| *h != InputHints::default())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::UsbKind;
    use group::GROUP_WINDOW;

    const HUB: &str = "/devices/pci0000:00/0000:00:01.2/0000:02:00.0/usb1/1-1";

    fn path(port: &str) -> String {
        format!("{HUB}/{port}")
    }

    fn pendrive(port: &str) -> UsbProps {
        UsbProps {
            devpath: path(port),
            device_class: Some("00".into()),
            interfaces: Some(":080650:".into()),
            model_db: Some("Ultra Fit".into()),
            vendor_db: Some("SanDisk".into()),
            vid_pid: Some((0x0781, 0x5583)),
            ..UsbProps::default()
        }
    }

    fn gamepad_2c(port: &str) -> UsbProps {
        UsbProps {
            devpath: path(port),
            device_class: Some("ef".into()),
            interfaces: Some(":030101:030102:ff5d01:".into()),
            vendor_db: Some("8BitDo".into()),
            manufacturer: Some("8BitDo".into()),
            product: Some("8BitDo Ultimate 2C Wireless Controller".into()),
            vid_pid: Some((0x2dc8, 0x310a)),
            ..UsbProps::default()
        }
    }

    fn hid_pad(port: &str) -> UsbProps {
        UsbProps {
            devpath: path(port),
            device_class: Some("00".into()),
            interfaces: Some(":030000:".into()),
            manufacturer: Some("8BitDo".into()),
            product: Some("IDLE".into()),
            ..UsbProps::default()
        }
    }

    fn input(port: &str, iface: &str, n: u32, hints: InputHints) -> UdevEvent {
        UdevEvent::Input(format!("{}/{port}:{iface}/0003:2DC8:310A.000{n}/input/input{n}", path(port)), hints)
    }

    const JOY: InputHints = InputHints { joystick: true, keyboard: false, mouse: false };
    const KBD: InputHints = InputHints { joystick: false, keyboard: true, mouse: false };
    const MOUSE: InputHints = InputHints { joystick: false, keyboard: false, mouse: true };
    const NOT_VISIBLE: fn(Change) -> bool = |_| false;

    fn one(kind: UsbKind, change: Change, name: &str) -> Vec<Announcement> {
        vec![Announcement { change, names: vec![name.to_owned()], kinds: vec![kind] }]
    }

    /// Avança até o prazo e drena; `None` se não havia nada pendente.
    fn settle(usb: &mut Usb, cfg: &Config) -> Option<Vec<Announcement>> {
        let due = usb.deadline()?;
        Some(usb.flush(due, cfg, NOT_VISIBLE))
    }

    #[test]
    fn present_at_start_silent_but_named_on_removal() {
        let cfg = Config::default();
        let mut usb = Usb::with_present(vec![pendrive("1-1.2")], vec![]);
        assert_eq!(usb.deadline(), None, "o que já estava plugado não é anunciado");
        let t = Instant::now();
        usb.on_event(UdevEvent::UsbRemove(path("1-1.2")), t, &cfg);
        assert_eq!(usb.deadline(), Some(t + GROUP_WINDOW));
        assert_eq!(usb.flush(t + GROUP_WINDOW, &cfg, NOT_VISIBLE), one(UsbKind::Armazenamento, Change::Removed, "SanDisk Ultra Fit"));
        // Remoção de quem não se conhecia: nada.
        usb.on_event(UdevEvent::UsbRemove(path("1-1.2")), t, &cfg);
        usb.on_event(UdevEvent::UsbRemove(path("1-1.9")), t, &cfg);
        assert_eq!(usb.deadline(), None);
    }

    #[test]
    fn present_at_start_uses_input_hints_for_kind() {
        let cfg = Config::default();
        let pad = hid_pad("1-1.1");
        let joy_node = format!("{}/1-1.1:1.0/input/input20", pad.devpath);
        let mut usb = Usb::with_present(vec![pad], vec![(joy_node, JOY)]);
        let t = Instant::now();
        usb.on_event(UdevEvent::UsbRemove(path("1-1.1")), t, &cfg);
        assert_eq!(settle(&mut usb, &cfg), Some(one(UsbKind::Controle, Change::Removed, "8BitDo IDLE")));
    }

    #[test]
    fn quiet_window_after_resume_suppresses_both() {
        let cfg = Config::default();
        let mut usb = Usb::with_present(vec![pendrive("1-1.2")], vec![]);
        let t = Instant::now();
        usb.on_sleep(true, t);
        // Durante a suspensão: nem remoção nem conexão.
        usb.on_event(UdevEvent::UsbRemove(path("1-1.2")), t + Duration::from_secs(1), &cfg);
        usb.on_event(UdevEvent::UsbAdd(hid_pad("1-1.1")), t + Duration::from_secs(2), &cfg);
        let woke = t + Duration::from_secs(60);
        usb.on_sleep(false, woke);
        // Até RESUME_QUIET depois de acordar: nada, mas o mapa é atualizado.
        usb.on_event(UdevEvent::UsbAdd(pendrive("1-1.2")), woke + Duration::from_secs(1), &cfg);
        usb.on_event(UdevEvent::UsbRemove(path("1-1.1")), woke + Duration::from_millis(4999), &cfg);
        if let Some(out) = settle(&mut usb, &cfg) {
            assert!(out.is_empty(), "nada pode sair na janela de silêncio: {out:?}");
        }
        // Depois da janela, o pendrive que voltou na suspensão é conhecido pelo nome.
        let after = woke + RESUME_QUIET;
        usb.on_event(UdevEvent::UsbRemove(path("1-1.2")), after, &cfg);
        usb.on_event(UdevEvent::UsbAdd(hid_pad("1-1.3")), after, &cfg);
        let out = usb.flush(after + GROUP_WINDOW, &cfg, NOT_VISIBLE);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(out.contains(&one(UsbKind::Armazenamento, Change::Removed, "SanDisk Ultra Fit")[0]));
        assert!(out.contains(&one(UsbKind::Generico, Change::Added, "8BitDo IDLE")[0]));
    }

    #[test]
    fn add_pending_when_suspending_is_not_announced_on_wake() {
        let cfg = Config::default();
        let mut usb = Usb::with_present(vec![], vec![]);
        let t = Instant::now();
        usb.on_event(UdevEvent::UsbAdd(hid_pad("1-1.1")), t, &cfg);
        // Suspende antes do prazo da conexão.
        usb.on_sleep(true, t + Duration::from_millis(50));
        let woke = t + Duration::from_secs(600);
        usb.on_sleep(false, woke);
        assert_eq!(usb.deadline(), None, "conexão pendente sobreviveu à suspensão");
        let out = usb.flush(woke + RESUME_QUIET + GROUP_WINDOW, &cfg, NOT_VISIBLE);
        assert!(out.is_empty(), "anunciou ao acordar: {out:?}");
    }

    #[test]
    fn sleeping_again_before_quiet_ends_stays_silent() {
        let cfg = Config::default();
        let mut usb = Usb::with_present(vec![], vec![]);
        let t = Instant::now();
        usb.on_sleep(true, t);
        usb.on_sleep(false, t + Duration::from_secs(1));
        usb.on_sleep(true, t + Duration::from_secs(2));
        usb.on_event(UdevEvent::UsbAdd(pendrive("1-1.2")), t + Duration::from_secs(30), &cfg);
        assert!(settle(&mut usb, &cfg).is_none_or(|out| out.is_empty()), "dormindo de novo");
    }

    #[test]
    fn disabled_kind_not_announced() {
        let only_pads = Config { usb_kinds: vec![UsbKind::Controle], ..Config::default() };
        let mut usb = Usb::with_present(vec![], vec![]);
        let t = Instant::now();
        usb.on_event(UdevEvent::UsbAdd(pendrive("1-1.2")), t, &only_pads);
        assert!(settle(&mut usb, &only_pads).is_none_or(|out| out.is_empty()));
        usb.on_event(UdevEvent::UsbRemove(path("1-1.2")), t + Duration::from_secs(5), &only_pads);
        assert!(settle(&mut usb, &only_pads).is_none_or(|out| out.is_empty()));

        // O tipo só se decide com as dicas de input: um HID genérico que é controle passa.
        let t = t + Duration::from_secs(10);
        usb.on_event(UdevEvent::UsbAdd(hid_pad("1-1.1")), t, &only_pads);
        usb.on_event(input("1-1.1", "1.0", 1, JOY), t + Duration::from_millis(300), &only_pads);
        assert_eq!(settle(&mut usb, &only_pads), Some(one(UsbKind::Controle, Change::Added, "8BitDo IDLE")));

        // `usb = nao`: nada é anunciado, mas o mapa guarda o nome.
        let off = Config { usb: false, ..Config::default() };
        let t = t + Duration::from_secs(10);
        usb.on_event(UdevEvent::UsbAdd(pendrive("1-1.4")), t, &off);
        assert!(settle(&mut usb, &off).is_none_or(|out| out.is_empty()));
        let on = Config::default();
        usb.on_event(UdevEvent::UsbRemove(path("1-1.4")), t + Duration::from_secs(5), &on);
        assert_eq!(settle(&mut usb, &on), Some(one(UsbKind::Armazenamento, Change::Removed, "SanDisk Ultra Fit")));
    }

    /// Fato do sistema: o 8BitDo Ultimate 2C aparece como `usb_device` e três nós de input
    /// (teclado, mouse, joystick via xpad). Tem de sair UM aviso "Controle conectado".
    #[test]
    fn controller_with_three_inputs_is_one_notice() {
        let cfg = Config::default();
        let mut usb = Usb::with_present(vec![], vec![]);
        let t = Instant::now();
        usb.on_event(UdevEvent::UsbAdd(gamepad_2c("1-1.1")), t, &cfg);
        usb.on_event(input("1-1.1", "1.0", 1, KBD), t + Duration::from_millis(40), &cfg);
        usb.on_event(input("1-1.1", "1.1", 2, MOUSE), t + Duration::from_millis(50), &cfg);
        usb.on_event(input("1-1.1", "1.2", 3, JOY), t + Duration::from_millis(400), &cfg);
        let out = settle(&mut usb, &cfg).unwrap_or_default();
        assert_eq!(out, one(UsbKind::Controle, Change::Added, "8BitDo Ultimate 2C Wireless Controller"));
        let a = &out[0];
        assert_eq!(group::title(a), "Controle conectado");
        assert_eq!(usb.deadline(), None, "sem pendentes, sem timer");
    }

    #[test]
    fn late_joystick_hint_refines_generic_hid() {
        let cfg = Config::default();
        let mut usb = Usb::with_present(vec![], vec![]);
        let t = Instant::now();
        usb.on_event(UdevEvent::UsbAdd(hid_pad("1-1.1")), t, &cfg);
        usb.on_event(input("1-1.1", "1.0", 1, JOY), t + Duration::from_millis(1200), &cfg);
        assert_eq!(settle(&mut usb, &cfg), Some(one(UsbKind::Controle, Change::Added, "8BitDo IDLE")));
        // E a remoção usa o tipo já refinado.
        usb.on_event(UdevEvent::UsbRemove(path("1-1.1")), t + Duration::from_secs(10), &cfg);
        assert_eq!(settle(&mut usb, &cfg), Some(one(UsbKind::Controle, Change::Removed, "8BitDo IDLE")));
    }

    #[test]
    fn input_of_sibling_port_does_not_refine() {
        // "1-1.1" é prefixo textual de "1-1.10": a dica do joystick em 1-1.10 não é do 1-1.1.
        let cfg = Config::default();
        let mut usb = Usb::with_present(vec![], vec![]);
        let t = Instant::now();
        usb.on_event(UdevEvent::UsbAdd(hid_pad("1-1.1")), t, &cfg);
        usb.on_event(input("1-1.10", "1.0", 1, JOY), t + Duration::from_millis(100), &cfg);
        assert_eq!(settle(&mut usb, &cfg), Some(one(UsbKind::Generico, Change::Added, "8BitDo IDLE")));
    }

    #[test]
    fn hubs_are_ignored() {
        let cfg = Config::default();
        let mut usb = Usb::with_present(vec![], vec![]);
        let t = Instant::now();
        let hub = UsbProps { devpath: path("1-1.5"), device_class: Some("09".into()), interfaces: Some(":090000:".into()), ..UsbProps::default() };
        usb.on_event(UdevEvent::UsbAdd(hub), t, &cfg);
        assert_eq!(usb.deadline(), None);
        usb.on_event(UdevEvent::UsbRemove(path("1-1.5")), t, &cfg);
        assert_eq!(usb.deadline(), None, "hub não entra no mapa");
    }

    #[test]
    fn hub_with_several_devices_is_one_group() {
        let cfg = Config::default();
        let mut usb = Usb::with_present(vec![], vec![]);
        let t = Instant::now();
        usb.on_event(UdevEvent::UsbAdd(pendrive("1-1.5.1")), t, &cfg);
        usb.on_event(UdevEvent::UsbAdd(gamepad_2c("1-1.5.2")), t + Duration::from_millis(300), &cfg);
        usb.on_event(input("1-1.5.2", "1.2", 3, JOY), t + Duration::from_millis(500), &cfg);
        usb.on_event(UdevEvent::UsbAdd(hid_pad("1-1.5.3")), t + Duration::from_millis(700), &cfg);
        assert_eq!(usb.deadline(), Some(t + Duration::from_millis(700) + GROUP_WINDOW));
        let out = settle(&mut usb, &cfg).unwrap_or_default();
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].names, vec!["SanDisk Ultra Fit", "8BitDo Ultimate 2C Wireless Controller", "8BitDo IDLE"]);
        assert_eq!(out[0].kinds, vec![UsbKind::Armazenamento, UsbKind::Controle, UsbKind::Generico]);
        assert_eq!(group::title(&out[0]), "3 dispositivos conectados");
    }

    #[test]
    fn plugged_and_pulled_within_window_announces_both() {
        let cfg = Config::default();
        let mut usb = Usb::with_present(vec![], vec![]);
        let t = Instant::now();
        usb.on_event(UdevEvent::UsbAdd(pendrive("1-1.2")), t, &cfg);
        usb.on_event(UdevEvent::UsbRemove(path("1-1.2")), t + Duration::from_millis(500), &cfg);
        let out = usb.flush(t + Duration::from_secs(3), &cfg, NOT_VISIBLE);
        assert_eq!(out, vec![
            one(UsbKind::Armazenamento, Change::Added, "SanDisk Ultra Fit").remove(0),
            one(UsbKind::Armazenamento, Change::Removed, "SanDisk Ultra Fit").remove(0),
        ]);
        assert_eq!(usb.deadline(), None);
    }

    /// O aviso vai como `Notify` ao próprio nome (num dbus-daemon privado): a chamada volta pelo
    /// laço, vira cartão, é respondida (a Central casa chamada e resposta) e, com o cartão de grupo
    /// na tela, o próximo dispositivo soma a ele no mesmo lugar.
    mod via_bus {
        use std::io::{BufRead, BufReader};
        use std::process::{Child, Command, Stdio};

        use dbus::blocking::Connection;
        use dbus::channel::Channel;
        use dbus::message::MessageType;
        use smithay_client_toolkit::reexports::calloop::EventLoop;

        use super::*;
        use crate::Screen;
        use crate::dbus::conn::Bus;
        use crate::headless::Headless;
        use crate::store::Store;

        struct PrivateBus {
            address: String,
            child: Child,
        }

        impl PrivateBus {
            fn start() -> PrivateBus {
                let mut child = Command::new("dbus-daemon")
                    .args(["--session", "--nofork", "--print-address=1"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap();
                let mut line = String::new();
                BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
                PrivateBus { address: line.trim().to_string(), child }
            }

            fn channel(&self) -> Channel {
                let mut channel = Channel::open_private(&self.address).unwrap();
                channel.register().unwrap();
                channel
            }
        }

        impl Drop for PrivateBus {
            fn drop(&mut self) {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }

        fn run_until(event_loop: &mut EventLoop<'static, State>, state: &mut State, done: impl Fn(&State) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !done(state) {
                assert!(Instant::now() < deadline, "não aconteceu em 5 s");
                event_loop.dispatch(Some(Duration::from_millis(50)), state).unwrap();
            }
        }

        #[test]
        fn announcement_goes_through_own_name_and_merges_while_visible() {
            let bus = PrivateBus::start();
            // Monitor no barramento, como a Central: vê a chamada e a resposta.
            let monitor = Connection::from(bus.channel());
            let rules: Vec<&str> = vec![];
            monitor
                .with_proxy("org.freedesktop.DBus", "/org/freedesktop/DBus", Duration::from_secs(2))
                .method_call::<(), _, _, _>("org.freedesktop.DBus.Monitoring", "BecomeMonitor", (rules, 0u32))
                .unwrap();

            let mut event_loop: EventLoop<'static, State> = EventLoop::try_new().unwrap();
            let handle = event_loop.handle();
            let conn = Bus::connect(&handle, bus.channel()).unwrap();
            let mut state = State {
                store: Store::new(4),
                cfg: Config { sound: false, ..Config::default() },
                bus: Some(conn),
                screen: Screen::Headless(Headless::new(handle.clone())),
                dnd: false,
                dnd_dir: std::path::PathBuf::new(),
                dnd_watched: true,
                handle,
                debug: false,
                _panel_fd: None,
                pending: Vec::new(),
                token_timer: None,
                player: crate::sound::Player::new(),
                usb: Some(Usb::with_present(vec![], vec![])),
            };
            event_loop.dispatch(Some(Duration::ZERO), &mut state).unwrap();

            state.on_udev(vec![UdevEvent::UsbAdd(pendrive("1-1.2"))]);
            assert!(state.usb.as_ref().is_some_and(|u| u.timer.is_some()), "timer só com pendentes");
            let key = group::sync_key(Change::Added);
            run_until(&mut event_loop, &mut state, |s| s.store.visible_with_sync_key(key).is_some());
            let id = state.store.visible_with_sync_key(key).unwrap();
            let n = state.store.get(id).unwrap();
            assert_eq!((n.app_name.as_str(), n.summary.as_str(), n.body.as_str()), ("Dispositivos USB", "Armazenamento USB conectado", "SanDisk Ultra Fit"));
            assert!(state.usb.as_ref().is_some_and(|u| u.timer.is_none()), "sem pendentes, sem timer");

            // A Central vê o Notify e a resposta dele.
            let mut call_serial = None;
            let mut answered = false;
            let until = Instant::now() + Duration::from_secs(2);
            while !answered && Instant::now() < until {
                let Ok(Some(m)) = monitor.channel().blocking_pop_message(Duration::from_millis(50)) else { continue };
                match m.msg_type() {
                    MessageType::MethodCall if m.member().is_some_and(|x| &*x == "Notify") => call_serial = m.get_serial(),
                    MessageType::MethodReturn if call_serial.is_some() && m.get_reply_serial() == call_serial => answered = true,
                    _ => {}
                }
            }
            assert!(call_serial.is_some(), "o Notify não passou pelo barramento");
            assert!(answered, "o Notify ficou sem resposta");

            // Outro dispositivo com o cartão de grupo na tela: soma, no mesmo cartão.
            state.on_udev(vec![UdevEvent::UsbAdd(gamepad_2c("1-1.1"))]);
            run_until(&mut event_loop, &mut state, |s| s.store.get(id).is_some_and(|n| n.summary == "2 dispositivos conectados"));
            assert_eq!(state.store.stack(), vec![id], "substituiu no lugar");
            let n = state.store.get(id).unwrap();
            assert_eq!(n.body, "SanDisk Ultra Fit e 8BitDo Ultimate 2C Wireless Controller");
        }
    }
}

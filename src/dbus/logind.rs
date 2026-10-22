//! logind no barramento de sistema: só o `PrepareForSleep` (a Tarefa 14, de USB, vai usá-lo).
//!
//! A tela de bloqueio ficou fora (decisão do usuário): nada de `LockedHint` nem de caminho de
//! sessão. Respeita `DBUS_SYSTEM_BUS_ADDRESS` (o libdbus lê a variável).
//!
//! Integração no calloop como a do barramento de sessão (`conn.rs`): uma cópia do fd do watch num
//! `Generic` (`READ`, nível); a cada evento, `read_write(0)` e drenar até `None`. O `AddMatch` vai
//! sem esperar resposta (nada bloqueia no barramento de sistema depois do `Hello`); a resposta
//! chega no laço e, com `NOTIFICACOES_DEBUG=1`, imprime `logind: escutando`.
//!
//! O canal fica com a fonte (e com o idle da primeira drenagem), não no `State`: a conexão caiu,
//! a fonte se remove e leva o canal junto.

use std::os::fd::BorrowedFd;
use std::rc::Rc;
use std::time::{Duration, Instant};

use dbus::Message;
use dbus::channel::{BusType, Channel};
use dbus::message::MessageType;
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};

use crate::State;

const LOGIN1: &str = "org.freedesktop.login1";
const LOGIN1_PATH: &str = "/org/freedesktop/login1";
const MANAGER: &str = "org.freedesktop.login1.Manager";
const PREPARE_FOR_SLEEP: &str = "PrepareForSleep";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogindEvent {
    /// `PrepareForSleep(b)`: `true` antes de suspender, `false` ao voltar.
    Sleep(bool),
}

/// O canal com o barramento de sistema e o serial do `AddMatch` (para reconhecer a resposta).
struct Logind {
    channel: Channel,
    add_match: u32,
}

/// Conecta ao barramento de sistema, pede o match do `PrepareForSleep` e põe o fd no laço.
/// Erro = segue sem logind (quem chama loga).
pub fn connect(handle: &LoopHandle<'static, State>) -> Result<(), dbus::Error> {
    let mut channel = Channel::get_private(BusType::System)?;
    channel.set_watch_enabled(true);
    // SAFETY: o fd do watch está aberto enquanto o canal (vivo aqui) existir; só é duplicado.
    let fd = unsafe { BorrowedFd::borrow_raw(channel.watch().fd) }
        .try_clone_to_owned()
        .map_err(|e| dbus::Error::new_failed(&format!("dup do fd do barramento de sistema: {e}")))?;
    let rule = format!(
        "type='signal',sender='{LOGIN1}',path='{LOGIN1_PATH}',interface='{MANAGER}',member='{PREPARE_FOR_SLEEP}'"
    );
    let add_match = Message::new_method_call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch")
        .map_err(|e| dbus::Error::new_failed(&e))?
        .append1(rule);
    let serial = channel.send(add_match).map_err(|()| dbus::Error::new_failed("AddMatch não enfileirou"))?;
    channel.flush();
    let logind = Rc::new(Logind { channel, add_match: serial });

    let source = Generic::new(fd, Interest::READ, Mode::Level);
    let in_source = Rc::clone(&logind);
    handle
        .insert_source(source, move |_, _, state: &mut State| Ok(drain(&in_source, state)))
        .map_err(|e| dbus::Error::new_failed(&format!("fd do barramento de sistema no laço: {}", e.error)))?;
    // O `flush` (com leitura) pode ter posto mensagens no buffer sem o fd ficar legível.
    handle.insert_idle(move |state| {
        drain(&logind, state);
    });
    Ok(())
}

/// Lê o que houver e trata as mensagens. Conexão caída: loga e remove a fonte.
fn drain(logind: &Logind, state: &mut State) -> PostAction {
    let channel = &logind.channel;
    let mut alive = channel.read_write(Some(Duration::ZERO)).is_ok() && channel.is_connected();
    while let Some(mut msg) = channel.pop_message() {
        match classify(&msg, logind.add_match) {
            Incoming::Event(e) => state.on_logind(e),
            Incoming::Listening => {
                if state.debug {
                    eprintln!("notificacoes: logind: escutando");
                }
            }
            Incoming::MatchFailed => {
                let why = msg.as_result().err().map(|e| e.to_string()).unwrap_or_default();
                eprintln!("notificacoes: o AddMatch do logind falhou ({why}); sem aviso de suspensão");
            }
            Incoming::Disconnected => alive = false,
            Incoming::Other => {}
        }
    }
    if alive {
        return PostAction::Continue;
    }
    eprintln!("notificacoes: a conexão com o barramento de sistema caiu; seguindo sem logind");
    PostAction::Remove
}

#[derive(Debug, PartialEq, Eq)]
enum Incoming {
    Event(LogindEvent),
    /// A resposta do nosso `AddMatch`.
    Listening,
    MatchFailed,
    Disconnected,
    Other,
}

/// O que a mensagem é para nós. O barramento só entrega em difusão o que bate com o match (que
/// exige o dono de `org.freedesktop.login1`); um sinal endereçado a nós passa por cima do match,
/// então só vale sem destino.
fn classify(msg: &Message, add_match: u32) -> Incoming {
    match msg.msg_type() {
        MessageType::MethodReturn if msg.get_reply_serial() == Some(add_match) => Incoming::Listening,
        MessageType::Error if msg.get_reply_serial() == Some(add_match) => Incoming::MatchFailed,
        MessageType::Signal => {
            let is = |a: Option<&str>, b: &str| a == Some(b);
            let interface = msg.interface();
            let member = msg.member();
            let path = msg.path();
            if is(interface.as_deref(), "org.freedesktop.DBus.Local") && is(member.as_deref(), "Disconnected") {
                return Incoming::Disconnected;
            }
            let ours = msg.destination().is_none()
                && is(path.as_deref(), LOGIN1_PATH)
                && is(interface.as_deref(), MANAGER)
                && is(member.as_deref(), PREPARE_FOR_SLEEP);
            match msg.read1::<bool>() {
                Ok(going) if ours => Incoming::Event(LogindEvent::Sleep(going)),
                _ => Incoming::Other,
            }
        }
        _ => Incoming::Other,
    }
}

impl State {
    /// Evento do logind. `Sleep` silencia os avisos de USB durante a suspensão e logo depois dela.
    pub fn on_logind(&mut self, e: LogindEvent) {
        match e {
            LogindEvent::Sleep(going) => {
                eprintln!("notificacoes: logind: suspensao={going}");
                if let Some(usb) = self.usb.as_mut() {
                    usb.on_sleep(going, Instant::now());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(path: &str, interface: &str, member: &str, arg: bool) -> Message {
        Message::new_signal(path, interface, member).unwrap().append1(arg)
    }

    #[test]
    fn classifies_prepare_for_sleep_only_on_manager_path_and_broadcast() {
        let m = signal(LOGIN1_PATH, MANAGER, PREPARE_FOR_SLEEP, true);
        assert_eq!(classify(&m, 7), Incoming::Event(LogindEvent::Sleep(true)));
        let m = signal(LOGIN1_PATH, MANAGER, PREPARE_FOR_SLEEP, false);
        assert_eq!(classify(&m, 7), Incoming::Event(LogindEvent::Sleep(false)));
        let m = signal("/org/freedesktop/login1/session/_31", MANAGER, PREPARE_FOR_SLEEP, true);
        assert_eq!(classify(&m, 7), Incoming::Other);
        let m = signal(LOGIN1_PATH, MANAGER, "PrepareForShutdown", true);
        assert_eq!(classify(&m, 7), Incoming::Other);
        let mut m = signal(LOGIN1_PATH, MANAGER, PREPARE_FOR_SLEEP, true);
        m.set_destination(Some(":1.42".into()));
        assert_eq!(classify(&m, 7), Incoming::Other, "endereçado a nós: não veio pelo match");
        // Argumento com tipo errado: ignorado.
        let m = Message::new_signal(LOGIN1_PATH, MANAGER, PREPARE_FOR_SLEEP).unwrap().append1("sim");
        assert_eq!(classify(&m, 7), Incoming::Other);
    }
}

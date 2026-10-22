//! Conexão com o barramento de sessão, integrada ao calloop pela fd do libdbus.
//!
//! Uma cópia (`dup`) do fd do watch entra num `Generic` (`READ`, nível): o libdbus fecha o fd
//! dele quando a conexão cai, e o calloop não pode tocar num fd fechado (EBADF derrubaria o laço).
//! A cópia é da fonte e fecha junto com ela. A cada evento: `read_write(0)` e drenar
//! `pop_message()` até `None`. Também se drena logo após registrar e depois de qualquer chamada
//! bloqueante (`flush`, `RequestName`), porque o libdbus pode ter lido mensagens para o buffer
//! interno sem que o fd volte a ficar legível. Isso é do módulo, não de quem chama: todo `send`
//! fora de um `drain` agenda um idle de drenagem (um por rajada), que roda no fim do mesmo
//! dispatch, antes do próximo poll.

use std::cell::Cell;
use std::os::fd::BorrowedFd;
use std::time::Duration;

use dbus::blocking::LocalConnection;
use dbus::blocking::stdintf::org_freedesktop_dbus::RequestNameReply;
use dbus::channel::{BusType, Channel};
use dbus::message::MessageType;
use dbus::Message;
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};

use super::server::{NAME, effect_signal};
use crate::State;
use crate::store::Effect;

pub struct Bus {
    conn: LocalConnection,
    handle: LoopHandle<'static, State>,
    /// A conexão caiu; o `Bus` sai do estado no próximo idle e a fonte se remove.
    dead: Cell<bool>,
    /// Um `drain` já está em curso ou agendado: um `send` agora não precisa agendar outro.
    drain_pending: Cell<bool>,
    #[cfg(test)]
    idles_scheduled: Cell<u32>,
}

impl Bus {
    /// Conecta à sessão, pede o nome (flags das Global Constraints), registra o fd do watch no calloop.
    pub fn session(handle: &LoopHandle<'static, State>) -> Result<Bus, dbus::Error> {
        Bus::connect(handle, Channel::get_private(BusType::Session)?)
    }

    /// Como `session`, sobre um canal já registrado (os testes usam um barramento privado).
    pub(crate) fn connect(handle: &LoopHandle<'static, State>, mut channel: Channel) -> Result<Bus, dbus::Error> {
        channel.set_watch_enabled(true);
        // SAFETY: o fd do watch está aberto enquanto o canal (vivo aqui) existir; só é duplicado.
        let fd = unsafe { BorrowedFd::borrow_raw(channel.watch().fd) }
            .try_clone_to_owned()
            .map_err(|e| dbus::Error::new_failed(&format!("dup do fd do D-Bus: {e}")))?;
        let conn = LocalConnection::from(channel);

        // allow_replacement = false, replace_existing = true, do_not_queue = false.
        match conn.request_name(NAME, false, true, false) {
            Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {}
            Ok(RequestNameReply::InQueue) => {
                eprintln!("notificacoes: {NAME} tem dono que não cede; na fila até ele sair");
            }
            Ok(RequestNameReply::Exists) => eprintln!("notificacoes: {NAME} já tem dono; seguindo sem o nome"),
            Err(e) => eprintln!("notificacoes: RequestName({NAME}) falhou: {e}; seguindo sem o nome"),
        }

        let source = Generic::new(fd, Interest::READ, Mode::Level);
        handle
            .insert_source(source, |_, _, state: &mut State| Ok(drain(state)))
            .map_err(|e| dbus::Error::new_failed(&format!("fd do D-Bus no laço: {}", e.error)))?;
        let bus = Bus {
            conn,
            handle: handle.clone(),
            dead: Cell::new(false),
            drain_pending: Cell::new(false),
            #[cfg(test)]
            idles_scheduled: Cell::new(0),
        };
        // O RequestName bloqueou: pode haver mensagens já lidas esperando no buffer.
        bus.schedule_drain();
        Ok(bus)
    }

    /// Responde, a menos que a chamada tenha `NO_REPLY_EXPECTED`.
    pub fn reply(&self, to: &Message, reply: Message) {
        if !to.get_no_reply() {
            self.send(reply);
        }
    }

    pub fn emit(&self, e: &Effect) {
        if let Some(signal) = effect_signal(e) {
            self.send(signal);
        }
    }

    pub fn send(&self, msg: Message) {
        let channel = self.conn.channel();
        if channel.send(msg).is_err() {
            eprintln!("notificacoes: mensagem D-Bus não enfileirou");
        }
        // `flush` é DO_READING|DO_WRITING|BLOCK: pode ter lido mensagens sem o fd ficar legível.
        channel.flush();
        self.schedule_drain();
    }

    /// Agenda um `drain` no fim do dispatch atual, a menos que um já esteja em curso ou agendado.
    fn schedule_drain(&self) {
        if self.dead.get() || self.drain_pending.replace(true) {
            return;
        }
        #[cfg(test)]
        self.idles_scheduled.set(self.idles_scheduled.get() + 1);
        self.handle.insert_idle(|state| {
            drain(state);
        });
    }

    /// Lê o socket sem bloquear; `false` se a conexão caiu.
    fn pump(&self) -> bool {
        let channel = self.conn.channel();
        let alive = channel.read_write(Some(Duration::ZERO)).is_ok() && channel.is_connected();
        if !alive {
            self.dead.set(true);
        }
        alive
    }

    fn pop(&self) -> Option<Message> {
        self.conn.channel().pop_message()
    }
}

fn is_disconnected(msg: &Message) -> bool {
    msg.msg_type() == MessageType::Signal
        && msg.interface().is_some_and(|i| &*i == "org.freedesktop.DBus.Local")
        && msg.member().is_some_and(|m| &*m == "Disconnected")
}

/// Lê o que houver e trata todas as mensagens enfileiradas. Se a conexão caiu: loga uma vez,
/// agenda a saída do `Bus` e pede a remoção da fonte (o daemon segue sem D-Bus). Fora do
/// callback da fonte o `PostAction` é ignorado; o HUP da cópia do fd a remove depois.
pub fn drain(state: &mut State) -> PostAction {
    let mut alive = match &state.bus {
        Some(bus) if bus.dead.get() => return PostAction::Remove,
        Some(bus) => {
            // Os `send` das respostas abaixo não agendam: o laço de `pop` já cobre o que o
            // `flush` deles ler para o buffer.
            bus.drain_pending.set(true);
            bus.pump()
        }
        None => return PostAction::Remove,
    };
    while let Some(msg) = state.bus.as_ref().and_then(Bus::pop) {
        if is_disconnected(&msg) {
            alive = false;
            continue;
        }
        state.on_message(msg);
    }
    if alive {
        if let Some(bus) = &state.bus {
            bus.drain_pending.set(false);
        }
        return PostAction::Continue;
    }
    if let Some(bus) = &state.bus {
        bus.dead.set(true);
    }
    eprintln!("notificacoes: a conexão com o barramento de sessão caiu; seguindo sem D-Bus");
    state.handle.insert_idle(|state| state.bus = None);
    PostAction::Remove
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::time::Instant;

    use dbus::blocking::Connection;
    use smithay_client_toolkit::reexports::calloop::EventLoop;

    use super::*;
    use crate::dbus::server::{NotifyArgs, notify_call};
    use crate::headless::Headless;
    use crate::store::{CloseReason, Store};
    use crate::Screen;

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

    /// Laço + estado com o `Bus` no barramento privado; o idle inicial já rodou.
    fn daemon(bus: &PrivateBus) -> (EventLoop<'static, State>, State) {
        let event_loop: EventLoop<'static, State> = EventLoop::try_new().unwrap();
        let handle = event_loop.handle();
        let conn = Bus::connect(&handle, bus.channel()).unwrap();
        let mut state = State {
            store: Store::new(4),
            cfg: crate::config::Config { sound: false, ..crate::config::Config::default() },
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
            usb: None,
        };
        let mut event_loop = event_loop;
        event_loop.dispatch(Some(Duration::ZERO), &mut state).unwrap();
        (event_loop, state)
    }

    fn wait_readable(fd: i32) {
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        assert_eq!(unsafe { libc::poll(&mut pfd, 1, 2000) }, 1, "a chamada não chegou ao socket do daemon");
    }

    fn fd_readable_now(fd: i32) -> bool {
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        unsafe { libc::poll(&mut pfd, 1, 0) == 1 }
    }

    /// A resposta de `serial` chegou ao cliente em até 500 ms (ignora o resto, como `NameAcquired`).
    fn answered(client: &Connection, serial: u32) -> bool {
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            if let Ok(Some(m)) = client.channel().blocking_pop_message(Duration::from_millis(50))
                && m.msg_type() == MessageType::MethodReturn
                && m.get_reply_serial() == Some(serial)
            {
                return true;
            }
        }
        false
    }

    fn args() -> NotifyArgs {
        NotifyArgs {
            app_name: "teste".into(),
            replaces_id: 0,
            app_icon: String::new(),
            summary: "s".into(),
            body: String::new(),
            hints: vec![],
            expire_timeout: -1,
        }
    }

    /// Um `send` fora do `drain` (fonte Wayland, udev, timer) cujo `flush` deixou uma chamada no
    /// buffer interno do libdbus, com o fd já não legível: a chamada tem de ser atendida no mesmo
    /// dispatch, sem esperar o próximo evento qualquer.
    #[test]
    fn send_outside_drain_handles_buffered_calls_in_same_dispatch() {
        let bus = PrivateBus::start();
        let (mut event_loop, mut state) = daemon(&bus);
        let client = Connection::from(bus.channel());
        let serial = client.channel().send(notify_call(&args())).unwrap();
        client.channel().flush();

        let daemon_bus = state.bus.as_ref().unwrap();
        let fd = daemon_bus.conn.channel().watch().fd;
        wait_readable(fd);
        // O que o `flush` (DO_READING|DO_WRITING|BLOCK) pode fazer: ler a chamada para o buffer.
        daemon_bus.conn.channel().read_write(Some(Duration::ZERO)).unwrap();
        assert!(!fd_readable_now(fd), "o fd ainda está legível; o cenário não se montou");

        daemon_bus.emit(&Effect::Closed(99, CloseReason::Closed));
        event_loop.dispatch(Some(Duration::ZERO), &mut state).unwrap();

        assert!(answered(&client, serial), "o Notify ficou no buffer sem resposta");
    }

    /// Uma rajada de `send` fora do `drain` agenda exatamente um idle; depois do dispatch, a
    /// próxima rajada agenda outro. Dentro do `drain` (respostas), nenhum.
    #[test]
    fn one_idle_per_burst() {
        let bus = PrivateBus::start();
        let (mut event_loop, mut state) = daemon(&bus);
        let daemon_bus = state.bus.as_ref().unwrap();
        let before = daemon_bus.idles_scheduled.get();
        for id in 1..=3 {
            daemon_bus.emit(&Effect::Closed(id, CloseReason::Closed));
        }
        assert_eq!(daemon_bus.idles_scheduled.get(), before + 1);
        event_loop.dispatch(Some(Duration::ZERO), &mut state).unwrap();
        let daemon_bus = state.bus.as_ref().unwrap();
        assert!(!daemon_bus.drain_pending.get());
        daemon_bus.emit(&Effect::Closed(4, CloseReason::Closed));
        daemon_bus.emit(&Effect::Closed(5, CloseReason::Closed));
        assert_eq!(daemon_bus.idles_scheduled.get(), before + 2);
        event_loop.dispatch(Some(Duration::ZERO), &mut state).unwrap();

        // Chamadas atendidas dentro do `drain` respondem sem agendar idle.
        let client = Connection::from(bus.channel());
        let serial = client.channel().send(notify_call(&args())).unwrap();
        client.channel().flush();
        let fd = state.bus.as_ref().unwrap().conn.channel().watch().fd;
        wait_readable(fd);
        let scheduled = state.bus.as_ref().unwrap().idles_scheduled.get();
        drain(&mut state);
        let daemon_bus = state.bus.as_ref().unwrap();
        assert_eq!(daemon_bus.idles_scheduled.get(), scheduled);
        assert!(!daemon_bus.drain_pending.get());
        assert!(answered(&client, serial), "o Notify atendido no drain ficou sem resposta");
    }
}

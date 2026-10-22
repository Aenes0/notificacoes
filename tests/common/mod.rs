//! Apoio dos testes de integração: barramento de sessão privado, o daemon `--sem-tela` e um
//! cliente D-Bus. Os testes só falam com o processo pelo barramento.

#![allow(dead_code)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use dbus::arg::IterAppend;
use dbus::blocking::Connection;
use dbus::channel::Channel;
use dbus::message::MessageType;
use dbus::strings::Signature;
use dbus::Message;

pub const NAME: &str = "org.freedesktop.Notifications";
pub const PATH_FDO: &str = "/org/freedesktop/Notifications";
pub const IFACE_FDO: &str = "org.freedesktop.Notifications";
pub const PATH_APPLET: &str = "/com/system76/NotificationsApplet";
pub const IFACE_APPLET: &str = "com.system76.NotificationsApplet";
pub const TIMEOUT: Duration = Duration::from_secs(2);

/// `dbus-daemon` de sessão só deste teste; morre no `Drop`.
pub struct PrivateBus {
    pub address: String,
    child: Child,
}

impl PrivateBus {
    pub fn start() -> PrivateBus {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("dbus-daemon não iniciou");
        let stdout = child.stdout.take().expect("sem stdout do dbus-daemon");
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).expect("dbus-daemon não imprimiu o endereço");
        let address = line.trim().to_string();
        assert!(!address.is_empty(), "endereço vazio do dbus-daemon");
        PrivateBus { address, child }
    }

    /// Derruba o barramento (o `Drop` continua seguro).
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Cliente novo, registrado no barramento.
    pub fn client(&self) -> Connection {
        let mut channel = Channel::open_private(&self.address).expect("cliente não conectou");
        channel.register().expect("cliente não registrou");
        Connection::from(channel)
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Mata o daemon ao sair do escopo (declare depois do `PrivateBus`).
pub struct KillOnDrop(pub Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Diretório próprio de `XDG_CONFIG_HOME` dentro de `CARGO_TARGET_TMPDIR`.
pub fn config_dir(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("xdg").join(test);
    std::fs::create_dir_all(&dir).expect("não criou o XDG_CONFIG_HOME do teste");
    dir
}

/// Comando do daemon `--sem-tela` no barramento privado, sem herdar variáveis do ambiente do teste.
pub fn daemon_command(bus: &PrivateBus, xdg_config_home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_notificacoes"));
    cmd.arg("--sem-tela")
        .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
        .env("XDG_CONFIG_HOME", xdg_config_home)
        // Sem temas de som no ambiente do teste: o daemon não acha arquivo e não toca nada de verdade.
        .env("XDG_DATA_HOME", "/nonexistent-notificacoes-teste")
        .env("XDG_DATA_DIRS", "/nonexistent-notificacoes-teste")
        .env_remove("DAEMON_NOTIFICATIONS_FD")
        .env_remove("NOTIFICACOES_DEBUG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(if std::env::var_os("NOTIFICACOES_TESTE_LOG").is_some() { Stdio::inherit() } else { Stdio::null() });
    cmd
}

/// Sobe o daemon e espera o nome ter dono (≤ 2 s).
pub fn spawn_daemon(bus: &PrivateBus, xdg_config_home: &Path) -> Child {
    spawn_and_wait(daemon_command(bus, xdg_config_home), bus)
}

pub fn spawn_and_wait(mut cmd: Command, bus: &PrivateBus) -> Child {
    let mut child = cmd.spawn().expect("daemon não iniciou");
    let client = bus.client();
    if !wait_until(TIMEOUT, || name_has_owner(&client)) {
        let _ = child.kill();
        let _ = child.wait();
        panic!("o daemon não tomou {NAME} em {TIMEOUT:?}");
    }
    child
}

pub fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn bus_call<R: dbus::arg::ReadAll, A: dbus::arg::AppendAll>(conn: &Connection, member: &str, args: A) -> Result<R, dbus::Error> {
    conn.with_proxy("org.freedesktop.DBus", "/org/freedesktop/DBus", TIMEOUT).method_call("org.freedesktop.DBus", member, args)
}

pub fn name_has_owner(conn: &Connection) -> bool {
    bus_call::<(bool,), _>(conn, "NameHasOwner", (NAME,)).map(|(b,)| b).unwrap_or(false)
}

pub fn name_owner(conn: &Connection) -> Option<String> {
    bus_call::<(String,), _>(conn, "GetNameOwner", (NAME,)).ok().map(|(s,)| s)
}

pub fn queued_owners(conn: &Connection) -> Vec<String> {
    bus_call::<(Vec<String>,), _>(conn, "ListQueuedOwners", (NAME,)).map(|(v,)| v).unwrap_or_default()
}

/// Uma dica `{sv}` para `notify_msg`.
pub enum Hint {
    Bool(&'static str, bool),
    Str(&'static str, &'static str),
    Byte(&'static str, u8),
}

/// Mensagem `Notify` completa (sem enviar).
pub fn notify_msg(summary: &str, actions: &[&str], hints: &[Hint], expire_timeout: i32) -> Message {
    let mut m = Message::new_method_call(NAME, PATH_FDO, IFACE_FDO, "Notify").expect("Notify");
    {
        let mut it = IterAppend::new(&mut m);
        it.append("teste");
        it.append(0u32);
        it.append("");
        it.append(summary);
        it.append("corpo");
        it.append(actions.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        it.append_dict(&Signature::new("s").expect("s"), &Signature::new("v").expect("v"), |d| {
            for hint in hints {
                d.append_dict_entry(|e| match hint {
                    Hint::Bool(k, v) => {
                        e.append(*k);
                        e.append(dbus::arg::Variant(*v));
                    }
                    Hint::Str(k, v) => {
                        e.append(*k);
                        e.append(dbus::arg::Variant(*v));
                    }
                    Hint::Byte(k, v) => {
                        e.append(*k);
                        e.append(dbus::arg::Variant(*v));
                    }
                });
            }
        });
        it.append(expire_timeout);
    }
    m
}

/// Envia `Notify` e devolve o id.
pub fn notify(conn: &Connection, summary: &str, actions: &[&str], hints: &[Hint], expire_timeout: i32) -> u32 {
    let reply = conn
        .channel()
        .send_with_reply_and_block(notify_msg(summary, actions, hints, expire_timeout), TIMEOUT)
        .expect("Notify sem resposta");
    reply.read1::<u32>().expect("Notify não devolveu u32")
}

pub fn close_notification(conn: &Connection, id: u32) {
    conn.with_proxy(NAME, PATH_FDO, TIMEOUT)
        .method_call::<(), _, _, _>(IFACE_FDO, "CloseNotification", (id,))
        .expect("CloseNotification falhou");
}

pub fn invoke_action(conn: &Connection, id: u32, key: &str) {
    conn.with_proxy(NAME, PATH_APPLET, TIMEOUT)
        .method_call::<(), _, _, _>(IFACE_APPLET, "InvokeAction", (id, key))
        .expect("InvokeAction falhou");
}

/// Passa a receber os sinais da interface de notificações.
pub fn subscribe(conn: &Connection) {
    conn.add_match_no_cb(&format!("type='signal',interface='{IFACE_FDO}'")).expect("AddMatch falhou");
}

#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    Closed(u32, u32),
    Action(u32, String),
    Token(u32, String),
}

fn parse_signal(m: &Message) -> Option<Signal> {
    if m.msg_type() != MessageType::Signal || &*m.interface()? != IFACE_FDO {
        return None;
    }
    match &*m.member()? {
        "NotificationClosed" => m.read2::<u32, u32>().ok().map(|(id, r)| Signal::Closed(id, r)),
        "ActionInvoked" => m.read2::<u32, String>().ok().map(|(id, k)| Signal::Action(id, k)),
        "ActivationToken" => m.read2::<u32, String>().ok().map(|(id, t)| Signal::Token(id, t)),
        _ => None,
    }
}

/// Todas as mensagens que chegarem em `window` (ou até `stop` dizer chega).
pub fn messages(conn: &Connection, window: Duration, mut stop: impl FnMut(&[Message]) -> bool) -> Vec<Message> {
    let deadline = Instant::now() + window;
    let mut out = Vec::new();
    loop {
        let now = Instant::now();
        if now >= deadline || stop(&out) {
            return out;
        }
        match conn.channel().blocking_pop_message(deadline - now) {
            Ok(Some(m)) => out.push(m),
            Ok(None) => {}
            Err(_) => return out,
        }
    }
}

/// Sinais recebidos em `window`.
pub fn signals_within(conn: &Connection, window: Duration) -> Vec<Signal> {
    messages(conn, window, |_| false).iter().filter_map(parse_signal).collect()
}

/// Espera até `n` sinais (≤ 2 s) e devolve os que vieram.
pub fn wait_signals(conn: &Connection, n: usize) -> Vec<Signal> {
    let mut got = Vec::new();
    let deadline = Instant::now() + TIMEOUT;
    while got.len() < n {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match conn.channel().blocking_pop_message(deadline - now) {
            Ok(Some(m)) => got.extend(parse_signal(&m)),
            Ok(None) => {}
            Err(_) => break,
        }
    }
    got
}

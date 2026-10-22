//! Integração do logind: um segundo barramento privado faz papel de barramento de sistema. O teste
//! é o `org.freedesktop.login1` e emite `PrepareForSleep`; o daemon (`--sem-tela`,
//! `NOTIFICACOES_DEBUG=1`) escreve `logind: suspensao=<b>` no stderr.
//!
//! A tela de bloqueio ficou fora (decisão do usuário): nada de `LockedHint` nem de sessão.

mod common;

use std::io::{BufRead, BufReader};
use std::process::Stdio;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use common::*;
use dbus::Message;
use dbus::blocking::Connection;

const LOGIN1: &str = "org.freedesktop.login1";
const LOGIN1_PATH: &str = "/org/freedesktop/login1";
const MANAGER: &str = "org.freedesktop.login1.Manager";
const LISTENING: &str = "notificacoes: logind: escutando";

/// O daemon com o barramento de sistema em `system` e as linhas do stderr num canal.
fn spawn_with_system(session: &PrivateBus, system_address: &str, test: &str) -> (KillOnDrop, Receiver<String>) {
    let mut cmd = daemon_command(session, &config_dir(test));
    cmd.env("DBUS_SYSTEM_BUS_ADDRESS", system_address).env("NOTIFICACOES_DEBUG", "1").stderr(Stdio::piped());
    let mut child = spawn_and_wait(cmd, session);
    let stderr = child.stderr.take().expect("stderr");
    let (tx, rx) = mpsc::channel();
    // Só o teste usa thread: o daemon continua com uma.
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    (KillOnDrop(child), rx)
}

/// Espera uma linha que contenha `needle` (≤ 2 s); as outras são descartadas.
fn wait_line(rx: &Receiver<String>, needle: &str) -> Option<String> {
    let deadline = Instant::now() + TIMEOUT;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(line) if line.contains(needle) => return Some(line),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    None
}

/// Linhas com `needle` que chegarem em `window`.
fn lines_within(rx: &Receiver<String>, needle: &str, window: Duration) -> Vec<String> {
    let deadline = Instant::now() + window;
    let mut out = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(line) if line.contains(needle) => out.push(line),
            Ok(_) => {}
            Err(_) => break,
        }
    }
    out
}

fn sleep_signal(path: &str, going: bool) -> Message {
    Message::new_signal(path, MANAGER, "PrepareForSleep").expect("sinal").append1(going)
}

fn emit(conn: &Connection, msg: Message) {
    conn.channel().send(msg).expect("send do sinal");
    conn.channel().flush();
}

/// Barramento de sistema com o `org.freedesktop.login1` deste teste.
fn system_with_login1() -> (PrivateBus, Connection) {
    let system = PrivateBus::start();
    let login1 = system.client();
    login1.request_name(LOGIN1, false, true, true).expect("RequestName login1");
    (system, login1)
}

#[test]
fn prepare_for_sleep_reported() {
    let session = PrivateBus::start();
    let (system, login1) = system_with_login1();
    let (_d, rx) = spawn_with_system(&session, &system.address, "prepare_for_sleep_reported");
    assert!(wait_line(&rx, LISTENING).is_some(), "o daemon não se inscreveu no logind");

    emit(&login1, sleep_signal(LOGIN1_PATH, true));
    assert_eq!(wait_line(&rx, "logind: suspensao=").as_deref(), Some("notificacoes: logind: suspensao=true"));
    emit(&login1, sleep_signal(LOGIN1_PATH, false));
    assert_eq!(wait_line(&rx, "logind: suspensao=").as_deref(), Some("notificacoes: logind: suspensao=false"));
}

/// No lugar do `falls_back_to_auto_session_path` do brief (sem `LockedHint` não há caminho de
/// sessão): só o dono de `org.freedesktop.login1`, em difusão e no caminho do Manager, conta.
#[test]
fn prepare_for_sleep_from_impostor_ignored() {
    let session = PrivateBus::start();
    let (system, login1) = system_with_login1();
    let (_d, rx) = spawn_with_system(&session, &system.address, "prepare_for_sleep_from_impostor_ignored");
    assert!(wait_line(&rx, LISTENING).is_some(), "o daemon não se inscreveu no logind");

    // Outro processo no barramento de sistema, em difusão.
    let impostor = system.client();
    emit(&impostor, sleep_signal(LOGIN1_PATH, true));
    // Outro processo, endereçado direto ao daemon (passa por cima das regras de match).
    let daemon_name = daemon_unique_name(&impostor, &[login1.unique_name().to_string(), impostor.unique_name().to_string()]);
    let mut direct = sleep_signal(LOGIN1_PATH, true);
    direct.set_destination(Some(daemon_name.into()));
    emit(&impostor, direct);
    // O próprio logind, mas em outro caminho.
    emit(&login1, sleep_signal("/org/freedesktop/login1/session/_31", true));
    assert_eq!(lines_within(&rx, "logind: suspensao=", Duration::from_millis(300)), Vec::<String>::new());

    // O verdadeiro ainda passa.
    emit(&login1, sleep_signal(LOGIN1_PATH, true));
    assert_eq!(wait_line(&rx, "logind: suspensao=").as_deref(), Some("notificacoes: logind: suspensao=true"));
}

/// O nome único do daemon no barramento: o único que não é do teste nem do próprio barramento.
fn daemon_unique_name(conn: &Connection, ours: &[String]) -> String {
    let names: (Vec<String>,) = conn
        .with_proxy("org.freedesktop.DBus", "/org/freedesktop/DBus", TIMEOUT)
        .method_call("org.freedesktop.DBus", "ListNames", ())
        .expect("ListNames");
    let others: Vec<String> = names.0.into_iter().filter(|n| n.starts_with(':') && !ours.contains(n)).collect();
    assert_eq!(others.len(), 1, "esperava só o daemon além do teste: {others:?}");
    others.into_iter().next().expect("nome do daemon")
}

#[test]
fn system_bus_failure_keeps_daemon_running() {
    let session = PrivateBus::start();
    let missing = format!("unix:path={}/nao-existe/system_bus_socket", env!("CARGO_TARGET_TMPDIR"));
    let (mut d, rx) = spawn_with_system(&session, &missing, "system_bus_failure_keeps_daemon_running");
    let line = wait_line(&rx, "logind").expect("sem linha do logind");
    assert!(line.starts_with("notificacoes: sem barramento de sistema"), "{line}");
    // O barramento de sessão segue atendendo e o processo continua vivo.
    let c = session.client();
    assert_eq!(notify(&c, "sem logind", &[], &[], -1), 1);
    assert!(d.0.try_wait().expect("try_wait").is_none(), "o daemon saiu sem o barramento de sistema");
}

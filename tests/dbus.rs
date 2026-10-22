//! Integração do servidor D-Bus: o binário `--sem-tela` num barramento de sessão privado.

mod common;

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::time::Duration;

use common::*;
use dbus::message::MessageType;

fn setup(test: &str) -> (PrivateBus, std::path::PathBuf) {
    (PrivateBus::start(), config_dir(test))
}

#[test]
fn notify_returns_increasing_ids() {
    let (bus, dir) = setup("notify_returns_increasing_ids");
    let _d = KillOnDrop(spawn_daemon(&bus, &dir));
    let c = bus.client();
    assert_eq!(notify(&c, "um", &[], &[], -1), 1);
    assert_eq!(notify(&c, "dois", &[], &[], -1), 2);
    assert_eq!(notify(&c, "três", &[], &[], -1), 3);
}

#[test]
fn capabilities_and_server_information() {
    let (bus, dir) = setup("capabilities_and_server_information");
    let _d = KillOnDrop(spawn_daemon(&bus, &dir));
    let c = bus.client();
    let p = c.with_proxy(NAME, PATH_FDO, TIMEOUT);
    let (caps,): (Vec<String>,) = p.method_call(IFACE_FDO, "GetCapabilities", ()).unwrap();
    assert_eq!(caps, ["actions", "action-icons", "body", "body-markup", "icon-static", "persistence", "sound"]);
    let info: (String, String, String, String) = p.method_call(IFACE_FDO, "GetServerInformation", ()).unwrap();
    assert_eq!(info, ("notificacoes".into(), "Aenes".into(), env!("CARGO_PKG_VERSION").into(), "1.2".into()));
}

#[test]
fn close_notification_emits_reason_3() {
    let (bus, dir) = setup("close_notification_emits_reason_3");
    let _d = KillOnDrop(spawn_daemon(&bus, &dir));
    let c = bus.client();
    subscribe(&c);
    let id = notify(&c, "fechar", &[], &[], -1);
    close_notification(&c, id);
    assert_eq!(wait_signals(&c, 1), [Signal::Closed(id, 3)]);
}

#[test]
fn invoke_action_emits_action_then_closed_2() {
    let (bus, dir) = setup("invoke_action_emits_action_then_closed_2");
    let _d = KillOnDrop(spawn_daemon(&bus, &dir));
    let c = bus.client();
    subscribe(&c);
    let id = notify(&c, "ação", &["ok", "OK", "nao", "Não"], &[], -1);
    invoke_action(&c, id, "ok");
    assert_eq!(wait_signals(&c, 2), [Signal::Action(id, "ok".into()), Signal::Closed(id, 2)]);
}

#[test]
fn expired_card_emits_nothing_and_stays_invokable() {
    let (bus, dir) = setup("expired_card_emits_nothing_and_stays_invokable");
    let _d = KillOnDrop(spawn_daemon(&bus, &dir));
    let c = bus.client();
    subscribe(&c);
    let id = notify(&c, "expira", &["default", "Abrir"], &[], 300);
    assert_eq!(signals_within(&c, Duration::from_millis(600)), []);
    invoke_action(&c, id, "default");
    assert_eq!(wait_signals(&c, 2), [Signal::Action(id, "default".into()), Signal::Closed(id, 2)]);
}

#[test]
fn transient_expires_with_reason_1() {
    let (bus, dir) = setup("transient_expires_with_reason_1");
    let _d = KillOnDrop(spawn_daemon(&bus, &dir));
    let c = bus.client();
    subscribe(&c);
    let id = notify(&c, "passageira", &[], &[Hint::Bool("transient", true), Hint::Byte("urgency", 1)], 300);
    assert_eq!(signals_within(&c, Duration::from_millis(150)), []);
    assert_eq!(wait_signals(&c, 1), [Signal::Closed(id, 1)]);
}

#[test]
fn no_reply_expected_gets_no_reply() {
    let (bus, dir) = setup("no_reply_expected_gets_no_reply");
    let _d = KillOnDrop(spawn_daemon(&bus, &dir));
    let c = bus.client();
    let mut m = notify_msg("sem resposta", &[], &[], -1);
    m.set_no_reply(true);
    let serial = c.channel().send(m).expect("send");
    let got = messages(&c, Duration::from_millis(300), |_| false);
    assert!(
        !got.iter().any(|m| matches!(m.msg_type(), MessageType::MethodReturn | MessageType::Error) && m.get_reply_serial() == Some(serial)),
        "veio resposta para uma chamada sem resposta"
    );
    assert_eq!(notify(&c, "com resposta", &[], &[], -1), 2);
}

#[test]
fn takes_name_from_owner_that_allows_replacement() {
    let (bus, dir) = setup("takes_name_from_owner_that_allows_replacement");
    let previous = bus.client();
    previous.request_name(NAME, true, false, false).expect("RequestName do dono prévio");
    let mine = previous.unique_name().to_string();
    let _d = KillOnDrop(spawn_and_wait(daemon_command(&bus, &dir), &bus));
    let c = bus.client();
    assert!(wait_until(TIMEOUT, || name_owner(&c).is_some_and(|o| o != mine)), "o daemon não tomou o nome");
    assert_eq!(notify(&c, "tomado", &[], &[], -1), 1);
}

#[test]
fn waits_in_queue_when_owner_forbids_replacement() {
    let (bus, dir) = setup("waits_in_queue_when_owner_forbids_replacement");
    let previous = bus.client();
    previous.request_name(NAME, false, false, false).expect("RequestName do dono prévio");
    let mine = previous.unique_name().to_string();
    let mut daemon = KillOnDrop(daemon_command(&bus, &dir).spawn().expect("daemon"));
    let c = bus.client();
    assert!(wait_until(TIMEOUT, || queued_owners(&c).len() == 2), "o daemon não entrou na fila");
    assert_eq!(name_owner(&c), Some(mine.clone()));
    std::thread::sleep(Duration::from_millis(200));
    assert!(daemon.0.try_wait().expect("try_wait").is_none(), "o daemon saiu em vez de esperar na fila");
    previous.release_name(NAME).expect("ReleaseName");
    assert!(wait_until(TIMEOUT, || name_owner(&c).is_some_and(|o| o != mine)), "o daemon não virou dono");
    assert_eq!(notify(&c, "da fila", &[], &[], -1), 1);
}

#[test]
fn panel_fd_is_kept_and_cloexec() {
    let (bus, dir) = setup("panel_fd_is_kept_and_cloexec");
    let mut fds = [0i32; 2];
    // SOCK_CLOEXEC: só o filho deste teste herda (o pre_exec tira o CLOEXEC de um lado).
    let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0, fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "socketpair falhou");
    let [panel, other] = fds;
    let mut cmd = daemon_command(&bus, &dir);
    cmd.env("DAEMON_NOTIFICATIONS_FD", panel.to_string());
    unsafe {
        cmd.pre_exec(move || {
            let flags = libc::fcntl(panel, libc::F_GETFD);
            if flags < 0 || libc::fcntl(panel, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let d = KillOnDrop(spawn_and_wait(cmd, &bus));
    let pid = d.0.id();

    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::fstat(panel, &mut st) }, 0);
    let link = std::fs::read_link(format!("/proc/{pid}/fd/{panel}")).expect("o daemon não tem o fd do painel");
    assert_eq!(link.to_string_lossy(), format!("socket:[{}]", st.st_ino));
    let info = std::fs::read_to_string(format!("/proc/{pid}/fdinfo/{panel}")).expect("fdinfo");
    let flags = info
        .lines()
        .find_map(|l| l.strip_prefix("flags:"))
        .and_then(|v| u32::from_str_radix(v.trim(), 8).ok())
        .expect("sem flags no fdinfo");
    assert_ne!(flags & libc::O_CLOEXEC as u32, 0, "fd do painel sem FD_CLOEXEC (flags {flags:o})");
    unsafe {
        libc::close(panel);
        libc::close(other);
    }
}

#[test]
fn debug_env_prints_notify_line() {
    let (bus, dir) = setup("debug_env_prints_notify_line");
    let mut cmd = daemon_command(&bus, &dir);
    cmd.env("NOTIFICACOES_DEBUG", "1").stderr(Stdio::piped());
    let mut d = KillOnDrop(spawn_and_wait(cmd, &bus));
    let c = bus.client();
    let id = notify(&c, "depurar", &[], &[], -1);
    std::thread::sleep(Duration::from_millis(200));
    // Matar antes de ler: a leitura acaba no EOF, nunca trava.
    let _ = d.0.kill();
    let stderr = d.0.stderr.take().expect("stderr");
    let line = BufReader::new(stderr)
        .lines()
        .map_while(Result::ok)
        .find(|l| l.contains("notify id="))
        .expect("sem linha de depuração");
    assert_eq!(line, format!("notificacoes: notify id={id} app=teste"));
}

#[test]
fn single_thread() {
    let (bus, dir) = setup("single_thread");
    let d = KillOnDrop(spawn_daemon(&bus, &dir));
    let c = bus.client();
    notify(&c, "uma thread", &[], &[], 300);
    let status = std::fs::read_to_string(format!("/proc/{}/status", d.0.id())).expect("status");
    assert!(status.lines().any(|l| l.split_whitespace().collect::<Vec<_>>() == ["Threads:", "1"]), "{status}");
}

#[test]
fn survives_bus_death_without_spinning() {
    let (mut bus, dir) = setup("survives_bus_death_without_spinning");
    let mut d = KillOnDrop(spawn_daemon(&bus, &dir));
    let c = bus.client();
    notify(&c, "antes de cair", &[], &[], -1);
    drop(c);
    bus.kill();
    std::thread::sleep(Duration::from_millis(300));
    assert!(d.0.try_wait().expect("try_wait").is_none(), "o daemon saiu quando o barramento caiu");
    let switches = || -> u64 {
        let status = std::fs::read_to_string(format!("/proc/{}/status", d.0.id())).expect("status");
        status
            .lines()
            .filter(|l| l.contains("ctxt_switches"))
            .filter_map(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
            .sum()
    };
    let before = switches();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(switches() - before, 0, "o daemon acorda sem barramento");
}

/// Grava como o COSMIC: arquivo temporário + rename dentro do diretório vigiado.
fn write_dnd(xdg: &std::path::Path, value: &str) {
    let dir = xdg.join("cosmic/com.system76.CosmicNotifications/v1");
    std::fs::create_dir_all(&dir).expect("diretório do dnd");
    let tmp = dir.join(".do_not_disturb.tmp");
    std::fs::write(&tmp, value).expect("tmp");
    std::fs::rename(&tmp, dir.join("do_not_disturb")).expect("rename");
}

#[test]
fn dnd_file_is_picked_up_live_by_the_watcher() {
    let (bus, dir) = setup("dnd_file_is_picked_up_live_by_the_watcher");
    let _ = std::fs::remove_dir_all(dir.join("cosmic"));
    let _d = KillOnDrop(spawn_daemon(&bus, &dir));
    let c = bus.client();
    subscribe(&c);
    let transient = [Hint::Bool("transient", true), Hint::Byte("urgency", 1)];
    // Desligado: a passageira aparece e só sai com o tempo (não há sinal imediato).
    let id = notify(&c, "a", &[], &transient, 400);
    assert_eq!(signals_within(&c, Duration::from_millis(150)), []);
    assert_eq!(wait_signals(&c, 1), [Signal::Closed(id, 1)]);
    // Ligado pelo arquivo: o aviso é aceito e some na hora, sem reiniciar o daemon.
    write_dnd(&dir, "true\n");
    std::thread::sleep(Duration::from_millis(200));
    let id = notify(&c, "b", &[], &transient, 400);
    assert_eq!(signals_within(&c, Duration::from_millis(150)), [Signal::Closed(id, 1)]);
    // Desligado de novo.
    write_dnd(&dir, "false\n");
    std::thread::sleep(Duration::from_millis(200));
    let id = notify(&c, "c", &[], &transient, 400);
    assert_eq!(signals_within(&c, Duration::from_millis(150)), []);
    assert_eq!(wait_signals(&c, 1), [Signal::Closed(id, 1)]);
}

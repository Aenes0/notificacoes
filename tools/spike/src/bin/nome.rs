//! Spike: pede `org.freedesktop.Notifications` com as flags das Global Constraints
//! (`allow_replacement = false`, `replace_existing = true`, `do_not_queue = false`), imprime a
//! resposta (`PrimaryOwner`/`InQueue`/…), responde `Notify` com ids crescentes e imprime cada
//! mensagem recebida (chamadas e sinais do barramento, como `NameAcquired`/`NameLost`).

use std::time::Duration;

use dbus::blocking::Connection;
use dbus::channel::{MatchingReceiver, Sender};
use dbus::message::{MatchRule, MessageType};
use dbus::Message;

const NAME: &str = "org.freedesktop.Notifications";

fn main() {
    if let Err(e) = run() {
        eprintln!("nome: erro: {e}");
        std::process::exit(1);
    }
}

fn args_debug(msg: &Message) -> Vec<String> {
    let mut out = Vec::new();
    let mut it = msg.iter_init();
    loop {
        if let Some(a) = it.get_refarg() {
            out.push(format!("{a:?}"));
        }
        if !it.next() {
            break;
        }
    }
    out
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::new_session()?;
    println!("nome: unique_name={} pid={}", conn.unique_name(), std::process::id());
    let reply = conn.request_name(NAME, false, true, false)?;
    println!("nome: RequestName({NAME}, allow_replacement=false, replace_existing=true, do_not_queue=false) = {reply:?}");

    let mut next_id: u32 = 0;
    conn.start_receive(
        MatchRule::new(),
        Box::new(move |msg: Message, conn: &Connection| {
            let member = msg.member().map(|m| m.to_string()).unwrap_or_default();
            let iface = msg.interface().map(|m| m.to_string()).unwrap_or_default();
            match msg.msg_type() {
                MessageType::Signal => {
                    println!("nome: sinal {iface}.{member} {:?}", args_debug(&msg));
                }
                MessageType::MethodCall => {
                    println!(
                        "nome: chamada {iface}.{member} de {} {:?}",
                        msg.sender().map(|s| s.to_string()).unwrap_or_default(),
                        args_debug(&msg)
                    );
                    let resposta = match member.as_str() {
                        "Notify" => {
                            next_id = next_id.wrapping_add(1).max(1);
                            println!("nome: Notify -> id {next_id}");
                            Some(msg.method_return().append1(next_id))
                        }
                        "GetCapabilities" => Some(msg.method_return().append1(vec!["body".to_string()])),
                        "GetServerInformation" => Some(
                            msg.method_return()
                                .append3("notificacoes-spike", "Aenes", "0.0.0")
                                .append1("1.2"),
                        ),
                        "CloseNotification" => Some(msg.method_return()),
                        _ => Some(msg.error(
                            &"org.freedesktop.DBus.Error.UnknownMethod".into(),
                            c"spike: metodo desconhecido",
                        )),
                    };
                    if !msg.get_no_reply()
                        && let Some(r) = resposta
                        && conn.send(r).is_err()
                    {
                        eprintln!("nome: falha ao enviar resposta");
                    }
                }
                _ => {}
            }
            true
        }),
    );

    loop {
        conn.process(Duration::from_secs(3600))?;
    }
}

//! Protocolo D-Bus puro: decodifica chamadas recebidas em `Call` e monta sinais e chamadas.
//! Sem conexão e sem estado; quem fala com o barramento é `conn.rs`.

use dbus::arg::{ArgType, Iter, IterAppend, Variant};
use dbus::Message;

use crate::model::{HintValue, RawImage};
use crate::store::Effect;

pub const NAME: &str = "org.freedesktop.Notifications";
pub const PATH_FDO: &str = "/org/freedesktop/Notifications";
pub const IFACE_FDO: &str = "org.freedesktop.Notifications";
pub const PATH_APPLET: &str = "/com/system76/NotificationsApplet";
pub const IFACE_APPLET: &str = "com.system76.NotificationsApplet";
pub const CAPABILITIES: &[&str] = &["actions", "action-icons", "body", "body-markup", "icon-static", "persistence", "sound"];

#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    Notify {
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: Vec<(String, HintValue)>,
        expire_timeout: i32,
    },
    CloseNotification(u32),
    GetCapabilities,
    GetServerInformation,
    InvokeAction(u32, String),
    Introspect,
    Ping,
    GetMachineId,
}

#[derive(Debug)]
pub struct DecodeError {
    pub name: &'static str,
    pub text: String,
}

const ERR_UNKNOWN_METHOD: &str = "org.freedesktop.DBus.Error.UnknownMethod";
const ERR_INVALID_ARGS: &str = "org.freedesktop.DBus.Error.InvalidArgs";
const ERR_UNKNOWN_OBJECT: &str = "org.freedesktop.DBus.Error.UnknownObject";
const IFACE_INTROSPECTABLE: &str = "org.freedesktop.DBus.Introspectable";
const IFACE_PEER: &str = "org.freedesktop.DBus.Peer";
const NOTIFY_SIGNATURE: &str = "susssasa{sv}i";
const IMAGE_SIGNATURE: &str = "(iiibiiay)";

/// Caminhos que existem na árvore de objetos (os dois objetos e os nós intermediários).
const NODES: &[&str] = &["/", "/org", "/org/freedesktop", PATH_FDO, "/com", "/com/system76", PATH_APPLET];

/// Traduz uma chamada recebida. A assinatura é conferida inteira antes de ler qualquer argumento.
pub fn decode(msg: &Message) -> Result<Call, DecodeError> {
    let path = msg.path().map(|p| p.to_string()).unwrap_or_default();
    let iface = msg.interface().map(|i| i.to_string());
    let member = msg.member().map(|m| m.to_string()).unwrap_or_default();
    let sig = args_signature(msg);
    // Sem interface, a spec deixa o método valer por nome em qualquer interface do objeto.
    let is = |name: &str| iface.as_deref().is_none_or(|i| i == name);

    if !NODES.contains(&path.as_str()) {
        return Err(DecodeError { name: ERR_UNKNOWN_OBJECT, text: format!("objeto desconhecido: {path}") });
    }
    if is(IFACE_INTROSPECTABLE) && member == "Introspect" {
        return expect(&sig, "", || Ok(Call::Introspect));
    }
    if is(IFACE_PEER) && member == "Ping" {
        return expect(&sig, "", || Ok(Call::Ping));
    }
    if is(IFACE_PEER) && member == "GetMachineId" {
        return expect(&sig, "", || Ok(Call::GetMachineId));
    }
    match path.as_str() {
        PATH_FDO if is(IFACE_FDO) => match member.as_str() {
            "Notify" => expect(&sig, NOTIFY_SIGNATURE, || read_notify(msg)),
            "CloseNotification" => expect(&sig, "u", || msg.read1::<u32>().map(Call::CloseNotification).map_err(invalid)),
            "GetCapabilities" => expect(&sig, "", || Ok(Call::GetCapabilities)),
            "GetServerInformation" => expect(&sig, "", || Ok(Call::GetServerInformation)),
            _ => Err(unknown_method(&iface, &member)),
        },
        PATH_APPLET if is(IFACE_APPLET) && member == "InvokeAction" => {
            expect(&sig, "us", || msg.read2::<u32, String>().map(|(id, key)| Call::InvokeAction(id, key)).map_err(invalid))
        }
        _ => Err(unknown_method(&iface, &member)),
    }
}

fn unknown_method(iface: &Option<String>, member: &str) -> DecodeError {
    DecodeError { name: ERR_UNKNOWN_METHOD, text: format!("método desconhecido: {}.{member}", iface.as_deref().unwrap_or("")) }
}

fn invalid(e: impl std::fmt::Display) -> DecodeError {
    DecodeError { name: ERR_INVALID_ARGS, text: e.to_string() }
}

fn expect(sig: &str, want: &str, read: impl FnOnce() -> Result<Call, DecodeError>) -> Result<Call, DecodeError> {
    if sig != want {
        return Err(DecodeError { name: ERR_INVALID_ARGS, text: format!("assinatura \"{sig}\", esperada \"{want}\"") });
    }
    read()
}

/// Assinatura de todos os argumentos, concatenada.
fn args_signature(msg: &Message) -> String {
    let mut it = msg.iter_init();
    let mut sig = String::new();
    while it.arg_type() != ArgType::Invalid {
        sig.push_str(&it.signature());
        if !it.next() {
            break;
        }
    }
    sig
}

fn read_notify(msg: &Message) -> Result<Call, DecodeError> {
    let mut it = msg.iter_init();
    let app_name: String = it.read().map_err(invalid)?;
    let replaces_id: u32 = it.read().map_err(invalid)?;
    let app_icon: String = it.read().map_err(invalid)?;
    let summary: String = it.read().map_err(invalid)?;
    let body: String = it.read().map_err(invalid)?;
    let actions: Vec<String> = it.read().map_err(invalid)?;
    let hints = read_hints(&mut it);
    it.next();
    let expire_timeout: i32 = it.read().map_err(invalid)?;
    Ok(Call::Notify { app_name, replaces_id, app_icon, summary, body, actions, hints, expire_timeout })
}

/// `a{sv}` em ordem; dica de tipo que não sabemos ler fica de fora.
fn read_hints(it: &mut Iter) -> Vec<(String, HintValue)> {
    let mut out = Vec::new();
    let Some(mut dict) = it.recurse(ArgType::Array) else {
        return out;
    };
    while dict.arg_type() == ArgType::DictEntry {
        if let Some(mut entry) = dict.recurse(ArgType::DictEntry) {
            let key: Option<String> = entry.get();
            entry.next();
            if let (Some(key), Some(mut value)) = (key, entry.recurse(ArgType::Variant))
                && let Some(v) = hint_value(&mut value)
            {
                out.push((key, v));
            }
        }
        if !dict.next() {
            break;
        }
    }
    out
}

fn hint_value(v: &mut Iter) -> Option<HintValue> {
    match v.arg_type() {
        ArgType::Boolean => v.get::<bool>().map(HintValue::Bool),
        ArgType::Byte => v.get::<u8>().map(|n| HintValue::Int(n.into())),
        ArgType::Int16 => v.get::<i16>().map(|n| HintValue::Int(n.into())),
        ArgType::UInt16 => v.get::<u16>().map(|n| HintValue::Int(n.into())),
        ArgType::Int32 => v.get::<i32>().map(|n| HintValue::Int(n.into())),
        ArgType::UInt32 => v.get::<u32>().map(|n| HintValue::Int(n.into())),
        ArgType::Int64 => v.get::<i64>().map(HintValue::Int),
        ArgType::UInt64 => v.get::<u64>().and_then(|n| i64::try_from(n).ok()).map(HintValue::Int),
        ArgType::String => v.get::<String>().map(HintValue::Str),
        ArgType::Struct if &*v.signature() == IMAGE_SIGNATURE => raw_image(v).map(HintValue::Image),
        _ => None,
    }
}

fn raw_image(v: &mut Iter) -> Option<RawImage> {
    let mut s = v.recurse(ArgType::Struct)?;
    Some(RawImage {
        width: s.read().ok()?,
        height: s.read().ok()?,
        rowstride: s.read().ok()?,
        has_alpha: s.read().ok()?,
        bits_per_sample: s.read().ok()?,
        channels: s.read().ok()?,
        data: s.get::<&[u8]>()?.to_vec(),
    })
}


macro_rules! standard_ifaces {
    () => {
        r#"  <interface name="org.freedesktop.DBus.Introspectable">
    <method name="Introspect"><arg name="xml_data" type="s" direction="out"/></method>
  </interface>
  <interface name="org.freedesktop.DBus.Peer">
    <method name="Ping"/>
    <method name="GetMachineId"><arg name="machine_uuid" type="s" direction="out"/></method>
  </interface>
"#
    };
}

macro_rules! introspection {
    ($($body:expr),* $(,)?) => {
        concat!(
            "<!DOCTYPE node PUBLIC \"-//freedesktop//DTD D-BUS Object Introspection 1.0//EN\"\n \"http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd\">\n",
            "<node>\n",
            $($body,)*
            "</node>\n"
        )
    };
}

const XML_FDO: &str = introspection!(
    r#"  <interface name="org.freedesktop.Notifications">
    <method name="Notify">
      <arg name="app_name" type="s" direction="in"/>
      <arg name="replaces_id" type="u" direction="in"/>
      <arg name="app_icon" type="s" direction="in"/>
      <arg name="summary" type="s" direction="in"/>
      <arg name="body" type="s" direction="in"/>
      <arg name="actions" type="as" direction="in"/>
      <arg name="hints" type="a{sv}" direction="in"/>
      <arg name="expire_timeout" type="i" direction="in"/>
      <arg name="id" type="u" direction="out"/>
    </method>
    <method name="CloseNotification"><arg name="id" type="u" direction="in"/></method>
    <method name="GetCapabilities"><arg name="capabilities" type="as" direction="out"/></method>
    <method name="GetServerInformation">
      <arg name="name" type="s" direction="out"/>
      <arg name="vendor" type="s" direction="out"/>
      <arg name="version" type="s" direction="out"/>
      <arg name="spec_version" type="s" direction="out"/>
    </method>
    <signal name="NotificationClosed"><arg name="id" type="u"/><arg name="reason" type="u"/></signal>
    <signal name="ActionInvoked"><arg name="id" type="u"/><arg name="action_key" type="s"/></signal>
    <signal name="ActivationToken"><arg name="id" type="u"/><arg name="activation_token" type="s"/></signal>
  </interface>
"#,
    standard_ifaces!(),
);

const XML_APPLET: &str = introspection!(
    r#"  <interface name="com.system76.NotificationsApplet">
    <method name="InvokeAction">
      <arg name="id" type="u" direction="in"/>
      <arg name="action" type="s" direction="in"/>
    </method>
  </interface>
"#,
    standard_ifaces!(),
);

const XML_ROOT: &str = introspection!(standard_ifaces!(), "  <node name=\"org\"/>\n  <node name=\"com\"/>\n");
const XML_ORG: &str = introspection!(standard_ifaces!(), "  <node name=\"freedesktop\"/>\n");
const XML_ORG_FREEDESKTOP: &str = introspection!(standard_ifaces!(), "  <node name=\"Notifications\"/>\n");
const XML_COM: &str = introspection!(standard_ifaces!(), "  <node name=\"system76\"/>\n");
const XML_COM_SYSTEM76: &str = introspection!(standard_ifaces!(), "  <node name=\"NotificationsApplet\"/>\n");
const XML_EMPTY: &str = introspection!();

/// XML de introspecção do caminho (nó vazio se o caminho não existe).
pub fn introspect_xml(path: &str) -> &'static str {
    match path {
        PATH_FDO => XML_FDO,
        PATH_APPLET => XML_APPLET,
        "/" => XML_ROOT,
        "/org" => XML_ORG,
        "/org/freedesktop" => XML_ORG_FREEDESKTOP,
        "/com" => XML_COM,
        "/com/system76" => XML_COM_SYSTEM76,
        _ => XML_EMPTY,
    }
}

fn signal(member: &'static str) -> Message {
    Message::signal(&PATH_FDO.into(), &IFACE_FDO.into(), &member.into())
}

/// Sinal que o efeito pede, em `PATH_FDO`; efeitos de tela não viram sinal.
pub fn effect_signal(e: &Effect) -> Option<Message> {
    match e {
        Effect::Closed(id, reason) => Some(signal("NotificationClosed").append2(*id, *reason as u32)),
        Effect::ActionInvoked(id, key) => Some(signal("ActionInvoked").append2(*id, key.as_str())),
        Effect::ActivationToken(id, token) => Some(signal("ActivationToken").append2(*id, token.as_str())),
        Effect::Show(_) | Effect::Update(_) | Effect::Hide(_) => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NotifyArgs {
    pub app_name: String,
    pub replaces_id: u32,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub hints: Vec<(String, HintValue)>,
    pub expire_timeout: i32,
}

/// Chamada `Notify` para `NAME`, sem ações. `urgency` vai como byte (spec); outros inteiros
/// como `i`, ou `x` se não couberem.
pub fn notify_call(args: &NotifyArgs) -> Message {
    let mut m = Message::method_call(&NAME.into(), &PATH_FDO.into(), &IFACE_FDO.into(), &"Notify".into());
    {
        let mut it = IterAppend::new(&mut m);
        it.append(args.app_name.as_str());
        it.append(args.replaces_id);
        it.append(args.app_icon.as_str());
        it.append(args.summary.as_str());
        it.append(args.body.as_str());
        it.append(Vec::<String>::new());
        it.append_dict(&"s".into(), &"v".into(), |d| {
            for (key, value) in &args.hints {
                d.append_dict_entry(|e| {
                    e.append(key.as_str());
                    append_hint(e, key, value);
                });
            }
        });
        it.append(args.expire_timeout);
    }
    m
}

fn append_hint(e: &mut IterAppend, key: &str, value: &HintValue) {
    match value {
        HintValue::Bool(b) => e.append(Variant(*b)),
        HintValue::Int(n) => match (key, u8::try_from(*n), i32::try_from(*n)) {
            ("urgency", Ok(byte), _) => e.append(Variant(byte)),
            (_, _, Ok(int)) => e.append(Variant(int)),
            _ => e.append(Variant(*n)),
        },
        HintValue::Str(s) => e.append(Variant(s.as_str())),
        HintValue::Image(img) => e.append_variant(&IMAGE_SIGNATURE.into(), |v| {
            v.append_struct(|s| {
                s.append(img.width);
                s.append(img.height);
                s.append(img.rowstride);
                s.append(img.has_alpha);
                s.append(img.bits_per_sample);
                s.append(img.channels);
                s.append(img.data.as_slice());
            })
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::CloseReason;
    use dbus::strings::Signature;

    fn call(path: &str, iface: &str, member: &str) -> Message {
        Message::new_method_call(NAME, path, iface, member).unwrap()
    }

    /// Uma entrada `{sv}` com a variante de assinatura `sig`.
    fn entry(d: &mut IterAppend, key: &str, sig: &str, f: impl FnOnce(&mut IterAppend)) {
        d.append_dict_entry(|e| {
            e.append(key);
            e.append_variant(&Signature::new(sig).unwrap(), f);
        });
    }

    fn notify_msg(hints: impl FnOnce(&mut IterAppend)) -> Message {
        let mut m = call(PATH_FDO, IFACE_FDO, "Notify");
        {
            let mut it = IterAppend::new(&mut m);
            it.append("app");
            it.append(7u32);
            it.append("icone");
            it.append("Título");
            it.append("<b>corpo</b>");
            it.append(vec!["default".to_string(), "Abrir".to_string()]);
            it.append_dict(&Signature::new("s").unwrap(), &Signature::new("v").unwrap(), hints);
            it.append(300i32);
        }
        m
    }

    fn hints_of(call: Call) -> Vec<(String, HintValue)> {
        match call {
            Call::Notify { hints, .. } => hints,
            other => panic!("esperava Notify, veio {other:?}"),
        }
    }

    #[test]
    fn decode_notify_full_signature() {
        let m = notify_msg(|d| {
            entry(d, "category", "s", |v| v.append("im.received"));
            entry(d, "transient", "b", |v| v.append(true));
            entry(d, "value", "i", |v| v.append(42i32));
        });
        let got = decode(&m).unwrap();
        assert_eq!(
            got,
            Call::Notify {
                app_name: "app".into(),
                replaces_id: 7,
                app_icon: "icone".into(),
                summary: "Título".into(),
                body: "<b>corpo</b>".into(),
                actions: vec!["default".into(), "Abrir".into()],
                hints: vec![
                    ("category".into(), HintValue::Str("im.received".into())),
                    ("transient".into(), HintValue::Bool(true)),
                    ("value".into(), HintValue::Int(42)),
                ],
                expire_timeout: 300,
            }
        );
    }

    #[test]
    fn decode_rejects_wrong_signature() {
        let m = call(PATH_FDO, IFACE_FDO, "Notify").append1("só uma string");
        let err = decode(&m).unwrap_err();
        assert_eq!(err.name, "org.freedesktop.DBus.Error.InvalidArgs");
    }

    #[test]
    fn image_data_struct_to_raw_image() {
        let m = notify_msg(|d| {
            entry(d, "image-data", "(iiibiiay)", |v| {
                v.append_struct(|s| {
                    s.append(2i32);
                    s.append(1i32);
                    s.append(8i32);
                    s.append(true);
                    s.append(8i32);
                    s.append(4i32);
                    s.append(vec![1u8; 8]);
                })
            });
        });
        let img = RawImage { width: 2, height: 1, rowstride: 8, has_alpha: true, bits_per_sample: 8, channels: 4, data: vec![1; 8] };
        assert_eq!(hints_of(decode(&m).unwrap()), vec![("image-data".into(), HintValue::Image(img))]);
    }

    #[test]
    fn image_data_wrong_signature_ignored() {
        let m = notify_msg(|d| {
            entry(d, "image-data", "(iii)", |v| {
                v.append_struct(|s| {
                    s.append(1i32);
                    s.append(2i32);
                    s.append(3i32);
                })
            });
            entry(d, "category", "s", |v| v.append("device.added"));
        });
        assert_eq!(hints_of(decode(&m).unwrap()), vec![("category".into(), HintValue::Str("device.added".into()))]);
    }

    #[test]
    fn byte_hint_becomes_int() {
        let m = notify_msg(|d| entry(d, "urgency", "y", |v| v.append(2u8)));
        assert_eq!(hints_of(decode(&m).unwrap()), vec![("urgency".into(), HintValue::Int(2))]);
    }

    #[test]
    fn other_int_widths_and_unknown_types() {
        let m = notify_msg(|d| {
            entry(d, "a", "n", |v| v.append(-3i16));
            entry(d, "b", "q", |v| v.append(3u16));
            entry(d, "c", "u", |v| v.append(4u32));
            entry(d, "d", "x", |v| v.append(-5i64));
            entry(d, "e", "t", |v| v.append(u64::MAX));
            entry(d, "f", "d", |v| v.append(1.5f64));
            entry(d, "g", "as", |v| v.append(vec!["x".to_string()]));
        });
        assert_eq!(
            hints_of(decode(&m).unwrap()),
            vec![
                ("a".into(), HintValue::Int(-3)),
                ("b".into(), HintValue::Int(3)),
                ("c".into(), HintValue::Int(4)),
                ("d".into(), HintValue::Int(-5)),
            ]
        );
    }

    #[test]
    fn unknown_method_error() {
        let err = decode(&call(PATH_FDO, IFACE_FDO, "Foo")).unwrap_err();
        assert_eq!(err.name, "org.freedesktop.DBus.Error.UnknownMethod");
    }

    #[test]
    fn unknown_object_error() {
        let err = decode(&call("/outro", IFACE_FDO, "Notify")).unwrap_err();
        assert_eq!(err.name, "org.freedesktop.DBus.Error.UnknownObject");
    }

    #[test]
    fn invoke_action_on_applet_path() {
        let m = call(PATH_APPLET, IFACE_APPLET, "InvokeAction").append2(5u32, "default");
        assert_eq!(decode(&m).unwrap(), Call::InvokeAction(5, "default".into()));
        let m = call(PATH_FDO, IFACE_FDO, "InvokeAction").append2(5u32, "default");
        assert_eq!(decode(&m).unwrap_err().name, "org.freedesktop.DBus.Error.UnknownMethod");
    }

    #[test]
    fn simple_methods_decode() {
        let m = call(PATH_FDO, IFACE_FDO, "CloseNotification").append1(9u32);
        assert_eq!(decode(&m).unwrap(), Call::CloseNotification(9));
        assert_eq!(decode(&call(PATH_FDO, IFACE_FDO, "GetCapabilities")).unwrap(), Call::GetCapabilities);
        assert_eq!(decode(&call(PATH_FDO, IFACE_FDO, "GetServerInformation")).unwrap(), Call::GetServerInformation);
        assert_eq!(decode(&call(PATH_APPLET, "org.freedesktop.DBus.Peer", "Ping")).unwrap(), Call::Ping);
        assert_eq!(decode(&call(PATH_FDO, "org.freedesktop.DBus.Peer", "GetMachineId")).unwrap(), Call::GetMachineId);
        let m = call(PATH_FDO, IFACE_FDO, "CloseNotification").append1("9");
        assert_eq!(decode(&m).unwrap_err().name, "org.freedesktop.DBus.Error.InvalidArgs");
    }

    #[test]
    fn introspect_lists_methods_and_signals() {
        let m = call(PATH_FDO, "org.freedesktop.DBus.Introspectable", "Introspect");
        assert_eq!(decode(&m).unwrap(), Call::Introspect);
        let fdo = introspect_xml(PATH_FDO);
        for item in ["Notify", "CloseNotification", "GetCapabilities", "GetServerInformation", "NotificationClosed", "ActionInvoked", "ActivationToken", "Introspect", "Ping"] {
            assert!(fdo.contains(item), "faltou {item}");
        }
        let applet = introspect_xml(PATH_APPLET);
        assert!(applet.contains("InvokeAction"));
        assert!(introspect_xml("/").contains("<node name=\"org\"/>"));
        assert!(introspect_xml("/org/freedesktop").contains("<node name=\"Notifications\"/>"));
        assert!(introspect_xml("/com/system76").contains("<node name=\"NotificationsApplet\"/>"));
    }

    #[test]
    fn effect_signals() {
        let s = effect_signal(&Effect::Closed(4, CloseReason::Closed)).unwrap();
        assert_eq!(s.member().unwrap().to_string(), "NotificationClosed");
        assert_eq!(s.path().unwrap().to_string(), PATH_FDO);
        assert_eq!(s.interface().unwrap().to_string(), IFACE_FDO);
        assert_eq!(s.read2::<u32, u32>().unwrap(), (4, 3));
        let s = effect_signal(&Effect::ActionInvoked(4, "ok".into())).unwrap();
        assert_eq!(s.member().unwrap().to_string(), "ActionInvoked");
        assert_eq!(s.read2::<u32, String>().unwrap(), (4, "ok".into()));
        let s = effect_signal(&Effect::ActivationToken(4, "tok".into())).unwrap();
        assert_eq!(s.member().unwrap().to_string(), "ActivationToken");
        assert_eq!(s.read2::<u32, String>().unwrap(), (4, "tok".into()));
        assert!(effect_signal(&Effect::Show(4)).is_none());
        assert!(effect_signal(&Effect::Hide(4)).is_none());
        assert!(effect_signal(&Effect::Update(4)).is_none());
    }

    #[test]
    fn notify_call_round_trips_through_decode() {
        let img = RawImage { width: 1, height: 1, rowstride: 4, has_alpha: true, bits_per_sample: 8, channels: 4, data: vec![9; 4] };
        let args = NotifyArgs {
            app_name: "Dispositivos USB".into(),
            replaces_id: 0,
            app_icon: "drive-removable-media".into(),
            summary: "Mouse conectado".into(),
            body: "corpo".into(),
            hints: vec![
                ("x-estilo".into(), HintValue::Str("compacto".into())),
                ("urgency".into(), HintValue::Int(1)),
                ("transient".into(), HintValue::Bool(true)),
                ("x-pontos".into(), HintValue::Int(3)),
                ("grande".into(), HintValue::Int(i64::from(i32::MAX) + 1)),
                ("image-data".into(), HintValue::Image(img)),
            ],
            expire_timeout: -1,
        };
        let m = notify_call(&args);
        assert_eq!(m.destination().unwrap().to_string(), NAME);
        // A dica `urgency` vai como byte, como a spec manda.
        let mut it = m.iter_init();
        for _ in 0..6 {
            it.next();
        }
        let mut dict = it.recurse(dbus::arg::ArgType::Array).unwrap();
        dict.next();
        let mut urgency = dict.recurse(dbus::arg::ArgType::DictEntry).unwrap();
        urgency.next();
        assert_eq!(&*urgency.signature(), "v");
        let mut inner = urgency.recurse(dbus::arg::ArgType::Variant).unwrap();
        assert_eq!(&*inner.signature(), "y");
        assert_eq!(
            decode(&m).unwrap(),
            Call::Notify {
                app_name: args.app_name.clone(),
                replaces_id: 0,
                app_icon: args.app_icon.clone(),
                summary: args.summary.clone(),
                body: args.body.clone(),
                actions: vec![],
                hints: args.hints.clone(),
                expire_timeout: -1,
            }
        );
    }
}

//! Modelo puro: notificação, dicas D-Bus já interpretadas, ações e níveis.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub enum HintValue { Bool(bool), Int(i64), Str(String), Image(RawImage) }

#[derive(Debug, Clone, PartialEq)]
pub struct RawImage {
    pub width: i32, pub height: i32, pub rowstride: i32, pub has_alpha: bool,
    pub bits_per_sample: i32, pub channels: i32, pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Urgency { Low, #[default] Normal, Critical }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level { Normal, Atencao, Critico }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tom { Normal, Atencao, Critico, Apagado }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Estilo { Osd, Cartao, Imagem, Compacto }

#[derive(Debug, Clone, PartialEq)]
pub enum ImageHint { Data(RawImage), File(PathBuf), IconName(String) }

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Hints {
    pub urgency: Urgency, pub category: Option<String>, pub desktop_entry: Option<String>, pub transient: bool,
    pub image: Option<ImageHint>, pub value: Option<u8>, pub sound_name: Option<String>, pub sound_file: Option<PathBuf>,
    pub suppress_sound: bool, pub sync_key: Option<String>, pub estilo: Option<Estilo>, pub tom: Option<Tom>,
    pub pontos: Option<(u8, u8)>, pub action_icons: bool,
}

fn get<'a>(raw: &'a [(String, HintValue)], key: &str) -> Option<&'a HintValue> {
    raw.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Texto não vazio; qualquer outro tipo ou string vazia = ausente.
fn text(raw: &[(String, HintValue)], key: &str) -> Option<String> {
    match get(raw, key) {
        Some(HintValue::Str(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// Booleano: `b` ou inteiro ≠ 0; outro tipo = falso.
fn flag(raw: &[(String, HintValue)], key: &str) -> bool {
    match get(raw, key) {
        Some(HintValue::Bool(b)) => *b,
        Some(HintValue::Int(n)) => *n != 0,
        _ => false,
    }
}

fn int(raw: &[(String, HintValue)], key: &str) -> Option<i64> {
    match get(raw, key) {
        Some(HintValue::Int(n)) => Some(*n),
        _ => None,
    }
}

fn image_data(raw: &[(String, HintValue)], key: &str) -> Option<ImageHint> {
    match get(raw, key) {
        Some(HintValue::Image(img)) => Some(ImageHint::Data(img.clone())),
        _ => None,
    }
}

fn image_path(raw: &[(String, HintValue)], key: &str) -> Option<ImageHint> {
    let value = text(raw, key)?;
    if value.starts_with('/') {
        Some(ImageHint::File(PathBuf::from(value)))
    } else if let Some(encoded) = value.strip_prefix("file://") {
        Some(ImageHint::File(PathBuf::from(OsStr::from_bytes(&percent_decode(encoded)))))
    } else {
        Some(ImageHint::IconName(value))
    }
}

fn percent_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let hex = |j: usize| bytes.get(j).and_then(|c| (*c as char).to_digit(16));
        if b == b'%'
            && let (Some(hi), Some(lo)) = (hex(i + 1), hex(i + 2))
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    out
}

impl Hints {
    pub fn parse(raw: Vec<(String, HintValue)>) -> Hints {
        let raw = raw.as_slice();
        let image = image_data(raw, "image-data")
            .or_else(|| image_data(raw, "image_data"))
            .or_else(|| image_path(raw, "image-path"))
            .or_else(|| image_path(raw, "image_path"))
            .or_else(|| image_data(raw, "icon_data"));
        let urgency = match int(raw, "urgency") {
            Some(0) => Urgency::Low,
            Some(2) => Urgency::Critical,
            _ => Urgency::Normal,
        };
        let pontos = match (int(raw, "x-pontos"), int(raw, "x-pontos-total")) {
            (Some(on), Some(total)) if total >= 1 => {
                let total = total.min(20);
                Some((on.clamp(0, total) as u8, total as u8))
            }
            _ => None,
        };
        Hints {
            urgency,
            category: text(raw, "category"),
            desktop_entry: text(raw, "desktop-entry"),
            transient: flag(raw, "transient"),
            image,
            value: int(raw, "value").map(|v| v.clamp(0, 100) as u8),
            sound_name: text(raw, "sound-name"),
            sound_file: text(raw, "sound-file").map(PathBuf::from),
            suppress_sound: flag(raw, "suppress-sound"),
            sync_key: text(raw, "x-canonical-private-synchronous").or_else(|| text(raw, "synchronous")),
            estilo: text(raw, "x-estilo").and_then(|s| match s.as_str() {
                "osd" => Some(Estilo::Osd),
                "cartao" => Some(Estilo::Cartao),
                "imagem" => Some(Estilo::Imagem),
                "compacto" => Some(Estilo::Compacto),
                _ => None,
            }),
            tom: text(raw, "x-tom").and_then(|s| match s.as_str() {
                "normal" => Some(Tom::Normal),
                "atencao" => Some(Tom::Atencao),
                "critico" => Some(Tom::Critico),
                "apagado" => Some(Tom::Apagado),
                _ => None,
            }),
            pontos,
            action_icons: flag(raw, "action-icons"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action { pub key: String, pub label: String }

/// Lista plana `[chave, rótulo, chave, rótulo…]` em pares; sobra ímpar é descartada.
pub fn parse_actions(flat: Vec<String>) -> Vec<Action> {
    flat.chunks_exact(2)
        .map(|pair| Action { key: pair[0].clone(), label: pair[1].clone() })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub id: u32, pub app_name: String, pub app_icon: String, pub summary: String,
    pub body: String /* já em texto puro */, pub actions: Vec<Action>, pub hints: Hints, pub expire_timeout: i32,
}

impl Notification {
    pub fn level(&self) -> Level {
        match self.hints.tom {
            Some(Tom::Critico) => Level::Critico,
            Some(Tom::Atencao) => Level::Atencao,
            Some(Tom::Normal | Tom::Apagado) => Level::Normal,
            None if self.hints.urgency == Urgency::Critical => Level::Critico,
            None => Level::Normal,
        }
    }

    pub fn is_pill(&self) -> bool {
        self.hints.estilo == Some(Estilo::Osd)
    }

    pub fn is_transient(&self) -> bool {
        self.hints.transient || self.is_pill()
    }

    pub fn is_private(&self) -> bool {
        if self.is_pill() || self.hints.estilo == Some(Estilo::Compacto) {
            return false;
        }
        !self.hints.category.as_deref().is_some_and(|c| c.starts_with("device"))
    }

    pub fn display_app(&self) -> &str {
        let app = self.app_name.trim();
        if app.is_empty() { "Notificação" } else { app }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsbKind { Controle, Armazenamento, Celular, Audio, Teclado, Mouse, Generico }

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn s(v: &str) -> HintValue { HintValue::Str(v.to_string()) }
    fn raw(w: i32) -> RawImage {
        RawImage { width: w, height: 1, rowstride: w * 4, has_alpha: true, bits_per_sample: 8, channels: 4, data: vec![1; (w * 4) as usize] }
    }
    fn h(items: Vec<(&str, HintValue)>) -> Hints {
        Hints::parse(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }
    fn notif(app: &str, hints: Hints) -> Notification {
        Notification { id: 1, app_name: app.into(), app_icon: String::new(), summary: "s".into(), body: String::new(), actions: vec![], hints, expire_timeout: -1 }
    }

    #[test]
    fn image_variants_and_priority() {
        let got = h(vec![("image-path", s("/x/a.png")), ("image-data", HintValue::Image(raw(2)))]);
        assert_eq!(got.image, Some(ImageHint::Data(raw(2))));
        let got = h(vec![("icon_data", HintValue::Image(raw(3))), ("image_path", s("/x/b.png"))]);
        assert_eq!(got.image, Some(ImageHint::File(PathBuf::from("/x/b.png"))));
        let got = h(vec![("image_data", HintValue::Image(raw(4))), ("image-path", s("/x/c.png"))]);
        assert_eq!(got.image, Some(ImageHint::Data(raw(4))));
        let got = h(vec![("icon_data", HintValue::Image(raw(5)))]);
        assert_eq!(got.image, Some(ImageHint::Data(raw(5))));
    }

    #[test]
    fn image_path_forms() {
        assert_eq!(h(vec![("image-path", s("/x/a.png"))]).image, Some(ImageHint::File(PathBuf::from("/x/a.png"))));
        assert_eq!(h(vec![("image-path", s("file:///x/a%20b.png"))]).image, Some(ImageHint::File(PathBuf::from("/x/a b.png"))));
        assert_eq!(h(vec![("image-path", s("dialog-information"))]).image, Some(ImageHint::IconName("dialog-information".into())));
        assert_eq!(h(vec![("image-path", s(""))]).image, None);
    }

    #[test]
    fn file_uri_percent_decoded() {
        let got = h(vec![("image-path", s("file:///home/u/Imagens/Captura%C3%A7%20x.png"))]);
        assert_eq!(got.image, Some(ImageHint::File(PathBuf::from("/home/u/Imagens/Capturaç x.png"))));
    }

    #[test]
    fn urgency_from_int_values() {
        assert_eq!(h(vec![("urgency", HintValue::Int(0))]).urgency, Urgency::Low);
        assert_eq!(h(vec![("urgency", HintValue::Int(1))]).urgency, Urgency::Normal);
        assert_eq!(h(vec![("urgency", HintValue::Int(2))]).urgency, Urgency::Critical);
        assert_eq!(h(vec![("urgency", HintValue::Int(7))]).urgency, Urgency::Normal);
        assert_eq!(h(vec![("urgency", s("2"))]).urgency, Urgency::Normal);
    }

    #[test]
    fn transient_accepts_bool_and_nonzero_int() {
        assert!(h(vec![("transient", HintValue::Bool(true))]).transient);
        assert!(h(vec![("transient", HintValue::Int(1))]).transient);
        assert!(!h(vec![("transient", HintValue::Bool(false))]).transient);
        assert!(!h(vec![("transient", HintValue::Int(0))]).transient);
        assert!(!h(vec![("transient", s("true"))]).transient);
    }

    #[test]
    fn sync_key_from_both_names() {
        assert_eq!(h(vec![("x-canonical-private-synchronous", s("vol"))]).sync_key, Some("vol".into()));
        assert_eq!(h(vec![("synchronous", s("bri"))]).sync_key, Some("bri".into()));
        assert_eq!(h(vec![("synchronous", s(""))]).sync_key, None);
    }

    #[test]
    fn estilo_and_tom_parse() {
        assert_eq!(h(vec![("x-estilo", s("osd"))]).estilo, Some(Estilo::Osd));
        assert_eq!(h(vec![("x-estilo", s("cartao"))]).estilo, Some(Estilo::Cartao));
        assert_eq!(h(vec![("x-estilo", s("imagem"))]).estilo, Some(Estilo::Imagem));
        assert_eq!(h(vec![("x-estilo", s("compacto"))]).estilo, Some(Estilo::Compacto));
        assert_eq!(h(vec![("x-tom", s("apagado"))]).tom, Some(Tom::Apagado));
        assert_eq!(h(vec![("x-tom", s("azul"))]).tom, None);
        assert_eq!(h(vec![("x-estilo", s("azul"))]).estilo, None);
    }

    #[test]
    fn pontos_requires_both_and_clamps() {
        let p = |on, total| h(vec![("x-pontos", HintValue::Int(on)), ("x-pontos-total", HintValue::Int(total))]).pontos;
        assert_eq!(p(5, 3), Some((3, 3)));
        assert_eq!(p(2, 5), Some((2, 5)));
        assert_eq!(p(1, 0), None);
        assert_eq!(p(1, 99), Some((1, 20)));
        assert_eq!(h(vec![("x-pontos", HintValue::Int(2))]).pontos, None);
    }

    #[test]
    fn value_clamped() {
        assert_eq!(h(vec![("value", HintValue::Int(150))]).value, Some(100));
        assert_eq!(h(vec![("value", HintValue::Int(-3))]).value, Some(0));
        assert_eq!(h(vec![("value", HintValue::Int(42))]).value, Some(42));
    }

    #[test]
    fn wrong_types_are_ignored() {
        assert_eq!(h(vec![("category", HintValue::Int(3))]).category, None);
        assert!(!h(vec![("suppress-sound", s("yes"))]).suppress_sound);
        assert!(h(vec![("suppress-sound", HintValue::Bool(true))]).suppress_sound);
    }

    #[test]
    fn actions_in_pairs_odd_dropped() {
        let got = parse_actions(vec!["default".into(), "Abrir".into(), "x".into()]);
        assert_eq!(got, vec![Action { key: "default".into(), label: "Abrir".into() }]);
    }

    #[test]
    fn level_rules() {
        assert_eq!(notif("a", h(vec![("x-tom", s("critico")), ("urgency", HintValue::Int(0))])).level(), Level::Critico);
        assert_eq!(notif("a", h(vec![("urgency", HintValue::Int(2))])).level(), Level::Critico);
        assert_eq!(notif("a", h(vec![("x-tom", s("apagado")), ("urgency", HintValue::Int(2))])).level(), Level::Normal);
        assert_eq!(notif("a", h(vec![("x-tom", s("atencao"))])).level(), Level::Atencao);
        assert_eq!(notif("a", h(vec![])).level(), Level::Normal);
    }

    #[test]
    fn private_rules() {
        assert!(!notif("a", h(vec![("x-estilo", s("osd"))])).is_private());
        assert!(!notif("a", h(vec![("x-estilo", s("compacto"))])).is_private());
        assert!(!notif("a", h(vec![("category", s("device.added"))])).is_private());
        assert!(!notif("a", h(vec![("category", s("device"))])).is_private());
        assert!(notif("a", h(vec![("category", s("im.received"))])).is_private());
        assert!(notif("a", h(vec![])).is_private());
    }

    #[test]
    fn pill_and_transient() {
        let pill = notif("a", h(vec![("x-estilo", s("osd"))]));
        assert!(pill.is_pill() && pill.is_transient());
        let t = notif("a", h(vec![("transient", HintValue::Bool(true))]));
        assert!(!t.is_pill() && t.is_transient());
    }

    #[test]
    fn empty_app_name_shows_notificacao() {
        assert_eq!(notif("", h(vec![])).display_app(), "Notificação");
        assert_eq!(notif("   ", h(vec![])).display_app(), "Notificação");
        assert_eq!(notif("  Firefox ", h(vec![])).display_app(), "Firefox");
    }
}

//! Arquivo de configuração opcional: `chave = valor`, uma por linha.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use crate::model::UsbKind;
use crate::timing::Durations;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub durations: Durations,
    pub max_visible: usize,
    pub sound: bool,
    pub sound_default: Option<String>,
    /// `som.<categoria>`: `None` = silenciar a categoria.
    pub sounds: Vec<(String, Option<String>)>,
    pub player: String,
    pub usb: bool,
    pub usb_kinds: Vec<UsbKind>,
    /// `None` = fonte da interface do COSMIC.
    pub font: Option<String>,
}

const ALL_USB_KINDS: [UsbKind; 7] = [
    UsbKind::Controle, UsbKind::Armazenamento, UsbKind::Celular, UsbKind::Audio,
    UsbKind::Teclado, UsbKind::Mouse, UsbKind::Generico,
];

impl Default for Config {
    fn default() -> Self {
        let sound = |cat: &str, name: &str| (cat.to_owned(), Some(name.to_owned()));
        Self {
            durations: Durations::default(),
            max_visible: 4,
            sound: true,
            sound_default: Some("message".to_owned()),
            sounds: vec![
                sound("device.added", "device-added"),
                sound("device.removed", "device-removed"),
                sound("im.received", "message-new-instant"),
                sound("email.arrived", "message-new-email"),
            ],
            player: "pw-play".to_owned(),
            usb: true,
            usb_kinds: ALL_USB_KINDS.to_vec(),
            font: None,
        }
    }
}

impl Config {
    /// `som.<categoria>` (`None` se `nao`); sem entrada, `som_padrao`.
    pub fn sound_for_category(&self, category: Option<&str>) -> Option<&str> {
        if let Some(cat) = category
            && let Some((_, name)) = self.sounds.iter().find(|(c, _)| c == cat)
        {
            return name.as_deref();
        }
        self.sound_default.as_deref()
    }
}

/// Lê o texto da configuração. Linhas inválidas mantêm o padrão e geram um aviso com o nº da linha.
pub fn parse(text: &str) -> (Config, Vec<String>) {
    let mut cfg = Config::default();
    let mut warnings = Vec::new();

    for (index, raw) in text.lines().enumerate() {
        let line_no = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            warnings.push(format!("linha {line_no}: esperado `chave = valor`"));
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if let Err(reason) = apply(&mut cfg, key, value, line_no, &mut warnings) {
            warnings.push(format!("linha {line_no}: {reason}"));
        }
    }
    (cfg, warnings)
}

fn apply(cfg: &mut Config, key: &str, value: &str, line_no: usize, warnings: &mut Vec<String>) -> Result<(), String> {
    match key {
        "duracao_normal_ms" => cfg.durations.normal = millis(value)?,
        "duracao_atencao_ms" => cfg.durations.atencao = millis(value)?,
        "duracao_critico_ms" => cfg.durations.critico = millis(value)?,
        "max_visiveis" => cfg.max_visible = in_range(value, 1, 8)? as usize,
        "som" => cfg.sound = yes_no(value)?,
        "som_padrao" => cfg.sound_default = non_empty(value),
        "tocador" => {
            cfg.player = non_empty(value).ok_or_else(|| "tocador não pode ser vazio".to_owned())?;
        }
        "usb" => cfg.usb = yes_no(value)?,
        "usb_avisa" => cfg.usb_kinds = usb_kinds(value, line_no, warnings),
        "fonte" => cfg.font = non_empty(value),
        _ => match key.strip_prefix("som.") {
            Some(category) if !category.is_empty() => {
                let entry = if value.eq_ignore_ascii_case("nao") { None } else { non_empty(value) };
                match cfg.sounds.iter_mut().find(|(c, _)| c == category) {
                    Some(slot) => slot.1 = entry,
                    None => cfg.sounds.push((category.to_owned(), entry)),
                }
            }
            _ => return Err(format!("chave desconhecida `{key}`")),
        },
    }
    Ok(())
}

fn millis(value: &str) -> Result<Duration, String> {
    in_range(value, 500, 120_000).map(Duration::from_millis)
}

fn in_range(value: &str, min: u64, max: u64) -> Result<u64, String> {
    let n: u64 = value.parse().map_err(|_| format!("valor inválido `{value}`"))?;
    if (min..=max).contains(&n) {
        Ok(n)
    } else {
        Err(format!("valor {n} fora da faixa {min}..={max}"))
    }
}

fn yes_no(value: &str) -> Result<bool, String> {
    match value.to_ascii_lowercase().as_str() {
        "sim" => Ok(true),
        "nao" => Ok(false),
        _ => Err(format!("valor inválido `{value}` (use sim ou nao)")),
    }
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn usb_kinds(value: &str, line_no: usize, warnings: &mut Vec<String>) -> Vec<UsbKind> {
    let mut kinds = Vec::new();
    for word in value.split_whitespace() {
        let kind = match word {
            "controle" => UsbKind::Controle,
            "armazenamento" => UsbKind::Armazenamento,
            "celular" => UsbKind::Celular,
            "audio" => UsbKind::Audio,
            "teclado" => UsbKind::Teclado,
            "mouse" => UsbKind::Mouse,
            "generico" => UsbKind::Generico,
            _ => {
                warnings.push(format!("linha {line_no}: tipo USB desconhecido `{word}`"));
                continue;
            }
        };
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds
}

/// Carrega a configuração. Arquivo ausente = padrões sem aviso; erros e avisos vão para stderr.
pub fn load() -> Config {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let (cfg, warnings) = parse(&text);
            for warning in &warnings {
                eprintln!("notificacoes: {}: {warning}", path.display());
            }
            cfg
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(err) => {
            eprintln!("notificacoes: {}: {err}; usando os padrões", path.display());
            Config::default()
        }
    }
}

/// `$XDG_CONFIG_HOME/notificacoes/config`, com fallback para `~/.config/notificacoes/config`.
pub fn config_path() -> PathBuf {
    path_from(std::env::var_os("XDG_CONFIG_HOME"), std::env::var_os("HOME"))
}

/// `$XDG_CONFIG_HOME` (se absoluto), senão `~/.config`.
pub fn config_home() -> PathBuf {
    home_from(std::env::var_os("XDG_CONFIG_HOME"), std::env::var_os("HOME"))
}

fn home_from(xdg_config_home: Option<OsString>, home: Option<OsString>) -> PathBuf {
    match xdg_config_home.filter(|v| !v.is_empty()).map(PathBuf::from) {
        Some(path) if path.is_absolute() => path,
        _ => PathBuf::from(home.unwrap_or_default()).join(".config"),
    }
}

fn path_from(xdg_config_home: Option<OsString>, home: Option<OsString>) -> PathBuf {
    home_from(xdg_config_home, home).join("notificacoes").join("config")
}

#[cfg(test)]
mod tests {
    use super::*;
    const fn ms(n: u64) -> Duration { Duration::from_millis(n) }

    #[test] fn defaults_match_design() {
        let c = Config::default();
        assert_eq!((c.durations.normal, c.durations.atencao, c.durations.critico), (ms(6000), ms(10000), ms(15000)));
        assert_eq!(c.max_visible, 4);
        assert!(c.sound);
        assert_eq!(c.sound_default.as_deref(), Some("message"));
        assert_eq!(c.sounds.len(), 4);
        assert_eq!(c.player, "pw-play");
        assert!(c.usb);
        assert_eq!(c.usb_kinds.len(), 7);
        assert_eq!(c.font, None);
        assert_eq!(parse("").0, c);
    }

    #[test] fn duration_range_warns() {
        let (c, w) = parse("duracao_normal_ms = 100\n");
        assert_eq!(c.durations.normal, ms(6000));
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("linha 1"));
    }

    #[test] fn sound_category_off_is_none() {
        let (c, w) = parse("som.im.received = nao\n");
        assert!(w.is_empty());
        assert_eq!(c.sound_for_category(Some("im.received")), None);
        assert_eq!(c.sound_for_category(Some("device.added")), Some("device-added"));
    }

    #[test] fn sound_default_fallback() {
        let c = Config::default();
        assert_eq!(c.sound_for_category(Some("outra")), Some("message"));
        assert_eq!(c.sound_for_category(None), Some("message"));
        assert_eq!(parse("som_padrao =\n").0.sound_for_category(Some("outra")), None);
    }

    #[test] fn max_visible_range() {
        let (c, w) = parse("max_visiveis = 0\nmax_visiveis = 9\n");
        assert_eq!(c.max_visible, 4);
        assert_eq!(w.len(), 2);
        assert!(w[0].contains("linha 1") && w[1].contains("linha 2"));
    }

    #[test] fn usb_kinds_parse() {
        let (c, w) = parse("usb_avisa = controle mouse\n");
        assert!(w.is_empty());
        assert_eq!(c.usb_kinds, vec![UsbKind::Controle, UsbKind::Mouse]);
        let (c, w) = parse("usb_avisa = xyz mouse\n");
        assert_eq!(w.len(), 1);
        assert_eq!(c.usb_kinds, vec![UsbKind::Mouse]);
        assert!(parse("usb_avisa =\n").0.usb_kinds.is_empty());
    }

    #[test] fn misc_keys_and_errors() {
        let (c, w) = parse("som = nao\nusb = nao\ntocador = paplay\nfonte = Open Sans\ncor = azul\nsom = talvez\nlinha solta\n");
        assert!(!c.sound && !c.usb);
        assert_eq!(c.player, "paplay");
        assert_eq!(c.font.as_deref(), Some("Open Sans"));
        assert_eq!(w.len(), 3);
    }

    #[test] fn config_path_like_logi_osd() {
        assert_eq!(path_from(Some("/x/cfg".into()), Some("/h".into())), PathBuf::from("/x/cfg/notificacoes/config"));
        assert_eq!(path_from(Some("rel".into()), Some("/h".into())), PathBuf::from("/h/.config/notificacoes/config"));
        assert_eq!(path_from(Some("".into()), Some("/h".into())), PathBuf::from("/h/.config/notificacoes/config"));
        assert_eq!(path_from(None, Some("/h".into())), PathBuf::from("/h/.config/notificacoes/config"));
    }
}

//! Classificação e nome legível de um `usb_device` a partir das propriedades do udev. Pura.

use crate::model::UsbKind;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsbProps {
    pub devpath: String,
    pub device_class: Option<String>,
    pub interfaces: Option<String>,
    pub model_db: Option<String>,
    pub vendor_db: Option<String>,
    pub product: Option<String>,
    pub manufacturer: Option<String>,
    pub vid_pid: Option<(u16, u16)>,
    pub mtp: bool,
    pub media_player: bool,
}

/// O que os eventos `input` filhos do dispositivo disseram (`ID_INPUT_* = 1`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputHints {
    pub joystick: bool,
    pub keyboard: bool,
    pub mouse: bool,
}

/// Nome mais longo antes de cortar com "…".
const NAME_MAX: usize = 60;

/// Classes de interface (`ID_USB_INTERFACES` = `:ccssppp:…:`) que começam com `prefix`.
fn has_iface(p: &UsbProps, prefix: &str) -> bool {
    p.interfaces.as_deref().is_some_and(|all| all.split(':').any(|e| !e.is_empty() && e.starts_with(prefix)))
}

/// Classe do dispositivo `09`, ou só interfaces de hub.
pub fn is_hub(p: &UsbProps) -> bool {
    if p.device_class.as_deref().is_some_and(|c| c.trim().eq_ignore_ascii_case("09")) {
        return true;
    }
    let mut entries = p.interfaces.as_deref().unwrap_or_default().split(':').filter(|e| !e.is_empty()).peekable();
    entries.peek().is_some() && entries.all(|e| e.starts_with("09"))
}

/// Primeira regra que casa (ordem das Global Constraints/brief).
pub fn kind(p: &UsbProps, i: InputHints) -> UsbKind {
    if i.joystick || ["ff5d01", "ff5d81", "ff47d0"].iter().any(|c| has_iface(p, c)) {
        UsbKind::Controle
    } else if has_iface(p, "08") {
        UsbKind::Armazenamento
    } else if p.mtp || p.media_player || has_iface(p, "0601") {
        UsbKind::Celular
    } else if has_iface(p, "01") {
        UsbKind::Audio
    } else if i.keyboard || has_iface(p, "030101") {
        UsbKind::Teclado
    } else if i.mouse || has_iface(p, "030102") {
        UsbKind::Mouse
    } else {
        UsbKind::Generico
    }
}

/// Sublinhados viram espaço, espaços repetidos colapsam, aparado. Vazio → `None`.
fn clean(s: Option<&str>) -> Option<String> {
    let words: Vec<&str> = s?.split(|c: char| c == '_' || c.is_whitespace()).filter(|w| !w.is_empty()).collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// `prefix` antes de `main`, a menos que já esteja contido nele (sem diferenciar maiúsculas).
fn joined(prefix: Option<String>, main: String) -> String {
    match prefix {
        Some(p) if !main.to_lowercase().contains(&p.to_lowercase()) => format!("{p} {main}"),
        _ => main,
    }
}

/// Modelo do banco (com o fabricante do banco) → descritores → ids; máx. `NAME_MAX` caracteres.
pub fn display_name(p: &UsbProps) -> String {
    let model = clean(p.model_db.as_deref());
    let product = clean(p.product.as_deref());
    let manufacturer = clean(p.manufacturer.as_deref());
    let name = if let Some(model) = model {
        joined(clean(p.vendor_db.as_deref()), model)
    } else if let Some(product) = product {
        joined(manufacturer, product)
    } else if let Some(manufacturer) = manufacturer {
        manufacturer
    } else if let Some((vid, pid)) = p.vid_pid {
        format!("Dispositivo USB {vid:04x}:{pid:04x}")
    } else {
        "Dispositivo USB".to_owned()
    };
    if name.chars().count() <= NAME_MAX {
        return name;
    }
    let cut: String = name.chars().take(NAME_MAX - 1).collect();
    let mut cut = cut.trim_end().to_owned();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_ifaces(ifaces: &str) -> UsbProps {
        UsbProps { interfaces: Some(ifaces.to_owned()), ..UsbProps::default() }
    }

    #[test]
    fn hub_is_ignored() {
        let hub = UsbProps { device_class: Some("09".into()), ..UsbProps::default() };
        assert!(is_hub(&hub));
        assert!(is_hub(&with_ifaces(":090000:090001:")), "só interfaces de hub");
        assert!(!is_hub(&with_ifaces(":090000:030101:")), "hub com teclado embutido é dispositivo");
        assert!(!is_hub(&UsbProps { device_class: Some("00".into()), ..with_ifaces(":030000:") }));
        assert!(!is_hub(&UsbProps::default()), "sem dados não é hub");
    }

    #[test]
    fn kind_table() {
        let none = InputHints::default();
        assert_eq!(kind(&with_ifaces(":080650:"), none), UsbKind::Armazenamento);
        assert_eq!(kind(&with_ifaces(":010100:010200:"), none), UsbKind::Audio);
        assert_eq!(kind(&with_ifaces(":ff5d01:"), none), UsbKind::Controle);
        assert_eq!(kind(&with_ifaces(":ff5d81:"), none), UsbKind::Controle);
        assert_eq!(kind(&with_ifaces(":ff47d0:"), none), UsbKind::Controle);
        assert_eq!(kind(&with_ifaces(":030101:"), none), UsbKind::Teclado);
        assert_eq!(kind(&with_ifaces(":030102:"), none), UsbKind::Mouse);
        assert_eq!(kind(&UsbProps { mtp: true, ..UsbProps::default() }, none), UsbKind::Celular);
        assert_eq!(kind(&UsbProps { media_player: true, ..UsbProps::default() }, none), UsbKind::Celular);
        assert_eq!(kind(&with_ifaces(":060101:"), none), UsbKind::Celular);
        assert_eq!(kind(&with_ifaces(":ffff00:"), none), UsbKind::Generico);
        assert_eq!(kind(&UsbProps::default(), none), UsbKind::Generico);
        // O celular real (Samsung em MTP): imagem + CDC + vendor.
        let phone = UsbProps { mtp: true, media_player: true, ..with_ifaces(":060101:020201:0a0000:ff4201:") };
        assert_eq!(kind(&phone, none), UsbKind::Celular);
        // A primeira regra que casa vence: controle antes de teclado, armazenamento antes de áudio.
        assert_eq!(kind(&with_ifaces(":030101:ff5d01:"), none), UsbKind::Controle);
        assert_eq!(kind(&with_ifaces(":010100:080650:"), none), UsbKind::Armazenamento);
        // Casa pela classe da interface, não por pedaço de outra: ":0a0108" não é armazenamento,
        // ":020201" não é áudio, ":ff0101" não é áudio.
        assert_eq!(kind(&with_ifaces(":0a0108:"), none), UsbKind::Generico);
        assert_eq!(kind(&with_ifaces(":020201:ff0101:"), none), UsbKind::Generico);
        // HID genérico sem dica de input: genérico.
        assert_eq!(kind(&with_ifaces(":030000:"), none), UsbKind::Generico);
    }

    #[test]
    fn input_hints_refine_hid_gamepad() {
        let hid = with_ifaces(":030000:");
        let joy = InputHints { joystick: true, ..InputHints::default() };
        assert_eq!(kind(&hid, joy), UsbKind::Controle);
        assert_eq!(kind(&hid, InputHints { keyboard: true, ..InputHints::default() }), UsbKind::Teclado);
        assert_eq!(kind(&hid, InputHints { mouse: true, ..InputHints::default() }), UsbKind::Mouse);
        // 8BitDo Ultimate 2C: teclado, mouse e joystick ao mesmo tempo → controle.
        let all = InputHints { joystick: true, keyboard: true, mouse: true };
        assert_eq!(kind(&with_ifaces(":030101:030102:030000:"), all), UsbKind::Controle);
        // Teclado com mouse embutido (receptor): teclado vem antes.
        let km = InputHints { keyboard: true, mouse: true, joystick: false };
        assert_eq!(kind(&hid, km), UsbKind::Teclado);
    }

    #[test]
    fn name_prefers_database_then_descriptors_then_ids() {
        let full = UsbProps {
            model_db: Some("Ultra Fit".into()),
            vendor_db: Some("SanDisk Corp.".into()),
            product: Some("Ultra USB 3.0".into()),
            manufacturer: Some("SanDisk".into()),
            vid_pid: Some((0x0781, 0x5583)),
            ..UsbProps::default()
        };
        assert_eq!(display_name(&full), "SanDisk Corp. Ultra Fit");
        // Fabricante já contido no modelo: não repete.
        let contained = UsbProps { model_db: Some("Logitech Unifying Receiver".into()), vendor_db: Some("Logitech".into()), ..UsbProps::default() };
        assert_eq!(display_name(&contained), "Logitech Unifying Receiver");
        let model_only = UsbProps { model_db: Some("Ultra Fit".into()), ..UsbProps::default() };
        assert_eq!(display_name(&model_only), "Ultra Fit");
        // Sem modelo no banco: descritores (o fabricante do banco sozinho não basta).
        let desc = UsbProps {
            vendor_db: Some("8BitDo".into()),
            product: Some("IDLE".into()),
            manufacturer: Some("8BitDo".into()),
            vid_pid: Some((0x2dc8, 0x301c)),
            ..UsbProps::default()
        };
        assert_eq!(display_name(&desc), "8BitDo IDLE");
        let product_only = UsbProps { product: Some("Gamepad".into()), ..UsbProps::default() };
        assert_eq!(display_name(&product_only), "Gamepad");
        let manufacturer_only = UsbProps { manufacturer: Some("ACME".into()), ..UsbProps::default() };
        assert_eq!(display_name(&manufacturer_only), "ACME");
        // Nada legível: ids.
        let ids = UsbProps { vid_pid: Some((0x2dc8, 0x310a)), ..UsbProps::default() };
        assert_eq!(display_name(&ids), "Dispositivo USB 2dc8:310a");
        // Campos vazios ou só espaço contam como ausentes.
        let blank = UsbProps { model_db: Some("  ".into()), product: Some(String::new()), vid_pid: Some((1, 2)), ..UsbProps::default() };
        assert_eq!(display_name(&blank), "Dispositivo USB 0001:0002");
        assert_eq!(display_name(&UsbProps::default()), "Dispositivo USB");
    }

    #[test]
    fn underscores_and_length_cleaned() {
        // O celular real: produto com sublinhado e fabricante contido nele.
        let phone = UsbProps { product: Some("SAMSUNG_Android".into()), manufacturer: Some("SAMSUNG".into()), ..UsbProps::default() };
        assert_eq!(display_name(&phone), "SAMSUNG Android");
        let messy = UsbProps { product: Some("  Super__Pad_  ".into()), ..UsbProps::default() };
        assert_eq!(display_name(&messy), "Super Pad");
        // Exatamente 60 caracteres passam inteiros (o Samsung do banco tem 60).
        let samsung = UsbProps {
            model_db: Some("Galaxy series, misc. (MTP mode)".into()),
            vendor_db: Some("Samsung Electronics Co., Ltd".into()),
            ..UsbProps::default()
        };
        assert_eq!(display_name(&samsung), "Samsung Electronics Co., Ltd Galaxy series, misc. (MTP mode)");
        assert_eq!(display_name(&samsung).chars().count(), 60);
        // Mais de 60: corta em 60 com reticências, contando caracteres (não bytes).
        let long = UsbProps { product: Some("Ç".repeat(80)), ..UsbProps::default() };
        let name = display_name(&long);
        assert_eq!(name.chars().count(), 60);
        assert!(name.ends_with('…'));
        assert!(name.starts_with("ÇÇÇ"));
    }
}

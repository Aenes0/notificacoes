//! Modo privado: o que um cartão pode mostrar com a tela como ela está.
//!
//! Um cartão de app (privado, `Notification::is_private`) aparece redigido (ícone, nome do app e
//! "Nova notificação") quando uma janela em tela cheia está em foco. Avisos do sistema (pílula,
//! compacto, `category` `device*`) aparecem sempre inteiros.
//!
//! A tela de bloqueio ficou fora por decisão do usuário (sem `show_on_lock`, sem `LockedHint`):
//! no bloqueio o compositor esconde a camada Overlay e nenhum cartão aparece. Por isso o
//! `ScreenState` não tem campo `locked`.

use crate::model::Notification;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exposure {
    Full,
    Redacted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenState {
    /// Uma janela em tela cheia está em foco. `None` = ainda desconhecido (falha fechada).
    pub fullscreen: Option<bool>,
}

/// Privado e (tela cheia em foco ou tela cheia desconhecida) → redigido.
pub fn exposure(n: &Notification, s: &ScreenState) -> Exposure {
    if !n.is_private() {
        return Exposure::Full;
    }
    match s.fullscreen {
        Some(false) => Exposure::Full,
        Some(true) | None => Exposure::Redacted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{HintValue, Hints};

    fn notif(hints: Vec<(&str, &str)>) -> Notification {
        Notification {
            id: 1,
            app_name: "Telegram".into(),
            app_icon: String::new(),
            summary: "Fulano".into(),
            body: "segredo".into(),
            actions: vec![],
            hints: Hints::parse(hints.into_iter().map(|(k, v)| (k.to_owned(), HintValue::Str(v.to_owned()))).collect()),
            expire_timeout: -1,
        }
    }

    const FULLSCREEN: ScreenState = ScreenState { fullscreen: Some(true) };
    const WINDOWED: ScreenState = ScreenState { fullscreen: Some(false) };
    const UNKNOWN: ScreenState = ScreenState { fullscreen: None };

    #[test]
    fn private_card_redacted_when_fullscreen() {
        let n = notif(vec![("category", "im.received")]);
        assert_eq!(exposure(&n, &FULLSCREEN), Exposure::Redacted);
        assert_eq!(exposure(&n, &WINDOWED), Exposure::Full);
        let plain = notif(vec![]);
        assert_eq!(exposure(&plain, &FULLSCREEN), Exposure::Redacted);
        assert_eq!(exposure(&plain, &WINDOWED), Exposure::Full);
    }

    #[test]
    fn system_notices_never_redacted() {
        let notices = [
            notif(vec![("x-estilo", "osd")]),
            notif(vec![("x-estilo", "compacto")]),
            notif(vec![("category", "device.added")]),
            notif(vec![("category", "device")]),
        ];
        for n in &notices {
            for s in [FULLSCREEN, WINDOWED, UNKNOWN] {
                assert_eq!(exposure(n, &s), Exposure::Full, "{:?} em {s:?}", n.hints);
            }
        }
    }

    #[test]
    fn unknown_fullscreen_fails_closed() {
        assert_eq!(exposure(&notif(vec![]), &UNKNOWN), Exposure::Redacted);
        assert_eq!(exposure(&notif(vec![("category", "email.arrived")]), &UNKNOWN), Exposure::Redacted);
    }
}

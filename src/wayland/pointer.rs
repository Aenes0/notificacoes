//! Ponteiro, forma do cursor e tokens de ativação (`xdg_activation_v1`).
//!
//! Um `wl_pointer` por seat com ponteiro; o compositor só manda eventos quando o cursor está numa
//! superfície nossa, então parado nada acorda. Só o botão esquerdo conta. O token de ativação é
//! pedido por clique e o objeto do token é destruído assim que responde (ou no prazo, por
//! `Ui::drop_token`).

use smithay_client_toolkit::reexports::client::protocol::{wl_pointer, wl_seat};
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, Proxy, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use smithay_client_toolkit::reexports::protocols::xdg::activation::v1::client::{
    xdg_activation_token_v1, xdg_activation_v1,
};
use smithay_client_toolkit::seat::pointer::{BTN_LEFT, PointerData, PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::{delegate_pointer, delegate_seat};

use super::{SurfaceKey, Ui};
use crate::{PointerEv, State};

/// O app que pede a ativação (o mesmo id do daemon oficial).
pub const APP_ID: &str = "com.system76.CosmicNotifications";

/// Um `wl_pointer` e o dispositivo de forma do cursor dele (se o compositor tiver o protocolo).
pub struct Pointer {
    wl: wl_pointer::WlPointer,
    shape: Option<WpCursorShapeDeviceV1>,
}

impl Pointer {
    fn release(self) {
        if let Some(shape) = self.shape {
            shape.destroy();
        }
        if self.wl.version() >= 3 {
            self.wl.release();
        }
    }
}

/// Dado de um pedido de token: a notificação a que ele pertence.
pub struct TokenFor(pub u32);

/// Evento do sctk → evento nosso. Só o botão esquerdo; rolagem e outros botões ficam de fora.
pub fn convert(kind: &PointerEventKind, (x, y): (f64, f64)) -> Option<PointerEv> {
    let (x, y) = (x as f32, y as f32);
    match *kind {
        PointerEventKind::Enter { serial } => Some(PointerEv::Enter(x, y, serial)),
        PointerEventKind::Leave { .. } => Some(PointerEv::Leave),
        PointerEventKind::Motion { .. } => Some(PointerEv::Motion(x, y)),
        PointerEventKind::Press { button: BTN_LEFT, serial, .. } => Some(PointerEv::Press(serial)),
        PointerEventKind::Release { button: BTN_LEFT, .. } => Some(PointerEv::Release),
        PointerEventKind::Press { .. } | PointerEventKind::Release { .. } | PointerEventKind::Axis { .. } => None,
    }
}

impl Ui {
    /// Mãozinha (`Pointer`) ou seta (`Default`) no ponteiro que está numa superfície nossa.
    pub fn set_cursor(&self, hand: bool) {
        let Some(wl) = &self.active_pointer else { return };
        let Some(p) = self.pointers.iter().find(|p| &p.wl == wl) else { return };
        let Some(shape) = &p.shape else { return };
        let Some(serial) = wl.data::<PointerData>().and_then(PointerData::latest_enter_serial) else { return };
        shape.set_shape(serial, if hand { Shape::Pointer } else { Shape::Default });
    }

    /// Pede um token para a notificação `id`. `click` = serial do clique e superfície do cartão;
    /// `None` (D-Bus) pede só com o app_id. `false` = o compositor não tem `xdg_activation_v1`.
    pub fn request_token(&mut self, id: u32, click: Option<(u32, SurfaceKey)>) -> bool {
        let Some(activation) = &self.activation else { return false };
        let token = activation.get_activation_token(&self.qh, TokenFor(id));
        token.set_app_id(APP_ID.to_owned());
        if let Some((serial, key)) = click {
            let seat = self.active_pointer.as_ref().and_then(|wl| wl.data::<PointerData>()).map(|d| d.seat().clone());
            if let Some(seat) = seat {
                token.set_serial(serial, &seat);
            }
            if let Some((_, s)) = self.surfaces.iter().find(|(k, _)| *k == key) {
                token.set_surface(s.layer.wl_surface());
            }
        }
        token.commit();
        self.tokens.push((id, token));
        true
    }

    /// Esquece (e destrói) o pedido de token de `id`, se ainda houver.
    pub fn drop_token(&mut self, id: u32) {
        self.tokens.retain(|(t, token)| {
            if *t == id {
                token.destroy();
            }
            *t != id
        });
    }

    fn drop_pointers_of(&mut self, seat: &wl_seat::WlSeat) {
        let (gone, keep): (Vec<Pointer>, Vec<Pointer>) = std::mem::take(&mut self.pointers)
            .into_iter()
            .partition(|p| p.wl.data::<PointerData>().is_some_and(|d| d.seat() == seat));
        self.pointers = keep;
        for p in gone {
            if self.active_pointer.as_ref() == Some(&p.wl) {
                self.active_pointer = None;
            }
            p.release();
        }
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.ui_or_unreachable().seat
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap != Capability::Pointer {
            return;
        }
        let Some(ui) = self.ui_mut() else { return };
        ui.drop_pointers_of(&seat);
        match ui.seat.get_pointer(qh, &seat) {
            Ok(wl) => {
                let shape = ui.cursor_shape.as_ref().map(|m| m.get_shape_device(&wl, qh));
                ui.pointers.push(Pointer { wl, shape });
            }
            Err(e) => eprintln!("notificacoes: ponteiro não criado: {e}"),
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Pointer
            && let Some(ui) = self.ui_mut()
        {
            ui.drop_pointers_of(&seat);
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        if let Some(ui) = self.ui_mut() {
            ui.drop_pointers_of(&seat);
        }
    }
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for e in events {
            let Some(ui) = self.ui_mut() else { return };
            let Some(key) = ui.key_of(&e.surface) else { continue };
            if matches!(e.kind, PointerEventKind::Enter { .. }) {
                ui.active_pointer = Some(pointer.clone());
            }
            if let Some(ev) = convert(&e.kind, e.position) {
                self.on_pointer(key, ev);
            }
        }
    }
}

impl Dispatch<xdg_activation_v1::XdgActivationV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &xdg_activation_v1::XdgActivationV1,
        _: xdg_activation_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // O protocolo não tem eventos neste objeto.
    }
}

impl Dispatch<xdg_activation_token_v1::XdgActivationTokenV1, TokenFor> for State {
    fn event(
        state: &mut Self,
        token: &xdg_activation_token_v1::XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        data: &TokenFor,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token: value } = event {
            if let Some(ui) = state.ui_mut() {
                ui.tokens.retain(|(_, t)| t != token);
            }
            token.destroy();
            state.on_token(data.0, Some(value));
        }
    }
}

delegate_seat!(State);
delegate_pointer!(State);

#[cfg(test)]
mod tests {
    use super::*;
    use smithay_client_toolkit::seat::pointer::BTN_RIGHT;

    fn ev(kind: PointerEventKind) -> Option<PointerEv> {
        convert(&kind, (12.5, 30.0))
    }

    #[test]
    fn only_left_button_clicks() {
        assert_eq!(ev(PointerEventKind::Press { time: 0, button: BTN_LEFT, serial: 7 }), Some(PointerEv::Press(7)));
        assert_eq!(ev(PointerEventKind::Release { time: 0, button: BTN_LEFT, serial: 8 }), Some(PointerEv::Release));
        assert_eq!(ev(PointerEventKind::Press { time: 0, button: BTN_RIGHT, serial: 9 }), None);
        assert_eq!(ev(PointerEventKind::Release { time: 0, button: BTN_RIGHT, serial: 9 }), None);
        assert_eq!(ev(PointerEventKind::Enter { serial: 3 }), Some(PointerEv::Enter(12.5, 30.0, 3)));
        assert_eq!(ev(PointerEventKind::Motion { time: 0 }), Some(PointerEv::Motion(12.5, 30.0)));
        assert_eq!(ev(PointerEventKind::Leave { serial: 4 }), Some(PointerEv::Leave));
    }
}

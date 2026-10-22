//! Tela nenhuma (`--sem-tela`): só a contagem de expiração de cada notificação visível.
//! Um único `Timer` calloop, no menor prazo; sem nada visível, nenhum timer (zero acordadas).

use std::time::{Duration, Instant};

use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::{LoopHandle, RegistrationToken};

use crate::State;
use crate::timing::Countdown;

/// Contagens por id visível (puro, sem laço).
#[derive(Debug, Default)]
struct Deadlines {
    items: Vec<(u32, Countdown)>,
}

impl Deadlines {
    /// Começa do zero, mesmo se o id já existir (o Store pode mandar `Hide` e `Show` do mesmo id).
    fn show(&mut self, id: u32, total: Duration, now: Instant) {
        self.hide(id);
        self.items.push((id, Countdown::start(total, now)));
    }

    /// Conteúdo trocado: a contagem recomeça.
    fn update(&mut self, id: u32, total: Duration, now: Instant) {
        match self.items.iter_mut().find(|(i, _)| *i == id) {
            Some((_, countdown)) => countdown.restart(total, now),
            None => self.show(id, total, now),
        }
    }

    fn hide(&mut self, id: u32) {
        self.items.retain(|(i, _)| *i != id);
    }

    /// Tira e devolve os ids cujo prazo já passou, na ordem em que entraram.
    fn take_due(&mut self, now: Instant) -> Vec<u32> {
        let due: Vec<u32> =
            self.items.iter().filter(|(_, c)| c.deadline().is_some_and(|d| d <= now)).map(|(id, _)| *id).collect();
        self.items.retain(|(id, _)| !due.contains(id));
        due
    }

    fn next(&self) -> Option<Instant> {
        self.items.iter().filter_map(|(_, c)| c.deadline()).min()
    }
}

pub struct Headless {
    handle: LoopHandle<'static, State>,
    deadlines: Deadlines,
    /// O timer registrado e o prazo dele.
    timer: Option<(RegistrationToken, Instant)>,
}

impl Headless {
    pub fn new(handle: LoopHandle<'static, State>) -> Headless {
        Headless { handle, deadlines: Deadlines::default(), timer: None }
    }

    pub fn show(&mut self, id: u32, total: Duration, now: Instant) {
        self.deadlines.show(id, total, now);
    }

    pub fn update(&mut self, id: u32, total: Duration, now: Instant) {
        self.deadlines.update(id, total, now);
    }

    pub fn hide(&mut self, id: u32) {
        self.deadlines.hide(id);
    }

    /// Chamado pelo próprio timer (que se descarta ao voltar): devolve os ids vencidos.
    pub fn on_timer(&mut self, now: Instant) -> Vec<u32> {
        self.timer = None;
        self.deadlines.take_due(now)
    }

    /// Deixa registrado exatamente um timer no menor prazo, ou nenhum.
    pub fn rearm(&mut self) {
        let next = self.deadlines.next();
        if next == self.timer.map(|(_, deadline)| deadline) {
            return;
        }
        if let Some((token, _)) = self.timer.take() {
            self.handle.remove(token);
        }
        let Some(deadline) = next else {
            return;
        };
        let timer = Timer::from_deadline(deadline);
        match self.handle.insert_source(timer, |_, _, state: &mut State| {
            state.on_screen_timer();
            TimeoutAction::Drop
        }) {
            Ok(token) => self.timer = Some((token, deadline)),
            Err(e) => eprintln!("notificacoes: o timer de expiração não registrou: {}", e.error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn next_is_the_earliest_deadline() {
        let t0 = Instant::now();
        let mut d = Deadlines::default();
        assert_eq!(d.next(), None);
        d.show(1, 300 * MS, t0);
        d.show(2, 100 * MS, t0);
        d.show(3, 200 * MS, t0);
        assert_eq!(d.next(), Some(t0 + 100 * MS));
        d.hide(2);
        assert_eq!(d.next(), Some(t0 + 200 * MS));
    }

    #[test]
    fn take_due_removes_only_expired() {
        let t0 = Instant::now();
        let mut d = Deadlines::default();
        d.show(1, 100 * MS, t0);
        d.show(2, 300 * MS, t0);
        d.show(3, 100 * MS, t0);
        assert_eq!(d.take_due(t0 + 50 * MS), Vec::<u32>::new());
        assert_eq!(d.take_due(t0 + 100 * MS), vec![1, 3]);
        assert_eq!(d.next(), Some(t0 + 300 * MS));
        assert_eq!(d.take_due(t0 + 400 * MS), vec![2]);
        assert_eq!(d.next(), None);
    }

    #[test]
    fn hide_then_show_same_id_restarts() {
        let t0 = Instant::now();
        let mut d = Deadlines::default();
        d.show(1, 100 * MS, t0);
        d.hide(1);
        d.show(1, 500 * MS, t0 + 50 * MS);
        assert_eq!(d.items.len(), 1);
        assert_eq!(d.next(), Some(t0 + 550 * MS));
        // Show repetido sem Hide também não duplica.
        d.show(1, 100 * MS, t0 + 60 * MS);
        assert_eq!(d.items.len(), 1);
        assert_eq!(d.next(), Some(t0 + 160 * MS));
    }

    #[test]
    fn update_restarts_or_creates() {
        let t0 = Instant::now();
        let mut d = Deadlines::default();
        d.show(1, 100 * MS, t0);
        d.update(1, 300 * MS, t0 + 80 * MS);
        assert_eq!(d.next(), Some(t0 + 380 * MS));
        d.update(2, 50 * MS, t0);
        assert_eq!(d.next(), Some(t0 + 50 * MS));
        assert_eq!(d.items.len(), 2);
    }
}

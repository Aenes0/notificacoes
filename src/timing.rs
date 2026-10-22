//! Regras puras de tempo: duração por nível, contagem regressiva com pausa, passo da barra e fade.

use std::time::{Duration, Instant};

use crate::model::Level;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Durations {
    pub normal: Duration,
    pub atencao: Duration,
    pub critico: Duration,
}

impl Default for Durations {
    fn default() -> Self {
        Self {
            normal: Duration::from_millis(6000),
            atencao: Duration::from_millis(10_000),
            critico: Duration::from_millis(15_000),
        }
    }
}

/// `expire_timeout` só vale se `0 < t < tempo do nível`; `0`, `-1` e maiores usam o tempo do nível.
pub fn display_duration(level: Level, expire_timeout: i32, d: &Durations) -> Duration {
    let of_level = match level {
        Level::Normal => d.normal,
        Level::Atencao => d.atencao,
        Level::Critico => d.critico,
    };
    match u64::try_from(expire_timeout) {
        Ok(ms) if ms > 0 && Duration::from_millis(ms) < of_level => Duration::from_millis(ms),
        _ => of_level,
    }
}

/// Contagem regressiva que congela ao pausar.
#[derive(Debug, Clone)]
pub struct Countdown {
    total: Duration,
    /// Restante no momento em que a corrida atual começou (ou congelado, se pausado).
    frozen: Duration,
    /// Início da corrida atual; `None` = pausado.
    running_since: Option<Instant>,
}

impl Countdown {
    pub fn start(total: Duration, now: Instant) -> Countdown {
        Countdown { total, frozen: total, running_since: Some(now) }
    }

    pub fn pause(&mut self, now: Instant) {
        if self.running_since.is_some() {
            self.frozen = self.remaining(now);
            self.running_since = None;
        }
    }

    pub fn resume(&mut self, now: Instant) {
        if self.running_since.is_none() {
            self.running_since = Some(now);
        }
    }

    /// Recomeça com um novo total, mantendo o estado de pausa.
    pub fn restart(&mut self, total: Duration, now: Instant) {
        self.total = total;
        self.frozen = total;
        if self.running_since.is_some() {
            self.running_since = Some(now);
        }
    }

    pub fn remaining(&self, now: Instant) -> Duration {
        match self.running_since {
            Some(since) => self.frozen.saturating_sub(now.saturating_duration_since(since)),
            None => self.frozen,
        }
    }

    /// Fração restante, de 1.0 a 0.0.
    pub fn fraction(&self, now: Instant) -> f32 {
        if self.total.is_zero() {
            return 0.0;
        }
        (self.remaining(now).as_secs_f64() / self.total.as_secs_f64()).clamp(0.0, 1.0) as f32
    }

    /// `None` enquanto pausado.
    pub fn deadline(&self) -> Option<Instant> {
        self.running_since.map(|since| since + self.frozen)
    }

    pub fn is_paused(&self) -> bool {
        self.running_since.is_none()
    }
}

pub const MIN_FRAME: Duration = Duration::from_millis(16);

/// Instante em que a barra encolhe 1 px de dispositivo; nunca antes de `now + MIN_FRAME`;
/// `None` se pausado, acabou ou a barra não tem pixels.
pub fn next_bar_step(c: &Countdown, bar_device_px: u32, now: Instant) -> Option<Instant> {
    if c.is_paused() || bar_device_px == 0 || c.total.is_zero() {
        return None;
    }
    let remaining = c.remaining(now);
    if remaining.is_zero() {
        return None;
    }
    let total_ns = c.total.as_nanos();
    let px = u128::from(bar_device_px);
    // Pixels ainda visíveis e o restante em que o pixel de baixo some.
    let visible = (remaining.as_nanos() * px).div_ceil(total_ns);
    let target_ns = (visible - 1) * total_ns / px;
    let delta_ns = remaining.as_nanos().saturating_sub(target_ns);
    let delta = Duration::from_nanos(u64::try_from(delta_ns).unwrap_or(u64::MAX));
    Some(now + delta.max(MIN_FRAME))
}

pub const FADE_IN: Duration = Duration::from_millis(180);

/// Ease-out cúbica: `1 - (1 - t)^3`, `t = elapsed / 180 ms` limitado a 1.
pub fn fade_alpha(elapsed: Duration) -> f32 {
    let t = (elapsed.as_secs_f32() / FADE_IN.as_secs_f32()).clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

#[cfg(test)]
mod tests {
    use super::*;
    const fn ms(n: u64) -> Duration { Duration::from_millis(n) }

    #[test] fn display_duration_rule() {
        let d = Durations::default();
        assert_eq!(display_duration(Level::Normal, 3000, &d), ms(3000));
        assert_eq!(display_duration(Level::Normal, 9000, &d), ms(6000));
        assert_eq!(display_duration(Level::Normal, 0, &d), ms(6000));
        assert_eq!(display_duration(Level::Normal, -1, &d), ms(6000));
        assert_eq!(display_duration(Level::Critico, 14000, &d), ms(14000));
        assert_eq!(display_duration(Level::Atencao, 20000, &d), ms(10000));
    }

    #[test] fn countdown_pause_freezes() {
        let t = Instant::now();
        let mut c = Countdown::start(ms(6000), t);
        c.pause(t + ms(1000));
        assert!(c.is_paused());
        assert_eq!(c.remaining(t + ms(5000)), ms(5000));
        assert_eq!(c.deadline(), None);
    }

    #[test] fn countdown_resume_continues() {
        let t = Instant::now();
        let mut c = Countdown::start(ms(6000), t);
        c.pause(t + ms(1000));
        c.resume(t + ms(5000));
        assert!(!c.is_paused());
        assert_eq!(c.deadline(), Some(t + ms(10000)));
    }

    #[test] fn countdown_restart() {
        let t = Instant::now();
        let mut c = Countdown::start(ms(6000), t);
        c.restart(ms(10000), t + ms(2000));
        assert_eq!(c.remaining(t + ms(2000)), ms(10000));
        assert_eq!(c.fraction(t + ms(2000)), 1.0);
    }

    #[test] fn fraction_goes_one_to_zero() {
        let t = Instant::now();
        let c = Countdown::start(ms(6000), t);
        assert_eq!(c.fraction(t), 1.0);
        assert!((c.fraction(t + ms(3000)) - 0.5).abs() < 1e-6);
        assert_eq!(c.fraction(t + ms(9000)), 0.0);
    }

    #[test] fn bar_step_scales_with_pixels() {
        let t = Instant::now();
        let mut c = Countdown::start(ms(6000), t);
        let step = next_bar_step(&c, 320, t).expect("rodando");
        let got = step - t;
        assert!(got > Duration::from_micros(18_700) && got < Duration::from_micros(18_800), "{got:?}");
        assert_eq!(next_bar_step(&c, 640, t), Some(t + MIN_FRAME));
        c.pause(t);
        assert_eq!(next_bar_step(&c, 320, t), None);
    }

    #[test] fn bar_step_none_when_finished() {
        let t = Instant::now();
        let c = Countdown::start(ms(1000), t);
        assert!(next_bar_step(&c, 100, t + ms(500)).is_some());
        assert_eq!(next_bar_step(&c, 100, t + ms(1000)), None);
        assert_eq!(next_bar_step(&c, 0, t), None);
    }

    #[test] fn fade_alpha_curve() {
        assert_eq!(fade_alpha(ms(0)), 0.0);
        assert_eq!(fade_alpha(ms(180)), 1.0);
        assert_eq!(fade_alpha(ms(500)), 1.0);
        assert!((fade_alpha(ms(90)) - 0.875).abs() < 1e-6);
        let mut prev = -1.0;
        for step in 0..=36 {
            let a = fade_alpha(ms(step * 5));
            assert!(a >= prev);
            prev = a;
        }
    }
}

//! Tweens for the UI. Frames are driven by a short timer that only runs
//! while something is moving, so a still window costs no CPU.

use std::time::Instant;

pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

#[derive(Clone, Copy, Debug)]
pub struct Tween {
    pub from: f32,
    pub to: f32,
    pub start: Instant,
    pub ms: f32,
}

impl Tween {
    pub fn new(from: f32, to: f32, ms: f32) -> Self {
        Tween { from, to, start: Instant::now(), ms: if crate::settings::get().animate { ms } else { 0.0 } }
    }

    pub fn progress(&self) -> f32 {
        if self.ms <= 0.0 {
            1.0
        } else {
            (self.start.elapsed().as_secs_f32() * 1000.0 / self.ms).min(1.0)
        }
    }

    pub fn value(&self) -> f32 {
        self.from + (self.to - self.from) * ease_out(self.progress())
    }

    pub fn done(&self) -> bool {
        self.progress() >= 1.0
    }
}

/// 0..1 progress of an appear animation that began at `since`.
pub fn appear(since: Option<Instant>, ms: f32) -> f32 {
    match since {
        Some(t) if crate::settings::get().animate => ease_out(t.elapsed().as_secs_f32() * 1000.0 / ms),
        _ => 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_bounds() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert!(ease_out(0.5) > 0.5);
        let t = Tween { from: 10.0, to: 0.0, start: Instant::now(), ms: 0.0 };
        assert!(t.done());
        assert_eq!(t.value(), 0.0);
    }
}

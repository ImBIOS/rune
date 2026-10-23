//! Clock-driven periodic motion. Visibility is a renderer decision, not a second clock.
use crate::{Animated, Cycle, Easing, PropClass, Spring};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LoopKind {
    Opacity { low: f32, high: f32 },
    /// Piecewise-linear, wall-clock opacity with a bounded frame rate.
    OpacityCycle(Cycle),
    Rotation,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoopSpec {
    pub period: f64,
    pub delay: f64,
    pub kind: LoopKind,
}

impl LoopSpec {
    pub fn cycle(cycle: Cycle) -> Self {
        Self { period: f64::from(cycle.period), delay: 0.0, kind: LoopKind::OpacityCycle(cycle) }
    }
    pub fn opacity(period: f64, delay: f64, low: f32, high: f32) -> Self {
        Self { period, delay, kind: LoopKind::Opacity { low, high } }
    }
    pub fn rotation(period: f64) -> Self {
        Self { period, delay: 0.0, kind: LoopKind::Rotation }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LoopTrack {
    spec: LoopSpec,
    epoch: f64,
    phase0: f64,
    amplitude: Animated<f32>,
}

impl LoopTrack {
    pub fn new(spec: LoopSpec, now: f64) -> Self {
        assert!(spec.period.is_finite() && spec.period > 0.0);
        assert!(spec.delay.is_finite() && spec.delay >= 0.0);
        Self { spec, epoch: now + spec.delay, phase0: 0.0,
            amplitude: Animated::new(1.0).with_class(PropClass::Opacity) }
    }
    pub fn phase(&self, now: f64) -> f64 {
        (self.phase0 + (now - self.epoch).max(0.0) / self.spec.period).rem_euclid(1.0)
    }
    /// Preserve the presented phase when changing speed. An unchanged spec is a no-op.
    pub fn retime(&mut self, spec: LoopSpec, now: f64) {
        assert!(spec.period.is_finite() && spec.period > 0.0);
        if self.spec == spec { return; }
        self.phase0 = self.phase(now);
        self.epoch = now.max(self.epoch);
        self.spec = spec;
    }
    /// Cool to rest without stopping or resetting phase. Reversal inherits envelope velocity.
    pub fn enabled(&mut self, enabled: bool, now: f64) {
        self.amplitude.animate_to(if enabled { 1.0 } else { 0.0 }, Spring::new(0.18, 0.0), now);
    }
    pub fn active(&self, now: f64) -> bool {
        (self.amplitude.target() > 0.0 && now >= self.epoch) || !self.amplitude.is_settled(now)
    }
    /// The animation-clock instant to wake at; no frames are needed before it.
    pub fn pending_start(&self, now: f64) -> Option<f64> {
        (self.amplitude.target() > 0.0 && now < self.epoch).then_some(self.epoch)
    }
    /// Wall-clock cycles retain their cadence even when animation time is paused.
    pub fn frame_interval(&self) -> Option<f64> {
        match self.spec.kind {
            LoopKind::OpacityCycle(c) => Some(f64::from(1.0 / c.fps.max(1.0))),
            _ => None,
        }
    }
    /// Opacity multiplier or clockwise radians. Reduced motion uses the calm first pose.
    pub fn sample(&self, now: f64, reduced: bool) -> f32 {
        self.sample_at(now, now, reduced)
    }
    pub fn sample_at(&self, now: f64, wall: f64, reduced: bool) -> f32 {
        let phase = if reduced { 0.0 } else { self.phase(now) as f32 };
        match self.spec.kind {
            LoopKind::OpacityCycle(c) => if reduced { c.still } else { c.sample(wall) },
            LoopKind::Rotation => phase * core::f32::consts::TAU,
            LoopKind::Opacity { low, high } => {
                let triangle = if phase < 0.5 { phase * 2.0 } else { (1.0 - phase) * 2.0 };
                low + (high - low) * Easing::EASE_IN_OUT.eval(triangle) * self.amplitude.sample(now)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cycle_preserves_wall_phase_stops_still_and_cadence() {
        let c = Cycle { period: 1.7, phase: 0.3, stops: &[(0.0, 0.2), (0.2, 1.0), (0.7, 0.1), (1.0, 0.2)], still: 0.4, fps: 30.0 };
        let track = LoopTrack::new(LoopSpec::cycle(c), 4.0);
        for i in 0..200 {
            let wall = i as f64 * 0.037;
            assert_eq!(track.sample_at(4.0, wall, false), c.sample(wall));
            assert_eq!(track.sample_at(4.0, wall, true), c.still);
        }
        assert_eq!(track.frame_interval(), Some(f64::from(1.0 / 30.0_f32)));
    }
    #[test]
    fn delay_has_one_wake_and_no_active_frames() {
        let track = LoopTrack::new(LoopSpec::opacity(1.0, 2.0, 0.0, 1.0), 3.0);
        assert_eq!(track.pending_start(3.0), Some(5.0));
        assert!(!track.active(4.999));
        assert_eq!(track.sample(4.999, false), 0.0);
        assert_eq!(track.pending_start(5.0), None);
        assert!(track.active(5.0));
        assert_eq!(track.phase(5.0), 0.0);
    }
    #[test]
    fn phase_continuity_and_clock_sampling() {
        let mut t = LoopTrack::new(LoopSpec::rotation(0.9), 4.0);
        for i in 0..200 {
            let now = 4.0 + i as f64 * 0.037;
            let before = t.phase(now);
            t.retime(LoopSpec::rotation(0.2 + (i % 9) as f64 * 0.1), now);
            assert!((before - t.phase(now)).abs() < 1e-12);
            assert_eq!(t.sample(now, false), t.sample(now, false));
        }
        assert_eq!(t.sample(100.0, true), 0.0);
    }
    #[test]
    fn cooling_keeps_phase_and_settles() {
        let mut t = LoopTrack::new(LoopSpec::opacity(1.0, 0.0, 0.15, 0.65), 0.0);
        let before = t.sample(0.3, false);
        t.enabled(false, 0.3);
        assert_eq!(t.sample(0.3, false), before);
        assert!(t.active(0.31));
        assert!(!t.active(10.0));
        assert_eq!(t.sample(10.0, false), 0.15);
    }
    #[test]
    fn clock_pause_scale_step_and_seek_match_reference() {
        let spec = LoopSpec::rotation(0.9);
        let t = LoopTrack::new(spec, 0.0);
        let mut clock = crate::Clock::manual(120.0);
        for _ in 0..17 { clock.step(); }
        let first = t.sample(clock.now(), false);
        clock.pause(0.0);
        for _ in 0..40 { clock.step(); }
        assert_eq!(t.sample(clock.now(), false), first);
        clock.resume(0.0);
        clock.set_scale(0.25, 0.0);
        clock.step();
        let reference = ((17.0 / 120.0 + 0.25 / 120.0) / 0.9) as f32 * core::f32::consts::TAU;
        assert!((t.sample(clock.now(), false) - reference).abs() < 1e-6);
        clock.seek(0.45);
        assert!((t.sample(clock.now(), false) - core::f32::consts::PI).abs() < 1e-6);
    }
}

//! Script inputs a hand eases over a duration, one step a frame.
use super::{Ev, Kind, Pos};
use serde_json::Value;

/// `{"type": "scroll"}`: a trackpad swipe, deltas eased over `dur`, then (with "fling") the
/// release velocity.
pub(super) fn swipe(out: &mut Vec<Ev>, e: &Value, t: f64, pos: Pos) {
    let dx = e["dx"].as_f64().unwrap_or(0.0) as f32;
    let dy = e["dy"].as_f64().unwrap_or(0.0) as f32;
    let dur = e["dur"].as_f64().unwrap_or(0.2).max(0.01);
    out.push(Ev { t, kind: Kind::Move, pos: pos.clone() });
    let steps = (dur * 120.0).ceil() as usize;
    let mut prev = (0.0, 0.0);
    for s in 1..=steps {
        let f = s as f32 / steps as f32;
        let f = f * (2.0 - f);
        out.push(Ev { t: t + dur * s as f64 / steps as f64, kind: Kind::Wheel(dx * f - prev.0, dy * f - prev.1), pos: pos.clone() });
        prev = (dx * f, dy * f);
    }
    if e["fling"].as_bool().unwrap_or(false) {
        let v = |d: f32| 2.0 * d / dur as f32 * 0.25;
        out.push(Ev { t: t + dur + 1e-4, kind: Kind::Fling(v(dx), v(dy)), pos });
    }
}

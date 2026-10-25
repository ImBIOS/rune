//! Horizontal scrolling for `El::scroll_x` and `El::scroll_xy` viewports: the axis a wheel or
//! trackpad gesture locks to, Shift with a mouse wheel, momentum, `Tx::scroll_into_view` and
//! `Tx::scroll_to_x` on x.
//!
//! A gesture moves one axis, picked by its first delta, as AppKit's predominant-axis scrolling
//! does: mostly horizontal goes to the innermost horizontal viewport under the pointer, else to
//! the innermost vertical one, and a diagonal never moves both. The gesture keeps that axis
//! wherever the pointer ends up, as AppKit latches a gesture to the view it began on: one that
//! began outside any horizontal viewport stays vertical when the content scrolls one under the
//! pointer. Without phases (a mouse wheel), only a pause or a delta wholly on the other axis
//! ends the lock, so Shift + wheel and then the plain wheel each scroll at once.
use super::*;

/// Seconds without a delta after which the next one starts a new gesture, where the platform
/// reports no phases (a mouse wheel, most trackpads off macOS).
const WHEEL_IDLE: f64 = 0.1;
/// Seconds without a delta after which a gesture the platform began counts as over, should its
/// end never come (the window lost the pointer or focus mid-gesture).
const PHASED_IDLE: f64 = 1.0;
/// A gesture that begins within this many seconds of one ending is its momentum (AppKit
/// reports momentum as a phase of its own), so it keeps the axis.
const MOMENTUM_GAP: f64 = 0.05;

/// The axis of the wheel or trackpad gesture in progress.
#[derive(Debug, Default)]
pub(super) struct WheelLock {
    /// Horizontal (`true`) or vertical, once the gesture's first delta picked it.
    pub(super) horizontal: Option<bool>,
    /// When the last delta came, in clock seconds (`now_clock`: the wall in a window, the
    /// virtual clock headless, so scripts lock axes deterministically).
    at: Option<f64>,
    /// The platform began the gesture: it holds the axis until it ends, however slowly it moves.
    phased: bool,
    /// When the platform last ended a gesture.
    ended: Option<f64>,
    /// The gesture is the momentum of one that ended. One that begins as momentum ends is a
    /// new touch (a finger caught the coasting content), not more momentum.
    pub(super) momentum: bool,
    /// The `on_wheel` element that took the gesture's first delta and hears the rest.
    pub(super) handler: Option<String>,
}

/// A wheel event's delta in points, positive moving content right and down. With Shift, a delta
/// with no horizontal part turns horizontal, as browsers and Windows and Linux apps do for a
/// mouse wheel. macOS turns it itself (`shift` is false there), and a delta that already has a
/// horizontal part was turned by the system or comes from a trackpad.
pub(crate) fn wheel_delta(delta: winit::event::MouseScrollDelta, scale: f32, shift: bool) -> (f32, f32) {
    let (dx, dy) = match delta {
        winit::event::MouseScrollDelta::PixelDelta(p) => (p.x as f32 / scale, p.y as f32 / scale),
        winit::event::MouseScrollDelta::LineDelta(x, y) => (x * 40.0, y * 40.0),
    };
    if shift && dx == 0.0 { (dy, 0.0) } else { (dx, dy) }
}

/// Whether Shift turns the wheel on this platform (macOS does it before the app sees the event).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) const SHIFT_TURNS_WHEEL: bool = !cfg!(target_os = "macos");

#[cfg(any(test, target_arch = "wasm32"))]
impl ScrollState {
    /// The offset, velocity and limit on one axis.
    pub(super) fn axis(&mut self, horizontal: bool) -> (&mut f64, &mut f32, f64) {
        if horizontal { (&mut self.x.offset, &mut self.x.v, self.x.max) } else { (&mut self.offset, &mut self.v, self.max) }
    }
}

impl<S: 'static> Runtime<S> {
    /// A wheel or trackpad delta in points on both axes (positive moves content right and
    /// down). The gesture's first delta picks the axis and the rest of the gesture keeps it:
    /// mostly horizontal scrolls the innermost [`El::scroll_x`] or [`El::scroll_xy`] viewport
    /// under (x, y), anything else the innermost vertical one as [`Self::wheel`] does. Call
    /// [`Self::wheel_began`] and [`Self::wheel_ended`] where the platform reports phases;
    /// without them a pause of 0.1 s ends a gesture. An [`El::on_wheel`] handler under the
    /// pointer hears it first; [`Self::wheel_event`] also passes modifiers and phases.
    pub fn wheel_xy(&mut self, x: f32, y: f32, dx: f32, dy: f32) {
        if dx == 0.0 && dy == 0.0 { return; }
        self.wheel_event(crate::WheelEvent::new(dx, dy).at(x, y));
    }

    /// A delta arrived: a gesture idle too long, or ended and not followed by its momentum,
    /// is over, and this delta starts the next.
    pub(super) fn wheel_delta_arrived(&mut self) {
        let now = self.now_clock();
        let l = &mut self.wheel_lock;
        let idle = if l.phased { PHASED_IDLE } else { WHEEL_IDLE };
        let stale = l.ended.is_some_and(|e| now - e > MOMENTUM_GAP) || l.at.is_none_or(|a| now - a > idle);
        if stale { *l = WheelLock::default(); }
        l.at = Some(now);
    }

    /// Scrolls by a delta no `on_wheel` handler took, on the axis the gesture locked to.
    pub(super) fn wheel_scroll(&mut self, x: f32, y: f32, dx: f32, dy: f32) {
        if dx == 0.0 && dy == 0.0 { return; }
        let l = &self.wheel_lock;
        let horizontal = match l.horizontal {
            // Without phases, a delta wholly on the other axis is another wheel turning (Shift
            // let go, a tilt, a second device), so it picks afresh rather than being dropped.
            Some(h) if l.phased || if h { dx != 0.0 } else { dy != 0.0 } => h,
            _ => {
                let h = dx.abs() > dy.abs() && self.scroll_at_axis(x, y, true).is_some();
                self.wheel_lock.horizontal = Some(h);
                h
            }
        };
        if horizontal { if dx != 0.0 { self.wheel_x(x, y, dx); } } else if dy != 0.0 { self.wheel(x, y, dy); }
    }

    /// The platform began a trackpad gesture (winit's `TouchPhase::Started` on a wheel event):
    /// its first delta picks the axis afresh, unless it is the momentum of one that just ended.
    pub fn wheel_began(&mut self) {
        let now = self.now_clock();
        // A delta queued for `on_wheel` handlers hasn't picked who has the gesture yet.
        let queued = self.queue.iter().any(|a| matches!(a, Action::Wheel(..)));
        let l = &mut self.wheel_lock;
        let momentum = !l.momentum && (l.horizontal.is_some() || l.handler.is_some() || queued) && l.ended.is_some_and(|e| now - e <= MOMENTUM_GAP);
        if !momentum { l.horizontal = None; l.handler = None; }
        l.momentum = momentum;
        l.phased = true;
        l.ended = None;
        l.at = Some(now);
    }

    /// The platform ended a trackpad gesture (`TouchPhase::Ended` or `Cancelled`).
    pub fn wheel_ended(&mut self) {
        self.wheel_lock.ended = Some(self.now_clock());
    }

    /// The window lost the pointer or focus: a gesture in progress won't report its end here.
    pub(super) fn wheel_lost(&mut self) {
        self.wheel_lock = WheelLock::default();
    }

    /// Scrolls the innermost horizontal viewport at (x, y) by `dx` points, 1:1.
    fn wheel_x(&mut self, x: f32, y: f32, dx: f32) {
        self.dismiss_help();
        let Some(id) = self.scroll_at_axis(x, y, true) else { return };
        let Some(st) = self.scrolls.get_mut(&id) else { return };
        let before = st.x.offset;
        st.x.offset = (st.x.offset - f64::from(dx)).clamp(0.0, st.x.max);
        st.x.v = 0.0;
        st.direct = true;
        self.events.push(format!("wheel-x {dx:.1}"));
        if st.x.offset != before { self.scroll_cancels_press(id); }
    }

    /// Releases a scroll with velocity (vx, vy) in points per second (positive moves content
    /// right and down), which the runtime decays, on the axis [`Self::wheel_xy`] would pick
    /// for it. [`Self::fling`] is the vertical one.
    pub fn fling_xy(&mut self, x: f32, y: f32, vx: f32, vy: f32) {
        if vx.abs() <= vy.abs() || self.scroll_at_axis(x, y, true).is_none() {
            self.fling(x, y, vy);
            return;
        }
        self.dismiss_help();
        let Some(id) = self.scroll_at_axis(x, y, true) else { return };
        if let Some(st) = self.scrolls.get_mut(&id) {
            st.x.v = -vx;
            self.events.push(format!("fling-x {vx:.0}"));
            if vx != 0.0 { self.scroll_cancels_press(id); }
        }
    }

    /// How far horizontal viewport `i`'s content reaches past its right edge, negative while it
    /// fits.
    fn scroll_overflow_x(&self, i: usize) -> f64 {
        let view = self.nodes[i].target;
        let content_w = self.scroll_content(i).map_or(0.0, |c| f64::from(self.nodes[c].target[2] - view[0]));
        content_w - f64::from(view[2] - view[0])
    }

    /// Steps viewport `i`'s horizontal limit and momentum, as `step_scroll` does the vertical.
    pub(super) fn step_scroll_x(&mut self, i: usize, dt: f32) -> bool {
        let id = self.nodes[i].id;
        let overflow = self.scroll_overflow_x(i);
        #[cfg(target_arch = "wasm32")]
        if let Some(active) = self.web_step_scroll(id, overflow, dt, true) { return active; }
        let Some(st) = self.scrolls.get_mut(&id) else { return false };
        st.x.max = overflow.max(0.0);
        let mut moving = false;
        if st.x.v.abs() > 1.0 {
            let decay = (-SCROLL_DECAY * dt).exp();
            st.x.offset += f64::from(st.x.v * (1.0 - decay) / SCROLL_DECAY);
            st.x.v *= decay;
            if st.x.offset <= 0.0 || st.x.offset >= st.x.max { st.x.v = 0.0; }
            moving = true;
        } else {
            st.x.v = 0.0;
        }
        st.x.offset = st.x.offset.clamp(0.0, st.x.max);
        moving
    }

    /// `Tx::scroll_into_view` on x: scrolls the nearest horizontal viewport around row `key`
    /// just far enough to show it, leaving its `El::scroll_margin`.
    pub(super) fn reveal_row_x(&mut self, key: &str) {
        let Some(i) = self.nodes.iter().position(|n| n.key.as_deref() == Some(key) || n.path == key) else { return };
        let Some(v) = std::iter::successors(self.nodes[i].parent, |&p| self.nodes[p].parent)
            .find(|&p| self.nodes[p].scroll.is_some() && self.nodes[p].scroll_axes.x()) else { return };
        let (row, view) = (&self.nodes[i], &self.nodes[v]);
        let (left, width, viewport) = (f64::from(row.target[0] - view.target[0]), f64::from(row.local.w), f64::from(view.local.w));
        let (margin, id) = (f64::from(view.scroll_edges.margin), view.id);
        // A vertical reveal may have just mounted rows (virtual ones) that widened the content.
        let max = self.scroll_overflow_x(v).max(0.0);
        let Some(st) = self.scrolls.get_mut(&id) else { return };
        st.x.max = max;
        let offset = scroll_edge::reveal(st.x.offset, left, width, viewport, margin).clamp(0.0, max);
        if offset == st.x.offset { return; }
        st.x.offset = offset;
        st.x.v = 0.0;
        st.direct = true;
    }

    /// `Tx::scroll_to_x`: moves each keyed horizontal viewport to its offset, clamped.
    pub(super) fn scroll_to_x(&mut self, requests: Vec<(String, f32)>) {
        self.step_scroll(0.0);
        for (key, x) in requests {
            let Some(mut i) = self.nodes.iter().enumerate()
                .position(|(i, n)| (n.key.as_deref() == Some(&*key) || n.path == key) && !self.subtree_exiting(i)) else { continue };
            loop {
                if self.nodes[i].scroll.is_some() && self.nodes[i].scroll_axes.x() { break; }
                let Some(p) = self.nodes[i].parent else { break };
                i = p;
            }
            if !self.nodes[i].scroll_axes.x() || self.nodes[i].scroll.is_none() { continue; }
            let Some(st) = self.scrolls.get_mut(&self.nodes[i].id) else { continue };
            st.x.offset = f64::from(x).clamp(0.0, st.x.max);
            st.x.v = 0.0;
            st.direct = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::dpi::PhysicalPosition;
    use winit::event::MouseScrollDelta::{LineDelta, PixelDelta};

    #[test]
    fn shift_turns_only_a_purely_vertical_wheel() {
        assert_eq!(wheel_delta(LineDelta(0.0, -1.0), 2.0, false), (0.0, -40.0));
        assert_eq!(wheel_delta(LineDelta(0.0, -1.0), 2.0, true), (-40.0, 0.0), "Shift + mouse wheel");
        assert_eq!(wheel_delta(LineDelta(1.0, 0.0), 2.0, false), (40.0, 0.0), "a horizontal wheel (WM_MOUSEHWHEEL, X11 buttons 6/7)");
        let trackpad = PixelDelta(PhysicalPosition::new(-8.0, 20.0));
        assert_eq!(wheel_delta(trackpad, 2.0, true), (-4.0, 10.0), "a delta with a horizontal part stays as it is");
        assert!(SHIFT_TURNS_WHEEL != cfg!(target_os = "macos"));
    }
}

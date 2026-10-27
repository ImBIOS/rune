//! `Tx::scroll_to`, `Tx::scroll_to_instant`, `Tx::scroll_to_x` and `Tx::scroll_to_y`: moving a
//! viewport to an offset its app picked, gliding there or at once.
use super::*;

/// A request to move a viewport to an offset on one axis or both.
#[derive(Clone, Debug)]
pub(crate) struct ScrollRequest {
    pub key: String,
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub animated: bool,
}

impl<S: 'static> Runtime<S> {
    /// Each request moves the viewport keyed (or pathed) by its key, or the nearest one around
    /// it that scrolls an axis it asks for, clamped to its content. Axes the viewport doesn't
    /// scroll are left alone.
    pub(super) fn scroll_to_offsets(&mut self, requests: Vec<ScrollRequest>) {
        // This frame's commit may have changed the content: measure the extent first.
        self.step_scroll(0.0);
        let animate = !self.reduced_motion && !self.settled;
        // Jumps: (viewport, horizontal, offset), reapplied once virtual rows land.
        let mut jumps = Vec::new();
        for r in requests {
            let Some(i) = self.scroll_viewport_on(&r.key, r.x.is_some(), r.y.is_some()) else { continue };
            let (id, axes) = (self.nodes[i].id, self.nodes[i].scroll_axes);
            let (view_w, view_h) = (f64::from(self.nodes[i].local.w), f64::from(self.nodes[i].local.h));
            let Some(st) = self.scrolls.get_mut(&id) else { continue };
            let glide = r.animated && animate;
            if let Some(x) = r.x.filter(|_| axes.x()) {
                let to = f64::from(x).max(0.0);
                st.x.v = 0.0;
                if glide && (to.min(st.x.max) - st.x.offset).abs() > 0.5 {
                    // A long trip cuts to a couple of viewports out first, as a glide to the bottom does.
                    let reach = scroll_bottom::GLIDE_REACH * view_w;
                    let end = to.min(st.x.max);
                    st.x.offset = st.x.offset.clamp(end - reach, end + reach);
                    st.x.aim = to;
                    st.x.glide = Some(st.x.glide.unwrap_or(0.0));
                    st.hover_held = true;
                } else {
                    st.x.glide = None;
                    jumps.push((id, true, to));
                }
            }
            if let Some(y) = r.y.filter(|_| axes.y()) {
                let to = f64::from(y).max(0.0);
                st.v = 0.0;
                if glide && (to.min(st.max) - st.offset).abs() > 0.5 {
                    let reach = scroll_bottom::GLIDE_REACH * view_h;
                    let end = to.min(st.max);
                    st.offset = st.offset.clamp(end - reach, end + reach);
                    st.glide_to = Some(to);
                    st.glide = Some(st.glide.unwrap_or(0.0));
                    st.hover_held = true;
                    st.pinned = false;
                } else {
                    st.glide = None;
                    st.glide_to = None;
                    jumps.push((id, false, to));
                }
            }
        }
        self.apply_jumps(&jumps);
        // Landing mounts the virtual rows there, which measure and can move the end it clamped to.
        for _ in 0..3 {
            if !self.virtual_window_changed() { break; }
            self.commit(&Tx::default());
            self.step_scroll(0.0);
            self.apply_jumps(&jumps);
        }
    }

    fn apply_jumps(&mut self, jumps: &[(NodeId, bool, f64)]) {
        for &(id, horizontal, to) in jumps {
            let stick = self.nodes.iter().find(|n| n.id == id).and_then(|n| n.scroll).is_some_and(|s| s.stick_bottom);
            let Some(st) = self.scrolls.get_mut(&id) else { continue };
            st.direct = true;
            if horizontal {
                st.x.offset = to.min(st.x.max);
            } else {
                st.offset = to.min(st.max);
                st.pinned = stick && st.offset >= st.max - 0.5;
            }
        }
    }

    /// The viewport keyed (or pathed) `key`, or the nearest one around it, that scrolls
    /// horizontally (`x`) or vertically (`y`).
    fn scroll_viewport_on(&self, key: &str, x: bool, y: bool) -> Option<usize> {
        let i = self.nodes.iter().enumerate()
            .position(|(i, n)| n.named(key) && !self.subtree_exiting(i))?;
        std::iter::successors(Some(i), |&p| self.nodes[p].parent).find(|&p| {
            let n = &self.nodes[p];
            n.scroll.is_some() && ((x && n.scroll_axes.x()) || (y && n.scroll_axes.y()))
        })
    }
}

/// Steps a glide on one axis toward `to` on `GLIDE`. Returns whether it is still moving.
pub(super) fn glide_axis(offset: &mut f64, glide: &mut Option<f32>, to: f64, dt: f32) -> bool {
    let Some(v) = *glide else { return false };
    // Sample the displacement, not the absolute offset: f32 loses sub-point precision far down.
    let (d, v) = motion::GLIDE.sample((*offset - to) as f32, v, 0.0, dt);
    if d.abs() < 0.5 && v.abs() < 20.0 {
        *offset = to;
        *glide = None;
        return false;
    }
    *offset = to + f64::from(d);
    *glide = Some(v);
    true
}

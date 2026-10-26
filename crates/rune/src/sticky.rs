//! `El::sticky_top` and `El::sticky_bottom`: elements held to an edge of their scroll viewport
//! while their parent is in view (CSS `position: sticky`), and `Tx::keep_on_screen`, which holds
//! an element where it shows across a commit.
//!
//! The shift is worked out in the paint pass from the offsets that frame applies, so a stuck
//! header never trails a scroll. Layout is untouched: only drawing, hit testing and what tools
//! read move. A sticky element paints after its siblings (`update_layer_paint_order`).
use super::*;
use crate::el::StickyEdge;

/// No viewport band and no parent box: nothing to stick within.
pub(super) const UNBOUNDED: [f32; 4] = [f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY];

/// Points of shift over which `El::stuck_shadow` fades in.
pub(super) const SHADOW_RAMP: f32 = 8.0;

/// One `Tx::keep_on_screen` anchor captured before a commit.
pub(super) struct Keep {
    pub path: String,
    pub viewport: String,
    pub was: f32,
    /// When the element lives inside a virtual row: `(virtual_list_path, row_key, y_in_row)`.
    pub virtual_row: Option<(String, String, f32)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{col, el};

    #[test]
    fn cached_section_with_sticky_descendant_stays_live_on_scroll() {
        fn view(_: &(), _: &mut Cx<()>) -> El {
            col().key("scroll").size(300.0, 200.0).scroll(ScrollSpec { stick_bottom: false })
                .child(col().key("cached").w_full().cache()
                    .child(el().key("header").w_full().h(30.0).fill(Color::WHITE).sticky_top(0.0))
                    .child(el().w_full().h(600.0)))
        }
        let mut rt = Runtime::new((), view, (300.0, 200.0), Env::default());
        rt.frame(0.0);
        rt.wheel(100.0, 100.0, -100.0);
        rt.frame(0.0);
        let cached = rt.nodes.iter().position(|n| n.key.as_deref() == Some("cached")).unwrap();
        assert!(rt.cache_live_scratch[cached], "sticky contents must not reuse a frozen raster");
        assert!(!rt.node_caches.contains_key(&rt.nodes[cached].id));
        let h = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("header")).unwrap();
        assert_eq!(h.rect[1], 0.0);
        assert_eq!(h.stuck, 100.0);
    }
}

/// How far a sticky box with edges `top` and `bottom` moves from where scrolling put it.
/// `frame` holds its viewport's top and bottom, then its parent's content box top and bottom.
pub(super) fn shift(edge: StickyEdge, inset: f32, top: f32, bottom: f32, frame: [f32; 4]) -> f32 {
    let [band_top, band_bottom, box_top, box_bottom] = frame;
    let d = match edge {
        StickyEdge::Top => (band_top + inset - top).min(box_bottom - bottom).max(0.0),
        StickyEdge::Bottom => (band_bottom - inset - bottom).max(box_top - top).min(0.0),
    };
    if d.is_finite() { d } else { 0.0 }
}

/// Target rect of node `j` after its scroll viewports' offsets and any sticky shift on `j` or
/// its ancestors, for `El::anchored`.
pub(super) fn anchored_target(nodes: &[Node], scrolls: &HashMap<NodeId, ScrollState>, j: usize) -> [f32; 4] {
    let mut t = nodes[j].target;
    for [ox, oy] in scroll_offsets(scrolls, nodes, j) {
        t[0] -= ox;
        t[2] -= ox;
        t[1] -= oy;
        t[3] -= oy;
    }
    let dy = target_shift(nodes, scrolls, j);
    t[1] += dy;
    t[3] += dy;
    t
}

/// Total sticky shift on node `j` and its ancestors from `nodes`'s laid-out rects and `scrolls`.
fn target_shift(nodes: &[Node], scrolls: &HashMap<NodeId, ScrollState>, j: usize) -> f32 {
    let mut total = 0.0;
    for a in std::iter::successors(Some(j), |&p| nodes[p].parent) {
        let Some((edge, inset)) = nodes[a].sticky.as_ref().and_then(|s| Some((s.edge?, s.inset))) else { continue };
        let Some(p) = nodes[a].parent else { continue };
        let Some(s) = std::iter::successors(Some(p), |&u| nodes[u].parent)
            .find(|&u| nodes[u].scroll.is_some() && nodes[u].scroll_axes.y()) else { continue };
        let oy: f32 = std::iter::successors(Some(p), |&u| nodes[u].parent)
            .take_while(|&u| u != s)
            .chain(std::iter::once(s))
            .filter_map(|u| scrolls.get(&nodes[u].id).map(|st| st.offset as f32))
            .sum();
        let st_y = nodes[s].target[1];
        let band_h = nodes[s].target[3] - st_y;
        let box_top = nodes[p].target[1] - st_y - oy + nodes[p].inset[1];
        let box_bottom = nodes[p].target[3] - st_y - oy - nodes[p].inset[3];
        let top = nodes[a].target[1] - st_y - oy;
        let bottom = nodes[a].target[3] - st_y - oy;
        total += shift(edge, inset, top, bottom, [0.0, band_h, box_top, box_bottom]);
    }
    total
}

impl<S: 'static> Runtime<S> {
    /// The nearest vertical scroll viewport around node `i`.
    pub(super) fn viewport_of(&self, i: usize) -> Option<usize> {
        std::iter::successors(self.nodes[i].parent, |&p| self.nodes[p].parent)
            .find(|&p| self.nodes[p].scroll.is_some() && self.nodes[p].scroll_axes.y())
    }

    /// Records where each `Tx::keep_on_screen` element showed on the last frame, before the
    /// commit moves it.
    pub(super) fn capture_keeps(&mut self, tx: &mut Tx) {
        if tx.keep_on_screen.is_empty() { return; }
        let keys = std::mem::take(&mut tx.keep_on_screen);
        // The last frame's rects describe these nodes only until a commit replaces them.
        if !self.built || self.drawn.len() != self.nodes.len() { return; }
        for key in keys {
            let found = (0..self.nodes.len()).find(|&i| {
                let n = &self.nodes[i];
                n.named(&key) && !self.subtree_exiting(i)
            });
            let Some(i) = found else { continue };
            let Some(s) = self.viewport_of(i) else { continue };
            self.keeps.retain(|k| k.viewport != self.nodes[s].path);
            let virtual_row = std::iter::successors(Some(i), |&p| self.nodes[p].parent)
                .take_while(|&p| p != s)
                .find_map(|r| {
                    let c = self.nodes[r].parent?;
                    self.virtuals.contains_key(&self.nodes[c].path).then(|| (
                        self.nodes[c].path.clone(),
                        self.nodes[r].key.clone().unwrap_or_default(),
                        self.nodes[i].target[1] - self.nodes[r].target[1],
                    ))
                });
            self.keeps.push(Keep {
                path: self.nodes[i].path.clone(),
                viewport: self.nodes[s].path.clone(),
                was: self.drawn[i][1] - self.drawn[s][1],
                virtual_row,
            });
        }
    }

    /// Scrolls each kept element's viewport so the element's top lands where it showed before
    /// the commit, clamped to the content, re-pinning a `stick_bottom` end, and remounting any
    /// virtual rows the jump brings into view.
    pub(super) fn apply_keeps(&mut self) {
        if self.keeps.is_empty() { return; }
        let keeps = std::mem::take(&mut self.keeps);
        for pass in 0..4 {
            self.step_scroll(0.0);
            let mut moved = false;
            for k in &keeps {
                let Some(s) = self.nodes.iter().position(|n| n.path == k.viewport && n.scroll.is_some() && n.scroll_axes.y()) else { continue };
                let laid = if let Some(i) = self.nodes.iter().position(|n| n.path == k.path) {
                    if self.viewport_of(i) != Some(s) { continue; }
                    // Mounted anchors follow drawn geometry after the motion step.
                    continue;
                } else if let Some((v_path, row_key, in_row)) = &k.virtual_row
                    && let Some(v) = self.virtuals.get(v_path)
                    && let Some(idx) = v.index_of(row_key)
                {
                    f64::from(v.pad_top + v.prefix(idx) + *in_row)
                } else {
                    continue;
                };
                let stick = self.nodes[s].scroll.is_some_and(|sc| sc.stick_bottom);
                let id = self.nodes[s].id;
                let Some(st) = self.scrolls.get_mut(&id) else { continue };
                st.direct = true;
                let to = (laid - f64::from(k.was)).clamp(0.0, st.max);
                if pass > 0 && (to - st.offset).abs() < 0.5 { continue; }
                st.v = 0.0;
                st.glide = None;
                st.glide_to = None;
                st.offset = to;
                st.pinned = stick && st.offset >= st.max - 0.5;
                moved = true;
            }
            if !moved || !self.virtual_window_changed() { break; }
            self.commit(&Tx::default());
        }
        self.keeps = keeps;
    }

    /// Compensate this frame's drawn displacement, in viewport coordinates. Sticky
    /// clamping may absorb part of a correction; the caller permits a bounded repaint.
    pub(super) fn follow_keeps(&mut self) -> bool {
        let mut moved = false;
        for k in &self.keeps {
            let Some(i) = self.nodes.iter().enumerate().position(|(i, n)| n.path == k.path && !self.subtree_exiting(i)) else { continue };
            let Some(s) = self.viewport_of(i).filter(|&s| self.nodes[s].path == k.viewport) else { continue };
            let scale = self.drawn_scale[s];
            if scale <= 1e-4 { continue; }
            let delta = (self.drawn[i][1] - self.drawn[s][1] - k.was) / scale;
            if delta.abs() < 1.0 / 64.0 { continue; }
            let stick = self.nodes[s].scroll.is_some_and(|s| s.stick_bottom);
            let Some(st) = self.scrolls.get_mut(&self.nodes[s].id) else { continue };
            let to = (st.offset + f64::from(delta)).clamp(0.0, st.max);
            if (to - st.offset).abs() < 1e-5 { continue; }
            st.offset = to;
            st.v = 0.0;
            st.glide = None;
            st.glide_to = None;
            st.pinned = stick && to >= st.max - 0.5;
            st.direct = true;
            moved = true;
        }
        // Once the content end prevents holding the anchor, let stick-bottom own the
        // viewport again, including growth committed while the collapse is still settling.
        let mut keeps = std::mem::take(&mut self.keeps);
        keeps.retain(|k| {
            let Some(i) = self.nodes.iter().position(|n| n.path == k.path) else { return true };
            let Some(s) = self.viewport_of(i) else { return false };
            !(self.scrolls.get(&self.nodes[s].id).is_some_and(|st| st.pinned)
                && self.drawn[i][1] - self.drawn[s][1] > k.was + 0.5)
        });
        self.keeps = keeps;
        moved
    }

    /// Colour, hover or decorative motion must not extend the anchor's lifetime.
    pub(super) fn finish_keeps(&mut self) {
        if self.keeps.is_empty() { return; }
        let mut keeps = std::mem::take(&mut self.keeps);
        keeps.retain(|k| self.nodes.iter().any(|n| {
            let within = n.path == k.viewport || (n.path.starts_with(&k.viewport)
                && n.path.as_bytes().get(k.viewport.len()) == Some(&b'/'));
            within && (self.scene.store().is_animating(n.id, prop::FRAME)
                || self.scene.store().is_animating(n.id, prop::TRAVEL)
                || self.grows.get(&n.id).is_some_and(|g| g.moving())
                || self.scrolls.get(&n.id).is_some_and(|st| st.slack.moving()))
        }));
        self.keeps = keeps;
    }

    /// What a reveal of row `key` in viewport `path` must leave to stuck headers: `None` when
    /// the row is, or is inside, a header stuck in view (it shows already), else the height
    /// stuck headers cover at the viewport's top, which the reveal keeps the row clear of.
    pub(super) fn stuck_cover(&self, path: &str, key: &str) -> Option<f64> {
        if !self.has_sticky || self.stuck.len() != self.nodes.len() || self.drawn.len() != self.nodes.len() { return Some(0.0) }
        let Some(s) = self.nodes.iter().position(|n| n.path == path) else { return Some(0.0) };
        let view = self.drawn[s];
        let row = self.nodes.iter().position(|n| n.key.as_deref() == Some(key) || n.path == key);
        let holder = row.and_then(|r| std::iter::successors(Some(r), |&p| self.nodes[p].parent).take_while(|&p| p != s).find(|&p| self.stuck[p] != 0.0));
        if let (Some(r), Some(_)) = (row, holder) {
            let d = self.drawn[r];
            if d[1] >= view[1] - 0.5 && d[1] + d[3] <= view[1] + view[3] + 0.5 { return None; }
        }
        let cover = (0..self.nodes.len())
            .filter(|&j| self.stuck[j] > 0.0 && Some(j) != holder && self.viewport_of(j) == Some(s))
            .map(|j| self.drawn[j][1] + self.drawn[j][3] - view[1])
            .fold(0.0f32, f32::max);
        Some(f64::from(cover))
    }
}

//! Reading an element's laid-out rect: `Cx::layout` for views, `El::on_layout` for handlers,
//! and `El::on_screen_layout` for handlers in global screen points (the rect plus the window
//! origin its host reports through `Runtime::set_window_origin`).
//!
//! Both see where layout puts an element (window points, after scrolling), never a frame of
//! its spring, so they don't re-run through animations. A view reading something its own
//! output resizes (its own size, say) can feed back into itself; that settles instead of
//! looping:
//! - Inside a commit, the view re-runs while what it read moved, at most [`PASSES`] more times.
//!   The values it is given next are then the final layout's, so nothing re-runs by itself.
//! - Between commits, only scrolling moves a laid-out rect, which re-runs readers next frame.
//! - Commits caused only by layout reads or `on_layout`, with no scroll moving, count up; past
//!   [`ECHOES`] in a row, reads and `on_layout` stop causing commits (values still update
//!   silently) until anything else commits.
use super::*;

/// Extra measurements one commit may take while a read moves.
const PASSES: usize = 2;
/// Commits in a row that layout feedback alone may cause.
const ECHOES: u32 = 4;
/// A rect counts as moved when an edge moves more than this (points).
const MOVED: f32 = 0.5;

/// What each key a view read resolved to, as last given to the view.
pub(super) type LayoutSeen = HashMap<String, Option<[f32; 4]>>;

fn moved(a: Option<[f32; 4]>, b: Option<[f32; 4]>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.iter().zip(b).any(|(x, y)| (x - y).abs() > MOVED),
        (None, None) => false,
        _ => true,
    }
}

impl<S: 'static> Runtime<S> {
    /// Node `i`'s laid-out rect `[x, y, w, h]` in window points, after its viewports' scrolling.
    fn laid_out(&self, nodes: &[Node], i: usize) -> [f32; 4] {
        let t = nodes[i].target;
        let (mut dx, mut dy) = (0.0, 0.0);
        let mut up = nodes[i].parent;
        while let Some(p) = up {
            if let Some(st) = self.scrolls.get(&nodes[p].id) {
                let origin = self.virtuals.values().find(|v| v.scroll_path == nodes[p].path).map_or(0.0, |v| v.before);
                dy += st.offset as f32 - origin;
                dx += st.x.offset as f32;
            }
            up = nodes[p].parent;
        }
        [t[0] - dx, t[1] - dy, t[2] - t[0], t[3] - t[1]]
    }

    /// Node `i`'s unscrolled layout rect, scaled by its own and its ancestors' static `El::scale`.
    fn static_layout(&self, i: usize) -> [f32; 4] {
        let t = self.nodes[i].target;
        let k = std::iter::successors(Some(i), |&p| self.nodes[p].parent)
            .fold(1.0_f32, |k, p| k * self.nodes[p].static_scale);
        [t[0] * k, t[1] * k, (t[2] - t[0]) * k, (t[3] - t[1]) * k]
    }

    fn layout_of(&self, nodes: &[Node], key: &str) -> Option<[f32; 4]> {
        let i = nodes.iter().position(|n| crate::flight::selects(key, &n.path, n.key.as_deref()))?;
        Some(self.laid_out(nodes, i))
    }

    /// Refreshes what the next view sees for `reads` (the last view's reads when `None`) from
    /// `nodes` (the current frame's when `None`). Returns whether any of it moved.
    pub(super) fn layout_moved(&mut self, reads: Option<&[String]>, nodes: Option<&[Node]>) -> bool {
        let nodes = nodes.unwrap_or(&self.nodes);
        let next: LayoutSeen = match reads {
            Some(keys) => keys.iter().map(|k| (k.clone(), self.layout_of(nodes, k))).collect(),
            None => self.layout_seen.keys().map(|k| (k.clone(), self.layout_of(nodes, k))).collect(),
        };
        let changed = next.len() != self.layout_seen.len()
            || next.iter().any(|(k, v)| self.layout_seen.get(k).is_none_or(|old| moved(*old, *v)));
        self.layout_seen = next;
        changed
    }

    /// Re-measures `measured` while a layout read moved, at most [`PASSES`] times, then leaves
    /// the reads matching the final layout so a view that can't settle doesn't loop. Returns
    /// whether it re-measured, so the view's last run may have built elements the first didn't.
    pub(super) fn settle_layout_reads(&mut self, mut measured: Measured, tx: &Tx) -> (Measured, bool) {
        if measured.layout_reads.is_empty() {
            self.layout_seen.clear();
            return (measured, false);
        }
        for pass in 0..PASSES {
            if !self.layout_moved(Some(&measured.layout_reads), Some(&measured.nodes)) { return (measured, pass > 0); }
            self.layout = measured.layout;
            measured = self.measure(tx);
        }
        self.layout_moved(Some(&measured.layout_reads), Some(&measured.nodes));
        (measured, true)
    }

    /// Whether layout feedback may still cause a commit (see the module docs).
    pub(super) fn layout_feedback_open(&self) -> bool {
        self.layout_echoes < ECHOES
    }

    /// Counts a frame's commit toward [`ECHOES`]: `feedback` when layout reads or `on_layout`
    /// caused it, `other` when anything else did.
    pub(super) fn count_layout_echo(&mut self, feedback: bool, other: bool) {
        let scrolling = self.scrolls.values().any(|st| st.scrolling || st.direct || st.glide.is_some() || st.x.glide.is_some());
        if other || scrolling {
            self.layout_echoes = 0;
        } else if feedback {
            self.layout_echoes += 1;
        }
    }

    /// Queues `on_layout` and `on_screen_layout` for elements whose laid-out rect changed (or
    /// that were just laid out).
    pub(super) fn report_layout(&mut self) {
        let quiet = !self.layout_feedback_open();
        self.report_rects(quiet, false);
        self.report_rects(quiet, true);
    }

    /// The host's window content moved to `origin`, its top-left in global screen points.
    /// Queues `on_screen_layout` for the elements that moved with it; true when it queued any,
    /// so the host draws a frame to deliver them. A window move is outside feedback, like a
    /// scroll, so it reopens layout feedback (see the module docs).
    pub(crate) fn set_window_origin(&mut self, origin: (f32, f32)) -> bool {
        if self.window_origin == origin { return false; }
        self.window_origin = origin;
        self.layout_echoes = 0;
        self.report_rects(false, true)
    }

    /// Queues `on_drawn_layout` for elements whose drawn rect moved since it last reported,
    /// once their own and their ancestors' springs rest. A move its laid-out rect explains is
    /// layout feedback and falls quiet with the rest of it; one it doesn't (a live value) always
    /// reports, as scrolling reopens feedback. True when it queued any.
    pub(super) fn report_drawn(&mut self) -> bool {
        if !self.has_drawn_layout && self.drawn_fired.is_empty() { return false; }
        let mut queued = false;
        let quiet = !self.layout_feedback_open();
        let last = std::mem::take(&mut self.drawn_fired);
        let mut fired = HashMap::with_capacity(last.len());
        for i in 0..self.nodes.len() {
            let n = &self.nodes[i];
            if n.on_drawn_layout.is_none() || self.subtree_exiting(i) { continue; }
            let Some(&drawn) = self.drawn.get(i) else { continue };
            let id = n.id;
            if self.rect_springing(i) {
                if let Some(old) = last.get(&id) { fired.insert(id, *old); }
                continue;
            }
            let laid = self.static_layout(i);
            let old = last.get(&id).copied();
            if !moved(old.map(|o| o.0), Some(drawn)) {
                fired.insert(id, old.unwrap_or((drawn, laid)));
                continue;
            }
            let external = old.is_some_and(|(_, l)| !moved(Some(l), Some(laid)));
            if quiet && !external {
                fired.insert(id, (drawn, laid));
                continue;
            }
            let path = n.path.clone();
            self.queue.retain(|a| !matches!(a, Action::DrawnLayout(p, ..) if *p == path));
            self.queue.push(Action::DrawnLayout(path, drawn, external));
            fired.insert(id, (drawn, laid));
            queued = true;
        }
        self.drawn_fired = fired;
        queued
    }

    /// Whether a spring is moving node `i`'s drawn rect: its own, or an ancestor's.
    fn rect_springing(&self, i: usize) -> bool {
        let store = self.scene.store();
        let n = &self.nodes[i];
        self.flights.iter().any(|f| f.lands(&n.path, n.key.as_deref()))
            || std::iter::successors(Some(i), |&p| self.nodes[p].parent).any(|p| {
                let id = self.nodes[p].id;
                store.is_animating(id, prop::FRAME) || store.is_animating(id, prop::TRAVEL) || store.is_animating(id, prop::PRESENCE)
                    || store.is_animating(id, prop::OFFSET) || store.is_animating(id, prop::SCALE) || store.is_animating(id, P_PRESS_SCALE)
            })
    }

    /// Queues `on_layout`, or `on_screen_layout` when `screen`, for rects that moved; true when
    /// it queued any.
    fn report_rects(&mut self, quiet: bool, screen: bool) -> bool {
        let last = std::mem::take(if screen { &mut self.screen_fired } else { &mut self.layout_fired });
        let (ox, oy) = if screen { self.window_origin } else { (0.0, 0.0) };
        let mut fired = HashMap::with_capacity(last.len());
        let mut queued = false;
        for i in 0..self.nodes.len() {
            let n = &self.nodes[i];
            if (if screen { n.on_screen_layout.is_none() } else { n.on_layout.is_none() }) || self.subtree_exiting(i) { continue; }
            let id = n.id;
            let [x, y, w, h] = self.laid_out(&self.nodes, i);
            let r = [x + ox, y + oy, w, h];
            if !quiet && moved(last.get(&id).copied(), Some(r)) {
                let path = self.nodes[i].path.clone();
                queued = true;
                if screen {
                    // Several moves between frames (a window drag) deliver only the latest.
                    self.queue.retain(|a| !matches!(a, Action::ScreenLayout(p, _) if *p == path));
                    self.queue.push(Action::ScreenLayout(path, r));
                } else {
                    self.queue.push(Action::Layout(path, r));
                }
            } else if let Some(old) = last.get(&id).filter(|_| !quiet) {
                // Unchanged: keep the rect last reported, so drift below the threshold adds up.
                fired.insert(id, *old);
                continue;
            }
            fired.insert(id, r);
        }
        *(if screen { &mut self.screen_fired } else { &mut self.layout_fired }) = fired;
        queued
    }
}

#[cfg(test)]
mod tests {
    use crate::{Env, Runtime, col, el};

    #[derive(Default)]
    struct S { screen: Vec<[f32; 4]>, local: Vec<[f32; 4]> }

    fn view(_: &S, _: &mut crate::Cx<S>) -> crate::El {
        col().w_full().h_full().padding(10.0).child(el().key("card").w(100.0).h(40.0)
            .on_layout(|s: &mut S, r| s.local.push(r))
            .on_screen_layout(|s: &mut S, r| s.screen.push(r)))
    }

    fn frames(rt: &mut Runtime<S>, n: usize) {
        for _ in 0..n { rt.frame(1.0 / 120.0); }
    }

    #[test]
    fn a_screen_rect_is_the_laid_out_rect_plus_the_window_origin() {
        let mut rt = Runtime::new(S::default(), view, (300.0, 200.0), Env::new(true, 2.0));
        assert!(!rt.set_window_origin((400.0, 250.0)), "nothing laid out yet");
        rt.frame(0.0);
        frames(&mut rt, 4);
        assert_eq!(rt.state.local, [[10.0, 10.0, 100.0, 40.0]]);
        assert_eq!(rt.state.screen, [[410.0, 260.0, 100.0, 40.0]], "first layout, origin added");
    }

    #[test]
    fn moving_the_window_reports_screen_rects_without_a_relayout() {
        let mut rt = Runtime::new(S::default(), view, (300.0, 200.0), Env::new(true, 2.0));
        rt.set_window_origin((400.0, 250.0));
        rt.frame(0.0);
        frames(&mut rt, 4);
        let commits = rt.stats.commits;

        // A move draws a frame that delivers the new rect; on_layout hears nothing.
        assert!(rt.set_window_origin((1700.0, -80.0)), "queued, so the host draws");
        frames(&mut rt, 2);
        assert_eq!(rt.state.screen.last(), Some(&[1710.0, -70.0, 100.0, 40.0]));
        assert_eq!(rt.state.local.len(), 1, "on_layout is window points: unmoved");

        // Below half a point it waits, and the drift adds up until it counts.
        assert!(!rt.set_window_origin((1700.3, -80.0)));
        assert!(rt.set_window_origin((1700.6, -80.0)));
        frames(&mut rt, 2);
        assert_eq!(rt.state.screen.len(), 3, "{:?}", rt.state.screen);
        assert!((rt.state.screen[2][0] - 1710.6).abs() < 1e-3, "{:?}", rt.state.screen);

        // A drag's moves between two frames deliver only the latest.
        rt.set_window_origin((1800.0, 0.0));
        rt.set_window_origin((1900.0, 0.0));
        frames(&mut rt, 2);
        assert_eq!(&rt.state.screen[3..], [[1910.0, 10.0, 100.0, 40.0]]);
        assert!(rt.stats.commits > commits, "handlers changed state");

        // Settled: nothing more runs.
        let (screen, commits) = (rt.state.screen.len(), rt.stats.commits);
        frames(&mut rt, 10);
        assert_eq!((rt.state.screen.len(), rt.stats.commits), (screen, commits));
    }

    #[test]
    fn a_window_move_with_no_screen_handler_asks_for_no_frame() {
        let mut rt = Runtime::new(S::default(), |_: &S, _: &mut crate::Cx<S>| {
            el().key("card").w(100.0).h(40.0).on_layout(|s: &mut S, r| s.local.push(r))
        }, (300.0, 200.0), Env::new(true, 2.0));
        rt.frame(0.0);
        frames(&mut rt, 2);
        assert!(!rt.set_window_origin((50.0, 50.0)));
        assert_eq!(rt.state.local.len(), 1);
    }
}

//! Reading an element's laid-out rect: `Cx::layout` (or only its size, `Cx::layout_size`) for
//! views, `El::on_layout` for handlers, and `El::on_screen_layout` for handlers in global screen
//! points (the rect plus the window origin its host reports through `Runtime::set_window_origin`).
//!
//! Each read records the part of the rect it depends on, so a size read doesn't re-run its
//! view when only the position moves (scrolling, say).
//!
//! All of them see where layout puts an element (window points, after scrolling), never a frame of
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

/// The part of a laid-out rect a read depends on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LayoutPart {
    /// Width and height (`Cx::layout_size`).
    Size,
    /// The whole rect (`Cx::layout`).
    Rect,
}

/// The keys a view read, each with the part it depends on.
pub(crate) type LayoutReads = Vec<(String, LayoutPart)>;

/// Counts a read of `key`'s `part` in `reads`; a whole-rect read covers a size read.
pub(crate) fn note_read(reads: &mut LayoutReads, key: &str, part: LayoutPart) {
    match reads.iter_mut().find(|(k, _)| k == key) {
        Some((_, p)) => if part == LayoutPart::Rect { *p = part },
        None => reads.push((key.to_owned(), part)),
    }
}

/// What a key a view read resolved to, as last given to the view, and the part it depends on.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Seen {
    pub(crate) rect: Option<[f32; 4]>,
    pub(crate) part: LayoutPart,
}

/// What each key a view read resolved to.
pub(crate) type LayoutSeen = HashMap<String, Seen>;

/// The rect `seen` holds for `key`, when the view read it.
pub(crate) fn seen_rect(seen: &LayoutSeen, key: &str) -> Option<[f32; 4]> {
    seen.get(key).and_then(|s| s.rect)
}

/// `[w, h]` of `rect`.
pub(crate) fn size_of(rect: Option<[f32; 4]>) -> Option<[f32; 2]> {
    rect.map(|r| [r[2], r[3]])
}

/// Whether the `part` of the rect a read depends on moved between `a` and `b`.
fn part_moved(a: Option<[f32; 4]>, b: Option<[f32; 4]>, part: LayoutPart) -> bool {
    match part {
        LayoutPart::Rect => moved(a, b),
        LayoutPart::Size => match (size_of(a), size_of(b)) {
            (Some(a), Some(b)) => a.iter().zip(b).any(|(x, y)| (x - y).abs() > MOVED),
            (a, b) => a.is_some() != b.is_some(),
        },
    }
}

impl<S> Cx<S> {
    /// The laid-out rect `[x, y, w, h]` in window points, after scrolling, of the element keyed
    /// `key` (or named by a key path such as `"msg-42/body"`, as `Tx::fly_text` takes), or
    /// `None` when there is none. It is where layout puts the element, not a frame of its spring.
    ///
    /// A view that reads it re-runs when that rect moves or resizes past half a point: in the
    /// same commit when the change comes from layout, the next frame when scrolling moved it.
    /// Reading an element the view itself resizes (its own size, say) settles: the commit
    /// re-runs the view at most twice more while the value moves, then keeps the last layout,
    /// and layout reads alone can't keep causing commits. A view whose output never reaches a
    /// fixed point gets a stale value rather than oscillating.
    ///
    /// A view that only needs the size should call [`Self::layout_size`], which doesn't re-run
    /// it when the element only moves.
    pub fn layout(&self, key: &str) -> Option<[f32; 4]> {
        note_read(&mut self.layout_reads.borrow_mut(), key, LayoutPart::Rect);
        let rect = seen_rect(&self.layout, key);
        crate::memo::read_layout(key, rect);
        rect
    }

    /// `[w, h]` of the element [`Self::layout`] would return the rect of, in points. Prefer it
    /// whenever the view uses only the size: the view re-runs when the width or height changes
    /// past half a point, but not when the element only moves, so an element inside a scrolling
    /// viewport doesn't re-run the view every frame of the scroll. It settles on the first
    /// layout like [`Self::layout`], so the first frame already has the right size.
    pub fn layout_size(&self, key: &str) -> Option<[f32; 2]> {
        note_read(&mut self.layout_reads.borrow_mut(), key, LayoutPart::Size);
        let size = size_of(seen_rect(&self.layout, key));
        crate::memo::read_layout_size(key, size);
        size
    }
}

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
        let mut up = nodes[i].parent.filter(|_| nodes[i].above.is_none());
        while let Some(p) = up {
            if let Some(st) = self.scrolls.get(&nodes[p].id) {
                let origin = self.virtuals.values().find(|v| v.scroll_path == nodes[p].path).map_or(0.0, |v| v.before);
                dy += st.offset as f32 - origin;
                dx += st.x.offset as f32;
            }
            up = nodes[p].parent.filter(|_| nodes[p].above.is_none());
        }
        if let Some(anc) = std::iter::successors(Some(i), |&p| nodes[p].parent).find(|&p| nodes[p].above.is_some())
            && let Some(a) = &nodes[anc].above
            && let Some(k) = nodes[..anc].iter().position(|n| n.key.as_ref() == Some(&a.key))
        {
            let at = sticky::anchored_target(nodes, &self.scrolls, k, self.viewport);
            let (w, h) = (nodes[anc].target[2] - nodes[anc].target[0], nodes[anc].target[3] - nodes[anc].target[1]);
            let (ax, ay) = anchor_position(a, at, (w, h), self.viewport);
            dx -= ax - nodes[anc].target[0];
            dy -= ay - nodes[anc].target[1];
        }
        [t[0] - dx, t[1] - dy, t[2] - t[0], t[3] - t[1]]
    }

    /// Node `i`'s unscrolled layout rect, under its own and its ancestors' static `El::scale`,
    /// `El::origin` and `El::rotate`.
    fn static_layout(&self, i: usize) -> [f32; 4] {
        let t = self.nodes[i].target;
        let (k, turn) = std::iter::successors(Some(i), |&p| self.nodes[p].parent)
            .fold((1.0_f32, 0.0_f32), |(k, a), p| {
                let [ox, oy] = self.nodes[p].vis.origin.unwrap_or([0.5, 0.5]);
                (k * self.nodes[p].static_scale, a + self.nodes[p].vis.rotate + (ox - 0.5) + (oy - 0.5))
            });
        [t[0] * k + turn, t[1] * k + turn, (t[2] - t[0]) * k, (t[3] - t[1]) * k]
    }

    fn layout_of(&self, nodes: &[Node], key: &str) -> Option<[f32; 4]> {
        if let Some(i) = nodes.iter().position(|n| crate::flight::selects(key, &n.path, n.key.as_deref())) {
            return Some(self.laid_out(nodes, i));
        }
        let (i, [x0, y0, x1, y1]) = self.highlight_target_in(nodes, None, key)?;
        let node_rect = self.laid_out(nodes, i);
        let t = nodes[i].target;
        Some([node_rect[0] + (x0 - t[0]), node_rect[1] + (y0 - t[1]), x1 - x0, y1 - y0])
    }

    /// Refreshes what the next view sees for `reads` (the last view's reads when `None`) from
    /// `nodes` (the current frame's when `None`). Returns whether any of it moved.
    /// Only the part of each rect a read depends on counts.
    pub(super) fn layout_moved(&mut self, reads: Option<&[(String, LayoutPart)]>, nodes: Option<&[Node]>) -> bool {
        let nodes = nodes.unwrap_or(&self.nodes);
        let resolve = |k: &String, part| (k.clone(), Seen { rect: self.layout_of(nodes, k), part });
        let next: LayoutSeen = match reads {
            Some(keys) => keys.iter().map(|(k, part)| resolve(k, *part)).collect(),
            None => self.layout_seen.iter().map(|(k, s)| resolve(k, s.part)).collect(),
        };
        let changed = next.len() != self.layout_seen.len()
            || next.iter().any(|(k, v)| self.layout_seen.get(k).is_none_or(|old| part_moved(old.rect, v.rect, v.part)));
        self.layout_seen = next;
        changed
    }

    /// Re-measures `measured` while a layout read moved, at most [`PASSES`] times, then leaves
    /// the reads matching the final layout so a view that can't settle doesn't loop.
    pub(super) fn settle_layout_reads(&mut self, mut measured: Measured, tx: &Tx) -> Measured {
        if measured.layout_reads.is_empty() {
            self.layout_seen.clear();
            return measured;
        }
        for _ in 0..PASSES {
            if !self.layout_moved(Some(&measured.layout_reads), Some(&measured.nodes)) { return measured; }
            self.layout = measured.layout;
            measured = self.measure(tx);
        }
        self.layout_moved(Some(&measured.layout_reads), Some(&measured.nodes));
        measured
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

    /// Whether a spring is moving node `i`'s drawn rect: its own, its anchor's, or an ancestor's.
    pub(super) fn rect_springing(&self, i: usize) -> bool {
        let store = self.scene.store();
        let n = &self.nodes[i];
        let step = |p: usize| {
            if let Some(a) = &self.nodes[p].above
                && let Some(j) = self.nodes[..p].iter().position(|n| n.key.as_ref() == Some(&a.key))
            {
                Some(j)
            } else {
                self.nodes[p].parent
            }
        };
        self.flights.iter().any(|f| f.lands(&n.path, n.key.as_deref()))
            || std::iter::successors(Some(i), |&p| step(p)).any(|p| {
                let id = self.nodes[p].id;
                store.is_animating(id, prop::FRAME) || store.is_animating(id, prop::TRAVEL) || store.is_animating(id, prop::PRESENCE)
                    || store.is_animating(id, prop::OFFSET) || store.is_animating(id, prop::SCALE) || store.is_animating(id, P_PRESS_SCALE)
                    || store.is_animating(id, P_SCALE) || store.is_animating(id, prop::ROTATION)
                    || self.grows.get(&id).is_some_and(|g| g.moving())
                    || self.stacks.get(&id).is_some_and(|s| s.moving)
                    || self.scrolls.get(&id).is_some_and(|s| s.slack.height() > 0.0 || s.glide.is_some())
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

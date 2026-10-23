//! Reading an element's laid-out rect: `Cx::layout` for views, `El::on_layout` for handlers.
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
        let mut dy = 0.0;
        let mut up = nodes[i].parent;
        while let Some(p) = up {
            if let Some(st) = self.scrolls.get(&nodes[p].id) {
                let origin = self.virtuals.values().find(|v| v.scroll_path == nodes[p].path).map_or(0.0, |v| v.before);
                dy += st.offset as f32 - origin;
            }
            up = nodes[p].parent;
        }
        [t[0], t[1] - dy, t[2] - t[0], t[3] - t[1]]
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
        if other {
            self.layout_echoes = 0;
        } else if feedback && !self.scrolls.values().any(|st| st.scrolling || st.glide.is_some()) {
            self.layout_echoes += 1;
        }
    }

    /// Queues `on_layout` for elements whose laid-out rect changed (or that were just laid out).
    pub(super) fn report_layout(&mut self) {
        let quiet = !self.layout_feedback_open();
        let mut fired = HashMap::with_capacity(self.layout_fired.len());
        for i in 0..self.nodes.len() {
            if self.nodes[i].on_layout.is_none() || self.subtree_exiting(i) { continue; }
            let id = self.nodes[i].id;
            let r = self.laid_out(&self.nodes, i);
            if !quiet && moved(self.layout_fired.get(&id).copied(), Some(r)) {
                self.queue.push(Action::Layout(self.nodes[i].path.clone(), r));
            } else if let Some(old) = self.layout_fired.get(&id).filter(|_| !quiet) {
                // Unchanged: keep the rect last reported, so drift below the threshold adds up.
                fired.insert(id, *old);
                continue;
            }
            fired.insert(id, r);
        }
        self.layout_fired = fired;
    }
}

//! Files dragged onto a window from another app: `El::on_drop_files`, `El::accepts_files` and
//! `El::on_drop_hover`.
use super::*;
use crate::el::DropHoverFn;
use std::path::PathBuf;

/// One step of a file drag over the window, queued for the next frame.
pub(crate) enum FileEvent {
    /// The drag entered or moved. A position the platform can't give is `None`.
    Hover(Vec<PathBuf>, Option<(f32, f32)>),
    Drop(Vec<PathBuf>, Option<(f32, f32)>),
    Leave,
}

#[derive(Default)]
pub(crate) struct FileDrag {
    /// A drag is over the window (set when the step is queued, before it runs).
    active: bool,
    /// The files the drag carries, for `drag_files_to`.
    paths: Vec<PathBuf>,
    /// The target of the last hover queued or run, so a move that keeps it queues nothing.
    aimed: Option<String>,
    /// The drop target notified with `on_drop_hover(true)`, and its callback, which still hears
    /// `false` after the element unmounts or starts to exit.
    hovered: Option<(String, Option<DropHoverFn>)>,
    /// This frame's drop targets: (node, visible rect in points, `hits.len()` before it). Kept
    /// apart from `hits`, so a drop zone doesn't cover the context menus and help under it.
    hits: Vec<(NodeId, [f32; 4], usize)>,
    /// File steps waiting for this frame's build (`hold_files`), and whether those leading the
    /// queue already waited one.
    held: Vec<Action>,
    waited: bool,
}

impl<S: 'static> Runtime<S> {
    /// A file drag from another app entered the window or moved over it, carrying `paths`; `at`
    /// is its position in points, or `None` when the platform doesn't say. The innermost
    /// [`El::on_drop_files`] under it that accepts the files hears `on_drop_hover(true)` next
    /// frame.
    pub fn drag_files(&mut self, paths: Vec<PathBuf>, at: Option<(f32, f32)>) {
        self.file_drag.active = true;
        self.file_drag.aimed = self.drop_target(&paths, at).map(|(i, _)| self.nodes[i].path.clone());
        self.file_drag.paths.clone_from(&paths);
        self.queue.push(Action::Files(FileEvent::Hover(paths, at)));
    }

    /// The file drag is at `at`, with the files it entered with. Queues a hover, and returns
    /// true, only when that changes the drop target, so a host can sample the pointer often and
    /// redraw only for a new target. Sampling while still also notices a target that unmounted.
    pub fn drag_files_to(&mut self, at: Option<(f32, f32)>) -> bool {
        if !self.file_drag.active { return false; }
        let target = self.drop_target(&self.file_drag.paths, at).map(|(i, _)| self.nodes[i].path.clone());
        if target == self.file_drag.aimed { return false; }
        self.file_drag.aimed = target;
        let paths = self.file_drag.paths.clone();
        self.queue.push(Action::Files(FileEvent::Hover(paths, at)));
        true
    }

    /// The file drag left the window, was cancelled, or the window hid.
    pub fn drag_files_leave(&mut self) {
        if !std::mem::take(&mut self.file_drag.active) { return; }
        self.file_drag.paths.clear();
        self.file_drag.aimed = None;
        self.queue.push(Action::Files(FileEvent::Leave));
    }

    /// `paths` dropped at `at` (points, or `None` when the platform doesn't say). Next frame the
    /// innermost [`El::on_drop_files`] under it that accepts any of them gets those it accepts,
    /// in one call, and the drop hover ends. A drop nothing takes is ignored.
    pub fn drop_files(&mut self, paths: Vec<PathBuf>, at: Option<(f32, f32)>) {
        self.file_drag.active = false;
        self.file_drag.paths.clear();
        self.file_drag.aimed = None;
        self.queue.push(Action::Files(FileEvent::Drop(paths, at)));
    }

    /// A file drag is over the window. Hosts whose platform reports no drag positions sample
    /// the pointer while this holds.
    pub fn dragging_files(&self) -> bool { self.file_drag.active }

    /// The key (else path) of the element showing the drop hover, for tests and tools.
    pub fn drop_hovered(&self) -> Option<String> {
        let (path, _) = self.file_drag.hovered.as_ref()?;
        Some(self.node(path).and_then(|n| n.key.clone()).unwrap_or_else(|| path.clone()))
    }

    /// A file step that hit-tests (a hover or drop) reaching a tree changed since its last build
    /// (`stale`: state changed this frame, or while the window was covered and built nothing)
    /// waits for this frame's build, once, so it lands on what is mounted and laid out now. The
    /// file steps after it wait too, to keep their order.
    pub(super) fn hold_files(&mut self, queue: Vec<Action>, stale: bool) -> Vec<Action> {
        let waited = std::mem::take(&mut self.file_drag.waited);
        let aims = |a: &Action| matches!(a, Action::Files(FileEvent::Hover(..) | FileEvent::Drop(..)));
        if waited || !stale || !queue.iter().any(aims) { return queue; }
        let (files, rest) = queue.into_iter().partition(|a| matches!(a, Action::Files(_)));
        self.file_drag.held = files;
        rest
    }

    /// The held file steps go first next frame.
    pub(super) fn release_files(&mut self) {
        if self.file_drag.held.is_empty() { return; }
        self.file_drag.waited = true;
        self.queue.splice(0..0, std::mem::take(&mut self.file_drag.held));
    }

    /// File steps wait for the next frame (`hold_files`).
    pub(super) fn files_held(&self) -> bool { self.file_drag.waited }

    pub(super) fn clear_drop_hits(&mut self) { self.file_drag.hits.clear(); }

    /// During `build`: drop target `i` draws visible over `rect`, after the first `seq` hits.
    pub(super) fn push_drop_hit(&mut self, i: usize, rect: [f32; 4], seq: usize) {
        if rect[2] > rect[0] && rect[3] > rect[1] {
            self.file_drag.hits.push((self.nodes[i].id, rect, seq));
        }
    }

    /// The target for `paths` at `at` and the files it takes: walking out from the topmost
    /// element under the point, the first drop target that accepts any of them, then the root.
    /// While a modal is open its root stands in for the window's, and a point outside it
    /// reaches nothing, as a drop on the dimmed window behind a sheet does. A point outside the
    /// window reaches nothing; a drop without a position goes to the root.
    fn drop_target(&self, paths: &[PathBuf], at: Option<(f32, f32)>) -> Option<(usize, Vec<PathBuf>)> {
        let ok = |id: &NodeId| self.node_index.get(id).copied()
            .filter(|&i| i < self.nodes.len() && !self.subtree_exiting(i) && self.allowed(i));
        let start = match at {
            None => None,
            Some((x, y)) if !self.in_viewport(x, y) => return None,
            Some((x, y)) => {
                let hit = self.hits.iter().enumerate().rev()
                    .find_map(|(k, (id, r))| contains(*r, x, y).then(|| ok(id)).flatten().map(|i| (k, i)));
                let zone = self.file_drag.hits.iter().rev()
                    .find_map(|(id, r, seq)| contains(*r, x, y).then(|| ok(id)).flatten().map(|i| (*seq, i)));
                // Whichever was drawn last is on top: a hit drawn after a zone started covers it.
                let top = match (hit, zone) {
                    (Some((k, h)), Some((seq, z))) => Some(if k >= seq { h } else { z }),
                    (h, z) => h.or(z).map(|(_, i)| i),
                };
                if top.is_none() && self.active_modal().is_some() { return None; }
                top
            }
        };
        let root = self.active_modal().unwrap_or(0);
        std::iter::successors(start, |&i| self.nodes[i].parent)
            .chain(std::iter::once(root).filter(|&r| r < self.nodes.len()))
            .filter(|&i| self.allowed(i) && !self.subtree_exiting(i))
            .find_map(|i| {
                let files = self.nodes[i].drop.as_ref()?.take(paths);
                (!files.is_empty()).then_some((i, files))
            })
    }

    /// Moves the drop hover to `target`, notifying the old and new elements.
    fn set_drop_hover(&mut self, target: Option<String>, tx: &mut Tx) -> bool {
        if self.file_drag.hovered.as_ref().map(|(p, _)| p) == target.as_ref() { return false; }
        let old = self.file_drag.hovered.take();
        let new = target.map(|path| {
            let f = self.node(&path).and_then(|n| n.drop.as_ref()?.on_hover.clone());
            (path, f)
        });
        self.file_drag.hovered.clone_from(&new);
        let mut ran = false;
        for (hover, over) in [(old, false), (new, true)] {
            let Some((path, kept)) = hover else { continue };
            // The element's current callback, else the one it had if it has gone.
            let Some(f) = self.node(&path).and_then(|n| n.drop.as_ref()?.on_hover.clone()).or(kept) else { continue };
            self.trace_event(format_args!("drop-hover:{over}:{path}"));
            f(handler_state(&mut self.state), over, tx);
            ran = true;
        }
        ran
    }

    pub(super) fn run_files(&mut self, event: FileEvent, tx: &mut Tx) -> bool {
        match event {
            FileEvent::Hover(paths, at) => {
                let target = self.drop_target(&paths, at).map(|(i, _)| self.nodes[i].path.clone());
                if self.file_drag.active { self.file_drag.aimed.clone_from(&target); }
                self.set_drop_hover(target, tx)
            }
            FileEvent::Leave => self.set_drop_hover(None, tx),
            FileEvent::Drop(paths, at) => {
                let target = self.drop_target(&paths, at);
                let mut ran = self.set_drop_hover(None, tx);
                if let Some((i, files)) = target && let Some(f) = self.nodes[i].drop.as_ref().and_then(|d| d.on_drop.clone()) {
                    let path = self.nodes[i].path.clone();
                    self.trace_event(format_args!("drop-files:{}:{path}", files.len()));
                    f(handler_state(&mut self.state), &files, tx);
                    ran = true;
                }
                ran
            }
        }
    }
}

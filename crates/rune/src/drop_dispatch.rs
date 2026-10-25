//! Files dragged onto a window from another app: `El::on_drop_files`, `El::accepts_files` and
//! `El::on_drop_hover`.
use super::*;
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
    /// The drop target notified with `on_drop_hover(true)`.
    hovered: Option<String>,
}

impl<S: 'static> Runtime<S> {
    /// A file drag from another app entered the window or moved over it, carrying `paths`; `at`
    /// is its position in points, or `None` when the platform doesn't say. The innermost
    /// [`El::on_drop_files`] under it that accepts the files hears `on_drop_hover(true)` next
    /// frame. Hosts call this again as the drag moves.
    pub fn drag_files(&mut self, paths: Vec<PathBuf>, at: Option<(f32, f32)>) {
        self.file_drag.active = true;
        self.file_drag.paths.clone_from(&paths);
        self.queue.push(Action::Files(FileEvent::Hover(paths, at)));
    }

    /// The file drag moved to `at` with the files it entered with.
    pub fn drag_files_to(&mut self, at: Option<(f32, f32)>) {
        if !self.file_drag.active { return; }
        let paths = self.file_drag.paths.clone();
        self.queue.push(Action::Files(FileEvent::Hover(paths, at)));
    }

    /// The file drag left the window, or was cancelled.
    pub fn drag_files_leave(&mut self) {
        if !std::mem::take(&mut self.file_drag.active) { return; }
        self.file_drag.paths.clear();
        self.queue.push(Action::Files(FileEvent::Leave));
    }

    /// `paths` dropped at `at` (points, or `None` when the platform doesn't say). Next frame the
    /// innermost [`El::on_drop_files`] under it that accepts any of them gets those it accepts,
    /// in one call, and the drop hover ends. A drop nothing takes is ignored.
    pub fn drop_files(&mut self, paths: Vec<PathBuf>, at: Option<(f32, f32)>) {
        self.file_drag.active = false;
        self.file_drag.paths.clear();
        self.queue.push(Action::Files(FileEvent::Drop(paths, at)));
    }

    /// A file drag is over the window. Hosts whose platform reports no drag positions sample
    /// the pointer while this holds.
    pub fn dragging_files(&self) -> bool { self.file_drag.active }

    /// The key (else path) of the element showing the drop hover, for tests and tools.
    pub fn drop_hovered(&self) -> Option<String> {
        let path = self.file_drag.hovered.as_deref()?;
        Some(self.node(path).and_then(|n| n.key.clone()).unwrap_or_else(|| path.to_owned()))
    }

    /// The target for `paths` at `at` and the files it takes: walking out from the topmost
    /// element under the point, the first drop target that accepts any of them, then the root
    /// (or the open modal) for a point over nothing, or with no position.
    fn drop_target(&self, paths: &[PathBuf], at: Option<(f32, f32)>) -> Option<(usize, Vec<PathBuf>)> {
        let top = at.and_then(|(x, y)| self.hits.iter().rev().find_map(|&(id, r)| {
            if !contains(r, x, y) { return None; }
            let i = *self.node_index.get(&id)?;
            (!self.subtree_exiting(i) && self.allowed(i)).then_some(i)
        }));
        let root = self.active_modal().unwrap_or(0);
        std::iter::successors(top, |&i| self.nodes[i].parent)
            .chain(std::iter::once(root).filter(|&r| r < self.nodes.len()))
            .filter(|&i| self.allowed(i) && !self.subtree_exiting(i))
            .find_map(|i| {
                let files = self.nodes[i].drop.as_ref()?.take(paths);
                (!files.is_empty()).then_some((i, files))
            })
    }

    /// Moves the drop hover to `target`, notifying the old and new elements.
    fn set_drop_hover(&mut self, target: Option<String>, tx: &mut Tx) -> bool {
        if self.file_drag.hovered == target { return false; }
        let old = std::mem::replace(&mut self.file_drag.hovered, target.clone());
        let mut ran = false;
        for (path, over) in [(old, false), (target, true)] {
            let Some(path) = path else { continue };
            let Some(f) = self.node(&path).and_then(|n| n.drop.as_ref()?.on_hover.clone()) else { continue };
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

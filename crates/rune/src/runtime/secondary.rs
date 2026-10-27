//! The secondary gesture (right-click, macOS control-click): which element's menu it opens.
use super::*;

impl<S: 'static> Runtime<S> {
    /// The nearest element with `on_secondary_press` (or a link with `on_link_secondary`) at
    /// (x, y), else the selectable text there (its Copy menu): an app's context menu on a row
    /// wins over its text's.
    pub(super) fn secondary_target(&self, x: f32, y: f32) -> Option<&str> {
        let id = self.hits.iter().rev().find(|&&(id, r)| self.hit_contains(id, r, x, y))?.0;
        let mut index = *self.node_index.get(&id)?;
        let mut text = None;
        loop {
            let n = &self.nodes[index];
            if self.subtree_exiting(index) || !self.allowed(index) { break; }
            if n.on_secondary_press.is_some() || self.secondary_link_at(index, x, y).is_some() { return Some(&n.path); }
            if text.is_none() && self.is_selectable(index) { text = Some(n.path.as_str()); }
            let Some(p) = n.parent else { break };
            index = p;
        }
        text
    }

    /// Capture a secondary gesture without activating primary actions or text selection.
    pub fn secondary_down(&mut self, x: f32, y: f32) {
        self.pointer = Some((x, y));
        self.release_hover_hold();
        self.keyboard_focus = false;
        self.dismiss_help();
        self.secondary_capture = self.secondary_target(x, y).map(str::to_owned);
    }

    pub fn secondary_up(&mut self, x: f32, y: f32) {
        self.pointer = Some((x, y));
        if let Some(path) = self.secondary_capture.take()
            && self.secondary_target(x, y) == Some(path.as_str())
        {
            let i = self.nodes.iter().position(|n| n.path == path);
            let point = i.map_or(PointerEvent { x, y, window_x: x, window_y: y }, |i| self.pointer_event(i, x, y));
            if let Some(k) = i.and_then(|i| self.secondary_link_at(i, x, y)) {
                self.queue.push(Action::LinkSecondary(path, k, point));
            } else if !self.selection_menu(&path, x, y) {
                self.queue.push(Action::SecondaryPress(path, point));
            }
        }
        self.leave_after_release();
    }
}

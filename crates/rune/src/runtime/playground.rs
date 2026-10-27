//! The runtime's side of [`App::playground`](crate::App::playground): its keys, its requests,
//! and what a new stage leaves behind.

use super::*;
use crate::playground::{Nav, Request};

pub(crate) fn handler_states<'a, S: 'static>(state: &'a mut S, playground: &'a mut Option<Box<crate::playground::Live<S>>>) -> crate::states::States<'a> {
    match playground {
        Some(playground) => playground.states(state),
        None => crate::states::States::one(handler_state(state)),
    }
}

pub(crate) fn handler_states_ref<'a, S: 'static>(state: &'a S, playground: Option<&'a crate::playground::Live<S>>) -> crate::states::StatesRef<'a> {
    match playground {
        Some(playground) => playground.states_ref(state),
        None => crate::states::StatesRef::one(handler_state_ref(state)),
    }
}

impl<S: 'static> Runtime<S> {
    /// The node the app's view returned: the first, or with the playground attached, the one
    /// above its bar or on its stage. Root handlers (window focus, paste, file drops) are there.
    pub(super) fn app_root(&self) -> usize {
        self.playground.as_ref().map_or(0, |p| p.root)
    }

    pub(super) fn find_app_root(&mut self) {
        let Some(playground) = &mut self.playground else { return };
        let wraps = |k: &str| k == "playground-closed" || crate::playground::is_stage(k);
        let nodes = &self.nodes;
        playground.root = nodes.iter().position(|n| n.parent.is_some_and(|p| nodes[p].key.as_deref().is_some_and(wraps))).unwrap_or(0);
    }

    /// Moves a Tx's playground requests to the runtime, for the next commit to apply before its
    /// view.
    pub(super) fn latch_playground(&mut self, tx: &mut Tx) {
        let requests = std::mem::take(&mut tx.playground);
        if requests.is_empty() { return; }
        // The playground swaps whole views: no fades between them.
        tx.instant = true;
        let Some(playground) = &mut self.playground else { return };
        playground.request(requests);
        self.dirty = true;
    }

    /// Applies queued playground requests. Focus leaves the old stage first, while the states
    /// its fields were built for are still there; a replay keeps it in the side column.
    pub(super) fn apply_playground(&mut self) {
        let Some(playground) = &self.playground else { return };
        if !playground.has_pending() { return; }
        let swaps = playground.pending_swaps_stage();
        let keep = playground.pending_replays_only() && self.focused_index().is_some_and(|i| self.under(i, |k| k == "playground-side"));
        let was_open = playground.is_open();
        if swaps && !keep && self.focused.is_some() { self.set_focus(None, false); }
        let clock = self.clock;
        if !self.playground.as_mut().is_some_and(|p| p.apply(clock)) { return; }
        self.memo.clear();
        if swaps { self.leave_stage(was_open); }
        // A new stage state gets its first poll on the next frame, as an app's does.
        if self.playground.as_ref().is_some_and(|p| p.polls()) { self.schedule_wakes([std::time::Duration::ZERO]); }
    }

    /// The old stage's elements, and the pointer's hold on them, go with its state: their
    /// handlers were built for it. Its exits end now, nothing it held hears a release or a
    /// hover ending, and the time scale it set goes back to the app's.
    fn leave_stage(&mut self, was_open: bool) {
        let exiting: Vec<_> = self.nodes.iter().map(|n| n.id).filter(|&id| self.is_exiting(id)).collect();
        for id in exiting { self.scene.unmount(id); }
        self.capture = None;
        self.secondary_capture = None;
        self.capture_inside = false;
        self.left_in_capture = false;
        self.drag_out_armed = false;
        self.drop_drag_out_request();
        self.text_sel.dragging = false;
        self.input_click.clear();
        self.hovered = None;
        self.hover_gone.clear();
        self.hover_within.clear();
        self.hover_shown.clear();
        self.hover_pending.clear();
        self.forget_drop_targets();
        self.last_press = None;
        self.last_drag_press = None;
        self.double_armed = false;
        let Some(playground) = &mut self.playground else { return };
        if was_open { self.time_scale = playground.app_time_scale; } else { playground.app_time_scale = self.time_scale; }
    }

    /// Whether node `i` or an ancestor has a key `is` takes.
    fn under(&self, i: usize, is: impl Fn(&str) -> bool) -> bool {
        std::iter::successors(Some(i), |&j| self.nodes[j].parent).any(|j| self.nodes[j].key.as_deref().is_some_and(&is))
    }

    /// The keyboard is on an element in the stage, which gets keys before the playground.
    fn focus_on_stage(&self) -> bool {
        !self.hover_focus && self.focused_index().is_some_and(|i| self.under(i, crate::playground::is_stage))
    }

    /// Keys nothing else took: Escape leaves an element in the stage, else goes Back.
    pub(super) fn playground_key(&mut self, ev: &KeyEvent, one: &mut Tx) -> bool {
        let plain = !ev.mods.cmd && !ev.mods.alt && !ev.mods.ctrl && !ev.mods.shift;
        if ev.key != crate::Key::Escape || !plain || !self.playground.as_ref().is_some_and(|p| p.is_open()) { return false; }
        if self.focus_on_stage() { self.set_focus(None, true); } else { one.close_playground(); }
        true
    }

    /// The playground's own keys, ahead of the stage's `on_key` and focus traversal: its
    /// shortcut, and while it's open, moving between stories, triggers, and into and around the
    /// side column. A focused text field, or element in the stage, keeps its keys.
    pub(super) fn playground_nav(&mut self, ev: &KeyEvent, one: &mut Tx) -> bool {
        let Some(playground) = &self.playground else { return false };
        if playground.is_shortcut(ev) {
            if !one.key_repeat { one.toggle_playground(); }
            return true;
        }
        if !playground.is_open() || self.focused_input().is_some() || self.focus_on_stage() { return false; }
        let side = self.playground_side();
        let at = self.focused.as_deref().filter(|_| !self.hover_focus).and_then(|f| side.iter().position(|p| p == f));
        match playground.nav(ev, at.is_some()) {
            Nav::Pass => false,
            Nav::Request(request) => {
                let trigger = matches!(request, Request::Trigger(_) | Request::Replay);
                if !(trigger && one.key_repeat) { one.playground.push(request); }
                true
            }
            Nav::IntoSide => {
                let Some(first) = side.first() else { return false };
                self.set_focus(Some(first.clone()), true);
                true
            }
            Nav::OutOfSide => {
                self.set_focus(None, true);
                true
            }
            Nav::SideStep(by) => {
                let (at, n) = (at.unwrap_or(0) as isize, side.len() as isize);
                let path = side[(at + by).rem_euclid(n) as usize].clone();
                self.set_focus(Some(path.clone()), true);
                one.scroll_to.push((None, path));
                true
            }
        }
    }

    /// The paths of the focusable controls in the playground's side column, in order.
    fn playground_side(&self) -> Vec<String> {
        (0..self.nodes.len()).filter(|&i| self.can_focus(i) && self.under(i, |k| k == "playground-side")).map(|i| self.nodes[i].path.clone()).collect()
    }
}

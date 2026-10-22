//! Focus is keyed retained state. Platform AX can read the same flags as the inspector.
use super::*;
use crate::edit::Key;

impl<S: 'static> Runtime<S> {
    fn descendant(&self, mut i: usize, ancestor: usize) -> bool {
        loop {
            if i == ancestor { return true; }
            let Some(p) = self.nodes[i].parent else { return false };
            i = p;
        }
    }
    pub(super) fn active_modal(&self) -> Option<usize> {
        self.modal_index
    }
    pub(super) fn refresh_modal_index(&mut self) {
        self.modal_index = self.nodes.iter().enumerate().rev()
            .find(|(i, n)| n.modal.is_some() && !self.subtree_exiting(*i)).map(|(i, _)| i);
    }
    pub(super) fn allowed(&self, i: usize) -> bool {
        self.active_modal().is_none_or(|m| self.descendant(i, m))
    }
    pub(super) fn can_focus(&self, i: usize) -> bool {
        let n = &self.nodes[i];
        let mut parent = n.parent;
        while let Some(p) = parent {
            if self.nodes[p].vis.opacity <= 0.0 { return false; }
            parent = self.nodes[p].parent;
        }
        (n.focus.enabled || n.input.as_ref().is_some_and(|s| s.access.is_some()))
            && self.allowed(i) && !self.subtree_exiting(i)
            && n.vis.opacity > 0.0 && n.local.w > 0.0 && n.local.h > 0.0
    }
    fn group(&self, i: usize) -> Option<usize> {
        let mut p = self.nodes[i].parent;
        while let Some(i) = p {
            if self.nodes[i].focus.group { return Some(i); }
            p = self.nodes[i].parent;
        }
        None
    }
    pub(super) fn focus_ring_visible(&self, mut i: usize) -> bool {
        if !self.keyboard_focus { return false; }
        loop {
            if self.focused.as_deref() == Some(&self.nodes[i].path) { return true; }
            // A nested focusable owns its ring, never inherit another control's focus.
            if self.nodes[i].focus.enabled { return false; }
            let Some(p) = self.nodes[i].parent else { return false };
            i = p;
        }
    }
    pub(super) fn set_focus(&mut self, path: Option<String>, keyboard: bool) {
        if self.focused != path {
            if let Some(a) = self.focused.as_deref().and_then(|p| self.node(p))
                .and_then(|n| n.input.as_ref()?.access.clone()) {
                let e = a(&mut self.state);
                e.focused = false;
                e.break_history_group();
            }
            if let Some(a) = path.as_deref().and_then(|p| self.node(p))
                .and_then(|n| n.input.as_ref()?.access.clone()) {
                let e = a(&mut self.state);
                e.focused = true;
                e.break_history_group();
            }
            self.focused = path;
            for n in &mut self.nodes {
                if let Some(input) = &mut n.input {
                    input.edit.focused = self.focused.as_deref() == Some(&n.path);
                }
            }
        }
        self.keyboard_focus = keyboard;
    }
    fn tab_order(&self) -> Vec<usize> {
        let all: Vec<_> = (0..self.nodes.len()).filter(|&i| self.can_focus(i) && self.nodes[i].focus.order >= 0).collect();
        let mut order: Vec<_> = all.iter().copied().filter(|&i| {
            let Some(g) = self.group(i) else { return true; };
            let chosen = all.iter().copied().filter(|&j| self.group(j) == Some(g))
                .find(|&j| self.focused.as_deref() == Some(&self.nodes[j].path))
                .or_else(|| all.iter().copied().find(|&j| self.group(j) == Some(g) && self.nodes[j].focus.selected))
                .or_else(|| all.iter().copied().find(|&j| self.group(j) == Some(g)));
            chosen == Some(i)
        }).collect();
        order.sort_by_key(|&i| { let n = self.nodes[i].focus.order; (if n > 0 { n } else { i32::MAX }, i) });
        order
    }
    pub(super) fn focus_key(&mut self, ev: &KeyEvent, tx: &mut Tx) -> bool {
        if ev.mods.cmd || ev.mods.ctrl || ev.mods.alt || ev.mods.logo { return false; }
        self.keyboard_focus = true;
        if ev.key == Key::Escape && let Some(i) = self.active_modal() {
            let f = self.nodes[i].modal.as_ref().unwrap().dismiss.clone();
            f(&mut self.state, tx);
            return true;
        }
        if ev.key == Key::Tab {
            let order = self.tab_order();
            if order.is_empty() { return self.active_modal().is_some(); }
            let current = order.iter().position(|&i| self.focused.as_deref() == Some(&self.nodes[i].path));
            let next = match current {
                Some(i) if ev.mods.shift => (i + order.len() - 1) % order.len(),
                Some(i) => (i + 1) % order.len(),
                None if ev.mods.shift => order.len() - 1,
                None => 0,
            };
            self.set_focus(Some(self.nodes[order[next]].path.clone()), true);
            return true;
        }
        let Some(i) = self.nodes.iter().position(|n| self.focused.as_deref() == Some(&n.path)) else { return false; };
        if !self.can_focus(i) { return false; }
        if let Some(g) = self.group(i) {
            let backward = matches!(ev.key, Key::Left | Key::Up);
            if backward || matches!(ev.key, Key::Right | Key::Down | Key::Home | Key::End) {
                let items: Vec<_> = (0..self.nodes.len()).filter(|&j| self.can_focus(j) && self.group(j) == Some(g)).collect();
                let pos = items.iter().position(|&j| j == i).unwrap();
                let next = match ev.key {
                    Key::Home => 0, Key::End => items.len() - 1,
                    _ if backward => (pos + items.len() - 1) % items.len(),
                    _ => (pos + 1) % items.len(),
                };
                let n = &self.nodes[items[next]];
                let action = (n.role == "radio" || n.role == "tab").then(|| n.on_press.clone()).flatten();
                let path = n.path.clone();
                self.set_focus(Some(path), true);
                if let Some(f) = action { f(&mut self.state, tx); }
                return true;
            }
        }
        if self.nodes[i].input.is_none() && (ev.key == Key::Enter || ev.key == Key::Text(" ".into())) {
            self.keyboard_focus = true;
            if let Some(f) = self.nodes[i].on_press.clone() { f(&mut self.state, tx); }
            return true;
        }
        false
    }
    pub(super) fn sync_modals(&mut self) {
        // Removed modals restore in reverse nesting order. Exiting visuals are already inert.
        while self.modal_stack.last().is_some_and(|(p, _)| self.node(p).is_none()) {
            let (_, restore) = self.modal_stack.pop().unwrap();
            let restore = restore.filter(|p| self.node(p).is_some());
            self.set_focus(restore, self.keyboard_focus);
        }
        let Some(i) = self.active_modal() else { return };
        let path = self.nodes[i].path.clone();
        if !self.modal_stack.iter().any(|(p, _)| p == &path) {
            self.modal_stack.push((path.clone(), self.focused.clone()));
            // Cancel any gesture captured before the modal opened.
            self.capture = None;
            self.hovered = None;
        }
        let inside = self.focused.as_deref().and_then(|p| self.nodes.iter().position(|n| n.path == p))
            .is_some_and(|j| self.descendant(j, i));
        if !inside {
            let target = self.tab_order().first().map(|&j| self.nodes[j].path.clone()).unwrap_or(path);
            self.set_focus(Some(target), self.keyboard_focus);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{col, el, Mods};

    #[derive(Default)]
    struct State { open: bool, nested: bool, picks: usize, selected: usize }
    fn button(key: &str) -> El { el().key(key).size(30.0, 30.0).focusable(true).role("button") }
    fn view(s: &State, _: &mut Cx<State>) -> El {
        let mut root = col().key("root").w_full().h_full()
            .child(button("opener").on_press(|s: &mut State| s.open = true))
            .child(button("early").tab_index(1))
            .child(button("skip").tab_index(-1))
            .child(button("disabled").focusable(false))
            .child(col().key("radios").focus_group()
                .children((0..3).map(|i| button(&format!("radio{i}")).role("radio").focus_selected(s.selected == i)
                    .focusable(i != 1).on_press(move |s: &mut State| { s.selected = i; s.picks += 1; }))));
        if s.open {
            root = root.child(col().key("modal").absolute(100.0, 0.0).size(100.0, 120.0)
                .modal(true, |s: &mut State| s.open = false)
                .child(button("one").on_press(|s: &mut State| s.nested = true))
                .child(button("two")));
        }
        if s.open && s.nested {
            root = root.child(col().key("nested").absolute(200.0, 0.0).size(90.0, 90.0)
                .modal(false, |s: &mut State| s.nested = false).child(button("inner")));
        }
        root
    }
    fn runtime() -> Runtime<State> {
        let mut rt = Runtime::new(State::default(), view, (300.0, 300.0), Env { system_dark: true, scale: 2.0 });
        rt.frame(0.0); rt
    }
    fn key(rt: &mut Runtime<State>, key: Key, shift: bool) {
        rt.key(KeyEvent { key, mods: Mods { shift, ..Default::default() } }); rt.frame(0.0);
    }
    fn focused(rt: &Runtime<State>) -> String {
        rt.elements().into_iter().find(|e| e.focused).and_then(|e| e.key).unwrap()
    }
    #[test]
    fn focus_document_order_overrides_roving_and_activation() {
        let mut rt = runtime();
        key(&mut rt, Key::Tab, false); assert_eq!(focused(&rt), "early");
        key(&mut rt, Key::Tab, false); assert_eq!(focused(&rt), "opener");
        key(&mut rt, Key::Tab, false); assert_eq!(focused(&rt), "radio0");
        key(&mut rt, Key::Right, false); assert_eq!(focused(&rt), "radio2");
        assert_eq!((rt.state.selected, rt.state.picks), (2, 1));
        key(&mut rt, Key::Text(" ".into()), false); assert_eq!(rt.state.picks, 2);
        key(&mut rt, Key::Tab, false); assert_eq!(focused(&rt), "early");
        key(&mut rt, Key::Tab, true); assert_eq!(focused(&rt), "radio2");
        let (x, y) = rt.find("early").unwrap();
        rt.pointer_down(x, y); rt.pointer_up(x, y); rt.frame(0.0);
        assert_eq!(focused(&rt), "early");
        assert!(!rt.elements().iter().any(|e| e.focus_visible));
    }
    #[test]
    fn modal_traps_restores_nested_and_blocks_background() {
        let mut rt = runtime();
        key(&mut rt, Key::Tab, false); key(&mut rt, Key::Tab, false);
        key(&mut rt, Key::Enter, false); assert_eq!(focused(&rt), "one");
        key(&mut rt, Key::Tab, true); assert_eq!(focused(&rt), "two");
        key(&mut rt, Key::Tab, false); assert_eq!(focused(&rt), "one");
        key(&mut rt, Key::Enter, false); assert_eq!(focused(&rt), "inner");
        let (x, y) = rt.find("opener").unwrap();
        rt.pointer_down(x, y); rt.pointer_up(x, y); rt.frame(0.0);
        assert!(rt.state.nested); assert_eq!(focused(&rt), "inner");
        key(&mut rt, Key::Escape, false); assert_eq!(focused(&rt), "one");
        key(&mut rt, Key::Escape, false); assert_eq!(focused(&rt), "opener");
        assert!(!rt.state.open);
        key(&mut rt, Key::Enter, false);
        rt.state.nested = false; rt.invalidate(); rt.frame(0.0);
        rt.pointer_down(5.0, 280.0); rt.pointer_up(5.0, 280.0); rt.frame(0.0);
        assert!(!rt.state.open); assert_eq!(focused(&rt), "opener");
        assert!(!rt.elements().iter().any(|e| e.focus_visible));
    }
}

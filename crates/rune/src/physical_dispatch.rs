use super::*;
use crate::PhysicalInput;

pub(super) struct Fallback { key: KeyEvent, paste: Option<String> }

impl<S: 'static> Runtime<S> {
    /// Queue physical input without a logical fallback (also useful for headless drivers).
    pub fn physical_input(&mut self, event: PhysicalInput) {
        self.queue.push(Action::Physical(event, None));
    }
    pub(crate) fn physical_with_fallback(&mut self, event: PhysicalInput, key: Option<KeyEvent>, paste: Option<String>) {
        let fallback = key.map(|key| Fallback { key, paste });
        self.queue.push(Action::Physical(event, fallback));
    }
    pub(super) fn dispatch_physical(&mut self, event: PhysicalInput, fallback: Option<Fallback>, tx: &mut Tx) -> bool {
        let handler = self.focused.as_deref().and_then(|p| self.node(p))
            .and_then(|n| n.focus.physical.clone());
        if let Some(handler) = handler {
            let mut one = Tx::default();
            let consumed = handler(&mut self.state, &event, &mut one);
            merge(tx, one);
            if consumed { return true; }
        }
        match fallback {
            Some(Fallback { key, paste }) => {
                if key.key == crate::Key::Command('v') && self.has_focus() && let Some(text) = paste {
                    self.run_action(Action::Paste(text), tx)
                } else { self.run_action(Action::Key(key), tx) }
            }
            None => false,
        }
    }
    pub(super) fn physical_focus_lost(&mut self) -> bool {
        let mut tx = Tx::default();
        let changed = self.dispatch_physical(PhysicalInput::FocusLost, None, &mut tx);
        merge(&mut self.pending, tx);
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cx, El, Env, Key, Mods, PhysicalKeyCode, TextEdit, TextStyle, col, el, text_input};

    #[derive(Default)]
    struct State { capture: bool, physical: usize, logical: usize, dismissed: usize, edit: TextEdit }
    fn view(s: &State, _: &mut Cx<State>) -> El {
        col().key("modal").size(300.0, 200.0).modal(false, |s: &mut State| s.dismissed += 1)
            .child(el().key("recorder").size(100.0, 40.0).focusable(true)
                .on_press(|s: &mut State| s.capture = true)
                .on_physical_input(|s: &mut State, _, _| {
                    if s.capture { s.physical += 1; true } else { false }
                }))
            .child(text_input(&s.edit, TextStyle::new(14.0, 400.0)).key("edit").size(200.0, 40.0)
                .on_edit(|s: &mut State| &mut s.edit))
    }
    #[test]
    fn physical_dispatch_precedes_modal_app_and_paste_after_same_frame_click() {
        let mut rt = Runtime::new(State::default(), view, (300.0, 200.0), Env { system_dark: true, scale: 1.0 });
        rt.on_key = Some(|s, _, _| { s.logical += 1; true });
        rt.frame(0.0);
        let (x, y) = rt.find("recorder").unwrap(); rt.pointer_down(x, y); rt.pointer_up(x, y);
        let mods = Mods { cmd: true, ..Mods::default() };
        rt.physical_with_fallback(PhysicalInput::Key { key: PhysicalKeyCode::KeyV, mods, pressed: true, repeat: false },
            Some(KeyEvent { key: Key::Command('v'), mods }), Some("paste".into()));
        rt.physical_with_fallback(PhysicalInput::Key { key: PhysicalKeyCode::Escape, mods: Mods::default(), pressed: true, repeat: false },
            Some(KeyEvent { key: Key::Escape, mods: Mods::default() }), None);
        rt.frame(0.0);
        assert_eq!((rt.state.physical, rt.state.logical, rt.state.dismissed), (2, 0, 0));
        assert_eq!(rt.state.edit.text, "");
        rt.state.capture = false;
        let (x, y) = rt.find("edit").unwrap(); rt.pointer_down(x, y); rt.pointer_up(x, y);
        rt.physical_with_fallback(PhysicalInput::Key { key: PhysicalKeyCode::KeyV, mods, pressed: true, repeat: false },
            Some(KeyEvent { key: Key::Command('v'), mods }), Some("paste".into()));
        rt.frame(0.0);
        assert_eq!(rt.state.edit.text, "paste");
        rt.physical_with_fallback(PhysicalInput::Key { key: PhysicalKeyCode::Escape, mods: Mods::default(), pressed: true, repeat: false },
            Some(KeyEvent { key: Key::Escape, mods: Mods::default() }), None);
        rt.frame(0.0);
        assert_eq!(rt.state.dismissed, 1);
    }
}

use super::*;
use crate::PhysicalInput;

pub(super) struct Fallback { key: KeyEvent }

impl<S: 'static> Runtime<S> {
    /// Queue physical input without a logical fallback (also useful for headless drivers).
    pub fn physical_input(&mut self, event: PhysicalInput) {
        self.queue.push(Action::Physical(event, None));
    }
    pub(crate) fn physical_with_fallback(&mut self, event: PhysicalInput, key: Option<KeyEvent>) {
        let fallback = key.map(|key| Fallback { key });
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
            Some(Fallback { key }) => {
                if key.key == crate::Key::Command('v') && self.accepts_paste() {
                    // Capture the target in event order, before another queued click can
                    // change focus. An empty/image clipboard still consumes the command.
                    self.paste_request = Some(self.paste_target());
                    false
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
            Some(KeyEvent { key: Key::Command('v'), mods }));
        rt.physical_with_fallback(PhysicalInput::Key { key: PhysicalKeyCode::Escape, mods: Mods::default(), pressed: true, repeat: false },
            Some(KeyEvent { key: Key::Escape, mods: Mods::default() }));
        rt.frame(0.0);
        assert_eq!((rt.state.physical, rt.state.logical, rt.state.dismissed), (2, 0, 0));
        assert_eq!(rt.state.edit.text, "");
        assert!(rt.paste_request.is_none(), "capture must not read the clipboard");
        rt.state.capture = false;
        let (x, y) = rt.find("edit").unwrap(); rt.pointer_down(x, y); rt.pointer_up(x, y);
        rt.physical_with_fallback(PhysicalInput::Key { key: PhysicalKeyCode::KeyV, mods, pressed: true, repeat: false },
            Some(KeyEvent { key: Key::Command('v'), mods }));
        rt.frame(0.0);
        let target = rt.paste_request.take().expect("paste requested");
        rt.paste_result(target, zeroize::Zeroizing::new("paste".into()));
        rt.frame(0.0);
        assert_eq!(rt.state.edit.text, "paste");
        rt.physical_with_fallback(PhysicalInput::Key { key: PhysicalKeyCode::Escape, mods: Mods::default(), pressed: true, repeat: false },
            Some(KeyEvent { key: Key::Escape, mods: Mods::default() }));
        rt.frame(0.0);
        assert_eq!(rt.state.dismissed, 1);
    }

    #[derive(Default)]
    struct Fields { first: TextEdit, second: TextEdit, secure: bool, keys: usize, app_keys: usize }
    fn fields(s: &Fields, _: &mut Cx<Fields>) -> El {
        let first = text_input(&s.first, TextStyle::new(14.0, 400.0)).single_line()
            .key("first").size(100.0, 32.0).on_edit(|s: &mut Fields| &mut s.first)
            .on_input_key(|s: &mut Fields, _, _| { s.keys += 1; false });
        col().size(300.0, 200.0)
            .child(if s.secure { first.secure() } else { first })
            .child(text_input(&s.second, TextStyle::new(14.0, 400.0)).key("second")
                .size(100.0, 32.0).on_edit(|s: &mut Fields| &mut s.second))
    }
    fn click<S: 'static>(rt: &mut Runtime<S>, key: &str) {
        let (x, y) = rt.find(key).unwrap(); rt.pointer_down(x, y); rt.pointer_up(x, y);
    }
    fn key<S: 'static>(rt: &mut Runtime<S>, physical: PhysicalKeyCode, key: Key) {
        let mods = Mods { cmd: matches!(key, Key::Command(_)), ..Mods::default() };
        rt.physical_with_fallback(PhysicalInput::Key { key: physical, mods, pressed: true, repeat: false },
            Some(KeyEvent { key, mods }));
    }
    fn fields_runtime(secure: bool) -> Runtime<Fields> {
        let mut rt = Runtime::new(Fields { secure, ..Fields::default() }, fields, (300.0, 200.0), Env::new(true, 1.0));
        rt.on_key = Some(|s, _, _| { s.app_keys += 1; true });
        rt.frame(0.0); rt
    }

    #[test]
    fn paste_is_swallowed_without_text_and_target_survives_a_later_queued_click() {
        let mut rt = fields_runtime(false);
        key(&mut rt, PhysicalKeyCode::KeyV, Key::Command('v'));
        rt.frame(0.0);
        assert!(rt.paste_request.is_none(), "no focused editor or paste handler: no clipboard read");
        assert_eq!(rt.state.app_keys, 1);
        click(&mut rt, "first");
        key(&mut rt, PhysicalKeyCode::KeyV, Key::Command('v'));
        rt.frame(0.0);
        let first = rt.paste_request.take().unwrap();
        assert!(first.is_some());
        assert_eq!((rt.state.keys, rt.state.app_keys), (0, 1));
        // Image-only clipboard sends no reply. An empty text reply also never falls
        // through to either key handler.
        rt.paste_result(first.clone(), zeroize::Zeroizing::new(String::new()));
        rt.frame(0.0);
        assert_eq!((rt.state.keys, rt.state.app_keys), (0, 1));
        key(&mut rt, PhysicalKeyCode::KeyV, Key::Command('v'));
        click(&mut rt, "second");
        rt.frame(0.0);
        let target = rt.paste_request.take().unwrap();
        assert_eq!(target, first, "target captured before the next click, not at worker flush");
        assert_ne!(target, rt.paste_target());
        rt.paste_result(target, zeroize::Zeroizing::new("late".into()));
        rt.frame(0.0);
        assert!(rt.state.first.text.is_empty() && rt.state.second.text.is_empty());
    }

    #[test]
    fn physical_fallback_edits_single_line_and_preserves_secure_copy_protection() {
        for secure in [false, true] {
            let mut rt = fields_runtime(secure);
            click(&mut rt, "first");
            key(&mut rt, PhysicalKeyCode::KeyX, Key::Text("x".into()));
            rt.frame(0.0);
            assert_eq!(rt.state.first.text, "x");
            assert_eq!(rt.has_secure_focus(), secure);
            key(&mut rt, PhysicalKeyCode::KeyA, Key::Command('a'));
            key(&mut rt, PhysicalKeyCode::KeyC, Key::Command('c'));
            rt.frame(0.0);
            assert_eq!(rt.copy_request.is_none(), secure);
            key(&mut rt, PhysicalKeyCode::KeyV, Key::Command('v'));
            rt.frame(0.0);
            let target = rt.paste_request.take().unwrap();
            rt.paste_result(target, zeroize::Zeroizing::new("replacement".into()));
            rt.frame(0.0);
            assert_eq!(rt.state.first.text, "replacement");
            if secure {
                let elements = rt.elements();
                let field = elements.iter().find(|e| e.key.as_deref() == Some("first")).unwrap();
                assert!(field.text.is_none());
            }
        }
    }

    #[test]
    fn physical_paste_uses_app_handler_without_a_focused_editor() {
        fn view(_: &String, _: &mut Cx<String>) -> El {
            el().size(100.0, 100.0).on_paste(|s: &mut String, text, _| *s = text.into())
        }
        let mut rt = Runtime::new(String::new(), view, (100.0, 100.0), Env::new(true, 1.0));
        rt.frame(0.0);
        key(&mut rt, PhysicalKeyCode::KeyV, Key::Command('v'));
        rt.frame(0.0);
        assert_eq!(rt.paste_request, Some(None));
        let target = rt.paste_request.take().unwrap();
        rt.paste_result(target, zeroize::Zeroizing::new("app".into()));
        rt.frame(0.0);
        assert_eq!(rt.state, "app");
    }
}

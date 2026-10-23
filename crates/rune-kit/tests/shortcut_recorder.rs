use rune::{Cx, El, Env, Key, KeyEvent, Mods, PhysicalInput, PhysicalKeyCode as Code, Runtime, ShortcutError};
use rune_kit::{Kit, ShortcutRecorder, ShortcutRecorderState};

struct State { recorder: ShortcutRecorderState, reject: bool, disabled: bool, changes: usize }
fn view(s: &State, _: &mut Cx<State>) -> El {
    rune::col().size(400.0, 200.0).child(
        ShortcutRecorder::new("shortcut", &s.recorder, |s: &mut State| &mut s.recorder)
            .disabled(s.disabled)
            .on_change(|s, _, _| { s.changes += 1; if s.reject { Err(ShortcutError::Conflict) } else { Ok(()) } })
            .build(&Kit::neutral(true)))
        .child(rune::el().key("other").size(100.0, 32.0).focusable(true))
}
fn runtime() -> Runtime<State> {
    let mut rt = Runtime::new(State { recorder: ShortcutRecorderState::new(Some("ctrl+k".parse().unwrap())), reject: false, disabled: false, changes: 0 },
        view, (400.0, 200.0), Env { system_dark: true, scale: 1.0 });
    rt.frame(0.0); rt
}
fn click(rt: &mut Runtime<State>, name: &str) {
    let (x, y) = rt.find(name).unwrap(); rt.pointer_down(x, y); rt.pointer_up(x, y);
}
fn physical(rt: &mut Runtime<State>, key: Code, mods: Mods, pressed: bool, repeat: bool) {
    rt.physical_input(PhysicalInput::Key { key, mods, pressed, repeat }); rt.frame(0.0);
}

#[test]
fn click_and_chord_in_one_frame_capture_physical_key_and_ignore_repeat() {
    let mut rt = runtime(); click(&mut rt, "shortcut");
    physical(&mut rt, Code::KeyQ, Mods { ctrl: true, ..Mods::default() }, true, false);
    assert_eq!(rt.state.recorder.shortcut().unwrap().to_string(), "ctrl+q");
    assert!(!rt.state.recorder.is_recording());
    physical(&mut rt, Code::KeyQ, Mods::default(), true, true);
    assert_eq!(rt.state.changes, 1);
}

#[test]
fn held_modifiers_preview_release_cancel_and_clear() {
    let mut rt = runtime(); click(&mut rt, "shortcut");
    rt.physical_input(PhysicalInput::Modifiers(Mods { ctrl: true, shift: true, ..Mods::default() })); rt.frame(0.0);
    assert_eq!(rt.state.recorder.keycaps().len(), 2);
    physical(&mut rt, Code::ShiftLeft, Mods { ctrl: true, ..Mods::default() }, false, false);
    assert_eq!(rt.state.recorder.keycaps().len(), 1);
    assert_eq!(rt.state.changes, 0);
    physical(&mut rt, Code::Escape, Mods::default(), true, false);
    assert!(!rt.state.recorder.is_recording());
    assert_eq!(rt.state.recorder.shortcut().unwrap().to_string(), "ctrl+k");
    click(&mut rt, "shortcut");
    physical(&mut rt, Code::Backspace, Mods::default(), true, false);
    assert!(rt.state.recorder.shortcut().is_none());
    assert_eq!(rt.state.changes, 1);
}

#[test]
fn conflicts_keep_old_binding_and_unchanged_chords_skip_registration() {
    let mut rt = runtime(); rt.state.reject = true; click(&mut rt, "shortcut");
    physical(&mut rt, Code::KeyJ, Mods { ctrl: true, ..Mods::default() }, true, false);
    assert_eq!(rt.state.recorder.error(), Some(&ShortcutError::Conflict));
    assert_eq!(rt.state.recorder.shortcut().unwrap().to_string(), "ctrl+k");
    assert!(rt.state.recorder.is_recording());
    physical(&mut rt, Code::KeyK, Mods { ctrl: true, ..Mods::default() }, true, false);
    assert!(rt.state.recorder.error().is_none());
    assert!(!rt.state.recorder.is_recording());
    assert_eq!(rt.state.changes, 1);
    click(&mut rt, "shortcut");
    physical(&mut rt, Code::Backspace, Mods::default(), true, false);
    assert_eq!(rt.state.recorder.error(), Some(&ShortcutError::Conflict));
    assert!(rt.state.recorder.shortcut().is_some());
    physical(&mut rt, Code::Escape, Mods::default(), true, false);
    assert!(rt.state.recorder.error().is_none());
    assert!(!rt.state.recorder.is_recording());
    assert_eq!(rt.state.recorder.shortcut().unwrap().to_string(), "ctrl+k");
}

#[test]
fn keyboard_activation_and_both_kinds_of_blur() {
    let mut rt = runtime();
    rt.key(KeyEvent { key: Key::Tab, mods: Mods::default() });
    rt.key(KeyEvent { key: Key::Enter, mods: Mods::default() }); rt.frame(0.0);
    assert!(rt.state.recorder.is_recording());
    click(&mut rt, "other"); rt.frame(0.0);
    assert!(!rt.state.recorder.is_recording());
    click(&mut rt, "shortcut"); rt.physical_input(PhysicalInput::FocusLost); rt.frame(0.0);
    assert!(!rt.state.recorder.is_recording());
    assert_eq!(rt.state.changes, 0);
}

#[test]
fn disabling_a_focused_recorder_cancels_capture() {
    let mut rt = runtime(); click(&mut rt, "shortcut"); rt.frame(0.0);
    assert!(rt.state.recorder.is_recording());
    rt.state.disabled = true; rt.invalidate(); rt.frame(0.0);
    assert!(!rt.state.recorder.is_recording());
    physical(&mut rt, Code::KeyJ, Mods::default(), true, false);
    assert_eq!(rt.state.changes, 0);
}

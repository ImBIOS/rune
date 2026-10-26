//! `Tx::live_only` on the physical key path native windows use, and debug builds' check that
//! a live-only handler didn't change what the view reads.
use super::*;
use crate::{Cx, El, Env, Key, Live, Mods, PhysicalInput, PhysicalKeyCode, el, text};

struct S {
    /// Read by the view.
    count: u32,
    /// Handlers bump `count` (what the view reads) as well as `pan`.
    misuse: bool,
    pan: Live<[f32; 2]>,
    physical: usize,
}

impl S {
    fn nudge(&mut self, tx: &mut Tx) {
        let [x, y] = self.pan.get();
        self.pan.set([x + 10.0, y]);
        if self.misuse { self.count += 1; }
        tx.live_only();
    }
}

fn view(s: &S, _: &mut Cx<S>) -> El {
    el().size(200.0, 200.0)
        .child(el().key("camera").size(50.0, 50.0).live_offset(&s.pan).on_press(|s: &mut S| s.count += 1))
        .child(text(format!("{}", s.count)).key("count"))
        .child(el().key("recorder").size(40.0, 40.0).focusable(true)
            .on_physical_input(|s: &mut S, e, tx| {
                if !matches!(e, PhysicalInput::Key { pressed: false, .. }) { return false; }
                s.physical += 1;
                s.nudge(tx);
                true
            }))
        .on_key_up(|s: &mut S, _, tx| { s.nudge(tx); true })
}

fn app(misuse: bool) -> Runtime<S> {
    let state = S { count: 0, misuse, pan: Live::new([0.0, 0.0]), physical: 0 };
    let mut rt = Runtime::new(state, view, (200.0, 200.0), Env::new(true, 1.0));
    rt.on_key = Some(|s, e, tx| { if e.key != Key::Right { return false; } s.nudge(tx); true });
    rt.frame(0.0);
    rt
}

fn key(code: PhysicalKeyCode, key: Key, pressed: bool) -> (PhysicalInput, Option<KeyEvent>) {
    (PhysicalInput::Key { key: code, mods: Mods::default(), pressed, repeat: false }, Some(KeyEvent { key, mods: Mods::default() }))
}

fn camera_x(rt: &Runtime<S>) -> f32 {
    rt.elements().into_iter().find(|e| e.key.as_deref() == Some("camera")).unwrap().rect[0]
}

#[test]
fn physical_keys_and_their_releases_can_be_live_only() {
    let mut rt = app(false);
    rt.check_live_only(false);
    let before = rt.stats.commits;
    let (ev, fallback) = key(PhysicalKeyCode::ArrowRight, Key::Right, true);
    rt.physical_with_fallback(ev, fallback);
    rt.frame(1.0 / 120.0);
    let (ev, fallback) = key(PhysicalKeyCode::ArrowRight, Key::Right, false);
    rt.physical_with_fallback(ev, fallback);
    rt.frame(1.0 / 120.0);
    rt.frame(1.0 / 120.0);
    assert_eq!((rt.stats.commits - before, rt.live_only_skips), (0, 2), "on_key, then on_key_up");
    assert_eq!(camera_x(&rt), 20.0, "the frames drew both");

    // A key the app declines goes on to Rune's own handling and commits as before.
    let (ev, fallback) = key(PhysicalKeyCode::Tab, Key::Tab, true);
    rt.physical_with_fallback(ev, fallback);
    rt.frame(1.0 / 120.0);
    assert_eq!(rt.stats.commits - before, 1);
}

#[test]
fn a_physical_release_heard_at_rest_can_be_live_only() {
    let mut rt = app(false);
    rt.check_live_only(false);
    let (x, y) = rt.find("recorder").unwrap();
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    for _ in 0..60 { rt.frame(1.0 / 120.0); }
    let before = rt.stats.commits;
    // No logical key: the focused handler hears it at once, outside a frame.
    let event = PhysicalInput::Key { key: PhysicalKeyCode::ShiftLeft, mods: Mods::default(), pressed: false, repeat: false };
    assert!(rt.physical_with_fallback(event, None), "it still asks for a frame to draw");
    assert!(!rt.dirty);
    rt.frame(1.0 / 120.0);
    assert_eq!((rt.state.physical, rt.stats.commits - before), (1, 0));
    assert_eq!(camera_x(&rt), 10.0);
}

#[test]
fn debug_builds_warn_when_a_live_only_handler_changed_what_the_view_reads() {
    let mut rt = app(true);
    // A quarter second after the last commit, a live-only event is checked: it commits, and
    // the view built a different count.
    rt.frame(0.5);
    rt.key(KeyEvent { key: Key::Right, mods: Mods::default() });
    rt.frame(1.0 / 120.0);
    assert_eq!(rt.live_only_warnings.len(), 1, "{:?}", rt.live_only_warnings);
    assert!(rt.live_only_warnings[0].contains("on_key"), "{}", rt.live_only_warnings[0]);

    // Used as meant, the check commits and stays quiet; events between checks skip the view.
    let mut rt = app(false);
    rt.frame(0.5);
    let before = rt.stats.commits;
    for _ in 0..10 {
        rt.key_up(KeyEvent { key: Key::Right, mods: Mods::default() });
        rt.frame(1.0 / 120.0);
    }
    assert!(rt.live_only_warnings.is_empty(), "{:?}", rt.live_only_warnings);
    assert_eq!((rt.stats.commits - before, rt.live_checks, rt.live_only_skips), (1, 1, 9));
}

#[test]
fn the_check_ignores_a_change_another_event_made_in_the_same_frame() {
    let mut rt = app(false);
    rt.frame(0.5);
    // The release is checked, and then a press in the same frame changes the count.
    rt.key_up(KeyEvent { key: Key::Right, mods: Mods::default() });
    let (x, y) = rt.find("camera").unwrap();
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(1.0 / 120.0);
    assert_eq!((rt.state.count, rt.live_checks), (1, 1));
    assert!(rt.live_only_warnings.is_empty(), "{:?}", rt.live_only_warnings);
}

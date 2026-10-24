//! Modifier keys pressed and released on their own (`Key::Modifier`), for runtimes that opt in.
//!
//! Hosts already pass every physical key, modifiers included, to `physical_with_fallback`, so
//! the logical event is made here once for windows, overlays, layer surfaces and headless runs.
//! Presses come only from physical keys. A release also comes from a modifier that a
//! `PhysicalInput::Modifiers` turns off while its key is down (a key-up the platform never
//! sent), and every held key is released on `FocusLost`.

use super::*;
use crate::{Key, Modifier, Mods, PhysicalInput, PhysicalKeyCode as Code};

const ALL: [Modifier; 9] = [Modifier::ShiftLeft, Modifier::ShiftRight, Modifier::ControlLeft, Modifier::ControlRight,
    Modifier::AltLeft, Modifier::AltRight, Modifier::SuperLeft, Modifier::SuperRight, Modifier::Fn];

#[derive(Default)]
pub(super) struct ModifierKeys {
    pub(super) enabled: bool,
    /// Bit `i` set while `ALL[i]` is down.
    held: u16,
    /// The last `PhysicalInput::Modifiers`, to see a flag turn off. A flag that was never on
    /// says nothing: AltGr (Right Alt on many layouts) is down without `alt` on X11 and Wayland.
    last: Mods,
}

fn modifier(code: Code) -> Option<Modifier> {
    Some(match code {
        Code::ShiftLeft => Modifier::ShiftLeft,
        Code::ShiftRight => Modifier::ShiftRight,
        Code::ControlLeft => Modifier::ControlLeft,
        Code::ControlRight => Modifier::ControlRight,
        Code::AltLeft => Modifier::AltLeft,
        Code::AltRight => Modifier::AltRight,
        Code::SuperLeft => Modifier::SuperLeft,
        Code::SuperRight => Modifier::SuperRight,
        Code::Fn => Modifier::Fn,
        _ => return None,
    })
}

fn bit(m: Modifier) -> u16 { 1 << ALL.iter().position(|&a| a == m).unwrap_or(0) }

/// Both sides of `m`'s modifier.
fn pair(m: Modifier) -> u16 {
    use Modifier::*;
    match m {
        ShiftLeft | ShiftRight => bit(ShiftLeft) | bit(ShiftRight),
        ControlLeft | ControlRight => bit(ControlLeft) | bit(ControlRight),
        AltLeft | AltRight => bit(AltLeft) | bit(AltRight),
        SuperLeft | SuperRight => bit(SuperLeft) | bit(SuperRight),
        _ => bit(m),
    }
}

/// Whether `mods` has `m`'s modifier on. Fn has no flag.
fn active(mods: Mods, m: Modifier) -> bool {
    use Modifier::*;
    match m {
        ShiftLeft | ShiftRight => mods.shift,
        ControlLeft | ControlRight => mods.ctrl,
        AltLeft | AltRight => mods.alt,
        SuperLeft | SuperRight => mods.logo,
        _ => false,
    }
}

/// `mods` with `m`'s modifier set to `on`, including `cmd` (Command on macOS, Ctrl elsewhere).
fn with(mut mods: Mods, m: Modifier, on: bool) -> Mods {
    use Modifier::*;
    let mac = cfg!(target_os = "macos");
    match m {
        ShiftLeft | ShiftRight => mods.shift = on,
        ControlLeft | ControlRight => { mods.ctrl = on; if !mac { mods.cmd = on; } }
        AltLeft | AltRight => mods.alt = on,
        SuperLeft | SuperRight => { mods.logo = on; if mac { mods.cmd = on; } }
        _ => {}
    }
    mods
}

impl ModifierKeys {
    fn event(&self, m: Modifier, pressed: bool, mods: Mods) -> KeyEvent {
        // Platforms differ on whether a modifier's own key event carries the flags from
        // before or after it; report them after, from the keys held.
        KeyEvent { key: Key::Modifier { modifier: m, pressed }, mods: with(mods, m, self.held & pair(m) != 0) }
    }

    /// A modifier key going down or up. None for other keys, repeats, and a press or release
    /// that changes nothing (a release already sent, or a press while held).
    fn key(&mut self, code: Code, pressed: bool, repeat: bool, mods: Mods) -> Option<KeyEvent> {
        let m = modifier(code).filter(|_| self.enabled && !repeat)?;
        if (self.held & bit(m) != 0) == pressed { return None; }
        self.held ^= bit(m);
        Some(self.event(m, pressed, mods))
    }

    /// Releases for held keys that `event` shows are up: every one on `FocusLost`, and those
    /// whose flag `Modifiers` turns off (Fn has none, so only its key-up or focus loss ends it).
    fn releases(&mut self, event: &PhysicalInput) -> Vec<KeyEvent> {
        let (now, all) = match *event {
            PhysicalInput::Modifiers(mods) => (mods, false),
            PhysicalInput::FocusLost => (Mods::default(), true),
            PhysicalInput::Key { .. } => return Vec::new(),
        };
        let last = std::mem::replace(&mut self.last, now);
        if !self.enabled { self.held = 0; }
        let mut out = Vec::new();
        for m in ALL {
            if self.held & bit(m) == 0 || !(all || (active(last, m) && !active(now, m))) { continue; }
            self.held &= !bit(m);
            out.push(self.event(m, false, now));
        }
        out
    }
}

impl<S: 'static> Runtime<S> {
    /// Send `Key::Modifier` presses and releases to `on_key` and the focused input's key
    /// handler. Off by default: handlers written before it may act on any key.
    pub fn set_modifier_keys(&mut self, enabled: bool) {
        self.modifier_keys.enabled = enabled;
    }

    pub fn modifier_keys(&self) -> bool { self.modifier_keys.enabled }

    /// The logical event for a physical modifier key, if it is one and modifier keys are on.
    pub(super) fn modifier_key(&mut self, event: &PhysicalInput) -> Option<KeyEvent> {
        let PhysicalInput::Key { key, mods, pressed, repeat } = *event else { return None };
        self.modifier_keys.key(key, pressed, repeat, mods)
    }

    /// Queue releases for held modifier keys that `event` shows are up; true if any.
    pub(super) fn modifier_releases(&mut self, event: &PhysicalInput) -> bool {
        let released = self.modifier_keys.releases(event);
        let any = !released.is_empty();
        self.queue.extend(released.into_iter().map(|ev| Action::Key { ev, repeat: false }));
        any
    }

    /// A modifier key goes to the app and the focused input's key handler, in `route_key`'s
    /// order, and to nothing built in: no editing, selection, focus traversal or focus ring.
    pub(super) fn route_modifier(&mut self, ev: &KeyEvent, one: &mut Tx) -> bool {
        if !self.modifier_keys.enabled { return false; }
        one.key_repeat = false;
        let app_first = self.focused.is_none();
        if app_first && let Some(f) = self.on_key && f(&mut self.state, ev, one) { return true; }
        if let Some(f) = self.focused_input().and_then(|input| input.key.clone())
            && f(handler_state(&mut self.state), ev, one)
        {
            return true;
        }
        if !app_first && let Some(f) = self.on_key && f(&mut self.state, ev, one) {
            self.events.push(format!("key {:?}", ev.key));
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cx, El, Env, KeySide, TextEdit, TextStyle, col, el, text_input};

    #[derive(Default)]
    struct Keys { edit: TextEdit, app: Vec<KeyEvent>, field: Vec<KeyEvent>, field_takes: bool, chords: usize }
    fn view(s: &Keys, _: &mut Cx<Keys>) -> El {
        col().size(300.0, 200.0)
            .child(text_input(&s.edit, TextStyle::new(14.0, 400.0)).single_line().key("field").size(200.0, 32.0)
                .on_edit(|s: &mut Keys| &mut s.edit)
                .on_input_key(|s: &mut Keys, ev, _| {
                    if !matches!(ev.key, Key::Modifier { .. }) { return false; }
                    s.field.push(ev.clone());
                    s.field_takes
                }))
            .child(el().key("blank").size(100.0, 40.0))
    }
    fn runtime(enabled: bool) -> Runtime<Keys> {
        let mut rt = Runtime::new(Keys::default(), view, (300.0, 200.0), Env::new(true, 1.0));
        rt.set_modifier_keys(enabled);
        rt.on_key = Some(|s, ev, _| {
            // A typical chord binding: Cmd with any key.
            if ev.mods.cmd && matches!(ev.key, Key::Text(_) | Key::Command(_)) { s.chords += 1; }
            s.app.push(ev.clone());
            true
        });
        rt.frame(0.0);
        rt
    }
    fn click(rt: &mut Runtime<Keys>, key: &str) {
        let (x, y) = rt.find(key).unwrap(); rt.pointer_down(x, y); rt.pointer_up(x, y); rt.frame(0.0);
    }
    /// A physical modifier key, as hosts send it: `mods` from before the change, like macOS.
    fn modifier(rt: &mut Runtime<Keys>, code: Code, pressed: bool, mods: Mods) -> bool {
        let redraw = rt.physical_with_fallback(PhysicalInput::Key { key: code, mods, pressed, repeat: false }, None);
        rt.frame(0.0);
        redraw
    }
    fn flags(rt: &mut Runtime<Keys>, mods: Mods) -> bool {
        let redraw = rt.physical_with_fallback(PhysicalInput::Modifiers(mods), None);
        rt.frame(0.0);
        redraw
    }
    fn modifiers(log: &[KeyEvent]) -> Vec<(Modifier, bool)> {
        log.iter().filter_map(|ev| match ev.key { Key::Modifier { modifier, pressed } => Some((modifier, pressed)), _ => None }).collect()
    }
    const SHIFT: Mods = Mods { shift: true, ctrl: false, alt: false, logo: false, cmd: false };
    const ALT: Mods = Mods { shift: false, ctrl: false, alt: true, logo: false, cmd: false };

    #[test]
    fn shift_alone_sends_press_and_release_with_side_and_mods_after_the_change() {
        let mut rt = runtime(true);
        assert!(modifier(&mut rt, Code::ShiftLeft, true, Mods::default()), "a press needs a frame");
        assert!(!flags(&mut rt, SHIFT), "the flags of a press already sent add nothing");
        assert!(modifier(&mut rt, Code::ShiftLeft, false, SHIFT));
        assert!(!flags(&mut rt, Mods::default()));
        assert_eq!(modifiers(&rt.state.app), [(Modifier::ShiftLeft, true), (Modifier::ShiftLeft, false)]);
        assert_eq!((rt.state.app[0].mods, rt.state.app[1].mods), (SHIFT, Mods::default()));
        assert_eq!(Modifier::ShiftLeft.side(), Some(KeySide::Left));
        assert!(!rt.keyboard_focus, "a modifier tap shows no focus ring");
    }

    #[test]
    fn right_option_is_told_apart_from_left_option() {
        let mut rt = runtime(true);
        modifier(&mut rt, Code::AltRight, true, Mods::default());
        modifier(&mut rt, Code::AltLeft, true, ALT);
        modifier(&mut rt, Code::AltRight, false, ALT);
        modifier(&mut rt, Code::AltLeft, false, ALT);
        assert_eq!(modifiers(&rt.state.app), [(Modifier::AltRight, true), (Modifier::AltLeft, true),
            (Modifier::AltRight, false), (Modifier::AltLeft, false)]);
        let alt: Vec<bool> = rt.state.app.iter().map(|ev| ev.mods.alt).collect();
        assert_eq!(alt, [true, true, true, false], "Option stays on while the other side is down");
        assert_eq!((Modifier::AltRight.side(), Modifier::Fn.side()), (Some(KeySide::Right), None));
    }

    #[test]
    fn a_held_modifier_then_a_letter_is_the_usual_chord() {
        let mut rt = runtime(true);
        let cmd = Mods { cmd: true, ..Mods::default() };
        let code = if cfg!(target_os = "macos") { Code::SuperLeft } else { Code::ControlLeft };
        modifier(&mut rt, code, true, Mods::default());
        assert_eq!(rt.state.app[0].mods.cmd, true);
        assert_eq!(rt.state.chords, 0, "the modifier alone is no chord");
        rt.physical_with_fallback(PhysicalInput::Key { key: Code::KeyK, mods: cmd, pressed: true, repeat: false },
            Some(KeyEvent { key: Key::Command('k'), mods: cmd }));
        rt.frame(0.0);
        assert_eq!(rt.state.app[1], KeyEvent { key: Key::Command('k'), mods: cmd });
        assert_eq!(rt.state.chords, 1);
        // Shift held types capitals in a field as before.
        click(&mut rt, "field");
        modifier(&mut rt, Code::ShiftRight, true, Mods::default());
        rt.physical_with_fallback(PhysicalInput::Key { key: Code::KeyA, mods: SHIFT, pressed: true, repeat: false },
            Some(KeyEvent { key: Key::Text("A".into()), mods: SHIFT }));
        rt.frame(0.0);
        assert_eq!(rt.state.edit.text, "A");
    }

    #[test]
    fn modifier_keys_never_fire_chords_insert_text_or_repeat() {
        let mut rt = runtime(true);
        click(&mut rt, "field");
        rt.state.app.clear();
        for code in [Code::SuperLeft, Code::ControlLeft, Code::AltLeft, Code::ShiftLeft, Code::Fn] {
            modifier(&mut rt, code, true, Mods::default());
            rt.physical_with_fallback(PhysicalInput::Key { key: code, mods: Mods::default(), pressed: true, repeat: true }, None);
            rt.frame(0.0);
            modifier(&mut rt, code, false, Mods::default());
        }
        assert_eq!(rt.state.chords, 0);
        assert_eq!(rt.state.edit.text, "");
        assert_eq!(rt.state.app.len(), 10, "one press and one release each, no repeats");
        assert!(rt.state.app.iter().all(|ev| matches!(ev.key, Key::Modifier { .. })));
        // A KeyEvent sent straight to a field's editor changes nothing either.
        let ev = KeyEvent { key: Key::Modifier { modifier: Modifier::ShiftLeft, pressed: true }, mods: SHIFT };
        assert_eq!(rt.state.edit.key(&ev, None), crate::edit::EditOutcome::default());
    }

    #[test]
    fn a_focused_field_hears_modifiers_before_the_window_unless_nothing_is_focused() {
        let mut rt = runtime(true);
        modifier(&mut rt, Code::ShiftLeft, true, Mods::default());
        assert_eq!((rt.state.app.len(), rt.state.field.len()), (1, 0), "nothing focused: the window's on_key");
        modifier(&mut rt, Code::ShiftLeft, false, SHIFT);
        click(&mut rt, "field");
        rt.state.app.clear();
        rt.state.field_takes = true;
        modifier(&mut rt, Code::ControlRight, true, Mods::default());
        assert_eq!((rt.state.app.len(), rt.state.field.len()), (0, 1), "the field took it");
        rt.state.field_takes = false;
        modifier(&mut rt, Code::ControlRight, false, Mods { ctrl: true, ..Mods::default() });
        assert_eq!((rt.state.app.len(), rt.state.field.len()), (1, 2), "the field passed it on");
        assert_eq!(rt.state.app[0].key, Key::Modifier { modifier: Modifier::ControlRight, pressed: false });
    }

    #[test]
    fn off_by_default_existing_handlers_see_no_modifier_keys() {
        let mut rt = runtime(false);
        assert!(!rt.modifier_keys());
        modifier(&mut rt, Code::ShiftLeft, true, Mods::default());
        assert!(!modifier(&mut rt, Code::ShiftLeft, false, SHIFT), "an unused release needs no frame");
        assert!(!flags(&mut rt, Mods::default()));
        rt.physical_input(PhysicalInput::FocusLost);
        rt.frame(0.0);
        assert!(rt.state.app.is_empty());
    }

    #[test]
    fn a_key_up_the_platform_never_sent_comes_from_the_flags_or_focus_loss() {
        let mut rt = runtime(true);
        let both = Mods { shift: true, alt: true, ..Mods::default() };
        modifier(&mut rt, Code::ShiftLeft, true, Mods::default());
        flags(&mut rt, SHIFT);
        modifier(&mut rt, Code::AltRight, true, SHIFT);
        flags(&mut rt, both);
        modifier(&mut rt, Code::Fn, true, both);
        assert!(flags(&mut rt, ALT), "Shift went off without its key-up");
        assert_eq!(modifiers(&rt.state.app)[3..], [(Modifier::ShiftLeft, false)]);
        rt.physical_input(PhysicalInput::FocusLost);
        rt.frame(0.0);
        assert_eq!(modifiers(&rt.state.app)[4..], [(Modifier::AltRight, false), (Modifier::Fn, false)]);
        assert!(rt.state.app[4..].iter().all(|ev| ev.mods == Mods::default()));
        assert!(!modifier(&mut rt, Code::AltRight, false, Mods::default()), "a late key-up is not sent twice");
        assert_eq!(rt.state.app.len(), 6);
    }

    #[test]
    fn altgr_held_without_the_alt_flag_stays_down_until_its_key_up() {
        // X11 and Wayland report AltGr (Right Alt on many layouts) as Level 3, not `alt`.
        let mut rt = runtime(true);
        modifier(&mut rt, Code::AltRight, true, Mods::default());
        flags(&mut rt, Mods::default());
        flags(&mut rt, SHIFT);
        assert_eq!(modifiers(&rt.state.app), [(Modifier::AltRight, true)]);
        modifier(&mut rt, Code::AltRight, false, SHIFT);
        assert_eq!(modifiers(&rt.state.app), [(Modifier::AltRight, true), (Modifier::AltRight, false)]);
    }
}

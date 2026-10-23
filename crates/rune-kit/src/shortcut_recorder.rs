//! A physical shortcut editor. The app owns registration and can reject a proposed change.
use std::rc::Rc;
use rune::{El, Mods, PhysicalInput, PhysicalKeyCode, Shortcut, ShortcutError, Tx, col, row, text};
use crate::{Keybind, Kit};

#[derive(Clone, Debug, Default)]
pub struct ShortcutRecorderState {
    shortcut: Option<Shortcut>,
    recording: bool,
    held: Mods,
    error: Option<ShortcutError>,
    // Swallow auto-repeat until the captured/cancelled key is released.
    finishing: Option<PhysicalKeyCode>,
}

impl ShortcutRecorderState {
    pub fn new(shortcut: Option<Shortcut>) -> Self { Self { shortcut, ..Self::default() } }
    pub fn shortcut(&self) -> Option<&Shortcut> { self.shortcut.as_ref() }
    pub fn is_recording(&self) -> bool { self.recording }
    pub fn error(&self) -> Option<&ShortcutError> { self.error.as_ref() }
    pub fn begin(&mut self) { self.recording = true; self.error = None; self.finishing = None; }
    pub fn cancel(&mut self) { self.recording = false; self.held = Mods::default(); self.error = None; }
    pub fn keycaps(&self) -> Vec<String> {
        if self.recording { rune::modifier_keycaps(self.held) }
        else { self.shortcut.as_ref().map(Shortcut::keycaps).unwrap_or_default() }
    }

    fn input(&mut self, event: &PhysicalInput) -> Outcome {
        match *event {
            PhysicalInput::FocusLost => {
                let active = self.recording || self.finishing.is_some();
                self.cancel(); self.finishing = None;
                if active { Outcome::Consumed } else { Outcome::Pass }
            }
            PhysicalInput::Modifiers(mods) => {
                self.held = mods;
                if self.recording { Outcome::Consumed } else { Outcome::Pass }
            }
            PhysicalInput::Key { key, mods, pressed, repeat } => {
                self.held = mods;
                if self.finishing == Some(key) {
                    if !pressed { self.finishing = None; }
                    return Outcome::Consumed;
                }
                if !self.recording { return Outcome::Pass; }
                if !pressed || repeat || is_modifier(key) { return Outcome::Consumed; }
                self.finishing = Some(key);
                if key == PhysicalKeyCode::Escape { self.cancel(); return Outcome::Consumed; }
                if key == PhysicalKeyCode::Backspace { return Outcome::Change(None); }
                match Shortcut::from_physical_key(key, mods) {
                    Ok(shortcut) => Outcome::Change(Some(shortcut)),
                    Err(error) => { self.error = Some(error); Outcome::Consumed }
                }
            }
        }
    }
}

enum Outcome { Pass, Consumed, Change(Option<Shortcut>) }
fn is_modifier(key: PhysicalKeyCode) -> bool {
    use PhysicalKeyCode::*;
    matches!(key, ShiftLeft | ShiftRight | ControlLeft | ControlRight | AltLeft | AltRight | SuperLeft | SuperRight | Fn | FnLock)
}
type Access<S> = Rc<dyn for<'a> Fn(&'a mut S) -> &'a mut ShortcutRecorderState>;
type Change<S> = Rc<dyn Fn(&mut S, Option<&Shortcut>, &mut Tx) -> Result<(), ShortcutError>>;

/// Click (or activate with Enter/Space) to record. Escape cancels; Backspace clears.
/// `on_change` must register the proposed binding before releasing the previous registration:
/// returning an error keeps the old value and displays the error. An unchanged chord is a no-op.
pub struct ShortcutRecorder<S> {
    key: String,
    state: ShortcutRecorderState,
    access: Access<S>,
    change: Option<Change<S>>,
    width: f32,
    disabled: bool,
}
impl<S: 'static> ShortcutRecorder<S> {
    pub fn new(key: impl Into<String>, state: &ShortcutRecorderState,
        access: impl for<'a> Fn(&'a mut S) -> &'a mut ShortcutRecorderState + 'static) -> Self {
        Self { key: key.into(), state: state.clone(), access: Rc::new(access), change: None, width: 240.0, disabled: false }
    }
    pub fn on_change(mut self, change: impl Fn(&mut S, Option<&Shortcut>, &mut Tx) -> Result<(), ShortcutError> + 'static) -> Self {
        self.change = Some(Rc::new(change)); self
    }
    pub fn width(mut self, width: f32) -> Self { self.width = width; self }
    pub fn disabled(mut self, disabled: bool) -> Self { self.disabled = disabled; self }
    pub fn build(self, k: &Kit) -> El {
        let labels = self.state.keycaps();
        let placeholder = if self.state.recording { "Press shortcut" } else { "Record shortcut" };
        let border = if self.state.error.is_some() { k.c.danger.border }
            else if self.state.recording { k.c.border_focus } else { k.c.border_base };
        let mut control = row().key(&self.key).role("button")
            .label(if labels.is_empty() { placeholder.into() } else { labels.join(" ") })
            .w(self.width).h(32.0).px(8.0).center_y().radius(k.radius.md)
            .fill(k.c.bg_base).border(1.0, border).focusable(!self.disabled)
            .focus_ring(2.0, 2.0, k.c.border_focus);
        control = control.child(if labels.is_empty() { text(placeholder).font(k.ty.ui_compact).color(k.c.text_faint) }
            else { Keybind::new(&labels).build(k) });
        if !self.disabled {
            let access = self.access.clone();
            control = control.on_press(move |s: &mut S| access(s).begin());
        }
        {
            let access = self.access;
            let change = self.change;
            let disabled = self.disabled;
            control = control.on_physical_input(move |s: &mut S, event, tx| {
                if disabled && !matches!(event, PhysicalInput::FocusLost) { return false; }
                match access(s).input(event) {
                    Outcome::Pass => false,
                    Outcome::Consumed => true,
                    Outcome::Change(next) => {
                        let result = if access(s).shortcut == next { Ok(()) }
                            else { change.as_ref().map_or(Ok(()), |f| f(s, next.as_ref(), tx)) };
                        let state = access(s);
                        match result {
                            Ok(()) => { state.shortcut = next; state.error = None; state.cancel(); }
                            Err(error) => { state.error = Some(error); }
                        }
                        true
                    }
                }
            });
        }
        let mut root = col().key(format!("{}-field", self.key)).gap(4.0).child(control);
        if let Some(error) = self.state.error {
            root = root.child(text(error.to_string()).font(k.ty.small_medium).color(k.c.danger.fg));
        }
        root
    }
}

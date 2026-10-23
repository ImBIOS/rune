//! `cargo run -p rune-kit --example shortcut_recorder`
//! Ctrl-Shift-Space is reserved here to demonstrate real registration conflicts.
use rune::{App, AppHandle, AppSpec, Cx, El, Shortcut, ShortcutError, ShortcutId, Tx, col, text};
use rune_kit::{Kit, ShortcutRecorder, ShortcutRecorderState};

#[derive(Default)]
struct State {
    recorder: ShortcutRecorderState,
    handle: Option<AppHandle<State>>,
    registration: Option<ShortcutId>,
    activations: usize,
}
fn change(s: &mut State, next: Option<&Shortcut>, _: &mut Tx) -> Result<(), ShortcutError> {
    let handle = s.handle.as_ref().ok_or(ShortcutError::NotRunning)?;
    let new = next.map(|shortcut| handle.register_shortcut(shortcut.clone(), |s, _| s.activations += 1)).transpose()?;
    if let Some(old) = s.registration
        && let Err(error) = handle.unregister_shortcut(old) {
        if let Some(new) = new { handle.unregister_shortcut(new)?; }
        return Err(error);
    }
    s.registration = new;
    Ok(())
}
fn view(s: &State, _: &mut Cx<State>) -> El {
    let k = Kit::neutral(true);
    col().padding(24.0).gap(16.0).w_full().h_full().fill(k.c.page)
        .child(text("Launcher shortcut").font(k.ty.ui_compact).color(k.c.text_base))
        .child(ShortcutRecorder::new("shortcut", &s.recorder, |s: &mut State| &mut s.recorder).on_change(change).build(&k))
        .child(text(format!("Activations: {}", s.activations)).font(k.ty.ui_compact).color(k.c.text_muted))
}
fn main() {
    if std::env::args().any(|a| a == "--labels") {
        for key in ["super+q", "ctrl+slash", "shift+space"] {
            let shortcut: Shortcut = key.parse().unwrap();
            println!("{} {:?} {:?}", shortcut, shortcut.physical_key(), shortcut.keycaps());
        }
        return;
    }
    let app = App::new(AppSpec::new("Shortcut recorder", (360.0, 180.0), State::default, view))
        .global_shortcut("ctrl+shift+space", |_, _| {});
    let handle = app.handle();
    let shared = handle.clone();
    handle.update(move |s, _| s.handle = Some(shared));
    app.run();
}

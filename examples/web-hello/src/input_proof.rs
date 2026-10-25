//! Browser input regression fixture, selected only by ?input-proof.
use rune::{App, AppHandle, AppSpec, Cx, El, Key, TextEdit, col, el, text, text_input, tok};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = "
export function enabled() { return new URLSearchParams(location.search).has('input-proof'); }
export function report(plain, display, single, secretLength, submits, shortcuts, visible) {
    window.__inputProof = { plain, display, single, secretLength, submits, shortcuts, visible,
        polls: (window.__inputProof?.polls ?? 0) + 1 };
}
")]
extern "C" {
    pub fn enabled() -> bool;
    fn report(plain: &str, display: &str, single: &str, secret_length: usize, submits: u32, shortcuts: u32, visible: bool);
}

struct State {
    plain: TextEdit,
    single: TextEdit,
    secret: TextEdit,
    submits: u32,
    shortcuts: u32,
    handle: Option<AppHandle<State>>,
}

impl Default for State {
    fn default() -> Self {
        Self { plain: TextEdit::default(), single: TextEdit::default(), secret: TextEdit::default().secure(),
            submits: 0, shortcuts: 0, handle: None }
    }
}

fn view(s: &State, _: &mut Cx<State>) -> El {
    let field = |edit: &TextEdit| text_input(edit, tok::type_::BODY).w_full().h(48.0);
    col().w_full().h_full().padding(16.0).gap(8.0)
        .child(field(&s.plain).key("plain").on_edit(|s: &mut State| &mut s.plain)
            .on_submit(|s: &mut State, _| s.submits += 1))
        .child(field(&s.single).key("single").single_line().on_edit(|s: &mut State| &mut s.single)
            .on_submit(|s: &mut State, _| s.submits += 1))
        .child(field(&s.secret).key("secret").single_line().on_edit(|s: &mut State| &mut s.secret)
            .on_submit(|s: &mut State, _| s.submits += 1))
        .child(el().key("button").focusable(true).w_full().h(48.0).child(text("Button"))
            .on_press(|s: &mut State| s.submits += 1))
}

pub fn run() {
    let mut spec = AppSpec::new("Input proof", (390.0, 760.0), State::default, view);
    spec.poll = Some(|s, _, _| {
        report(&s.plain.text, &s.plain.display().0, &s.single.text, s.secret.text.chars().count(),
            s.submits, s.shortcuts, s.handle.as_ref().is_some_and(AppHandle::is_visible));
        false
    });
    spec.on_key = Some(|s, event, _| {
        if event.key == Key::Command('k') { s.shortcuts += 1; true } else { false }
    });
    let app = App::new(spec);
    let handle = app.handle();
    let state_handle = handle.clone();
    handle.update(move |s, _| s.handle = Some(state_handle));
    handle.show();
    handle.set_position((0, 0)); // Must warn, never cover the canvas with an error page.
    app.run();
}

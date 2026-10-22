use rune::{Cx, El, Env, Ime, Key, KeyEvent, Mods, Runtime};
use rune_kit::{Combobox, ComboboxOption, ComboboxState, Kit};

#[derive(Default)]
struct State { combo: ComboboxState, chosen: Option<String> }
fn view(s: &State, _: &mut Cx<State>) -> El {
    let options: Vec<_> = (0..200).map(|i| {
        let mut o = ComboboxOption::new(format!("item-{i}"), format!("Model {i:03}"));
        o.disabled = i == 1;
        o
    }).collect();
    let mut layer = Vec::new();
    let body = Combobox::new("models", &s.combo, &options, |s: &mut State| &mut s.combo)
        .on_choose(|s, id, _| s.chosen = Some(id.into())).build(&Kit::neutral(true), &mut layer);
    rune_kit::root(body, layer)
}
fn runtime() -> Runtime<State> {
    let mut s = State::default();
    s.combo.open = true;
    s.combo.search.focused = true;
    let mut rt = Runtime::new(s, view, (400.0, 400.0), Env { system_dark: true, scale: 1.0 });
    rt.frame(0.0);
    rt
}
fn key(rt: &mut Runtime<State>, key: Key, mods: Mods) {
    rt.key(KeyEvent { key, mods }); rt.frame(0.0);
}

#[test]
fn combobox_loops_skips_disabled_and_scrolls_virtual_rows_into_view() {
    let mut rt = runtime();
    assert!(rt.elements().iter().filter(|e| e.role == "option").count() < 50);
    key(&mut rt, Key::Down, Mods::default());
    assert_eq!(rt.state.combo.active.as_deref(), Some("item-2"));
    key(&mut rt, Key::Up, Mods::default());
    key(&mut rt, Key::Up, Mods::default());
    assert_eq!(rt.state.combo.active.as_deref(), Some("item-199"));
    let last = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("models-option-item-199")).unwrap();
    assert!(last.visible[3] > 0.0);
    key(&mut rt, Key::Enter, Mods::default());
    assert_eq!(rt.state.chosen.as_deref(), Some("item-199"));
    assert!(!rt.state.combo.open);
}

#[test]
fn combobox_filters_from_scroll_end_preserves_editor_modifiers_and_ime() {
    let mut rt = runtime();
    key(&mut rt, Key::Up, Mods::default());
    key(&mut rt, Key::Text("099".into()), Mods::default());
    assert!(rt.elements().iter().any(|e| e.key.as_deref() == Some("models-option-item-99")));
    assert_eq!(rt.state.combo.search.text, "099");
    key(&mut rt, Key::Down, Mods { alt: true, ..Mods::default() });
    assert!(rt.state.combo.active.is_none());
    rt.ime(Ime::Preedit("x".into(), None)); rt.frame(0.0);
    key(&mut rt, Key::Enter, Mods::default());
    assert!(rt.state.combo.open && rt.state.chosen.is_none());
    rt.ime(Ime::Preedit(String::new(), None)); rt.frame(0.0);
    key(&mut rt, Key::Escape, Mods::default());
    assert!(!rt.state.combo.open);
}

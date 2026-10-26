//! `El::inert`: a subtree that draws and lays out while the pointer and keyboard pass through.
use rune::runtime::ElementInfo;
use rune::tok::TextStyle;
use rune::{Color, Cursor, Cx, El, Env, Key, KeyEvent, Mods, Runtime, TextEdit, el, text_input};

#[derive(Default)]
struct S {
    inert: bool,
    log: Vec<&'static str>,
    hover: Vec<(&'static str, bool)>,
    rects: Vec<[f32; 4]>,
    field: TextEdit,
}

/// A canvas that takes presses and shows a crosshair, with a document drawn over it whose
/// own elements would take presses, hovers and focus.
fn view(s: &S, _: &mut Cx<S>) -> El {
    let doc = el().key("doc").absolute(0.0, 0.0).size(100.0, 100.0).fill(Color::WHITE)
        .on_layout(|s: &mut S, r| s.rects.push(r))
        .child(el().key("button").size(40.0, 40.0).fill(Color::BLACK).cursor(Cursor::Pointer)
            .focusable(true)
            .on_press(|s: &mut S| s.log.push("button"))
            .on_hover_change(|s: &mut S, inside, _| s.hover.push(("button", inside))))
        .child(text_input(&s.field, TextStyle::new(14.0, 400.0)).key("field").w(60.0)
            .on_edit(|s: &mut S| &mut s.field));
    el().key("canvas").size(200.0, 200.0).cursor(Cursor::Crosshair)
        .on_press(|s: &mut S| s.log.push("canvas"))
        .on_hover_change(|s: &mut S, inside, _| s.hover.push(("canvas", inside)))
        .child(if s.inert { doc.inert() } else { doc })
}

fn info(rt: &Runtime<S>, key: &str) -> ElementInfo {
    rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap_or_else(|| panic!("{key}"))
}

fn runtime(inert: bool) -> Runtime<S> {
    let mut rt = Runtime::new(S { inert, ..S::default() }, view, (200.0, 200.0), Env::default());
    rt.frame(0.0);
    rt
}

fn click(rt: &mut Runtime<S>, x: f32, y: f32) {
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(1.0 / 60.0);
}

fn tab(rt: &mut Runtime<S>) {
    rt.key(KeyEvent { key: Key::Tab, mods: Mods::default() });
    rt.frame(1.0 / 60.0);
}

#[test]
fn presses_hovers_and_cursors_fall_through_to_what_is_under() {
    let mut rt = runtime(false);
    click(&mut rt, 20.0, 20.0);
    assert_eq!(rt.state.log, ["button"], "without inert, the document takes the press");
    assert_eq!(rt.cursor(), Cursor::Pointer);

    let mut rt = runtime(true);
    click(&mut rt, 20.0, 20.0);
    assert_eq!(rt.state.log, ["canvas"], "the press goes to the canvas under the document");
    assert_eq!(rt.cursor(), Cursor::Crosshair, "with the canvas's cursor");
    assert!(rt.state.hover.contains(&("canvas", true)));
    assert!(!rt.state.hover.iter().any(|(k, _)| *k == "button"), "the document never hovers");
    assert_eq!(info(&rt, "button").hover, 0.0);
}

#[test]
fn it_still_draws_and_lays_out() {
    let mut rt = runtime(true);
    rt.frame(1.0 / 60.0);
    let doc = info(&rt, "doc");
    assert_eq!(doc.rect, [0.0, 0.0, 100.0, 100.0]);
    assert!(doc.opacity > 0.99);
    assert!(rt.instances.iter().any(|i| i.rect[2] == 100.0 && i.rect[3] == 100.0), "the document's fill draws");
    assert_eq!(rt.state.rects.last(), Some(&[0.0, 0.0, 100.0, 100.0]), "on_layout still hears its rect");
    assert!(doc.inert && info(&rt, "button").inert, "accessibility reports the subtree inert");
    assert!(!info(&runtime(false), "button").inert);
}

#[test]
fn nothing_inside_takes_or_keeps_focus() {
    let mut rt = runtime(false);
    tab(&mut rt);
    tab(&mut rt);
    assert!(info(&rt, "field").focused || info(&rt, "button").focused, "the document is focusable");

    let mut rt = runtime(true);
    rt.set_full_keyboard_access(true);
    tab(&mut rt);
    assert!(!info(&rt, "button").focused && !info(&rt, "field").focused, "Tab skips the inert subtree");

    // A field inside asking for focus doesn't get it.
    let mut rt = runtime(true);
    rt.update(|s, _| s.field.focused = true);
    rt.frame(1.0 / 60.0);
    assert!(!info(&rt, "field").focused);
    assert!(!rt.state.field.focused, "and the model hears it didn't");

    // Focus inside drops when the subtree turns inert.
    let mut rt = runtime(false);
    rt.update(|s, _| s.field.focused = true);
    rt.frame(1.0 / 60.0);
    assert!(info(&rt, "field").focused);
    rt.update(|s, _| s.inert = true);
    rt.frame(1.0 / 60.0);
    assert!(!info(&rt, "field").focused);
    assert!(!rt.state.field.focused);
}

//! The text selection from code: `Tx::select_text`, `Tx::select_all_text` and
//! `Tx::clear_text_selection` set it after the handler's commit (in time for a key in the same
//! frame), `Tx::text_selection` reads it, and `El::selection_copy` decides what copying takes.
use std::cell::RefCell;
use std::ops::Range;

use rune::{Cx, El, Env, Key, KeyEvent, Mods, Runtime, TextEdit, TextSelection, TextStyle, col, row, text, text_input};

#[derive(Default)]
struct S { extra: bool, seen: Option<TextSelection>, field: TextEdit, app_keys: Vec<char> }

thread_local! {
    /// What the `selection_copy` provider saw last: (path, range, whole).
    static ASKED: RefCell<Vec<(String, Range<usize>, bool)>> = const { RefCell::new(Vec::new()) };
}

fn view(s: &S, _: &mut Cx<S>) -> El {
    let style = TextStyle::new(14.0, 400.0).lh(20.0);
    let t = |content: &str, key: &str| text(content.to_owned()).key(key).font(style).w_full().selectable(true);
    col().w_full().h_full().padding(10.0).gap(12.0)
        .child(col().key("doc").w(400.0).gap(12.0).selection_group(true)
            .selection_copy(|sel| {
                ASKED.with_borrow_mut(|a| *a = sel.texts().iter().map(|t| (t.path().to_owned(), t.range(), t.is_whole())).collect());
                sel.texts().iter().all(|t| t.is_whole()).then(|| "SOURCE".to_owned())
            })
            .child(t("first paragraph", "a"))
            .child(row().key("r").w_full().child(text("•").font(style).w(20.0)).child(t("second", "b").grow(1.0).shrink())))
        .child(t("lone text here", "lone").w(300.0))
        .child(t("café ok", "cafe").w(300.0))
        .children(s.extra.then(|| t("extra words", "extra").w(300.0)))
        .child(text_input(&s.field, style).single_line().key("field").w(300.0).on_edit(|s: &mut S| &mut s.field))
}

fn runtime() -> Runtime<S> {
    let mut rt = Runtime::new(S::default(), view, (500.0, 500.0), Env::new(true, 2.0));
    rt.frame(0.0);
    rt
}

fn tick(rt: &mut Runtime<S>) { rt.frame(1.0 / 60.0); }

fn command(rt: &mut Runtime<S>, c: char) {
    rt.key(KeyEvent { key: Key::Command(c), mods: Mods { cmd: true, ..Mods::default() } });
    tick(rt);
}

fn act(rt: &mut Runtime<S>, f: impl FnOnce(&mut rune::Tx)) {
    rt.update(|_, tx| f(tx));
    tick(rt);
}

#[test]
fn code_selects_a_range_reads_it_and_clears_it() {
    let mut rt = runtime();
    act(&mut rt, |tx| tx.select_text("lone", 5..9));
    assert_eq!(rt.selected_text().as_deref(), Some("text"));
    rt.update(|s, tx| s.seen = tx.text_selection().cloned());
    let seen = rt.state.seen.take().expect("a handler reads the selection");
    assert_eq!(seen.text(), "text");
    let [t] = seen.texts() else { panic!("{seen:?}") };
    assert!(t.path().ends_with("/lone") && t.key() == Some("lone"), "{t:?}");
    assert_eq!((t.range(), t.len(), t.text(), t.is_whole()), (5..9, 14, "text", false));

    // Clamped to the text and its character boundaries.
    act(&mut rt, |tx| tx.select_text("lone", 10..999));
    assert_eq!(rt.selected_text().as_deref(), Some("here"));
    act(&mut rt, |tx| tx.select_text("cafe", 0..4));
    assert_eq!(rt.selected_text().as_deref(), Some("caf"), "byte 4 is inside the é");
    // A key that names no selectable text leaves it be.
    act(&mut rt, |tx| tx.select_text("r", 0..2));
    act(&mut rt, |tx| tx.select_text("nothing", 0..2));
    assert_eq!(rt.selected_text().as_deref(), Some("caf"));

    act(&mut rt, |tx| tx.clear_text_selection());
    assert_eq!(rt.selected_text(), None);
    rt.update(|s, tx| s.seen = tx.text_selection().cloned());
    assert_eq!(rt.state.seen, None);
}

#[test]
fn select_all_takes_a_group_a_part_of_one_or_a_text() {
    let mut rt = runtime();
    act(&mut rt, |tx| tx.select_all_text("doc"));
    assert_eq!(rt.selected_text().as_deref(), Some("first paragraph\nsecond"));
    act(&mut rt, |tx| tx.select_all_text("r"));
    assert_eq!(rt.selected_text().as_deref(), Some("second"), "the texts inside a part of the group");
    act(&mut rt, |tx| tx.select_all_text("lone"));
    assert_eq!(rt.selected_text().as_deref(), Some("lone text here"));
    // Then Cmd+A widens a text's selection to its group, as after a click.
    act(&mut rt, |tx| tx.select_text("a", 0..5));
    command(&mut rt, 'a');
    assert_eq!(rt.selected_text().as_deref(), Some("first paragraph\nsecond"));
}

#[test]
fn a_text_the_same_change_builds_selects_and_keeps_its_selection_across_rebuilds() {
    let mut rt = runtime();
    rt.update(|s, tx| {
        s.extra = true;
        tx.select_all_text("extra");
    });
    tick(&mut rt);
    assert_eq!(rt.selected_text().as_deref(), Some("extra words"));
    for _ in 0..3 {
        rt.invalidate();
        tick(&mut rt);
    }
    assert_eq!(rt.selected_text().as_deref(), Some("extra words"));
}

#[test]
fn a_key_in_the_same_frame_copies_what_code_selected() {
    let mut rt = runtime();
    let clipboard = rune::clipboard::memory();
    rt.use_clipboard(&clipboard);
    rt.update(|_, tx| tx.select_text("lone", 0..4));
    command(&mut rt, 'c');
    assert_eq!(clipboard.text().as_deref().map(String::as_str), Some("lone"));
}

#[test]
fn selection_copy_says_what_copying_takes() {
    let mut rt = runtime();
    let clipboard = rune::clipboard::memory();
    rt.use_clipboard(&clipboard);
    act(&mut rt, |tx| tx.select_all_text("doc"));
    command(&mut rt, 'c');
    assert_eq!(clipboard.text().as_deref().map(String::as_str), Some("SOURCE"));
    assert_eq!(ASKED.take(), [("a".to_owned(), 0..15, true), ("r/b".to_owned(), 0..6, true)], "paths relative to the element");
    rt.update(|s, tx| s.seen = tx.text_selection().cloned());
    assert_eq!(rt.state.seen.as_ref().map(TextSelection::text), Some("first paragraph\nsecond"), "reading is as drawn");

    // It returns None for part of the group: the drawn text, and it sees what isn't selected.
    act(&mut rt, |tx| tx.select_text("b", 0..3));
    command(&mut rt, 'c');
    assert_eq!(clipboard.text().as_deref().map(String::as_str), Some("sec"));
    assert_eq!(ASKED.take(), [("a".to_owned(), 0..0, false), ("r/b".to_owned(), 0..3, false)]);

    // The Copy menu asks it too.
    act(&mut rt, |tx| tx.select_all_text("doc"));
    let a = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("a")).unwrap();
    let (x, y) = (a.rect[0] + 20.0, a.rect[1] + a.rect[3] / 2.0);
    rt.pointer_move(x, y);
    rt.secondary_down(x, y);
    rt.secondary_up(x, y);
    tick(&mut rt);
    rt.resolve_popup(Some("copy")).unwrap();
    tick(&mut rt);
    assert_eq!(clipboard.text().as_deref().map(String::as_str), Some("SOURCE"));

    // A text outside it copies as drawn.
    ASKED.take();
    act(&mut rt, |tx| tx.select_all_text("lone"));
    command(&mut rt, 'c');
    assert_eq!(clipboard.text().as_deref().map(String::as_str), Some("lone text here"));
    assert!(ASKED.take().is_empty());
}

/// An app's `on_key` that takes Cmd+A and Cmd+C copies the note its own way.
fn app_takes_copy_keys(rt: &mut Runtime<S>) {
    rt.on_key = Some(|s, ev, tx| match ev.key {
        Key::Command(c @ ('a' | 'c')) => {
            s.app_keys.push(c);
            if c == 'c' { tx.copy("APP"); }
            true
        }
        _ => false,
    });
}

#[test]
fn an_apps_on_key_takes_the_copy_keys_before_the_selection() {
    let mut rt = runtime();
    let clipboard = rune::clipboard::memory();
    rt.use_clipboard(&clipboard);
    app_takes_copy_keys(&mut rt);
    act(&mut rt, |tx| tx.select_text("lone", 0..4));
    command(&mut rt, 'c');
    assert_eq!(clipboard.text().as_deref().map(String::as_str), Some("APP"));
    command(&mut rt, 'a');
    assert_eq!(rt.selected_text().as_deref(), Some("lone"), "Cmd+A was the app's");
    assert_eq!(rt.state.app_keys, ['c', 'a']);

    // An app that passes them on leaves them to the selection.
    rt.on_key = Some(|s, ev, _| { if let Key::Command(c) = ev.key { s.app_keys.push(c); } false });
    command(&mut rt, 'a');
    assert_eq!(rt.selected_text().as_deref(), Some("lone text here"));
    command(&mut rt, 'c');
    assert_eq!(clipboard.text().as_deref().map(String::as_str), Some("lone text here"));
    assert_eq!(rt.state.app_keys, ['c', 'a', 'a', 'c'], "the app saw them first");
}

#[test]
fn a_focused_field_keeps_its_copy_keys_from_the_app() {
    let mut rt = runtime();
    let clipboard = rune::clipboard::memory();
    rt.use_clipboard(&clipboard);
    app_takes_copy_keys(&mut rt);
    rt.state.field = TextEdit::new("field words");
    rt.state.field.focused = true;
    rt.invalidate();
    tick(&mut rt);
    command(&mut rt, 'a');
    command(&mut rt, 'c');
    assert_eq!(clipboard.text().as_deref().map(String::as_str), Some("field words"));
    assert!(rt.state.app_keys.is_empty(), "{:?}", rt.state.app_keys);
}

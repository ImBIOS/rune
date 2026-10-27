//! `Markdown::copy_source`: copying a selection copies the Markdown source of the top-level
//! blocks it touches, the whole source verbatim for all of them, and the drawn text for part
//! of one block.
#![cfg(feature = "markdown")]

use rune::{Cx, El, Env, Key, KeyEvent, Mods, Runtime, col};
use rune_kit::{Kit, Markdown};

const SRC: &str = "# Title\n\nHello **world** and [link](https://x.dev/a).\n\n- alpha\n- [x] beta\n\n```rust\nfn main() {}\n```\n\nLast *line*.\n";

#[derive(Default)]
struct S { source: bool }

fn view(s: &S, _: &mut Cx<S>) -> El {
    let k = Kit::neutral(true);
    col().w_full().padding(10.0).child(Markdown::new(SRC).selectable(true).copy_source(s.source).build(&k).key("note"))
}

fn runtime(source: bool) -> (Runtime<S>, rune::clipboard::MemoryClipboard) {
    let mut rt = Runtime::new(S { source }, view, (480.0, 800.0), Env::new(true, 2.0));
    rt.frame(0.0);
    let clipboard = rune::clipboard::memory();
    rt.use_clipboard(&clipboard);
    (rt, clipboard)
}

fn tick(rt: &mut Runtime<S>) { rt.frame(1.0 / 60.0); }

fn copy(rt: &mut Runtime<S>, clipboard: &rune::clipboard::MemoryClipboard, f: impl FnOnce(&mut rune::Tx)) -> String {
    rt.update(|_, tx| f(tx));
    tick(rt);
    rt.key(KeyEvent { key: Key::Command('c'), mods: Mods { cmd: true, ..Mods::default() } });
    tick(rt);
    clipboard.text().map(|t| t.to_string()).unwrap_or_default()
}

#[test]
fn copying_takes_the_source_of_whole_blocks() {
    let (mut rt, clipboard) = runtime(true);
    assert_eq!(copy(&mut rt, &clipboard, |tx| tx.select_all_text("note")), SRC, "everything: the source verbatim");
    assert_eq!(rt.selected_text().as_deref().map(|t| t.starts_with("Title\nHello world")), Some(true), "selected as drawn");
    assert_eq!(copy(&mut rt, &clipboard, |tx| tx.select_all_text("note/b1")), "Hello **world** and [link](https://x.dev/a).");
    assert_eq!(copy(&mut rt, &clipboard, |tx| tx.select_all_text("note/b2")), "- alpha\n- [x] beta", "a whole list");
    assert_eq!(copy(&mut rt, &clipboard, |tx| tx.select_all_text("note/b3")), "```rust\nfn main() {}\n```");
    // Part of a paragraph or of a list: as drawn.
    assert_eq!(copy(&mut rt, &clipboard, |tx| tx.select_text("note/b1", 6..11)), "world");
    let alpha = rt.elements().into_iter().find(|e| e.path.ends_with("note/b2/i0/#1/b0")).expect("alpha").path;
    assert_eq!(copy(&mut rt, &clipboard, |tx| tx.select_all_text(alpha)), "alpha");
}

#[test]
fn a_drag_across_blocks_copies_each_whole() {
    let (mut rt, clipboard) = runtime(true);
    let find = |rt: &Runtime<S>, end: &str| rt.elements().into_iter().find(|e| e.path.ends_with(end)).unwrap_or_else(|| panic!("{end}"));
    let (para, last) = (find(&rt, "note/b1"), find(&rt, "note/b4"));
    let (x0, y0) = (para.rect[0] + 30.0, para.rect[1] + para.rect[3] / 2.0);
    let (x1, y1) = (last.rect[0] + 10.0, last.rect[1] + last.rect[3] / 2.0);
    rt.pointer_move(x0, y0);
    rt.pointer_down(x0, y0);
    tick(&mut rt);
    for k in 1..=6 {
        let f = k as f32 / 6.0;
        rt.pointer_move(x0 + (x1 - x0) * f, y0 + (y1 - y0) * f);
        tick(&mut rt);
    }
    rt.pointer_up(x1, y1);
    tick(&mut rt);
    assert!(rt.selected_text().is_some_and(|t| t.contains("alpha") && !t.starts_with("Hello")), "{:?}", rt.selected_text());
    rt.key(KeyEvent { key: Key::Command('c'), mods: Mods { cmd: true, ..Mods::default() } });
    tick(&mut rt);
    assert_eq!(clipboard.text().as_deref().map(String::as_str), Some(&SRC[9..]).map(|s| s.trim_end()));
}

#[test]
fn without_copy_source_copying_takes_the_drawn_text() {
    let (mut rt, clipboard) = runtime(false);
    let all = copy(&mut rt, &clipboard, |tx| tx.select_all_text("note"));
    assert_eq!(all, "Title\nHello world and link.\nalpha\nbeta\nfn main() {}\nLast line.");
}

//! `El::max_lines`: wrapped text stops after n lines with "…" ending the last, measures n lines
//! tall, cuts between graphemes, refits on resize, and copying and tools keep the whole string.
use super::*;
use crate::{Color, Cx, El, Env, Key, KeyEvent, Link, Mods, Span, TextStyle, col, rich};
use unicode_segmentation::UnicodeSegmentation;

const TITLE: &str = "Move the sidebar's search into the toolbar, keep its results in the list, and let the \
    filter chips wrap onto a second row when the window is narrow enough that they would overflow";

fn style() -> TextStyle { TextStyle::new(13.0, 400.0).lh(16.0) }

/// Content, the line limit, and a byte range that is both a coloured span and a link.
type S = (&'static str, usize, Option<(usize, usize)>);

fn mark() -> Color { crate::color::hex("#e6339a").unwrap() }

fn clamp(content: &'static str, lines: usize, marked: Option<(usize, usize)>, width: f32) -> Runtime<S> {
    fn view(s: &S, _: &mut Cx<S>) -> El {
        let spans = s.2.map(|(a, b)| Span { color: Some(mark()), ..Span::new(a, b) }).into_iter().collect();
        let mut t = rich(s.0, spans).key("t").font(style()).selectable(true).max_lines(s.1);
        if let Some((a, b)) = s.2 { t = t.link(Link::new(a..b, "marked")).on_link(|_: &mut S, _, _| {}); }
        col().w_full().child(t)
    }
    let mut rt = Runtime::new((content, lines, marked), view, (width, 400.0), Env::new(true, 2.0));
    rt.frame(0.0);
    rt
}

fn node<'a>(rt: &'a Runtime<S>) -> &'a Node {
    rt.nodes.iter().find(|n| n.key.as_deref() == Some("t")).unwrap()
}

/// The string node "t" draws, its cut, its line count and its height in points.
fn drawn(rt: &Runtime<S>) -> (String, Option<Elision>, usize, f32) {
    let n = node(rt);
    let content = &n.text.as_ref().unwrap().content;
    let shown = match n.elided {
        Some(e) => [&content[..e.head], "…", &content[e.tail..]].concat(),
        None => content.clone(),
    };
    (shown, n.elided, n.text_layout.as_ref().unwrap().len(), n.target[3] - n.target[1])
}

#[test]
fn stops_after_n_lines_with_an_ellipsis_and_measures_n_lines() {
    let mut rt = clamp(TITLE, 3, None, 200.0);
    let (shown, e, lines, h) = drawn(&rt);
    let e = e.expect("the title runs past three lines at 200 pt");
    assert!(shown.ends_with('…') && !shown[..shown.len() - "…".len()].ends_with(' '), "{shown}");
    assert_eq!(e.tail, TITLE.len(), "everything after the cut is hidden");
    assert_eq!((lines, h), (3, 48.0), "three 16 pt lines: {shown}");
    let el = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("t")).unwrap();
    assert_eq!(el.text.as_deref(), Some(TITLE), "tools and accessibility read the whole title");

    rt.set_viewport((320.0, 400.0), 2.0);
    rt.frame(0.1);
    let (wider, _, lines, h) = drawn(&rt);
    assert!(wider.len() > shown.len() && (lines, h) == (3, 48.0), "a wider box keeps more: {wider}");

    rt.set_viewport((2000.0, 400.0), 2.0);
    rt.frame(0.2);
    let (all, e, lines, h) = drawn(&rt);
    assert_eq!((all.as_str(), e, lines, h), (TITLE, None, 1, 16.0), "room for it all: no cut, one line tall");

    rt.set_viewport((200.0, 400.0), 2.0);
    rt.frame(0.3);
    assert_eq!(drawn(&rt).0, shown, "narrowing again cuts as before");
}

#[test]
fn text_within_the_limit_is_untouched() {
    let rt = clamp("Short title", 3, None, 200.0);
    assert_eq!(drawn(&rt), ("Short title".to_owned(), None, 1, 16.0));
    // 0 shows every line.
    let rt = clamp(TITLE, 0, None, 200.0);
    let (shown, e, lines, h) = drawn(&rt);
    assert!(shown == TITLE && e.is_none() && lines > 3 && h == 16.0 * lines as f32);
}

#[test]
fn a_line_break_before_the_cut_is_dropped() {
    let rt = clamp("first\nsecond\nthird", 2, None, 400.0);
    assert_eq!(drawn(&rt).0, "first\nsecond…");
    assert_eq!(drawn(&rt).3, 32.0);
}

#[test]
fn cuts_fall_between_graphemes() {
    const MIXED: &str = "👩‍👩‍👧‍👦 cafe\u{301} שלום עולם 🇯🇵 नमस्ते e\u{301}e\u{301}e\u{301} 👩‍👩‍👧‍👦 done and some more words to wrap 👩‍👩‍👧‍👦";
    let bounds: Vec<usize> = MIXED.grapheme_indices(true).map(|(i, _)| i).chain([MIXED.len()]).collect();
    let mut rt = clamp(MIXED, 2, None, 60.0);
    let mut cuts = 0;
    for w in (60..400).step_by(7) {
        rt.set_viewport((w as f32, 400.0), 2.0);
        rt.frame(1.0);
        let (shown, e, lines, _) = drawn(&rt);
        let Some(e) = e else { continue };
        cuts += 1;
        assert!(lines <= 2, "{w}: {shown}");
        assert!(bounds.contains(&e.head), "{w}: {e:?} splits a grapheme: {shown}");
    }
    assert!(cuts > 10, "most widths cut: {cuts}");
}

#[test]
fn copying_a_clamped_text_copies_all_of_it() {
    let mut rt = clamp(TITLE, 2, None, 200.0);
    let r = node(&rt).target;
    let (x, y) = (r[0] + 20.0, r[1] + 8.0);
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(1.0);
    rt.key(KeyEvent { key: Key::Command('a'), mods: Mods { cmd: true, ..Mods::default() } });
    rt.frame(1.1);
    assert_eq!(rt.selected_text().as_deref(), Some(TITLE));
}

#[test]
fn spans_and_links_past_the_cut_are_hidden() {
    let at = TITLE.find("overflow").unwrap();
    let marked = (at, at + "overflow".len());
    let coloured = |rt: &Runtime<S>| {
        let want = mark().premul(1.0);
        rt.instances.iter().filter(|i| i.fill.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-3)).count()
    };
    let rt = clamp(TITLE, 2, Some(marked), 200.0);
    assert!(drawn(&rt).1.is_some_and(|e| e.head < marked.0));
    assert!(!rt.elements().iter().any(|e| e.path.ends_with("#link-0")), "a link past the cut isn't pressable");
    assert_eq!(coloured(&rt), 0);
    let rt = clamp(TITLE, 0, Some(marked), 200.0);
    assert!(rt.elements().iter().any(|e| e.path.ends_with("#link-0")));
    assert!(coloured(&rt) > 0, "shown, the span draws its colour");
}

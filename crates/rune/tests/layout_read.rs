//! Views read another element's laid-out rect (`Cx::layout`) and handlers hear about it
//! (`El::on_layout`), re-running only when it moves, and settling when a view reads
//! something its own output resizes.
use rune::runtime::ElementInfo;
use rune::{Cx, El, Env, Runtime, ScrollSpec, col, el, row, text};
use std::cell::Cell;

#[derive(Default)]
struct S {
    banner: bool,
    views: Cell<u32>,
    rects: Vec<[f32; 4]>,
    grow_by: f32,
    width: f32,
}

fn find(rt: &Runtime<S>, key: &str) -> ElementInfo {
    rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap_or_else(|| panic!("{key}"))
}

fn frames(rt: &mut Runtime<S>, n: usize) {
    for _ in 0..n { rt.frame(1.0 / 120.0); }
}

/// A timeline below a banner; a badge hangs from the banner's bottom by reading its rect.
fn banner_view(s: &S, cx: &mut Cx<S>) -> El {
    s.views.set(s.views.get() + 1);
    assert_eq!(cx.layout("no-such-key"), None);
    let mut root = col().w_full().h_full().padding(10.0)
        .child(el().key("banner").w_full().h(if s.banner { 55.0 } else { 20.0 })
            .on_layout(|s: &mut S, r| s.rects.push(r)))
        .child(col().key("timeline").w_full().grow(1.0).shrink());
    if let Some([x, y, w, h]) = cx.layout("banner") {
        root = root.child(el().key("badge").absolute(x + w - 30.0, y + h).size(30.0, 10.0));
    }
    root
}

#[test]
fn a_view_places_from_another_elements_rect_in_the_same_commit() {
    let mut rt = Runtime::new(S::default(), banner_view, (300.0, 400.0), Env::new(true, 2.0));
    rt.frame(0.0);
    assert_eq!(find(&rt, "badge").target, [260.0, 30.0, 30.0, 10.0], "placed on the first frame");
    rt.state.banner = true;
    rt.invalidate();
    rt.frame(1.0 / 120.0);
    assert_eq!(find(&rt, "badge").target, [260.0, 65.0, 30.0, 10.0], "moved in the commit that grew the banner");

    // Animation frames re-run nothing and report nothing; settled, nothing re-runs.
    let views = rt.state.views.get();
    frames(&mut rt, 120);
    assert_eq!(rt.state.views.get(), views + 1, "one re-run, for on_layout's state change");
    assert_eq!(rt.state.rects, [[10.0, 10.0, 280.0, 20.0], [10.0, 10.0, 280.0, 55.0]], "once on layout, once on change");
    let commits = rt.stats.commits;
    frames(&mut rt, 10);
    assert_eq!(rt.stats.commits, commits);
}

/// Rows in a scroll; a marker follows row 10 as it scrolls.
fn scroll_view(s: &S, cx: &mut Cx<S>) -> El {
    s.views.set(s.views.get() + 1);
    let rows = (0..40).fold(col().w_full(), |c, i| c.child(el().key(format!("r{i}")).w_full().h(30.0)));
    let mut root = col().w_full().h_full()
        .child(col().key("scroll").w_full().h(300.0).scroll(ScrollSpec { stick_bottom: false }).child(rows));
    if let Some([_, y, _, _]) = cx.layout("r10") {
        root = root.child(el().key("marker").absolute(0.0, y).size(10.0, 10.0));
    }
    root
}

#[test]
fn scrolling_moves_a_read_rect_and_re_runs_its_reader() {
    let mut rt = Runtime::new(S::default(), scroll_view, (300.0, 400.0), Env::new(true, 2.0));
    rt.frame(0.0);
    assert_eq!(find(&rt, "marker").target[1], 300.0);
    rt.wheel(100.0, 100.0, -120.0);
    frames(&mut rt, 60);
    let row = find(&rt, "r10").rect[1];
    assert!(row < 250.0, "scrolled: {row}");
    assert_eq!(find(&rt, "marker").target[1], row, "the marker follows the row");
    let views = rt.state.views.get();
    frames(&mut rt, 10);
    assert_eq!(rt.state.views.get(), views, "still once scrolling stops");
}

/// A card reads its own width to pick a layout: two columns when wide, one when narrow.
fn responsive(s: &S, cx: &mut Cx<S>) -> El {
    s.views.set(s.views.get() + 1);
    let wide = cx.layout("card").is_some_and(|r| r[2] >= 400.0);
    let body = if wide { row().key("body").gap(10.0) } else { col().key("body").gap(10.0) };
    col().w_full().padding(10.0).child(
        col().key("card").w_full().child(body.child(el().key("a").w(150.0).h(40.0)).child(el().key("b").w(150.0).h(40.0))),
    )
}

#[test]
fn a_view_reading_its_own_size_settles() {
    let mut rt = Runtime::new(S::default(), responsive, (600.0, 400.0), Env::new(true, 2.0));
    rt.frame(0.0);
    assert_eq!(find(&rt, "b").target[1], find(&rt, "a").target[1], "wide: side by side on the first frame");
    rt.set_viewport((300.0, 400.0), 2.0);
    frames(&mut rt, 2);
    assert!(find(&rt, "b").target[1] > find(&rt, "a").target[1], "narrow: stacked");
    frames(&mut rt, 60);
    let (views, commits) = (rt.state.views.get(), rt.stats.commits);
    frames(&mut rt, 30);
    assert_eq!((rt.state.views.get(), rt.stats.commits), (views, commits), "settled");
}

/// A chip that reads its own width and swaps to the label that flips it back: no fixed point.
fn flip_flop(s: &S, cx: &mut Cx<S>) -> El {
    s.views.set(s.views.get() + 1);
    let wide = cx.layout("chip").is_some_and(|r| r[2] > 100.0);
    let label = if wide { "short" } else { "a much longer label than that" };
    col().w_full().items_start().child(row().key("chip").child(text(label)))
}

#[test]
fn a_view_without_a_fixed_point_stops_instead_of_oscillating() {
    let mut rt = Runtime::new(S::default(), flip_flop, (600.0, 400.0), Env::new(true, 2.0));
    rt.frame(0.0);
    let first = rt.state.views.get();
    assert!(first <= 4, "one commit re-runs the view at most twice more ({first} runs)");
    frames(&mut rt, 120);
    let (views, commits) = (rt.state.views.get(), rt.stats.commits);
    frames(&mut rt, 60);
    assert_eq!((rt.state.views.get(), rt.stats.commits), (views, commits), "no commits of its own");
}

/// A handler feeding an element's width back into it, growing forever if allowed.
fn runaway(s: &S, _: &mut Cx<S>) -> El {
    let grow_by = s.grow_by;
    col().w_full().child(el().key("bar").w(s.width.max(10.0)).h(10.0).on_layout(move |s: &mut S, r| {
        s.rects.push(r);
        s.width = r[2] + grow_by;
    }))
}

#[test]
fn on_layout_feedback_settles() {
    // A handler that reaches a fixed point: once to lay out, once for the change it caused.
    let mut rt = Runtime::new(S { grow_by: 0.0, width: 50.0, ..S::default() }, runaway, (600.0, 400.0), Env::new(true, 2.0));
    rt.frame(0.0);
    frames(&mut rt, 30);
    assert_eq!(rt.state.rects.len(), 1, "{:?}", rt.state.rects);

    // One that never would: it stops after a few commits, then nothing re-runs.
    let mut rt = Runtime::new(S { grow_by: 10.0, width: 50.0, ..S::default() }, runaway, (600.0, 400.0), Env::new(true, 2.0));
    rt.frame(0.0);
    frames(&mut rt, 60);
    let reports = rt.state.rects.len();
    assert!((2..=6).contains(&reports), "{:?}", rt.state.rects);
    let commits = rt.stats.commits;
    frames(&mut rt, 60);
    assert_eq!((rt.state.rects.len(), rt.stats.commits), (reports, commits));

    // Changes it missed meanwhile are not replayed; the next change from anywhere else is
    // reported, and feedback may run again.
    rt.invalidate();
    frames(&mut rt, 10);
    assert_eq!(rt.state.rects.len(), reports);
    rt.state.width = 200.0;
    rt.invalidate();
    frames(&mut rt, 60);
    assert_eq!(rt.state.rects[reports], [0.0, 0.0, 200.0, 10.0], "{:?}", rt.state.rects);
    assert!(rt.state.rects.len() > reports + 1, "{:?}", rt.state.rects);
}

#[derive(Default)]
struct Editor {
    editing: bool,
    field: rune::TextEdit,
}

/// An inline editor placed over a node by reading its rect, so it first appears on the
/// commit's re-run, never on its first measure.
fn editor_view(s: &Editor, cx: &mut Cx<Editor>) -> El {
    let mut root = col().w_full().h_full().padding(10.0).child(el().key("node").size(120.0, 30.0));
    if s.editing && let Some([x, y, w, _]) = cx.layout("node") {
        root = root.child(rune::text_input(&s.field, rune::tok::TextStyle::new(14.0, 400.0))
            .key("editor").absolute(x, y).w(w).on_edit(|s: &mut Editor| &mut s.field));
    }
    root
}

#[test]
fn a_field_that_appears_on_the_re_run_takes_the_focus_it_asks_for() {
    let mut rt = Runtime::new(Editor::default(), editor_view, (300.0, 200.0), Env::new(true, 2.0));
    rt.frame(0.0);
    rt.update(|s, _| { s.editing = true; s.field.focused = true; });
    rt.frame(1.0 / 120.0);
    let editor = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("editor")).expect("placed in this commit");
    assert_eq!(editor.target[..2], [10.0, 10.0]);
    assert!(editor.focused, "focused in the commit that placed it");
    rt.key(rune::KeyEvent { key: rune::Key::Text("a".into()), mods: Default::default() });
    rt.frame(1.0 / 120.0);
    assert_eq!(rt.state.field.text, "a", "typing reaches it");
}

#[test]
fn a_focused_field_remounting_after_a_layout_read_does_not_steal_newer_focus() {
    #[derive(Default)]
    struct Two {
        step: u8,
        first: rune::TextEdit,
        second: rune::TextEdit,
    }

    fn two_view(s: &Two, cx: &mut Cx<Two>) -> El {
        let mut root = col().w_full().h_full()
            .child(el().key("anchor").size(100.0, 20.0))
            .child(rune::text_input(&s.second, rune::tok::TextStyle::new(14.0, 400.0))
                .key("second").w(100.0).on_edit(|s: &mut Two| &mut s.second));
        if s.step == 0 {
            root = root.child(el().key("early").child(
                rune::text_input(&s.first, rune::tok::TextStyle::new(14.0, 400.0))
                    .key("first").w(100.0).on_edit(|s: &mut Two| &mut s.first),
            ));
        } else if s.step == 2 && cx.layout("anchor").is_some() {
            root = root.child(el().key("late").child(
                rune::text_input(&s.first, rune::tok::TextStyle::new(14.0, 400.0))
                    .key("first").w(100.0).on_edit(|s: &mut Two| &mut s.first),
            ));
        }
        root
    }

    let mut rt = Runtime::new(Two::default(), two_view, (300.0, 200.0), Env::new(true, 2.0));
    rt.update(|s, _| s.first.focused = true);
    rt.frame(0.0);
    // Unmount `first` while it still has `first.focused = true`, and focus `second`.
    rt.update(|s, _| { s.step = 1; s.second.focused = true; });
    rt.frame(1.0 / 60.0);
    // Remount `first` under a new path on a layout-read re-run: `second` keeps focus.
    rt.update(|s, _| s.step = 2);
    rt.frame(1.0 / 60.0);
    let focused: Vec<_> = rt.elements().into_iter().filter(|e| e.focused).filter_map(|e| e.key).collect();
    assert_eq!(focused, ["second"], "remounted field must not steal focus from the newer field");
    assert!(rt.state.second.focused && !rt.state.first.focused);
}

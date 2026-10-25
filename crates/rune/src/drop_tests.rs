//! `El::on_drop_files`: files dragged from another app reach the innermost element under the
//! drop that accepts them, in one call, with a drop hover while the drag is over it.
use super::*;
use crate::{Cx, El, Env, col, el};
use std::path::PathBuf;

#[derive(Default)]
struct Drops {
    /// Each `on_drop_files` call: the target and the files it got.
    dropped: Vec<(&'static str, Vec<String>)>,
    /// Each `on_drop_hover` call.
    hover: Vec<(&'static str, bool)>,
    root_takes: bool,
}

fn zone(key: &'static str) -> El {
    el().key(key)
        .on_drop_files(move |s: &mut Drops, files, _| s.dropped.push((key, files.iter().map(|f| f.display().to_string()).collect())))
        .on_drop_hover(move |s: &mut Drops, over, _| s.hover.push((key, over)))
}

/// An outer zone that takes anything around an inner one that takes pictures, in a 400 by 300
/// window whose root takes drops when `root_takes`.
fn view(s: &Drops, _: &mut Cx<Drops>) -> El {
    let root = col().w_full().h_full()
        .child(zone("outer").size(200.0, 200.0).child(zone("inner").size(100.0, 100.0).accepts_extensions(&["png", ".JPG"])));
    if s.root_takes { root.on_drop_files(|s: &mut Drops, files, _| s.dropped.push(("root", files.iter().map(|f| f.display().to_string()).collect()))) } else { root }
}

fn run(root_takes: bool) -> Runtime<Drops> {
    let mut rt = Runtime::new(Drops { root_takes, ..Default::default() }, view, (400.0, 300.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt
}

fn files(names: &[&str]) -> Vec<PathBuf> { names.iter().map(PathBuf::from).collect() }

fn drop_at(rt: &mut Runtime<Drops>, names: &[&str], at: (f32, f32)) {
    rt.drag_files(files(names), Some(at));
    rt.drop_files(files(names), Some(at));
    rt.frame(1.0 / 60.0);
}

fn dropped(rt: &Runtime<Drops>) -> Vec<(&'static str, Vec<&str>)> {
    rt.state.dropped.iter().map(|(k, f)| (*k, f.iter().map(String::as_str).collect())).collect()
}

#[test]
fn the_innermost_accepting_target_takes_the_drop() {
    let mut rt = run(true);
    drop_at(&mut rt, &["a.png"], (50.0, 50.0));
    drop_at(&mut rt, &["b.png"], (150.0, 150.0));
    assert_eq!(dropped(&rt), [("inner", vec!["a.png"]), ("outer", vec!["b.png"])]);
}

#[test]
fn files_a_target_refuses_pass_to_the_next_that_accepts_them() {
    let mut rt = run(true);
    drop_at(&mut rt, &["notes.txt"], (50.0, 50.0));
    assert_eq!(dropped(&rt), [("outer", vec!["notes.txt"])], "the inner zone takes only pictures");
    drop_at(&mut rt, &["c.png"], (300.0, 250.0));
    assert_eq!(dropped(&rt)[1], ("root", vec!["c.png"]), "a drop over no zone falls back to the root");
    rt.drop_files(files(&["d.png"]), None);
    rt.frame(2.0 / 60.0);
    assert_eq!(dropped(&rt)[2], ("root", vec!["d.png"]), "as does a drop with no position");

    let mut rt = run(false);
    drop_at(&mut rt, &["c.png"], (300.0, 250.0));
    assert_eq!(dropped(&rt), [], "with no root handler a drop nothing takes is ignored");
    assert_eq!(rt.state.hover, [], "and shows no hover");
}

#[test]
fn the_drop_hover_follows_the_drag_and_ends_when_it_leaves_or_drops() {
    let mut rt = run(false);
    rt.drag_files(files(&["a.png"]), Some((50.0, 50.0)));
    assert!(rt.dragging_files());
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.hover, [("inner", true)]);
    assert_eq!(rt.drop_hovered().as_deref(), Some("inner"));
    rt.drag_files_to(Some((150.0, 150.0)));
    rt.frame(2.0 / 60.0);
    assert_eq!(rt.state.hover[1..], [("inner", false), ("outer", true)], "only the innermost accepting target is hovered");
    rt.drag_files_leave();
    assert!(!rt.dragging_files());
    rt.frame(3.0 / 60.0);
    assert_eq!(rt.state.hover[3..], [("outer", false)], "leaving the window ends the hover");
    assert_eq!(rt.drop_hovered(), None);

    rt.state.hover.clear();
    rt.drag_files(files(&["notes.txt"]), Some((50.0, 50.0)));
    rt.frame(4.0 / 60.0);
    assert_eq!(rt.state.hover, [("outer", true)], "a target that refuses the files isn't hovered");
    rt.drop_files(files(&["notes.txt"]), Some((50.0, 50.0)));
    rt.frame(5.0 / 60.0);
    assert_eq!(rt.state.hover, [("outer", true), ("outer", false)], "dropping ends the hover");
    assert_eq!(dropped(&rt), [("outer", vec!["notes.txt"])]);
}

#[test]
fn several_files_arrive_in_one_call() {
    let mut rt = run(false);
    drop_at(&mut rt, &["a.png", "b.JPG", "c.txt"], (50.0, 50.0));
    assert_eq!(dropped(&rt), [("inner", vec!["a.png", "b.JPG"])], "the target gets the files it accepts, together");
    drop_at(&mut rt, &["a.png", "c.txt"], (150.0, 150.0));
    assert_eq!(dropped(&rt)[1], ("outer", vec!["a.png", "c.txt"]));
}

#[test]
fn a_covered_window_still_takes_a_drop() {
    let mut rt = run(false);
    // As the host does for `WindowEvent::Occluded(true)`, then for a drop (`covered_step`).
    rt.set_live_visible(false);
    rt.drag_files(files(&["a.png"]), Some((50.0, 50.0)));
    rt.drop_files(files(&["a.png"]), Some((50.0, 50.0)));
    assert!(rt.has_pending_input());
    rt.hidden_wake();
    assert_eq!(dropped(&rt), [("inner", vec!["a.png"])]);
    assert_eq!(rt.state.hover, [("inner", true), ("inner", false)]);
}

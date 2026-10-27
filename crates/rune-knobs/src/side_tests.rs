use rune::{Cx, Env, Runtime};

use super::*;
use crate::Knob;
use crate::rows::{Shape, shape};

struct T {
    knobs: Knobs,
}
const L: Knob = Knob::new("Left size", 0.0, 10.0, 5.0).label("Left column size");
const C: Knob = Knob::choice("Style", &["One", "Two", "Three"], 0);
const R: Knob = Knob::new("Right pad", 0.0, 40.0, 20.0);
const GROUPS: &[Group] = &[Group::new("Left", "left", &[L, C]), Group::new("Right", "right", &[R])];

fn view(s: &T, _: &mut Cx<T>) -> El {
    let page = row().key("cols").w_full().h_full().child(col().key("left").w(300.0).h_full()).child(col().key("right").grow(1.0).h_full());
    side(page, &s.knobs, GROUPS, Style::dark(), |s: &mut T| &mut s.knobs)
}

fn settle(rt: &mut Runtime<T>) {
    for _ in 0..40 {
        rt.frame(1.0 / 60.0);
    }
}

fn click(rt: &mut Runtime<T>, key: &str) {
    let (x, y) = rt.find(key).unwrap_or_else(|| panic!("no {key}"));
    rt.pointer_move(x, y);
    settle(rt);
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    settle(rt);
}

fn page_w(rt: &mut Runtime<T>) -> f32 {
    rt.elements().into_iter().find(|e| e.key.as_deref() == Some("knobs-page")).expect("page").target[2]
}

#[test]
fn opening_pushes_the_page_over_and_closing_gives_it_back() {
    let mut rt = Runtime::new(T { knobs: Knobs::default() }, view, (1000.0, 700.0), Env::new(true, 1.0));
    settle(&mut rt);
    assert_eq!(page_w(&mut rt), 1000.0);
    rt.state.knobs.show();
    rt.invalidate();
    settle(&mut rt);
    assert_eq!(page_w(&mut rt), 1000.0 - crate::SIDE_W);
    assert_eq!(rt.state.knobs.side_room(), crate::SIDE_W);
    assert!(rt.find("knobs-sec-0").is_some() && rt.find("knobs-sec-1").is_some(), "every group is a section");
    rt.state.knobs.close();
    rt.invalidate();
    settle(&mut rt);
    assert_eq!(page_w(&mut rt), 1000.0);
}

fn body(rt: &Runtime<T>, sec: &str) -> f32 {
    rt.elements().into_iter().find(|e| e.path.contains(sec) && e.key.as_deref() == Some("body")).map(|e| e.rect[3]).unwrap()
}

#[test]
fn a_header_opens_its_group_and_picks_it() {
    let mut s = T { knobs: Knobs::default() };
    s.knobs.show();
    let mut rt = Runtime::new(s, view, (1000.0, 700.0), Env::new(true, 1.0));
    settle(&mut rt);
    assert_eq!(body(&rt, "knobs-sec-1"), 0.0, "folded up front, though on screen");
    click(&mut rt, "knobs-sec-1/head");
    assert!(rt.state.knobs.unfolded.contains("Right"));
    assert_eq!(rt.state.knobs.group.as_deref(), Some("Right"));
    assert!(body(&rt, "knobs-sec-1") > 20.0);
    click(&mut rt, "knobs-sec-1/head");
    assert!(rt.state.knobs.folded.contains("Right"));
    assert_eq!(body(&rt, "knobs-sec-1"), 0.0);
}

#[test]
fn the_panel_reopens_folded_but_for_the_group_last_opened() {
    let mut s = T { knobs: Knobs::default() };
    s.knobs.show();
    let mut rt = Runtime::new(s, view, (1000.0, 700.0), Env::new(true, 1.0));
    settle(&mut rt);
    click(&mut rt, "knobs-sec-0/head");
    click(&mut rt, "knobs-sec-1/head");
    rt.state.knobs.close();
    rt.state.knobs.show();
    rt.invalidate();
    settle(&mut rt);
    assert_eq!(body(&rt, "knobs-sec-0"), 0.0);
    assert!(body(&rt, "knobs-sec-1") > 20.0);
}

#[test]
fn using_a_knob_makes_its_group_the_panels() {
    let mut s = T { knobs: Knobs::default() };
    s.knobs.show();
    let mut rt = Runtime::new(s, view, (1000.0, 700.0), Env::new(true, 1.0));
    settle(&mut rt);
    click(&mut rt, "knobs-sec-1/head");
    click(&mut rt, "knobs-sec-1/body/knobs-card/knobs-knob-0");
    assert_eq!(rt.state.knobs.group.as_deref(), Some("Right"));
    click(&mut rt, "knobs-random");
    assert!(rt.state.knobs.values.contains_key("Right pad"), "Random acts on the group in use");
    assert!(!rt.state.knobs.values.contains_key("Left size"));
}

#[test]
fn the_edge_drags_to_resize() {
    let mut s = T { knobs: Knobs::default() };
    s.knobs.show();
    let mut rt = Runtime::new(s, view, (1000.0, 700.0), Env::new(true, 1.0));
    settle(&mut rt);
    let (x, y) = rt.find("knobs-edge").unwrap();
    rt.pointer_down(x, y);
    rt.pointer_move(x - 60.0, y);
    rt.pointer_up(x - 60.0, y);
    settle(&mut rt);
    assert_eq!(rt.state.knobs.side_w(), crate::SIDE_W + 60.0);
    assert_eq!(page_w(&mut rt), 1000.0 - crate::SIDE_W - 60.0);
}

#[test]
fn choices_fit_the_width() {
    const FEW: Knob = Knob::choice("Edge", &["Fade", "Line", "Tint"], 0);
    const SIX: Knob = Knob::choice("Question glyph", &["Disc", "? disc", "? mark", "? square", "? ring", "Bang"], 0);
    const MANY: Knob = Knob::choice("Theme picker style", &["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"], 0);
    assert_eq!(shape(&FEW, 316.0, false), Shape::Inline);
    // Six options come out as two even rows of three, not four and two.
    assert_eq!(shape(&SIX, 316.0, false), Shape::Grid { cols: 3 });
    assert_eq!(shape(&SIX, 600.0, false), Shape::Inline);
    assert!(matches!(shape(&MANY, 316.0, false), Shape::Long { .. }));
}

#[test]
fn folding_rolls_the_rows_up_on_the_fold_spring() {
    let mut s = T { knobs: Knobs::default() };
    s.knobs.show();
    s.knobs.set_folded("Left", false);
    let mut rt = Runtime::new(s, view, (1000.0, 700.0), Env::new(true, 1.0));
    settle(&mut rt);
    let seen = |rt: &Runtime<T>| rt.elements().into_iter().find(|e| e.path.contains("knobs-sec-0") && e.key.as_deref() == Some("body")).map(|e| e.rect[3]).unwrap();
    let open = seen(&rt);
    let (x, y) = rt.find("knobs-sec-0/head").unwrap();
    rt.pointer_move(x, y);
    rt.frame(1.0 / 60.0);
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    for _ in 0..4 {
        rt.frame(1.0 / 60.0);
    }
    let mid = seen(&rt);
    assert!(mid > 1.0 && mid < open - 1.0, "partway: {mid} of {open}");
    settle(&mut rt);
    assert!(seen(&rt) < 0.5);
}

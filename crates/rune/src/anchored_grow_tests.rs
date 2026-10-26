//! `El::anchored`: while an anchored box's size springs, the edge facing the anchor stays a gap
//! away from it, the edge its alignment names stays on it (End keeps its end, Center its centre,
//! Start its start), and the box follows an anchor that moves without trailing on a spring of its own.
use super::*;
use crate::el::{Align, Side};
use crate::{Cx, El, Env, col, el, row, text};

#[derive(Clone, Copy)]
struct S {
    label: &'static str,
    side: Side,
    align: Align,
    /// The anchor's width.
    w: f32,
    /// The height of a box beside the anchor (`Side::Left` / `Side::Right`).
    h: f32,
    /// A 40 point box this deep away from the anchor, on any side, instead.
    depth: Option<f32>,
}

const SHORT: &str = "1 subagent";
const LONG: &str = "2 subagents and 1 shell";

fn view(s: &S, _: &mut Cx<S>) -> El {
    let vertical = matches!(s.side, Side::Top | Side::Bottom);
    let cap = if let Some(d) = s.depth {
        if vertical { el().key("cap").w(40.0).h(d) } else { el().key("cap").w(d).h(40.0) }
    } else if vertical {
        row().key("cap").h(28.0).pl(10.0).pr(10.0).items_center().child(text(s.label))
    } else {
        el().key("cap").w(40.0).h(s.h)
    };
    col().size(800.0, 600.0).items_center().justify_center()
        .child(el().key("target").size(s.w, 100.0))
        .child(cap.anchored("target", s.side, s.align, 10.0))
}

fn run(side: Side, align: Align) -> Runtime<S> {
    start(S { label: SHORT, side, align, w: 600.0, h: 40.0, depth: None })
}

fn start(s: S) -> Runtime<S> {
    let mut rt = Runtime::new(s, view, (800.0, 600.0), Env::new(true, 1.0));
    rt.frame(0.0);
    for _ in 0..240 { rt.frame(1.0 / 120.0); }
    rt
}

fn rect(rt: &Runtime<S>, key: &str) -> [f32; 4] { rt.presented(key).unwrap().0 }

/// Steps 1/120 s frames until the box settles, checking `held` (the aligned edge, expected)
/// each frame, and returns the box's size along the aligned axis per frame.
fn spring(rt: &mut Runtime<S>, vertical: bool, held: impl Fn(&Runtime<S>) -> (f32, f32)) -> Vec<f32> {
    let mut sizes = Vec::new();
    for frame in 0..240 {
        rt.frame(1.0 / 120.0);
        let (got, want) = held(rt);
        assert!((got - want).abs() < 0.5, "frame {frame}: the aligned edge is at {got}, wanted {want}");
        let r = rect(rt, "cap");
        sizes.push(if vertical { r[3] } else { r[2] });
    }
    sizes
}

/// No frame steps more than a tenth of the whole change, as a spring at 120 Hz doesn't.
fn assert_smooth(sizes: &[f32], from: f32, to: f32) {
    let step = (to - from).abs() * 0.1;
    for pair in sizes.windows(2) {
        assert!((pair[1] - pair[0]).abs() < step, "size stepped {} -> {}", pair[0], pair[1]);
    }
    assert!((sizes[0] - from).abs() < step, "first frame size {} is near {from}", sizes[0]);
    assert!((sizes.last().unwrap() - to).abs() < 0.5, "settles at {to}, not {}", sizes.last().unwrap());
    let mid = (from + to) * 0.5;
    assert!(sizes.iter().any(|w| (w - mid).abs() < (to - from).abs() * 0.25), "the size springs through {mid}: {sizes:?}");
}

#[test]
fn an_end_aligned_box_keeps_its_end_edge_while_its_width_springs() {
    let mut rt = run(Side::Top, Align::End);
    let [x, _, w, _] = rect(&rt, "cap");
    assert!((x + w - 700.0).abs() < 0.5, "settled at the anchor's right edge: {x} + {w}");
    let short = w;
    rt.update(|s, _| s.label = LONG);
    let right = |rt: &Runtime<S>| { let r = rect(rt, "cap"); (r[0] + r[2], 700.0) };
    let grow = spring(&mut rt, false, right);
    let long = *grow.last().unwrap();
    assert!(long > short + 40.0, "{long} grows from {short}");
    assert_smooth(&grow, short, long);

    rt.update(|s, _| s.label = SHORT);
    let shrink = spring(&mut rt, false, right);
    assert_smooth(&shrink, long, short);
}

#[test]
fn a_centred_box_keeps_its_centre_and_a_start_aligned_box_its_start() {
    let mut rt = run(Side::Bottom, Align::Center);
    rt.update(|s, _| s.label = LONG);
    spring(&mut rt, false, |rt| { let r = rect(rt, "cap"); (r[0] + r[2] * 0.5, 400.0) });

    let mut rt = run(Side::Top, Align::Start);
    rt.update(|s, _| s.label = LONG);
    spring(&mut rt, false, |rt| (rect(rt, "cap")[0], 100.0));
}

#[test]
fn a_box_beside_its_anchor_keeps_its_vertical_alignment_while_its_height_springs() {
    for (side, align, held) in [(Side::Right, Align::End, 1.0), (Side::Left, Align::Center, 0.5), (Side::Right, Align::Start, 0.0)] {
        let mut rt = run(side, align);
        rt.update(|s, _| s.h = 90.0);
        let edge = move |rt: &Runtime<S>| { let r = rect(rt, "cap"); (r[1] + r[3] * held, 250.0 + 100.0 * held) };
        let sizes = spring(&mut rt, true, edge);
        assert_smooth(&sizes, 40.0, 90.0);
        rt.update(|s, _| s.h = 20.0);
        let sizes = spring(&mut rt, true, edge);
        assert_smooth(&sizes, 90.0, 20.0);
    }
}

#[test]
fn an_anchored_box_follows_its_resizing_anchor_frame_by_frame() {
    let mut rt = run(Side::Top, Align::End);
    rt.update(|s, _| s.w = 400.0);
    let mut moved = false;
    for frame in 0..240 {
        rt.frame(1.0 / 120.0);
        let (cap, target) = (rect(&rt, "cap"), rect(&rt, "target"));
        moved |= (target[0] + target[2] - 700.0).abs() > 1.0 && (target[0] + target[2] - 600.0).abs() > 1.0;
        assert!((cap[0] + cap[2] - (target[0] + target[2])).abs() < 0.5,
            "frame {frame}: box ends at {}, its anchor at {}", cap[0] + cap[2], target[0] + target[2]);
    }
    assert!(moved, "the anchor springs to its new width");
    assert!((rect(&rt, "cap")[0] + rect(&rt, "cap")[2] - 600.0).abs() < 0.5);
}

#[test]
fn the_edge_facing_the_anchor_holds_while_the_box_springs_away_from_it() {
    // A 300 by 100 anchor spans x 250..550 and y 250..350; each box sits 10 points off it.
    let near = |side: Side, r: [f32; 4]| match side {
        Side::Top => (r[1] + r[3], 240.0),
        Side::Bottom => (r[1], 360.0),
        Side::Left => (r[0] + r[2], 240.0),
        Side::Right => (r[0], 560.0),
    };
    for side in [Side::Top, Side::Bottom, Side::Left, Side::Right] {
        let vertical = matches!(side, Side::Top | Side::Bottom);
        let mut rt = start(S { label: SHORT, side, align: Align::Start, w: 300.0, h: 40.0, depth: Some(180.0) });
        let held = move |rt: &Runtime<S>| near(side, rect(rt, "cap"));
        let (got, want) = held(&rt);
        assert!((got - want).abs() < 0.5, "{side:?} settles {want}, not {got}");
        rt.update(|s, _| s.depth = Some(57.0));
        let sizes = spring(&mut rt, vertical, held);
        assert_smooth(&sizes, 180.0, 57.0);
        rt.update(|s, _| s.depth = Some(180.0));
        let sizes = spring(&mut rt, vertical, held);
        assert_smooth(&sizes, 57.0, 180.0);
    }
}

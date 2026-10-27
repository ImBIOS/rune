use super::*;

const DT: f64 = 1.0 / 120.0;

/// What the rows heard: `on_hover` enters and `on_hover_change` changes.
#[derive(Default)]
struct Rows { growth: bool, open: bool, gone: bool, modal: bool, enters: Vec<usize>, changes: Vec<(usize, bool)> }

/// Twelve 20-point rows, each revealing an action on hover and darkening while pressed.
/// Opening row 0 grows it by 200 points, and the rows below slide down under the pointer.
fn view(s: &Rows, _: &mut Cx<Rows>) -> El {
    let rows = (0..12).filter(|&k| !(s.gone && k == 5)).map(|k| crate::row().key(format!("r{k}")).w_full()
        .h(if k == 0 && s.open { 220.0 } else { 20.0 }).hover_group().press(Color::WHITE).press_scale(0.9)
        .on_press(|_: &mut Rows| {})
        .on_hover(move |s: &mut Rows| s.enters.push(k))
        .on_hover_change(move |s: &mut Rows, inside, _| s.changes.push((k, inside)))
        .child(el().key(format!("a{k}")).size(10.0, 10.0).fill(Color::WHITE).group_opacity(0.0, 1.0)));
    let mut list = col().key("rows").w_full().children(rows);
    if s.growth { list = list.growth(crate::el::GrowthSpec::default()); }
    let root = col().size(200.0, 400.0).child(list);
    if s.modal { root.child(col().key("sheet").absolute(150.0, 350.0).size(40.0, 40.0).modal(true, |s: &mut Rows| s.modal = false)) } else { root }
}

/// The rows whose action shows at all this frame.
fn shown(rt: &Runtime<Rows>) -> Vec<String> {
    rt.elements().into_iter().filter(|e| e.key.as_ref().is_some_and(|k| k.starts_with('a')) && e.opacity > 0.0)
        .filter_map(|e| e.key).collect()
}

/// The row drawn under `y` this frame.
fn row_at(rt: &Runtime<Rows>, y: f32) -> Option<usize> {
    rt.elements().into_iter().filter(|e| e.rect[1] <= y && y < e.rect[1] + e.rect[3])
        .find_map(|e| e.key?.strip_prefix('r')?.parse().ok())
}

fn top(rt: &Runtime<Rows>, key: &str) -> f32 {
    rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap().rect[1]
}

/// Row 5 hovered under a still pointer, then row 0 opening.
fn sliding(growth: bool) -> Runtime<Rows> {
    let mut rt = Runtime::new(Rows { growth, ..Rows::default() }, view, (200.0, 400.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt.pointer_move(100.0, 110.0);
    rt.frame(DT);
    rt.frame(DT);
    assert_eq!(shown(&rt), ["a5"]);
    assert_eq!((&rt.state.enters[..], &rt.state.changes[..]), (&[5][..], &[(5, true)][..]));
    rt.update(|s, _| s.open = true);
    rt
}

/// Runs frames until row 11 stops moving, returning how many it moved on and every action shown.
fn settle(rt: &mut Runtime<Rows>) -> (usize, Vec<String>) {
    let (mut moved, mut seen) = (0, Vec::new());
    let mut y = top(rt, "r11");
    for _ in 0..600 {
        rt.frame(DT);
        for k in shown(rt) { if !seen.contains(&k) { seen.push(k); } }
        let now = top(rt, "r11");
        if (now - y).abs() < 1e-4 && !rt.animating { break; }
        if now != y { moved += 1; }
        y = now;
    }
    for _ in 0..30 { rt.frame(DT); }
    (moved, seen)
}

#[test]
fn rows_sliding_under_a_still_pointer_dont_take_its_hover() {
    for growth in [false, true] {
        let mut rt = sliding(growth);
        let (moved, seen) = settle(&mut rt);
        assert!(moved > 5, "growth {growth}: the rows slid");
        assert_eq!(seen, ["a5", "a0"], "growth {growth}: no row passing under the pointer lit up");
        assert_eq!(rt.state.enters, [5, 0], "growth {growth}");
        assert_eq!(rt.state.changes, [(5, true), (5, false), (0, true)], "growth {growth}");
        assert_eq!(row_at(&rt, 110.0), Some(0), "growth {growth}");
        assert_eq!(shown(&rt), ["a0"], "growth {growth}: once they settle, the row under the pointer has it");
        assert!(!rt.hover_hold.held && !rt.hover_hold.released);
    }
}

#[test]
fn moving_the_pointer_during_a_slide_retargets_at_once() {
    let mut rt = sliding(false);
    for _ in 0..6 { rt.frame(DT); }
    assert_eq!(shown(&rt), ["a5"]);
    assert!(rt.hover_hold.held);
    let under = row_at(&rt, 111.0).unwrap();
    assert_ne!(under, 5, "a row slid under the pointer");
    rt.pointer_move(100.0, 111.0);
    rt.frame(DT);
    assert_eq!(rt.state.enters, [5, under]);
    assert_eq!(rt.state.changes, [(5, true), (5, false), (under, true)]);
    let now = format!("a{}", row_at(&rt, 111.0).unwrap());
    assert!(shown(&rt).contains(&now), "{now}: {:?}", shown(&rt));
}

#[test]
fn the_hovered_row_leaving_mid_slide_lets_go() {
    let mut rt = sliding(false);
    for _ in 0..4 { rt.frame(DT); }
    rt.update(|s, _| s.gone = true);
    rt.frame(DT);
    rt.frame(DT);
    assert_eq!(rt.state.changes, [(5, true), (5, false)], "the leaving row hears the pointer go");
    let (_, seen) = settle(&mut rt);
    assert!(seen.iter().all(|k| k == "a5" || k == "a0"), "nothing else lit up: {seen:?}");
    assert_eq!(rt.state.enters, [5, 0]);
    assert_eq!(rt.state.changes, [(5, true), (5, false), (0, true)]);
}

#[test]
fn a_modal_opening_mid_slide_lets_go() {
    let mut rt = sliding(false);
    for _ in 0..4 { rt.frame(DT); }
    rt.update(|s, _| s.modal = true);
    rt.frame(DT);
    rt.frame(DT);
    assert_eq!(rt.state.changes, [(5, true), (5, false)], "the inert row hears the pointer go");
    settle(&mut rt);
    assert_eq!(shown(&rt), Vec::<String>::new());
    assert_eq!(rt.state.enters, [5]);
}

#[test]
fn a_press_during_a_slide_still_shows_and_lets_go() {
    let mut rt = sliding(false);
    for _ in 0..4 { rt.frame(DT); }
    let under = row_at(&rt, 110.0).unwrap();
    let id = rt.nodes.iter().find(|n| n.named(&format!("r{under}"))).unwrap().id;
    rt.pointer_down(100.0, 110.0);
    rt.frame(DT);
    assert_eq!(rt.scene.store().target(id, P_PRESS), Some(1.0), "the press shows mid-slide");
    assert_eq!(rt.scene.store().target(id, P_PRESS_SCALE), Some(0.9));
    rt.pointer_up(100.0, 110.0);
    rt.frame(DT);
    rt.frame(DT);
    assert_eq!(rt.scene.store().target(id, P_PRESS), Some(0.0), "the press lets go mid-slide");
    assert_eq!(rt.scene.store().target(id, P_PRESS_SCALE), Some(1.0));
    assert!(rt.hover_hold.held, "and hover waits on the slide again");
}

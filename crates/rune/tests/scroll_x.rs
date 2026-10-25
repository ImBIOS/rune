//! `El::scroll_x` and `El::scroll_xy`: horizontal offsets, clamping, momentum, the axis a wheel
//! gesture locks to in nested viewports, `Tx::scroll_into_view` and `Tx::scroll_to_x` on x, and
//! left and right edge fades.
use rune::render::unpack_fade;
use rune::{Color, Cx, El, Env, Runtime, ScrollSpec, col, el, row, text};

const FADE: f32 = 24.0;

#[derive(Default)]
struct S { nested: bool, xy: bool, prose: bool }

/// Ten 100 pt tiles in a 300 pt viewport: 700 pt to scroll.
fn strip() -> El {
    let tiles = (0..10).fold(row(), |r, i| r.child(el().key(format!("tile{i}")).w(100.0).h(100.0).fill(Color::WHITE)));
    col().key("strip").w_full().h(100.0).scroll_x().scroll_edge_fade(FADE).child(tiles)
}

fn view(s: &S, _: &mut Cx<S>) -> El {
    if s.xy {
        let grid = (0..6).fold(col(), |c, r| c.child(
            (0..6).fold(row(), |row, i| row.child(el().key(format!("cell{r}-{i}")).w(100.0).h(100.0)))));
        return col().w_full().h_full().child(col().key("grid").w_full().h(200.0).scroll_xy().child(grid));
    }
    if s.prose {
        let line = text("Wrapping text in a horizontal viewport keeps to one line and scrolls instead.").wrap();
        return col().w_full().h_full().child(col().key("line").w(200.0).scroll_x().child(line.key("text")));
    }
    if !s.nested { return col().w_full().h_full().child(strip()); }
    let rows = (0..20).fold(col().w_full().child(el().key("head").w_full().h(100.0)).child(strip()),
        |c, i| c.child(el().key(format!("row{i}")).w_full().h(40.0)));
    col().w_full().h_full().child(col().key("page").w_full().h(300.0).scroll(ScrollSpec { stick_bottom: false }).child(rows))
}

fn app(s: S) -> Runtime<S> {
    let mut rt = Runtime::new(s, view, (300.0, 300.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt
}

fn x_of(rt: &Runtime<S>, key: &str) -> f32 {
    rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap_or_else(|| panic!("{key}")).rect[0]
}

fn y_of(rt: &Runtime<S>, key: &str) -> f32 {
    rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap_or_else(|| panic!("{key}")).rect[1]
}

fn settle(rt: &mut Runtime<S>) {
    for _ in 0..600 { if !rt.frame(1.0 / 60.0) { return; } }
    panic!("never settled");
}

/// A pause long enough that the next delta starts a new gesture (the headless clock is virtual).
fn new_gesture(rt: &mut Runtime<S>) { rt.frame(0.15); }

#[test]
fn the_content_takes_its_natural_width_and_scrolls_clamped() {
    let mut rt = app(S::default());
    assert_eq!((x_of(&rt, "tile0"), x_of(&rt, "tile9")), (0.0, 900.0), "tiles keep their widths past the viewport");

    rt.wheel_xy(150.0, 50.0, -50.0, 0.0);
    rt.frame(1.0 / 60.0);
    assert_eq!(x_of(&rt, "tile0"), -50.0);

    new_gesture(&mut rt);
    rt.wheel_xy(150.0, 50.0, -10_000.0, 0.0);
    rt.frame(1.0 / 60.0);
    assert_eq!(x_of(&rt, "tile9"), 200.0, "clamped at the right end");
    new_gesture(&mut rt);
    rt.wheel_xy(150.0, 50.0, 10_000.0, 0.0);
    rt.frame(1.0 / 60.0);
    assert_eq!(x_of(&rt, "tile0"), 0.0, "clamped at the left end");
    assert!(!rt.frame(1.0 / 60.0), "no extra frames at rest");
}

#[test]
fn a_vertical_wheel_over_a_horizontal_only_viewport_does_not_move_it() {
    let mut rt = app(S::default());
    rt.wheel(150.0, 50.0, -50.0);
    rt.wheel_xy(150.0, 50.0, 0.0, -50.0);
    rt.frame(1.0 / 60.0);
    assert_eq!(x_of(&rt, "tile0"), 0.0);
}

#[test]
fn a_gesture_locks_to_its_first_axis_in_nested_viewports() {
    let mut rt = app(S { nested: true, ..S::default() });
    let (tile, head) = (x_of(&rt, "tile0"), y_of(&rt, "head"));

    // Mostly vertical over the filmstrip: the page scrolls, the strip stays, and the rest of
    // the gesture stays vertical even once it turns sideways.
    rt.wheel_began();
    rt.wheel_xy(150.0, 150.0, -3.0, -20.0);
    rt.wheel_xy(150.0, 150.0, -40.0, -1.0);
    rt.wheel_ended();
    rt.frame(1.0 / 60.0);
    assert_eq!((x_of(&rt, "tile0"), y_of(&rt, "head")), (tile, head - 21.0));

    // Mostly horizontal: the strip scrolls and the page stays, through the gesture's momentum
    // (a phase that begins as the touch ends).
    new_gesture(&mut rt);
    rt.wheel_began();
    let strip_at = y_of(&rt, "tile0") + 50.0;
    rt.wheel_xy(150.0, strip_at, -20.0, -3.0);
    rt.wheel_xy(150.0, strip_at, -1.0, -30.0);
    rt.wheel_ended();
    rt.wheel_began();
    rt.wheel_xy(150.0, strip_at, -10.0, -10.0);
    rt.wheel_ended();
    rt.frame(1.0 / 60.0);
    assert_eq!((x_of(&rt, "tile0"), y_of(&rt, "head")), (tile - 31.0, head - 21.0));

    // Without phases (a mouse wheel), a pause ends the gesture.
    new_gesture(&mut rt);
    rt.wheel_xy(150.0, strip_at, 0.0, -10.0);
    rt.frame(1.0 / 60.0);
    assert_eq!((x_of(&rt, "tile0"), y_of(&rt, "head")), (tile - 31.0, head - 31.0));
}

#[test]
fn a_horizontal_gesture_outside_any_horizontal_viewport_scrolls_vertically() {
    let mut rt = app(S { nested: true, ..S::default() });
    let head = y_of(&rt, "head");
    rt.wheel_xy(150.0, 280.0, -30.0, -5.0);
    rt.frame(1.0 / 60.0);
    assert_eq!(y_of(&rt, "head"), head - 5.0, "the vertical part moves the page");
}

#[test]
fn a_horizontal_fling_decays_and_clamps() {
    let mut rt = app(S::default());
    rt.fling_xy(150.0, 50.0, -1200.0, 100.0);
    rt.frame(1.0 / 60.0);
    let early = x_of(&rt, "tile0");
    assert!(early < 0.0, "{early}");
    settle(&mut rt);
    let rest = x_of(&rt, "tile0");
    assert!(rest < early && rest >= -700.0, "{early} {rest}");

    rt.fling_xy(150.0, 50.0, -20_000.0, 0.0);
    settle(&mut rt);
    assert_eq!(x_of(&rt, "tile9"), 200.0, "momentum stops at the end");
    assert!(rt.events.iter().any(|e| e.starts_with("fling-x")), "{:?}", rt.events);
}

#[test]
fn scroll_into_view_and_scroll_to_x_move_the_horizontal_offset() {
    let mut rt = app(S::default());
    rt.update(|_, tx| tx.scroll_into_view("tile6"));
    rt.frame(1.0 / 60.0);
    assert_eq!(x_of(&rt, "tile6"), 200.0, "just far enough to show it at the right edge");
    rt.update(|_, tx| tx.scroll_into_view("tile1"));
    rt.frame(1.0 / 60.0);
    assert_eq!(x_of(&rt, "tile1"), 0.0, "back to its left edge");

    rt.update(|_, tx| tx.scroll_to_x("strip", 250.0));
    rt.frame(1.0 / 60.0);
    assert_eq!(x_of(&rt, "tile0"), -250.0);
    rt.update(|_, tx| tx.scroll_to_x("tile3", 9_999.0));
    rt.frame(1.0 / 60.0);
    assert_eq!(x_of(&rt, "tile0"), -700.0, "a key inside finds its viewport; clamped");
}

#[test]
fn scroll_xy_scrolls_both_axes_one_gesture_at_a_time() {
    let mut rt = app(S { xy: true, ..S::default() });
    rt.wheel_xy(150.0, 100.0, -40.0, -10.0);
    rt.frame(1.0 / 60.0);
    assert_eq!((x_of(&rt, "cell0-0"), y_of(&rt, "cell0-0")), (-40.0, 0.0));
    new_gesture(&mut rt);
    rt.wheel_xy(150.0, 100.0, -10.0, -60.0);
    rt.frame(1.0 / 60.0);
    assert_eq!((x_of(&rt, "cell0-0"), y_of(&rt, "cell0-0")), (-40.0, -60.0));
    rt.update(|_, tx| tx.scroll_into_view("cell5-5"));
    rt.frame(1.0 / 60.0);
    assert_eq!((x_of(&rt, "cell5-5"), y_of(&rt, "cell5-5")), (200.0, 100.0));
}

/// The (left, right) fade widths in device px of the tiles' fill.
fn sides(rt: &Runtime<S>) -> [f32; 2] {
    let tile = rt.instances.iter().find(|i| i.fill[3] > 0.99).expect("a tile");
    let (w, _) = unpack_fade(tile.fade);
    assert_eq!((w[1], w[3]), (0.0, 0.0), "no top or bottom fade on a horizontal-only viewport");
    [w[0], w[2]]
}

#[test]
fn left_and_right_edges_fade_only_over_hidden_content() {
    let mut rt = app(S::default());
    assert_eq!(sides(&rt), [0.0, FADE]);
    rt.wheel_xy(150.0, 50.0, -15.0, 0.0);
    rt.frame(1.0 / 60.0);
    assert_eq!(sides(&rt), [FADE, FADE]);
    rt.update(|_, tx| tx.scroll_to_x("strip", 700.0));
    rt.frame(1.0 / 60.0);
    assert_eq!(sides(&rt), [FADE, 0.0]);
}

#[test]
fn wrapping_text_inside_keeps_to_one_line_and_the_viewport_fits_it() {
    let rt = app(S { prose: true, ..S::default() });
    let find = |key: &str| rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap().rect;
    let (line, text) = (find("line"), find("text"));
    assert!(text[2] > 300.0 && text[3] < 30.0, "one unwrapped line: {text:?}");
    assert_eq!(line[3], text[3], "the viewport is one line tall, not the wrapped text's height");
}

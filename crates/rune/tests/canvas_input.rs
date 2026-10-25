//! Canvas input: `on_wheel` ahead of scroll views, pinch, rotation and smart magnify in the
//! handler's own points, the middle button pressing and dragging elements that opt in, and key
//! releases.
use rune::{Cx, El, Env, GesturePhase, Key, KeyEvent, MagnifyEvent, Mods, Phase, PointerButton, RotateEvent, Runtime, ScrollSpec, WheelEvent, col, el};

#[derive(Default)]
struct S {
    /// The canvas takes wheel deltas (else it declines them).
    takes: bool,
    pan: (f32, f32),
    wheels: Vec<WheelEvent>,
    zoom: Vec<(f32, f32, f32)>,
    turns: Vec<f32>,
    smart: Vec<(f32, f32)>,
    drags: Vec<(Phase, PointerButton, f32, f32)>,
    presses: Vec<PointerButton>,
    plain_drags: usize,
    ups: Vec<(&'static str, Key)>,
    space: bool,
}

/// A 300 pt page that scrolls, holding a 200 pt canvas at (50, 50) and 600 pt of rows below.
fn view(_: &S, _: &mut Cx<S>) -> El {
    let canvas = el().key("canvas").w(200.0).h(200.0).ml(50.0).mt(50.0)
        .on_wheel(|s: &mut S, e, _| {
            s.wheels.push(e);
            if s.takes { s.pan.0 += e.dx; s.pan.1 += e.dy; }
            s.takes
        })
        .on_magnify(|s: &mut S, e, _| { s.zoom.push((e.delta, e.x, e.y)); true })
        .on_rotate(|s: &mut S, e, _| { s.turns.push(e.degrees); true })
        .on_smart_magnify(|s: &mut S, x, y, _| { s.smart.push((x, y)); true })
        .pointer_buttons([PointerButton::Middle])
        .on_drag(|s: &mut S, d, tx| s.drags.push((d.phase, tx.button(), d.dx, d.dy)))
        .on_press_tx(|s: &mut S, tx| s.presses.push(tx.button()))
        .child(el().key("shape").w(40.0).h(40.0).focusable(true)
            .on_key_up(|s: &mut S, e, _| { s.ups.push(("shape", e.key.clone())); false }));
    let plain = el().key("plain").w(100.0).h(40.0).on_drag(|s: &mut S, _, _| s.plain_drags += 1);
    let rows = (0..15).fold(col().w_full().child(canvas).child(plain), |c, i| c.child(el().key(format!("row{i}")).w_full().h(40.0)));
    col().w_full().h_full()
        .on_key_up(|s: &mut S, e, _| {
            s.ups.push(("root", e.key.clone()));
            let space = e.key == Key::Text(" ".into());
            if space { s.space = false; }
            space
        })
        .child(col().key("page").w_full().h(300.0).scroll(ScrollSpec { stick_bottom: false }).child(rows))
}

fn app(s: S) -> Runtime<S> {
    let mut rt = Runtime::new(s, view, (300.0, 300.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt
}

fn y_of(rt: &Runtime<S>, key: &str) -> f32 {
    rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap_or_else(|| panic!("{key}")).rect[1]
}

/// A pause long enough that the next delta starts a new gesture.
fn new_gesture(rt: &mut Runtime<S>) { rt.frame(0.15); }

#[test]
fn a_wheel_handler_hears_deltas_before_the_scroll_view_around_it() {
    let mut rt = app(S { takes: true, ..S::default() });
    let cmd = Mods { cmd: true, ..Mods::default() };
    rt.wheel_event(WheelEvent::new(-10.0, -20.0).at(80.0, 90.0).mods(cmd).precise(true));
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.pan, (-10.0, -20.0));
    let e = rt.state.wheels[0];
    assert_eq!((e.x, e.y, e.mods, e.precise), (30.0, 40.0, cmd, true), "the pointer in the canvas's own points");
    assert_eq!(y_of(&rt, "canvas"), 50.0, "the page didn't scroll");

    // The gesture stays the canvas's when the pointer leaves it, as a scroll keeps its axis.
    rt.wheel_xy(150.0, 280.0, 0.0, -30.0);
    rt.frame(1.0 / 60.0);
    assert_eq!((rt.state.pan.1, y_of(&rt, "canvas")), (-50.0, 50.0));

    // Off the canvas a new gesture scrolls the page, which the canvas never hears.
    new_gesture(&mut rt);
    let heard = rt.state.wheels.len();
    rt.wheel_xy(150.0, 280.0, 0.0, -30.0);
    rt.frame(1.0 / 60.0);
    assert_eq!((rt.state.wheels.len(), y_of(&rt, "canvas")), (heard, 20.0));
}

#[test]
fn a_declined_delta_scrolls_and_the_gesture_stays_a_scroll() {
    let mut rt = app(S::default());
    rt.wheel_xy(150.0, 150.0, 0.0, -30.0);
    rt.frame(1.0 / 60.0);
    assert_eq!(y_of(&rt, "canvas"), 20.0, "the canvas declined it, so the page scrolled");
    assert_eq!(rt.state.wheels.len(), 1);
    // The page has the gesture now: the canvas, taking deltas again, doesn't hear the rest.
    rt.state.takes = true;
    rt.wheel_xy(150.0, 150.0, 0.0, -10.0);
    rt.frame(1.0 / 60.0);
    assert_eq!((rt.state.wheels.len(), y_of(&rt, "canvas")), (1, 10.0));
}

#[test]
fn momentum_follows_the_handler_that_took_the_gesture() {
    let mut rt = app(S { takes: true, ..S::default() });
    rt.wheel_event(WheelEvent::new(0.0, -10.0).at(150.0, 150.0).phase(GesturePhase::Began));
    rt.wheel_event(WheelEvent::new(0.0, 0.0).at(150.0, 150.0).phase(GesturePhase::Ended));
    // Momentum begins at once, after the pointer moved off the canvas.
    rt.wheel_event(WheelEvent::new(0.0, -5.0).at(150.0, 280.0).phase(GesturePhase::Began));
    rt.frame(1.0 / 60.0);
    let phases: Vec<_> = rt.state.wheels.iter().map(|e| (e.phase, e.momentum, e.dy)).collect();
    assert_eq!(phases, [(GesturePhase::Began, false, -10.0), (GesturePhase::Ended, false, 0.0), (GesturePhase::Began, true, -5.0)]);
    assert_eq!((rt.state.pan.1, y_of(&rt, "canvas")), (-15.0, 50.0));
}

#[test]
fn pinch_rotation_and_smart_magnify_arrive_in_the_handlers_points() {
    let mut rt = app(S::default());
    rt.magnify_event(MagnifyEvent::new(0.25).at(70.0, 80.0));
    rt.rotate_event(RotateEvent::new(15.0).at(70.0, 80.0));
    rt.smart_magnify(100.0, 60.0);
    // Off the canvas nothing hears them.
    rt.magnify_event(MagnifyEvent::new(0.5).at(20.0, 20.0));
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.zoom, [(0.25, 20.0, 30.0)]);
    assert_eq!(rt.state.turns, [15.0]);
    assert_eq!(rt.state.smart, [(50.0, 10.0)]);
}

#[test]
fn the_middle_button_drags_only_elements_that_take_it() {
    let mut rt = app(S::default());
    // Over the canvas's child, the canvas takes the middle button.
    rt.pointer_move(70.0, 70.0);
    rt.button_down(70.0, 70.0, PointerButton::Middle);
    rt.pointer_move(90.0, 100.0);
    rt.button_up(90.0, 100.0, PointerButton::Middle);
    rt.frame(1.0 / 60.0);
    let drags = &rt.state.drags;
    assert_eq!(drags.first(), Some(&(Phase::Down, PointerButton::Middle, 0.0, 0.0)));
    assert!(drags.contains(&(Phase::Move, PointerButton::Middle, 20.0, 30.0)));
    assert_eq!(drags.last(), Some(&(Phase::Up, PointerButton::Middle, 20.0, 30.0)));
    assert!(drags.iter().all(|d| d.1 == PointerButton::Middle));
    assert_eq!(rt.state.presses, [PointerButton::Middle], "released over it, as a primary drag would be");

    // A middle click outside it presses nothing.
    rt.button_down(20.0, 20.0, PointerButton::Middle);
    rt.button_up(20.0, 20.0, PointerButton::Middle);
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.presses.len(), 1);

    // The primary button presses it as before, as the primary.
    rt.pointer_down(150.0, 150.0);
    rt.pointer_up(150.0, 150.0);
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.presses, [PointerButton::Middle, PointerButton::Primary]);

    // An element that didn't opt in never hears the middle button.
    let (x, y) = rt.find("plain").unwrap();
    rt.button_down(x, y, PointerButton::Middle);
    rt.pointer_move(x + 20.0, y);
    rt.button_up(x + 20.0, y, PointerButton::Middle);
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.plain_drags, 0);
}

#[test]
fn a_key_release_goes_to_the_focused_element_then_up_to_the_root() {
    let mut rt = app(S::default());
    let space = KeyEvent { key: Key::Text(" ".into()), mods: Mods::default() };
    rt.state.space = true;
    assert!(rt.key_up(space.clone()));
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.ups, [("root", Key::Text(" ".into()))], "with nothing focused, the root hears it");
    assert!(!rt.state.space);

    let (x, y) = rt.find("shape").unwrap();
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(1.0 / 60.0);
    rt.state.ups.clear();
    rt.key_up(KeyEvent { key: Key::Escape, mods: Mods::default() });
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.ups, [("shape", Key::Escape), ("root", Key::Escape)], "the focused element first, then its ancestors");
}

#[test]
fn without_key_up_handlers_a_release_needs_no_frame() {
    let mut rt = Runtime::new((), |_: &(), _: &mut Cx<()>| el().w(10.0).h(10.0), (100.0, 100.0), Env::new(true, 1.0));
    rt.frame(0.0);
    assert!(!rt.key_up(KeyEvent { key: Key::Escape, mods: Mods::default() }));
    assert!(!rt.frame(1.0 / 60.0));
}

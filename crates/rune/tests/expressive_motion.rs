//! Expressive presses: `El::scale` springs, `El::origin` pins the pivot, `El::rotate` turns a
//! subtree (and its hit tests), and shadow geometry springs with the element.
use rune::{Color, Cx, El, Env, Runtime, Spring, col, el};

#[derive(Default)]
struct S {
    k: f32,
    pressed: Vec<&'static str>,
}

fn view(s: &S, _: &mut Cx<S>) -> El {
    let k = if s.k == 0.0 { 1.0 } else { s.k };
    col().w_full().h_full().items_start().padding(50.0).child(
        el().key("a")
            .size(100.0, 40.0)
            .fill(Color::WHITE)
            .motion(Spring::new(0.3, 0.0))
            .scale(k)
            .on_press(|s: &mut S| s.pressed.push("a")),
    )
}

fn app() -> Runtime<S> {
    let mut rt = Runtime::new(S::default(), view, (200.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt
}

fn rect<T>(rt: &Runtime<T>, key: &str) -> [f32; 4] {
    rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap().rect
}

fn settle<T>(rt: &mut Runtime<T>) {
    for _ in 0..90 {
        rt.frame(1.0 / 60.0);
    }
}

fn click(rt: &mut Runtime<S>, x: f32, y: f32) {
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(0.0);
}

#[test]
fn a_scale_change_springs_to_rest_and_hits_follow_it() {
    let mut rt = app();
    assert_eq!(rect(&rt, "a"), [50.0, 50.0, 100.0, 40.0]);
    rt.update(|s, _| s.k = 0.5);
    rt.frame(1.0 / 60.0);
    let w = rect(&rt, "a")[2];
    assert!(w > 50.0 && w < 100.0, "mid-spring: {w}");
    settle(&mut rt);
    assert_eq!(rect(&rt, "a"), [75.0, 60.0, 50.0, 20.0], "rests at half size about the centre");
    click(&mut rt, 60.0, 55.0);
    assert!(rt.state.pressed.is_empty(), "outside the drawn rect");
    click(&mut rt, 80.0, 65.0);
    assert_eq!(rt.state.pressed, ["a"]);
}

#[test]
fn a_scale_change_lands_at_once_under_reduce_motion_and_in_an_instant_transaction() {
    let mut rt = app();
    rt.set_system_reduced_motion(true);
    rt.update(|s, _| s.k = 0.5);
    rt.frame(1.0 / 60.0);
    assert_eq!(rect(&rt, "a")[2], 50.0, "Reduce Motion");

    let mut rt = app();
    rt.update(|s, tx| {
        s.k = 2.0;
        tx.instant();
    });
    rt.frame(1.0 / 60.0);
    assert_eq!(rect(&rt, "a")[2], 200.0, "instant transaction");
}

#[test]
fn a_sprung_scale_multiplies_with_live_scale_and_press_scale() {
    struct M {
        k: f32,
        live: rune::Live<f32>,
    }
    fn view(m: &M, _: &mut Cx<M>) -> El {
        col().w_full().h_full().items_start().padding(50.0).child(
            el().key("a").size(100.0, 40.0).fill(Color::WHITE).scale(m.k).live_scale(&m.live).press_scale(0.5).on_press(|_: &mut M| {}),
        )
    }
    let live = rune::Live::new(0.5f32);
    let mut rt = Runtime::new(M { k: 1.0, live: live.clone() }, view, (200.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt.update(|m, _| m.k = 2.0);
    settle(&mut rt);
    assert_eq!(rect(&rt, "a")[2], 100.0, "2 x 0.5");
    rt.pointer_move(100.0, 70.0);
    rt.pointer_down(100.0, 70.0);
    settle(&mut rt);
    assert_eq!(rect(&rt, "a")[2], 50.0, "2 x 0.5 x 0.5 while pressed");
    rt.pointer_up(100.0, 70.0);
    live.set(1.0);
    settle(&mut rt);
    assert_eq!(rect(&rt, "a")[2], 200.0);
}

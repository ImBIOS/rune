//! `El::offset` shifts an element without moving its siblings and springs between shifts;
//! `El::raise` paints and hits an element above later siblings; `on_hover_change` hears an
//! element that appears under a still pointer.
use rune::{Cx, El, Env, Runtime, Spring, col, el};

#[derive(Default)]
struct S {
    shift: f32,
    raise: bool,
    pressed: Vec<&'static str>,
    show: bool,
    hover: Vec<bool>,
}

fn view(s: &S, _: &mut Cx<S>) -> El {
    let mut a = el()
        .key("a")
        .size(100.0, 40.0)
        .motion(Spring::new(0.3, 0.0))
        .offset(0.0, s.shift)
        .on_press(|s: &mut S| s.pressed.push("a"));
    if s.raise {
        a = a.raise();
    }
    let b = el().key("b").size(100.0, 40.0).on_press(|s: &mut S| s.pressed.push("b"));
    let mut root = col().w_full().h_full().child(a).child(b);
    if s.show {
        root = root.child(
            el().key("zone").size(200.0, 200.0).absolute(0.0, 0.0).on_hover_change(|s: &mut S, inside, _| s.hover.push(inside)),
        );
    }
    root
}

fn app() -> Runtime<S> {
    let mut rt = Runtime::new(S::default(), view, (200.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt
}

fn rect(rt: &Runtime<S>, key: &str) -> [f32; 4] {
    rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap().rect
}

fn click(rt: &mut Runtime<S>, x: f32, y: f32) {
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(0.0);
}

#[test]
fn an_offset_springs_without_moving_siblings_and_hits_follow_it() {
    let mut rt = app();
    rt.update(|s, _| s.shift = 40.0);
    rt.frame(1.0 / 60.0);
    let y = rect(&rt, "a")[1];
    assert!(y > 0.0 && y < 40.0, "mid-spring: {y}");
    assert_eq!(rect(&rt, "b")[1], 40.0, "the sibling stays in place");
    for _ in 0..60 {
        rt.frame(1.0 / 60.0);
    }
    assert_eq!(rect(&rt, "a")[1], 40.0);
    // `b` comes later in the tree, so it takes the press where they overlap.
    click(&mut rt, 50.0, 60.0);
    assert_eq!(rt.state.pressed, ["b"]);
    click(&mut rt, 50.0, 10.0);
    assert_eq!(rt.state.pressed, ["b"], "nothing is left where `a` was");

    rt.set_system_reduced_motion(true);
    rt.update(|s, _| s.shift = 0.0);
    rt.frame(1.0 / 60.0);
    assert_eq!(rect(&rt, "a")[1], 0.0, "Reduce Motion lands it at once");
}

#[test]
fn a_raised_element_paints_and_takes_presses_above_later_siblings() {
    let mut rt = app();
    rt.update(|s, _| {
        s.shift = 20.0;
        s.raise = true;
    });
    rt.set_system_reduced_motion(true);
    rt.frame(0.0);
    click(&mut rt, 50.0, 50.0);
    assert_eq!(rt.state.pressed, ["a"], "the raised element is on top where they overlap");
    let order: Vec<_> = rt.elements().into_iter().filter_map(|e| e.key).collect();
    assert_eq!(&order[..2], ["a", "b"], "tree order is unchanged");

    rt.update(|s, _| s.raise = false);
    rt.frame(0.0);
    click(&mut rt, 50.0, 50.0);
    assert_eq!(rt.state.pressed, ["a", "b"]);
}

#[test]
fn hover_change_hears_an_element_that_appears_under_a_still_pointer() {
    let mut rt = app();
    rt.pointer_move(150.0, 150.0);
    rt.frame(0.0);
    rt.update(|s, _| s.show = true);
    for _ in 0..30 {
        rt.frame(1.0 / 60.0);
    }
    assert_eq!(rt.state.hover, [true], "the pointer is inside without moving");
}

mod reorder {
    use std::borrow::Cow;

    use rune::{Cx, El, Env, GrowthSpec, Runtime, ScrollSpec, Spring, col, el};

    struct S {
        order: Vec<usize>,
        shift: Vec<f32>,
        list: u8,
    }

    fn row(s: &S, slot: usize) -> El {
        el().key(format!("r{}", s.order[slot])).w_full().h(40.0).motion(Spring::new(0.3, 0.0)).offset(0.0, s.shift[slot])
    }

    fn view(s: &S, _: &mut Cx<S>) -> El {
        let list = col().key("list").w_full();
        let list = match s.list {
            0 => list.children((0..s.order.len()).map(|i| row(s, i))),
            1 => list.growth(GrowthSpec::default()).children((0..s.order.len()).map(|i| row(s, i))),
            _ => list.growth(GrowthSpec::default()).virtual_rows(
                s.order.len(),
                40.0,
                |s: &S, i| Cow::Owned(format!("r{}", s.order[i])),
                |s: &S, i| row(s, i),
            ),
        };
        col().w_full().h_full().child(col().w_full().h_full().scroll(ScrollSpec { stick_bottom: false }).child(list))
    }

    fn y(rt: &Runtime<S>, k: &str) -> f32 {
        rt.elements().into_iter().find(|e| e.key.as_deref() == Some(k)).unwrap().rect[1]
    }

    /// Drag `r0` 50 points down past `r1`, which steps up, then drop it in slot 1.
    fn drop_reordered(list: u8) {
        let mut rt = Runtime::new(S { order: vec![0, 1, 2, 3], shift: vec![0.0; 4], list }, view, (200.0, 400.0), Env::new(true, 1.0));
        rt.frame(0.0);
        rt.update(|s, tx| {
            tx.instant();
            s.shift = vec![50.0, -40.0, 0.0, 0.0];
        });
        rt.frame(1.0 / 60.0);
        assert_eq!((y(&rt, "r0"), y(&rt, "r1")), (50.0, 0.0));
        rt.update(|s, _| {
            s.order = vec![1, 0, 2, 3];
            s.shift = vec![0.0; 4];
        });
        let mut last = 50.0;
        for _ in 0..90 {
            rt.frame(1.0 / 60.0);
            let (r0, r1) = (y(&rt, "r0"), y(&rt, "r1"));
            assert!(r0 <= last + 0.01 && r0 >= 39.99, "list {list}: r0 slides from 50 to 40, at {r0}");
            assert!(r1.abs() < 0.01, "list {list}: r1 stays where it was drawn, at {r1}");
            last = r0;
        }
        assert!((last - 40.0).abs() < 0.01);
    }

    #[test]
    fn a_reordered_row_that_drops_its_offset_slides_from_where_it_was_drawn() {
        drop_reordered(0);
        drop_reordered(1);
        drop_reordered(2);
    }
}

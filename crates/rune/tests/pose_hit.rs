//! `Runtime::presented_pose` and the moving flag cover shadow and rotation springs;
//! `Runtime::element_at` and `contains` answer where a press would land.
use rune::{Color, Cx, El, Env, Runtime, Shadow, Spring, col, el};
use std::f32::consts::FRAC_PI_4;

fn moving<T>(rt: &Runtime<T>, key: &str) -> bool {
    rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap().moving
}

fn settle<T>(rt: &mut Runtime<T>) {
    for _ in 0..120 {
        rt.frame(1.0 / 60.0);
    }
}

fn sh(y: f32) -> Shadow {
    Shadow { x: 0.0, y, blur: 8.0, spread: 0.0, color: Color::BLACK.alpha(0.3), inset: false }
}

fn card(on: &bool, _: &mut Cx<bool>) -> El {
    let shadow = sh(if *on { 12.0 } else { 2.0 });
    col().w_full().h_full().items_start().padding(50.0).child(
        el().key("a").size(100.0, 40.0).fill(Color::WHITE).motion(Spring::new(0.3, 0.0)).box_shadow(shadow),
    )
}

#[test]
fn a_shadow_only_step_reports_moving_until_it_lands() {
    let mut rt = Runtime::new(false, card, (200.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);
    assert!(!moving(&rt, "a"));
    assert_eq!(rt.presented_pose("a").unwrap().shadows()[0].y, 2.0);
    rt.update(|s, _| *s = true);
    rt.frame(1.0 / 60.0);
    assert!(moving(&rt, "a"), "the shadow's y is springing");
    let pose = rt.presented_pose("a").unwrap();
    assert!(pose.moving());
    let y = pose.shadows()[0].y;
    assert!(y > 2.0 && y < 12.0, "mid-spring y: {y}");
    settle(&mut rt);
    assert!(!moving(&rt, "a"));
    let pose = rt.presented_pose("a").unwrap();
    assert!(!pose.moving());
    assert_eq!(pose.shadows()[0].y, 12.0);
}

#[test]
fn a_rotation_only_step_reports_moving_until_it_lands() {
    fn view(on: &bool, _: &mut Cx<bool>) -> El {
        col().w_full().h_full().items_start().padding(50.0).child(
            el().key("a").size(100.0, 40.0).fill(Color::WHITE).motion(Spring::new(0.3, 0.0))
                .rotate(if *on { 1.0 } else { 0.0 }),
        )
    }
    let mut rt = Runtime::new(false, view, (200.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);
    assert!(!moving(&rt, "a"));
    rt.update(|s, _| *s = true);
    rt.frame(1.0 / 60.0);
    assert!(moving(&rt, "a"));
    let r = rt.presented_pose("a").unwrap().rotation();
    assert!(r > 0.0 && r < 1.0, "mid-spring rotation: {r}");
    settle(&mut rt);
    assert!(!moving(&rt, "a"));
    let pose = rt.presented_pose("a").unwrap();
    assert!((pose.rotation() - 1.0).abs() < 1e-3);
    assert!(!pose.moving());
}

#[derive(Default)]
struct Hits(Vec<&'static str>);

fn bar(_: &Hits, _: &mut Cx<Hits>) -> El {
    col().w_full().h_full().items_start().padding(50.0)
        .on_press(|s: &mut Hits| s.0.push("bg"))
        .child(
            // 100x20 about (100, 60), turned 45°.
            el().key("bar").size(100.0, 20.0).fill(Color::WHITE).rotate(FRAC_PI_4)
                .on_press(|s: &mut Hits| s.0.push("bar"))
                .child(el().key("inner").size(10.0, 10.0)),
        )
}

fn key_at(rt: &Runtime<Hits>, x: f32, y: f32) -> Option<String> {
    rt.element_at(x, y).and_then(|p| p.key().map(str::to_owned))
}

#[test]
fn element_at_follows_the_turned_box_and_agrees_with_presses() {
    let mut rt = Runtime::new(Hits::default(), bar, (200.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);
    // In the upright layout box [50..150, 50..70]; off the 45° diagonal.
    assert_ne!(key_at(&rt, 60.0, 52.0).as_deref(), Some("bar"));
    assert!(!rt.contains("bar", 60.0, 52.0));
    // On the diagonal, outside the upright box.
    assert_eq!(key_at(&rt, 120.0, 80.0).as_deref(), Some("bar"));
    assert!(rt.contains("bar", 120.0, 80.0));
    // In the upright bounding box, outside the turned rect.
    assert!(!rt.contains("bar", 130.0, 30.0));
    assert!(rt.element_at(130.0, 30.0).unwrap().key().is_none(), "the background column");
    // A child with no handlers is found.
    assert_eq!(key_at(&rt, 71.7, 24.6).as_deref(), Some("inner"));
    assert_eq!(rt.element_at(500.0, 500.0), None);

    for (x, y) in [(60.0, 52.0), (120.0, 80.0), (130.0, 30.0), (80.0, 40.0), (140.0, 90.0), (71.7, 24.6)] {
        let picked_bar = matches!(key_at(&rt, x, y).as_deref(), Some("bar" | "inner"));
        rt.state.0.clear();
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
        assert_eq!(rt.state.0 == ["bar"], picked_bar, "({x}, {y}): press {:?}", rt.state.0);
    }
}

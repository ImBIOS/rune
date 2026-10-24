use rune::{Color, Cx, El, Env, Runtime, el, row};

/// A tile that lives in the left box (40 pt) or, zoomed, in the right box (160 pt).
fn view(zoomed: &bool, _: &mut Cx<bool>) -> El {
    let tile = |size: f32| el().key("tile").matched("tile").size(size, size).fill(Color::WHITE).radius(4.0);
    let mut left = el().key("left").size(100.0, 200.0);
    let mut right = el().key("right").size(200.0, 200.0);
    if *zoomed { right = right.child(tile(160.0)) } else { left = left.child(tile(40.0)) }
    row().size(300.0, 200.0).child(left).child(right)
}

const GRID: [f32; 4] = [0.0, 0.0, 40.0, 40.0];
const ONE: [f32; 4] = [100.0, 0.0, 160.0, 160.0];

fn rt() -> Runtime<bool> {
    let mut rt = Runtime::new(false, view, (300.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt
}

fn tile(rt: &Runtime<bool>) -> [f32; 4] {
    rt.presented("tile").expect("tile").0
}

fn moving(rt: &Runtime<bool>) -> bool {
    rt.elements().iter().any(|e| e.key.as_deref() == Some("tile") && e.moving)
}

fn jump(a: [f32; 4], b: [f32; 4]) -> f32 {
    (0..4).map(|k| (a[k] - b[k]).abs()).fold(0.0, f32::max)
}

#[test]
fn a_matched_element_moving_between_parents_flies_from_where_it_was() {
    let mut rt = rt();
    assert_eq!(tile(&rt), GRID);
    rt.update(|zoomed, _| *zoomed = true);
    rt.frame(1.0 / 120.0);
    let first = tile(&rt);
    assert!(jump(first, GRID) < 8.0, "starts at the old rect: {first:?}");
    assert!(moving(&rt), "a flight reports `moving`");
    let mut last = first;
    for _ in 0..240 {
        rt.frame(1.0 / 120.0);
        let r = tile(&rt);
        assert!(jump(r, last) < 12.0, "continuous: {last:?} -> {r:?}");
        last = r;
    }
    assert_eq!(tile(&rt), ONE);
    assert!(!moving(&rt));
}

#[test]
fn toggling_mid_flight_turns_around_without_a_jump() {
    let mut rt = rt();
    let mut last = tile(&rt);
    let mut worst = 0.0f32;
    for round in 0..4 {
        rt.update(|zoomed, _| *zoomed = !*zoomed);
        for _ in 0..12 + round * 5 {
            rt.frame(1.0 / 120.0);
            let r = tile(&rt);
            worst = worst.max(jump(r, last));
            last = r;
        }
        assert!(moving(&rt), "still in flight when toggled again");
    }
    assert!(worst < 12.0, "no rect jumps between frames: {worst}");
    for _ in 0..240 {
        rt.frame(1.0 / 120.0);
    }
    assert_eq!(tile(&rt), GRID, "an even number of toggles lands back in the grid");
}

#[test]
fn reduced_motion_swaps_in_place() {
    let mut rt = rt();
    rt.set_system_reduced_motion(true);
    rt.update(|zoomed, _| *zoomed = true);
    rt.frame(1.0 / 120.0);
    assert_eq!(tile(&rt), ONE);
    for _ in 0..60 {
        rt.frame(1.0 / 120.0);
        assert_eq!(tile(&rt), ONE);
    }
}

#[test]
fn children_scale_with_the_flight() {
    fn view(zoomed: &bool, _: &mut Cx<bool>) -> El {
        let tile = |size: f32| el().key("tile").matched("tile").size(size, size).fill(Color::WHITE)
            .child(el().key("dot").size(size / 4.0, size / 4.0).fill(Color::BLACK));
        let mut left = el().key("left").size(100.0, 200.0);
        let mut right = el().key("right").size(200.0, 200.0);
        if *zoomed { right = right.child(tile(160.0)) } else { left = left.child(tile(40.0)) }
        row().size(300.0, 200.0).child(left).child(right)
    }
    let mut rt = Runtime::new(false, view, (300.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt.update(|zoomed, _| *zoomed = true);
    for _ in 0..10 {
        rt.frame(1.0 / 120.0);
    }
    let (t, _) = rt.presented("tile").unwrap();
    let (d, _) = rt.presented("dot").unwrap();
    assert!(t[2] > 50.0 && t[2] < 150.0, "mid-flight: {t:?}");
    assert!((d[0] - t[0]).abs() < 1.0 && (d[1] - t[1]).abs() < 1.0, "rides the tile's origin: {t:?} {d:?}");
    assert!((d[2] - t[2] / 4.0).abs() < 1.0 && (d[3] - t[3] / 4.0).abs() < 1.0, "a quarter of the tile: {t:?} {d:?}");
}

#[test]
fn unmatched_elements_are_untouched() {
    fn plain(zoomed: &bool, _: &mut Cx<bool>) -> El {
        let tile = |size: f32| el().key("tile").size(size, size).fill(Color::WHITE);
        let mut left = el().key("left").size(100.0, 200.0);
        let mut right = el().key("right").size(200.0, 200.0);
        if *zoomed { right = right.child(tile(160.0)) } else { left = left.child(tile(40.0)) }
        row().size(300.0, 200.0).child(left).child(right)
    }
    let mut rt = Runtime::new(false, plain, (300.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt.update(|zoomed, _| *zoomed = true);
    rt.frame(1.0 / 120.0);
    let (r, _) = rt.presented("tile").unwrap();
    assert_eq!([r[0], r[1]], [ONE[0], ONE[1]], "a plain remount enters at its own place");
}

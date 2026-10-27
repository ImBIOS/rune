use super::*;

type S = (bool, bool, bool);

/// A row whose "act" child rests at `s.0`'s level and shows on hover; `s.1` mounts a second
/// one; `s.2` turns on reduced motion.
fn view(s: &S, cx: &mut Cx<S>) -> El {
    cx.reduced_motion(s.2);
    let rest = if s.0 { 1.0 } else { 0.0 };
    let mut row = crate::row().key("row").size(200.0, 40.0).hover_group()
        .child(el().key("act").size(10.0, 10.0).fill(Color::WHITE).group_opacity(rest, 1.0));
    if s.1 {
        row = row.child(el().key("new").size(10.0, 10.0).fill(Color::WHITE).group_opacity(0.0, 1.0));
    }
    col().size(200.0, 100.0).child(row)
}

fn alpha(rt: &Runtime<S>, key: &str) -> f32 {
    rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).map_or(-1.0, |e| e.opacity)
}

#[test]
fn a_rest_change_animates_with_the_element() {
    let mut rt = Runtime::new((false, false, false), view, (200.0, 100.0), Env::new(true, 1.0));
    rt.frame(0.0);
    assert_eq!(alpha(&rt, "act"), 0.0);
    rt.update(|s, _| s.0 = true);
    let mut seen = Vec::new();
    for _ in 0..120 {
        rt.frame(1.0 / 120.0);
        seen.push(alpha(&rt, "act"));
    }
    assert!(seen[0] < 0.5, "the first frame doesn't cut to the new rest: {seen:?}");
    assert!(seen.windows(2).all(|w| w[1] >= w[0] - 1e-4), "it rises: {seen:?}");
    assert!((seen[119] - 1.0).abs() < 1e-3, "and lands: {seen:?}");
    assert!(!rt.animating);
}

#[test]
fn a_rest_change_snaps_when_instant_or_reduced() {
    let mut rt = Runtime::new((false, false, false), view, (200.0, 100.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt.update(|s, tx| { tx.instant(); s.0 = true });
    rt.frame(1.0 / 120.0);
    assert_eq!(alpha(&rt, "act"), 1.0, "an instant change lands in the frame");
    let mut rt = Runtime::new((true, false, true), view, (200.0, 100.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt.update(|s, _| s.0 = false);
    rt.frame(1.0 / 120.0);
    assert_eq!(alpha(&rt, "act"), 0.0, "reduced motion lands in the frame");
}

#[test]
fn a_row_mounting_before_a_hovered_one_draws_its_actions_at_rest() {
    // The new row comes first in the tree but sits away from the still pointer, so every later
    // index shifts under it.
    fn rows(n: &bool, _: &mut Cx<bool>) -> El {
        let row = |k: &str| crate::row().key(k).size(200.0, 20.0).hover_group()
            .child(el().key(format!("{k}-act")).size(10.0, 10.0).fill(Color::WHITE).group_opacity(0.0, 1.0));
        let mut list = col().size(200.0, 100.0);
        if *n { list = list.child(row("new").absolute(0.0, 60.0)); }
        list.child(row("old"))
    }
    let mut rt = Runtime::new(false, rows, (200.0, 100.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt.pointer_move(100.0, 10.0);
    rt.frame(1.0 / 120.0);
    let alpha = |rt: &Runtime<bool>, key: &str| rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap().opacity;
    assert_eq!(alpha(&rt, "old-act"), 1.0);
    rt.update(|n, _| *n = true);
    for f in 0..10 {
        rt.frame(1.0 / 120.0);
        assert_eq!(alpha(&rt, "new-act"), 0.0, "frame {f}");
        assert_eq!(alpha(&rt, "old-act"), 1.0, "frame {f}");
    }
}

#[test]
fn a_new_mount_draws_at_its_rest_on_its_first_frame() {
    let mut rt = Runtime::new((true, false, false), view, (200.0, 100.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt.update(|s, _| s.1 = true);
    for f in 0..30 {
        rt.frame(1.0 / 120.0);
        let a = alpha(&rt, "new");
        assert!((0.0..1.0 / 512.0).contains(&a), "frame {f}: a new child resting at 0 draws {a}");
    }
}

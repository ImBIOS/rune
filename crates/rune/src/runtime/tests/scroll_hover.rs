use super::*;

/// Forty 20-point rows in a 200-point viewport, each revealing an action on hover.
fn view(_: &(), _: &mut Cx<()>) -> El {
    let rows = col().key("rows").w_full().children((0..40).map(|k| crate::row().key(format!("r{k}")).w_full().h(20.0).hover_group()
        .child(el().key(format!("a{k}")).size(10.0, 10.0).fill(Color::WHITE).group_opacity(0.0, 1.0))));
    col().size(200.0, 200.0).child(col().key("scroll").size(200.0, 200.0).scroll(crate::ScrollSpec { stick_bottom: false }).child(rows))
}

/// The rows whose action shows at all this frame.
fn shown(rt: &Runtime<()>) -> Vec<String> {
    rt.elements().into_iter().filter(|e| e.key.as_ref().is_some_and(|k| k.starts_with('a')) && e.opacity > 0.0)
        .filter_map(|e| e.key).collect()
}

fn hovering() -> Runtime<()> {
    let mut rt = Runtime::new((), view, (200.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt.pointer_move(100.0, 110.0);
    rt.frame(1.0 / 120.0);
    assert_eq!(shown(&rt), ["a5"]);
    rt
}

#[test]
fn a_glide_holds_a_still_pointers_hover_until_it_stops() {
    let mut rt = hovering();
    rt.update(|_, tx| tx.scroll_to("scroll", 0.0, 400.0));
    let mut frames = 0;
    for f in 0..600 {
        rt.frame(1.0 / 120.0);
        if rt.scrolls.values().all(|s| s.glide.is_none()) { break; }
        frames += 1;
        assert_eq!(shown(&rt), ["a5"], "frame {f}: no row passing under the pointer takes its hover");
    }
    assert!(frames > 5, "it glided");
    for _ in 0..30 { rt.frame(1.0 / 120.0); }
    assert_eq!(shown(&rt), ["a25"], "once it stops, the row under the pointer has it");
}

#[test]
fn moving_the_pointer_during_a_glide_hovers_again() {
    let mut rt = hovering();
    rt.update(|_, tx| tx.scroll_to_bottom("scroll"));
    for _ in 0..4 { rt.frame(1.0 / 120.0); }
    assert_eq!(shown(&rt), ["a5"]);
    rt.pointer_move(100.0, 111.0);
    rt.frame(1.0 / 120.0);
    rt.frame(1.0 / 120.0);
    // Hover paints a frame behind the moving rows, as with a wheel scroll.
    let offset = rt.scrolls.values().next().unwrap().offset as f32;
    let under = ((offset + 111.0) / 20.0) as usize;
    let rows: Vec<usize> = shown(&rt).iter().map(|k| k[1..].parse().unwrap()).collect();
    assert!(rows.iter().any(|&k| k != 5 && k + 2 >= under && k <= under), "the moved pointer hovers near row {under}: {rows:?}");
}

#[test]
fn a_wheel_scroll_still_hovers_each_row_it_brings_under_the_pointer() {
    let mut rt = hovering();
    rt.wheel(100.0, 110.0, -40.0);
    // Hover retargets from the frame's drawn rows and paints on the next.
    rt.frame(1.0 / 120.0);
    rt.frame(1.0 / 120.0);
    let offset = rt.scrolls.values().next().unwrap().offset as f32;
    assert!(offset > 0.0);
    let under = format!("a{}", ((offset + 110.0) / 20.0) as usize);
    assert!(shown(&rt).contains(&under), "{under}: {:?}", shown(&rt));
}

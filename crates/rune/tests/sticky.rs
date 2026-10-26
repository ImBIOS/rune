//! Headless tests for `El::sticky_top`, `El::sticky_bottom`, `El::stuck_shadow`,
//! `ElementInfo::stuck`, and `Tx::keep_on_screen`.
use rune::runtime::ElementInfo;
use rune::{
    Align, Color, Cx, El, Env, GrowthSpec, Runtime, ScrollSpec, Shadow, Side, Tx, col, el, oklch,
    text,
};

fn find<S: 'static>(rt: &Runtime<S>, key: &str) -> ElementInfo {
    rt.elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key))
        .unwrap_or_else(|| panic!("missing element {key:?}"))
}

#[derive(Default)]
struct Basic {
    drawn_y: f32,
    hits: Vec<&'static str>,
}

fn basic_view(_: &Basic, _: &mut Cx<Basic>) -> El {
    let shadow = Shadow { y: 1.0, color: Color::BLACK.alpha(0.75), ..Default::default() };
    col()
        .w_full()
        .h_full()
        .child(
            col()
                .key("scroll")
                .w(300.0)
                .h(200.0)
                .scroll(ScrollSpec { stick_bottom: false })
                .child(
                    col()
                        .w_full()
                        .child(el().key("lead").w_full().h(12.0))
                        // Section 0: 12..272, bottom padding 10 so content box ends at 262.
                        .child(
                            col()
                                .key("sec-0")
                                .w_full()
                                .pb(10.0)
                                .child(
                                    col()
                                        .key("hdr-0")
                                        .w_full()
                                        .h(30.0)
                                        .fill(oklch(0.24, 0.02, 260.0))
                                        .sticky_top(8.0)
                                        .stuck_shadow(shadow)
                                        .on_drawn_layout(|s: &mut Basic, r| s.drawn_y = r[1])
                                        .on_press(|s: &mut Basic| s.hits.push("hdr-0"))
                                        .child(
                                            el().key("hdr-0-btn")
                                                .w(60.0)
                                                .h(20.0)
                                                .on_press(|s: &mut Basic| s.hits.push("hdr-0-btn")),
                                        ),
                                )
                                .child(
                                    el().key("row-0")
                                        .w_full()
                                        .h(220.0)
                                        .on_press(|s: &mut Basic| s.hits.push("row-0")),
                                ),
                        )
                        // Section 1: 260..554, with both a top sticky header and a bottom sticky footer.
                        .child(
                            col()
                                .key("sec-1")
                                .w_full()
                                .child(
                                    el().key("hdr-1")
                                        .w_full()
                                        .h(30.0)
                                        .fill(oklch(0.28, 0.02, 260.0))
                                        .sticky_top(8.0),
                                )
                                .child(el().key("row-1").w_full().h(240.0))
                                .child(
                                    el().key("ftr-1")
                                        .w_full()
                                        .h(24.0)
                                        .fill(oklch(0.28, 0.02, 260.0))
                                        .sticky_bottom(6.0),
                                ),
                        )
                        .child(el().key("tail").w_full().h(300.0)),
                ),
        )
}

#[test]
fn sticky_top_and_bottom_hold_push_out_hit_above_siblings_and_report_drawn_layout() {
    let mut rt = Runtime::new(Basic::default(), basic_view, (300.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);

    // At offset 0, hdr-0 sits at y=12 (below its 8pt inset), so it is not stuck yet.
    // Run one more frame so the initial `on_drawn_layout` action dispatches.
    rt.frame(1.0 / 120.0);
    let h0 = find(&rt, "hdr-0");
    assert_eq!(h0.rect, [0.0, 12.0, 300.0, 30.0]);
    assert_eq!(h0.stuck, 0.0);
    assert_eq!(rt.state.drawn_y, 12.0);
    let base_instances = rt.instances.len();

    // Fractional scroll at 120 Hz: offset = 16.5, natural top = -4.5 < 8.0, so it sticks at 8.0
    // with shift = 12.5 in the same frame as wheel().
    rt.wheel(100.0, 100.0, -16.5);
    rt.frame(1.0 / 120.0);
    let h0 = find(&rt, "hdr-0");
    assert!((h0.rect[1] - 8.0).abs() < 1e-3, "held at inset 8: {:?}", h0.rect);
    assert!((h0.stuck - 12.5).abs() < 1e-3, "stuck shift is 12.5: {}", h0.stuck);
    assert!(rt.instances.len() > base_instances, "stuck_shadow draws while stuck");
    rt.frame(1.0 / 120.0);
    assert!((rt.state.drawn_y - 8.0).abs() < 1e-3, "on_drawn_layout sees the stuck y");

    // Scroll to offset = 92: row-0 now spans -50..170, directly under hdr-0 at y=8..38.
    rt.wheel(100.0, 100.0, -75.5);
    rt.frame(1.0 / 120.0);
    let h0 = find(&rt, "hdr-0");
    let btn = find(&rt, "hdr-0-btn");
    assert_eq!(h0.rect[1], 8.0);
    assert_eq!(h0.stuck, 88.0);
    assert_eq!(btn.rect[1], 8.0, "children inside the header move with it");

    // Clicking the button and the header body over row-0 hits the header subtree, not row-0.
    rt.pointer_down(20.0, 15.0);
    rt.pointer_up(20.0, 15.0);
    rt.frame(1.0 / 120.0);
    assert_eq!(rt.state.hits, vec!["hdr-0-btn"]);

    rt.pointer_down(200.0, 20.0);
    rt.pointer_up(200.0, 20.0);
    rt.frame(1.0 / 120.0);
    assert_eq!(rt.state.hits, vec!["hdr-0-btn", "hdr-0"]);

    // sec-0's content box ends at 262 (272 - 10pt bottom padding). Once offset > 262 - 8 - 30 = 224,
    // sec-0's bottom edge pushes hdr-0 up. At offset = 242, hdr-0's bottom is at 262 - 242 = 20, so y = -10.
    rt.wheel(100.0, 100.0, -150.0);
    rt.frame(1.0 / 120.0);
    let h0 = find(&rt, "hdr-0");
    assert_eq!(h0.rect[1], -10.0, "pushed up by sec-0's padded bottom edge");
    assert_eq!(h0.stuck, 220.0);

    // At offset = 242, sec-1 spans 30..324 (height 294). Its footer (24pt tall, sticky_bottom(6.0))
    // naturally sits at 300..324, which is below 200 - 6 = 194, so it sticks at y = 170 (194 - 24).
    let f1 = find(&rt, "ftr-1");
    assert_eq!(f1.rect[1], 170.0, "footer held 6pt above viewport bottom");
    assert_eq!(f1.stuck, -130.0, "upward sticky shift is negative");
}

#[test]
fn nested_scroll_viewports_stick_each_header_to_its_nearest_viewport() {
    fn view(_: &(), _: &mut Cx<()>) -> El {
        col().w_full().h_full().child(
            col()
                .key("outer")
                .w(300.0)
                .h(240.0)
                .scroll(ScrollSpec { stick_bottom: false })
                .child(
                    col()
                        .w_full()
                        .child(
                            col()
                                .key("outer-sec")
                                .w_full()
                                .child(el().key("outer-hdr").w_full().h(28.0).sticky_top(0.0))
                                .child(el().w_full().h(60.0))
                                .child(
                                    col()
                                        .key("inner")
                                        .w_full()
                                        .h(120.0)
                                        .scroll(ScrollSpec { stick_bottom: false })
                                        .child(
                                            col()
                                                .w_full()
                                                .child(
                                                    col()
                                                        .w_full()
                                                        .child(el().key("inner-hdr").w_full().h(20.0).sticky_top(4.0))
                                                        .child(el().w_full().h(260.0)),
                                                ),
                                        ),
                                )
                                .child(el().w_full().h(300.0)),
                        ),
                ),
        )
    }

    let mut rt = Runtime::new((), view, (300.0, 240.0), Env::new(true, 1.0));
    rt.frame(0.0);
    assert_eq!(find(&rt, "outer-hdr").rect[1], 0.0);
    assert_eq!(find(&rt, "inner").rect[1], 88.0);
    assert_eq!(find(&rt, "inner-hdr").rect[1], 92.0);

    // Scroll inner viewport (at y=120): inner-hdr sticks at inner.top + 4 = 92.
    rt.wheel(150.0, 120.0, -50.0);
    rt.frame(1.0 / 120.0);
    assert_eq!(find(&rt, "inner-hdr").rect[1], 92.0);
    assert_eq!(find(&rt, "inner-hdr").stuck, 54.0);
    assert_eq!(find(&rt, "outer-hdr").stuck, 0.0);

    // Scroll outer viewport (at y=10, outside inner): outer-hdr sticks at 0, inner moves up by 30,
    // and inner-hdr stays stuck to inner's new top + 4 = 62.
    rt.wheel(150.0, 10.0, -30.0);
    rt.frame(1.0 / 120.0);
    assert_eq!(find(&rt, "outer-hdr").rect[1], 0.0);
    assert_eq!(find(&rt, "outer-hdr").stuck, 30.0);
    assert_eq!(find(&rt, "inner").rect[1], 58.0);
    assert_eq!(find(&rt, "inner-hdr").rect[1], 62.0);
}

#[derive(Default)]
struct CollapseApp {
    collapsed: [bool; 8],
}

fn virtual_sections(_: &CollapseApp, _: &mut Cx<CollapseApp>) -> El {
    col().w_full().h_full().child(
        col()
            .key("scroll")
            .w(320.0)
            .h(200.0)
            .scroll(ScrollSpec { stick_bottom: false })
            .child(
                col()
                    .key("list")
                    .w_full()
                    .growth(GrowthSpec::default())
                    .virtual_rows(
                        8,
                        160.0,
                        |_: &CollapseApp, i| format!("sec-{i}").into(),
                        |s: &CollapseApp, i| {
                            let collapsed = s.collapsed[i];
                            let hdr_key = format!("hdr-{i}");
                            let keep_key = hdr_key.clone();
                            let mut sec = col().w_full().child(
                                el().key(hdr_key)
                                    .w_full()
                                    .h(32.0)
                                    .fill(oklch(0.25, 0.02, 260.0))
                                    .sticky_top(0.0)
                                    .on_press_tx(move |s: &mut CollapseApp, tx: &mut Tx| {
                                        s.collapsed[i] = !s.collapsed[i];
                                        tx.keep_on_screen(&keep_key);
                                    }),
                            );
                            if !collapsed {
                                sec = sec.child(el().key(format!("body-{i}")).w_full().h(600.0));
                            }
                            sec
                        },
                    ),
            ),
    )
}

#[test]
fn virtual_rows_keep_stuck_header_on_screen_across_collapse_and_expand() {
    let mut rt = Runtime::new(CollapseApp::default(), virtual_sections, (320.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);

    // Scroll deep into sec-1 (starts at 632, ends at 1264): offset = 1050 puts us 418pt into sec-1.
    rt.wheel(160.0, 100.0, -1050.0);
    for _ in 0..4 {
        rt.frame(1.0 / 120.0);
    }
    let h1 = find(&rt, "hdr-1");
    assert_eq!(h1.rect[1], 0.0, "hdr-1 is stuck at the top of the viewport");
    assert!((h1.stuck - 418.0).abs() < 0.5, "hdr-1 stuck shift: {}", h1.stuck);

    // Click hdr-1 (at y=16) to collapse sec-1 from 632pt tall to 32pt tall.
    // Without keep_on_screen, sec-1 (632..664) would end 386pt above offset 1050 and be culled.
    rt.pointer_down(100.0, 16.0);
    rt.pointer_up(100.0, 16.0);
    for step in 0..10 {
        rt.frame(1.0 / 120.0);
        let h1 = find(&rt, "hdr-1");
        assert!((h1.rect[1] - 0.0).abs() < 0.5, "frame {step}: hdr-1 stayed at y=0 after collapse: {:?}", h1.rect);
    }
    assert_eq!(find(&rt, "hdr-2").rect[1], 32.0, "sec-2 sits immediately below collapsed sec-1");

    // Click hdr-1 again to expand sec-1: hdr-1 stays at y=0 while its body opens below it.
    rt.pointer_down(100.0, 16.0);
    rt.pointer_up(100.0, 16.0);
    for step in 0..10 {
        rt.frame(1.0 / 120.0);
        let h1 = find(&rt, "hdr-1");
        assert!((h1.rect[1] - 0.0).abs() < 0.5, "frame {step}: hdr-1 stayed at y=0 after expand: {:?}", h1.rect);
    }
}

#[derive(Default)]
struct Timeline {
    collapsed: bool,
    extra_rows: usize,
    menu_open: bool,
}

fn timeline_view(s: &Timeline, _: &mut Cx<Timeline>) -> El {
    let mut group = col().key("group").w_full().child(
        el().key("group-hdr")
            .w_full()
            .h(30.0)
            .fill(oklch(0.24, 0.02, 260.0))
            .sticky_top(0.0)
            .on_press_tx(|s: &mut Timeline, tx: &mut Tx| {
                s.collapsed = !s.collapsed;
                tx.keep_on_screen("group-hdr");
            }),
    );
    if !s.collapsed {
        group = group.child(el().key("group-body").w_full().h(400.0));
    }
    let mut stack = col()
        .key("stack")
        .w_full()
        .growth(GrowthSpec::default())
        .child(el().key("row-a").w_full().h(200.0))
        .child(group)
        .child(el().key("row-b").w_full().h(120.0));
    for i in 0..s.extra_rows {
        stack = stack.child(text(format!("line {i}")).key(format!("extra-{i}")).w_full().h(50.0));
    }
    let mut root = col().w_full().h_full().child(
        col()
            .key("scroll")
            .w(300.0)
            .h(200.0)
            .scroll(ScrollSpec { stick_bottom: true })
            .child(stack),
    );
    if s.menu_open {
        root = root.child(
            el().key("popover")
                .w(100.0)
                .h(40.0)
                .anchored("group-hdr", Side::Bottom, Align::Start, 4.0),
        );
    }
    root
}

#[test]
fn stick_bottom_growth_reveal_and_anchored_overlay_work_with_sticky_headers() {
    let init = Timeline { collapsed: false, extra_rows: 2, menu_open: false };
    let mut rt = Runtime::new(init, timeline_view, (300.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);

    // Opens pinned at the bottom (total height = 200 + 430 + 120 + 100 = 850, viewport = 200 -> offset = 650).
    // Scroll back into `group` (200..630) at offset = 450 so `group-hdr` is stuck at y=0, and open an
    // anchored overlay on `group-hdr`.
    rt.wheel(150.0, 100.0, 200.0);
    rt.update(|s, _| s.menu_open = true);
    rt.frame(1.0 / 120.0);
    assert_eq!(find(&rt, "group-hdr").rect[1], 0.0);
    assert_eq!(find(&rt, "popover").rect[1], 34.0, "anchored overlay sits 4pt below the stuck header");

    // Scrolling further while the overlay is open keeps the overlay attached to the stuck header,
    // and once the header is pushed up (at offset = 610, group ends at 630 -> hdr at -10), the
    // overlay moves with it (to -10 + 30 + 4 = 24).
    rt.wheel(150.0, 100.0, -160.0);
    rt.frame(1.0 / 120.0);
    assert_eq!(find(&rt, "group-hdr").rect[1], -10.0);
    assert_eq!(find(&rt, "popover").rect[1], 24.0, "anchored overlay tracks the header as it pushes out");

    // At offset = 400 (with extra_rows cleared), `group-hdr` is stuck at y=0..30. Collapse `group`
    // (height 430 -> 30): total content becomes 200 + 30 + 120 = 350, max offset = 150.
    // `keep_on_screen("group-hdr")` wants offset 200 - 0 = 200, which clamps to max offset 150
    // and re-pins `stick_bottom`!
    rt.update(|s, tx| {
        s.menu_open = false;
        s.extra_rows = 0;
        tx.scroll_to_y("scroll", 400.0);
    });
    for _ in 0..40 {
        rt.frame(1.0 / 60.0);
    }
    assert_eq!(find(&rt, "group-hdr").rect[1], 0.0);
    rt.pointer_down(100.0, 15.0);
    rt.pointer_up(100.0, 15.0);
    rt.frame(1.0 / 120.0);

    // Because collapsing brought the scroll offset to `max` (10.0), `stick_bottom` is re-pinned:
    // appending new growth rows keeps the bottom in view!
    rt.update(|s, _| s.extra_rows = 2);
    for _ in 0..60 {
        rt.frame(1.0 / 60.0);
    }
    let last = find(&rt, "extra-1");
    assert!((last.rect[1] + last.rect[3] - 200.0).abs() < 1.0, "stick_bottom followed new growth rows: {:?}", last.rect);
}

#[test]
fn scroll_into_view_lands_row_clear_of_stuck_header() {
    fn view(_: &(), _: &mut Cx<()>) -> El {
        col().w_full().h_full().child(
            col()
                .key("scroll")
                .w(300.0)
                .h(200.0)
                .scroll(ScrollSpec { stick_bottom: false })
                .child(
                    col()
                        .w_full()
                        .child(
                            col()
                                .w_full()
                                .child(el().key("hdr").w_full().h(40.0).sticky_top(0.0))
                                .child(el().key("r0").w_full().h(80.0))
                                .child(el().key("r1").w_full().h(80.0))
                                .child(el().key("r2").w_full().h(80.0))
                                .child(el().key("r3").w_full().h(80.0)),
                        ),
                ),
        )
    }

    let mut rt = Runtime::new((), view, (300.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);
    // Scroll down to offset = 150: `hdr` is stuck at y=0..40, and `r1` (120..200) sits at y=-30..50
    // (its top is hidden behind `hdr` and above the viewport).
    rt.update(|_, tx| tx.scroll_to_y("scroll", 150.0));
    rt.frame(1.0 / 120.0);
    assert_eq!(find(&rt, "hdr").rect[1], 0.0);
    assert_eq!(find(&rt, "r1").rect[1], -30.0);

    // Revealing `r1` scrolls far enough that `r1`'s top clears the 40pt stuck `hdr` (landing at y=40).
    rt.update(|_, tx| tx.scroll_into_view("r1"));
    rt.frame(1.0 / 120.0);
    assert_eq!(find(&rt, "hdr").rect[1], 0.0);
    assert_eq!(find(&rt, "r1").rect[1], 40.0, "r1 lands clear of the 40pt stuck header");
}

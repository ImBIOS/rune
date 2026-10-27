use super::*;
use crate::el::{GrowthSpec, ScrollSpec, row};

#[test]
fn scroll_with_fifty_direct_row_children_scrolls_to_the_last_row() {
    fn view(_: &(), _: &mut Cx<()>) -> El {
        col()
            .key("scroll")
            .size(200.0, 200.0)
            .scroll(ScrollSpec { stick_bottom: false })
            .children((0..50).map(|i| row().key(format!("r{i}")).w_full().h(30.0)))
    }
    let mut rt = Runtime::new((), view, (200.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);
    let last = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("r49")).unwrap();
    assert_eq!(last.rect[1], 1470.0);

    rt.wheel(100.0, 100.0, -10_000.0);
    rt.frame(1.0 / 60.0);
    let last = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("r49")).unwrap();
    assert_eq!(last.rect[1] + last.rect[3], 200.0, "scrolled flush to the 50th direct child");
    assert_eq!(rt.scroll_at_bottom("scroll"), Some(true));
}

#[test]
fn scroll_x_with_fifty_direct_children_scrolls_to_the_last_child() {
    fn view(_: &(), _: &mut Cx<()>) -> El {
        row()
            .key("strip")
            .size(200.0, 60.0)
            .scroll_x()
            .children((0..50).map(|i| el().key(format!("c{i}")).w(40.0).h(60.0)))
    }
    let mut rt = Runtime::new((), view, (200.0, 60.0), Env::new(true, 1.0));
    rt.frame(0.0);
    let last = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("c49")).unwrap();
    assert_eq!(last.rect[0], 1960.0);

    rt.wheel_xy(100.0, 30.0, -10_000.0, 0.0);
    rt.frame(1.0 / 60.0);
    let last = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("c49")).unwrap();
    assert_eq!(last.rect[0] + last.rect[2], 200.0, "scrolled flush to the 50th direct horizontal child");
}

#[test]
fn stick_bottom_with_several_direct_children_stays_pinned_as_rows_arrive() {
    fn view(count: &usize, _: &mut Cx<usize>) -> El {
        col()
            .key("scroll")
            .size(200.0, 150.0)
            .scroll(ScrollSpec { stick_bottom: true })
            .children((0..*count).map(|i| row().key(format!("r{i}")).w_full().h(40.0)))
    }
    let mut rt = Runtime::new(6usize, view, (200.0, 150.0), Env::new(true, 1.0));
    rt.frame(0.0);
    let last = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("r5")).unwrap();
    assert_eq!(last.rect[1] + last.rect[3], 150.0, "opens pinned to the last direct child");

    for next in 7..=10 {
        rt.update(move |n, _| *n = next);
        for _ in 0..60 {
            rt.frame(1.0 / 120.0);
        }
        let key = format!("r{}", next - 1);
        let last = rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key.as_str())).unwrap();
        assert!(
            (last.rect[1] + last.rect[3] - 150.0).abs() < 0.05,
            "after adding {key}, bottom was {} instead of 150.0",
            last.rect[1] + last.rect[3]
        );
        assert_eq!(rt.scroll_at_bottom("scroll"), Some(true));
    }
}

#[test]
fn scroll_into_view_reaches_the_fortieth_direct_child() {
    fn view(_: &(), _: &mut Cx<()>) -> El {
        col()
            .key("scroll")
            .size(200.0, 200.0)
            .scroll(ScrollSpec { stick_bottom: false })
            .children((0..50).map(|i| row().key(format!("r{i}")).w_full().h(30.0)))
    }
    let mut rt = Runtime::new((), view, (200.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt.update(|_, tx| tx.scroll_into_view("r39"));
    rt.frame(1.0 / 60.0);
    let r39 = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("r39")).unwrap();
    assert_eq!(r39.rect[1] + r39.rect[3], 200.0, "40th direct child brought into view at bottom edge");
}

#[test]
fn growth_stack_child_after_a_header_child_grows_and_stays_pinned() {
    fn view(count: &usize, _: &mut Cx<usize>) -> El {
        let stack = col()
            .key("stack")
            .w_full()
            .growth(GrowthSpec::default())
            .children((0..*count).map(|i| row().key(format!("r{i}")).w_full().h(50.0)));
        col()
            .key("scroll")
            .size(200.0, 200.0)
            .scroll(ScrollSpec { stick_bottom: true })
            .child(el().key("banner").w_full().h(60.0))
            .child(stack)
    }
    let mut rt = Runtime::new(5usize, view, (200.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);
    let r4 = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("r4")).unwrap();
    assert!((r4.rect[1] + r4.rect[3] - 200.0).abs() < 0.05);

    rt.update(|n, _| *n = 6);
    rt.frame(1.0 / 120.0);
    assert!(rt.animating, "growth stack child animates new row");
    for _ in 0..120 {
        rt.frame(1.0 / 120.0);
    }
    let r5 = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("r5")).unwrap();
    assert!((r5.rect[1] + r5.rect[3] - 200.0).abs() < 0.05);
}

#[test]
fn scroll_xy_with_direct_children_scrolls_both_axes_and_warns_on_trailing_growth_sibling() {
    fn view(bad: &bool, _: &mut Cx<bool>) -> El {
        if *bad {
            return col()
                .key("scroll")
                .size(200.0, 200.0)
                .scroll(ScrollSpec { stick_bottom: false })
                .child(col().key("stack").w_full().growth(GrowthSpec::default()).child(el().h(50.0)))
                .child(el().key("after").w_full().h(50.0));
        }
        col()
            .key("scroll")
            .size(200.0, 200.0)
            .scroll_xy()
            .children((0..20).map(|i| row().key(format!("r{i}")).w(500.0).h(30.0)))
    }
    let mut rt = Runtime::new(false, view, (200.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt.update(|_, tx| tx.scroll_to_instant("scroll", 9_999.0, 9_999.0));
    rt.frame(1.0 / 60.0);
    let r19 = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("r19")).unwrap();
    assert_eq!((r19.rect[0] + r19.rect[2], r19.rect[1] + r19.rect[3]), (200.0, 200.0));
    assert!(!layout_diagnostics::warned_at("growth-sibling:"));

    rt.update(|bad, _| *bad = true);
    rt.frame(1.0 / 60.0);
    assert!(layout_diagnostics::warned_at("growth-sibling:"));
}


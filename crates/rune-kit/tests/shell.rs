//! The app shell: pane resizing and collapse, the toggle shortcut, the source list's keyboard,
//! virtualised rows, accessibility roles and the title bar inset.

use std::borrow::Cow;

use rune::runtime::ElementInfo;
use rune::{Chrome, Cx, El, Env, Key, KeyEvent, Mods, Runtime, TitlebarAction, col, el};
use rune_kit::shell::{PaneState, Sidebar, SidebarRow, SidebarSection, SplitView, Toolbar, is_sidebar_toggle};
use rune_kit::Kit;

const NAMES: [&str; 4] = ["Alpha", "Bravo", "Charlie", "Delta"];

struct S {
    sidebar: PaneState,
    picked: usize,
    collapsed: bool,
    rows: Option<usize>,
}

fn state() -> S {
    S { sidebar: PaneState::new(240.0).range(180.0, 420.0), picked: 1, collapsed: false, rows: None }
}

fn row(s: &S, i: usize, k: &Kit) -> El {
    let label = NAMES.get(i).map_or_else(|| format!("Row {i}"), |n| n.to_string());
    SidebarRow::new(label).selected(s.picked == i).on_select(move |s: &mut S, _| s.picked = i).build(k).key(format!("row{i}"))
}

fn view(s: &S, cx: &mut Cx<S>) -> El {
    let k = Kit::neutral(true);
    let sidebar = match s.rows {
        Some(n) => {
            let kit = k.clone();
            Sidebar::new()
                .label("Sessions")
                .virtual_rows(n, |_: &S, i| Cow::Owned(format!("row{i}")), move |s: &S, i| row(s, i, &kit))
        }
        None => Sidebar::new().label("Sessions").section(
            SidebarSection::new("Today")
                .collapsed(s.collapsed)
                .on_toggle(|s: &mut S, _| s.collapsed = !s.collapsed)
                .rows((0..NAMES.len()).map(|i| row(s, i, &k))),
        ),
    };
    SplitView::new(col().key("body"))
        .sidebar(&s.sidebar, |s: &mut S| &mut s.sidebar, sidebar.build(&k))
        .toolbar(Toolbar::new().leading(el().key("lead").size(20.0, 20.0)).title("Title"))
        .titlebar_inset(cx.titlebar_inset())
        .build(&k)
}

fn runtime(s: S) -> Runtime<S> {
    let mut rt = Runtime::new(s, view, (1000.0, 600.0), Env::new(true, 1.0));
    rt.on_key = Some(|s, ev, _| {
        let hit = is_sidebar_toggle(ev);
        if hit {
            s.sidebar.toggle();
        }
        hit
    });
    rt.frame(0.0);
    rt
}

fn settle(rt: &mut Runtime<S>) {
    for _ in 0..120 {
        rt.frame(1.0 / 60.0);
    }
}

fn find(rt: &Runtime<S>, key: &str) -> ElementInfo {
    rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key) && !e.exiting).unwrap_or_else(|| panic!("no {key}"))
}

fn press(rt: &mut Runtime<S>, key: Key) {
    rt.key(KeyEvent { key, mods: Mods::default() });
    rt.frame(1.0 / 60.0);
}

fn toggle_shortcut() -> KeyEvent {
    KeyEvent { key: Key::Command('b'), mods: Mods { shift: true, cmd: true, ..Mods::default() } }
}

#[test]
fn dragging_the_grip_clamps_collapses_and_double_click_resets() {
    let mut rt = runtime(state());
    let (x, y) = rt.find("sidebar-grip").expect("grip");
    rt.pointer_move(x, y);
    rt.pointer_down(x, y);
    rt.pointer_move(x + 60.0, y);
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.sidebar.width(), 300.0, "follows the pointer");
    assert_eq!(find(&rt, "sidebar-pane").rect[2], 300.0, "a drag doesn't spring");
    rt.pointer_move(x + 600.0, y);
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.sidebar.width(), 420.0, "clamps to the maximum");
    rt.pointer_move(x - 140.0, y);
    rt.frame(1.0 / 60.0);
    assert!(rt.state.sidebar.is_open() && rt.state.sidebar.width() == 180.0, "clamps to the minimum");
    rt.pointer_move(x - 200.0, y);
    rt.frame(1.0 / 60.0);
    assert!(!rt.state.sidebar.is_open(), "below half the minimum it collapses");
    rt.pointer_move(x - 20.0, y);
    rt.frame(1.0 / 60.0);
    assert!(rt.state.sidebar.is_open(), "and dragging back out opens it");
    rt.pointer_up(x - 20.0, y);
    settle(&mut rt);
    assert_eq!(rt.state.sidebar.width(), 220.0);

    let (x, y) = rt.find("sidebar-grip").expect("grip");
    for _ in 0..2 {
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(1.0 / 60.0);
    }
    assert_eq!(rt.state.sidebar.width(), 240.0, "double-click resets the width");
}

#[test]
fn the_shortcut_collapses_on_a_spring_and_reduced_motion_snaps() {
    let mut rt = runtime(state());
    rt.key(toggle_shortcut());
    rt.frame(1.0 / 60.0);
    assert!(!rt.state.sidebar.is_open());
    let w = find(&rt, "sidebar-pane").rect[2];
    assert!(w > 0.0 && w < 240.0, "mid-spring: {w}");
    settle(&mut rt);
    assert_eq!(find(&rt, "sidebar-pane").rect[2], 0.0);
    assert!(!rt.elements().iter().any(|e| e.key.as_deref() == Some("sidebar-grip")), "no grip while collapsed");
    press(&mut rt, Key::Tab);
    let focused = rt.elements().into_iter().find(|e| e.focused).and_then(|e| e.key);
    assert_ne!(focused.as_deref(), Some("row1"), "a collapsed sidebar's rows are out of the Tab order");

    rt.set_system_reduced_motion(true);
    rt.key(toggle_shortcut());
    rt.frame(1.0 / 60.0);
    assert!(rt.state.sidebar.is_open());
    assert_eq!(find(&rt, "sidebar-pane").rect[2], 240.0, "Reduce Motion snaps it open");
    assert!(!is_sidebar_toggle(&KeyEvent { key: Key::Command('b'), mods: Mods { cmd: true, ..Mods::default() } }));
}

#[test]
fn the_list_is_one_tab_stop_whose_arrows_and_typing_move_the_selection() {
    let mut rt = runtime(state());
    let focused = |rt: &Runtime<S>| rt.elements().into_iter().find(|e| e.focused).and_then(|e| e.key);
    press(&mut rt, Key::Tab);
    assert_eq!(focused(&rt).as_deref(), Some("row1"), "Tab lands on the selected row");
    press(&mut rt, Key::Down);
    assert_eq!((focused(&rt).as_deref(), rt.state.picked), (Some("row2"), 2), "selection follows focus");
    for _ in 0..4 {
        press(&mut rt, Key::Down);
    }
    assert_eq!(rt.state.picked, 3, "stops at the end");
    press(&mut rt, Key::Home);
    assert_eq!(rt.state.picked, 0);
    press(&mut rt, Key::Text("c".into()));
    assert_eq!(rt.state.picked, 2, "type-select");
    rt.state.picked = 0;
    press(&mut rt, Key::Enter);
    assert_eq!(rt.state.picked, 2, "Return presses the focused row");
}

#[test]
fn long_lists_build_only_the_rows_near_the_viewport() {
    let mut rt = runtime(S { rows: Some(10_000), ..state() });
    settle(&mut rt);
    let rows = rt.elements().into_iter().filter(|e| e.role == "listitem").count();
    assert!((10..300).contains(&rows), "{rows} rows built");
    assert!(rt.find("row0").is_some() && rt.find("row9999").is_none());
    press(&mut rt, Key::Tab);
    press(&mut rt, Key::Down);
    assert_eq!(rt.state.picked, 2, "the keyboard works in a virtualised list");
}

#[test]
fn roles_and_labels_describe_the_shell() {
    let mut rt = runtime(state());
    let all = rt.elements();
    let role = |key: &str| all.iter().find(|e| e.key.as_deref() == Some(key)).map(|e| (e.role, e.label.clone()));
    assert_eq!(role("sidebar-list"), Some(("list", Some("Sessions".into()))));
    assert_eq!(role("row2"), Some(("listitem", Some("Charlie".into()))));
    assert_eq!(role("section-header"), Some(("button", Some("Today, expanded".into()))));
    assert_eq!(role("toolbar").map(|r| r.0), Some("toolbar"));
    assert_eq!(role("sidebar-grip"), Some(("separator", Some("Resize sidebar".into()))));
    assert_eq!(role("sidebar-toggle"), Some(("button", Some("Hide sidebar".into()))));
    let fill = |key: &str| all.iter().find(|e| e.key.as_deref() == Some(key)).unwrap().fill.a;
    assert!(fill("row1") > 0.0 && fill("row0") == 0.0, "the selected row is filled");

    let (x, y) = rt.find("section-header").unwrap();
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    settle(&mut rt);
    assert!(rt.state.collapsed);
    assert_eq!(find(&rt, "section-header").label.as_deref(), Some("Today, collapsed"));
    assert!(rt.find("row0").is_none(), "a collapsed section hides its rows");
}

#[test]
fn the_title_bar_clears_the_traffic_lights_and_drags_the_window() {
    let mut rt = runtime(state());
    let inset = Chrome::DEFAULT.titlebar_inset();
    assert!(inset > 0.0);
    rt.set_titlebar_inset(inset);
    rt.set_system_reduced_motion(true);
    rt.frame(1.0 / 60.0);
    assert_eq!(find(&rt, "sidebar-toggle").rect[0], inset + 8.0, "the toggle sits past the lights");
    rt.key(toggle_shortcut());
    rt.frame(1.0 / 60.0);
    assert_eq!(find(&rt, "lead").rect[0], inset + 8.0 + 24.0 + 8.0, "a collapsed sidebar's toolbar clears lights and toggle");

    rt.set_titlebar_inset(0.0);
    rt.frame(1.0 / 60.0);
    assert_eq!(find(&rt, "sidebar-toggle").rect[0], 12.0, "full screen has no lights to clear");
    assert_eq!(find(&rt, "lead").rect[0], 12.0 + 24.0 + 8.0);

    for (key, drags) in [("title", true), ("sidebar-toggle", false), ("lead", true)] {
        let (x, y) = rt.find(key).unwrap();
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        assert_eq!(rt.take_titlebar_action() == Some(TitlebarAction::Drag), drags, "{key}");
        rt.pointer_up(x, y);
        rt.frame(1.0 / 60.0);
    }
    assert!(rt.state.sidebar.is_open(), "the toggle button opened the sidebar again");
}

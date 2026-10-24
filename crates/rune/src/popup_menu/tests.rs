use super::*;
use crate::{Cx, El, Env, el};

#[derive(Default)]
struct State { theme: String, picks: Vec<Option<String>>, anchor: Option<PopupAnchor> }

fn theme_menu(selected: &str) -> PopupMenu {
    PopupMenu::picker([("system", "System"), ("light", "Light"), ("dark", "Dark")], selected)
}

fn record(s: &mut State, picked: Option<&str>, _: &mut Tx) {
    s.picks.push(picked.map(str::to_owned));
    if let Some(id) = picked { s.theme = id.into(); }
}

fn view(_: &State, _: &mut Cx<State>) -> El {
    el()
        .child(el().key("theme").size(80.0, 20.0).pin(Some(40.0), Some(30.0), None, None)
            .on_press_tx(|s: &mut State, tx: &mut Tx| {
                let anchor = s.anchor.clone().unwrap_or_else(|| "theme".into());
                tx.popup_menu(theme_menu(&s.theme), anchor, record);
            }))
        .child(el().key("canvas").size(100.0, 40.0).pin(Some(0.0), Some(60.0), None, None)
            .on_secondary_press(|_: &mut State, press: PointerPress, tx: &mut Tx| {
                let menu = PopupMenu::new().item(PopupItem::new("copy", "Copy")).separator()
                    .item(PopupItem::new("paste", "Paste").enabled(false))
                    .submenu("More", PopupMenu::new().item(PopupItem::new("share", "Share")));
                tx.popup_menu(menu, press, record);
            }))
}

fn runtime() -> Runtime<State> {
    let mut rt = Runtime::new(State { theme: "system".into(), ..State::default() }, view, (200.0, 120.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt
}

fn click(rt: &mut Runtime<State>, key: &str) {
    let (x, y) = rt.find(key).unwrap();
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(0.0);
}

#[test]
fn a_press_handler_asks_for_a_menu_that_opens_over_its_anchor() {
    let mut rt = runtime();
    click(&mut rt, "theme");
    assert_eq!(rt.popup_menu(), Some(&theme_menu("system")));
    let show = rt.take_popup().unwrap().unwrap();
    assert_eq!(show.positioned.as_deref(), Some("system"));
    assert_eq!((show.at, show.min_width), ((40.0, 30.0), 80.0), "the current value covers the button, at least as wide");
    assert_eq!(rt.take_popup(), None, "one menu at a time");

    rt.resolve_popup(Some("dark")).unwrap();
    assert_eq!(rt.state.picks, [Some("dark".to_owned())]);
    assert_eq!(rt.popup_menu(), None);
    rt.frame(0.0);
    assert_eq!(rt.state.theme, "dark");
}

#[test]
fn a_menu_without_a_selection_drops_down_below_its_element() {
    let mut rt = runtime();
    rt.update(|_, tx| tx.popup_menu(PopupMenu::new().item(PopupItem::new("a", "A")), "theme", record));
    let show = rt.take_popup().unwrap().unwrap();
    assert_eq!((show.at, show.positioned, show.min_width), ((40.0, 50.0), None, 80.0));
}

#[test]
fn a_context_menu_opens_at_the_press() {
    let mut rt = runtime();
    rt.secondary_down(30.0, 70.0);
    rt.secondary_up(30.0, 70.0);
    rt.frame(0.0);
    let show = rt.take_popup().unwrap().unwrap();
    assert_eq!((show.at, show.positioned, show.min_width), ((30.0, 70.0), None, 0.0));
    assert_eq!(rt.resolve_popup(Some("paste")), Err(PopupMenuError::Disabled("paste".into())));
    assert_eq!(rt.resolve_popup(Some("nope")), Err(PopupMenuError::UnknownItem("nope".into())));
    assert!(rt.popup_menu().is_some(), "a refused answer leaves the menu open");
    rt.resolve_popup(Some("share")).unwrap();
    assert_eq!(rt.state.picks, [Some("share".to_owned())], "submenu items answer too");
    assert_eq!(rt.resolve_popup(None), Err(PopupMenuError::NotOpen));
}

#[test]
fn a_point_anchor_with_a_selection_puts_that_item_under_the_point() {
    let mut rt = runtime();
    rt.state.anchor = Some((5.0, 6.0).into());
    click(&mut rt, "theme");
    let show = rt.take_popup().unwrap().unwrap();
    assert_eq!((show.at, show.positioned.as_deref(), show.min_width), ((5.0, 6.0), Some("system"), 0.0));
}

#[test]
fn a_menu_that_cannot_open_answers_none() {
    let mut rt = runtime();
    rt.update(|_, tx| tx.popup_menu(theme_menu("system"), "missing", record));
    rt.update(|_, tx| tx.popup_menu(PopupMenu::new().item(PopupItem::new("a", "A")).item(PopupItem::new("a", "B")), "theme", record));
    rt.update(|_, tx| tx.popup_menu(PopupMenu::new().item(PopupItem::new("", "Blank")), "theme", record));
    rt.update(|_, tx| tx.popup_menu(theme_menu("light"), "theme", record));
    rt.open_popup_headless();
    assert_eq!(rt.state.picks, [None, None, None], "a missing anchor, a repeated id and an empty id each answer None");
    assert_eq!(rt.popup_menu(), Some(&theme_menu("light")), "the next valid menu opens");
    assert!(rt.popup_open);
}

#[test]
fn menus_asked_for_together_open_one_after_another() {
    let mut rt = runtime();
    rt.update(|_, tx| {
        tx.popup_menu(theme_menu("system"), "theme", record);
        tx.popup_menu(theme_menu("dark"), "theme", record);
    });
    rt.open_popup_headless();
    rt.resolve_popup(None).unwrap();
    assert_eq!(rt.popup_menu(), Some(&theme_menu("dark")));
    assert!(!rt.popup_open, "the next waits for the host to open it");
    rt.open_popup_headless();
    rt.resolve_popup(Some("light")).unwrap();
    assert_eq!(rt.state.picks, [None, Some("light".to_owned())]);
}

#[test]
fn a_pick_can_ask_for_the_next_menu() {
    let mut rt = runtime();
    rt.update(|_, tx| tx.popup_menu(theme_menu("system"), "theme", |s: &mut State, picked, tx: &mut Tx| {
        record(s, picked, tx);
        tx.popup_menu(theme_menu("dark"), "theme", record);
    }));
    rt.open_popup_headless();
    rt.resolve_popup(Some("dark")).unwrap();
    assert_eq!(rt.popup_menu(), Some(&theme_menu("dark")));
}

#[test]
fn clones_of_a_tx_share_one_request() {
    let mut tx = Tx::default();
    tx.popup_menu(theme_menu("system"), "theme", record);
    let clone = tx.clone();
    assert!(clone.popups[0].take().is_some());
    assert!(tx.popups[0].take().is_none(), "taken once, whichever clone the runtime reads");
}

#[test]
fn the_tree_shows_the_open_menu() {
    let mut rt = runtime();
    assert!(crate::tooling::tree_json(&rt, 0.0).get("popupMenu").is_none());
    rt.update(|_, tx| tx.popup_menu(PopupMenu::new().item(PopupItem::new("copy", "Copy").checked(true).symbol("doc.on.doc")
        .key_equivalent("super+c".parse().unwrap())).separator(), (3.0, 4.0), record));
    rt.update(|_, tx| tx.popup_menu(theme_menu("system"), "theme", record));
    rt.open_popup_headless();
    let tree = crate::tooling::tree_json(&rt, 0.0);
    let menu = &tree["popupMenu"];
    assert_eq!(menu["open"], true);
    assert_eq!(menu["queued"], 1);
    assert_eq!(menu["anchor"], json!({ "x": 3.0, "y": 4.0 }));
    assert_eq!(menu["items"][0]["id"], "copy");
    assert_eq!(menu["items"][0]["checked"], true);
    assert_eq!(menu["items"][0]["symbol"], "doc.on.doc");
    assert!(menu["items"][0]["keyEquivalent"].is_string());
    assert_eq!(menu["items"][1], json!({ "separator": true }));
}

#[test]
fn support_is_macos_only() {
    assert_eq!(popup_menu_supported(), cfg!(target_os = "macos"));
}

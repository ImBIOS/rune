use rune::{Align, Cx, El, Env, Key, KeyEvent, Mods, Runtime, Side, TextEdit, col, icon, text};
use rune_kit::{Kit, PopupItem, PopupList, TextInput, root};

#[derive(Default)]
struct State {
    composer: TextEdit,
    open: bool,
    count: usize,
    active: Option<usize>,
    hovered: Vec<usize>,
    chosen: Vec<String>,
}

fn item_for(k: &Kit, i: usize) -> PopupItem {
    PopupItem::new(format!("cmd-{i}"), format!("/command-{i:02}"))
        .description(format!("Action {i}"))
        .leading(icon(&rune_kit::glyphs::SPARKLE, 14.0).color(k.c.icon_muted))
        .trailing(text("⏎").font(k.ty.small_medium).color(k.c.text_faint))
        .disabled(i == 1)
}

fn view(s: &State, _: &mut Cx<State>) -> El {
    let k = Kit::neutral(true);
    let mut layer = Vec::new();
    let field = TextInput::new(&s.composer)
        .placeholder("Type a command…")
        .animate_edit_layout()
        .on_edit(|s: &mut State| &mut s.composer)
        .on_key(|s: &mut State, ev, _| match ev.key {
            Key::Down => {
                if s.count > 0 {
                    s.active = Some((s.active.unwrap_or(0) + 1) % s.count);
                }
                true
            }
            Key::Up => {
                if s.count > 0 {
                    s.active = Some(s.active.unwrap_or(0).checked_sub(1).unwrap_or(s.count - 1));
                }
                true
            }
            _ => false,
        })
        .build(&k)
        .key("composer");
    let items: Vec<_> = (0..s.count).map(|i| item_for(&k, i)).collect();
    PopupList::new("commands", "composer")
        .anchored("composer", Side::Bottom, Align::Start, 4.0)
        .open(s.open)
        .items(items)
        .active(s.active)
        .empty_message("No matching commands")
        .on_active(|s: &mut State, i| {
            s.active = Some(i);
            s.hovered.push(i);
        })
        .on_choose(|s: &mut State, id, _| {
            s.chosen.push(id.into());
        })
        .build(&k, &mut layer);
    root(col().padding(16.0).child(field), layer)
}

fn runtime(count: usize) -> Runtime<State> {
    let mut composer = TextEdit::new("");
    composer.focused = true;
    let s = State {
        composer,
        open: true,
        count,
        active: Some(0),
        ..State::default()
    };
    let mut rt = Runtime::new(s, view, (480.0, 480.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt
}

#[test]
fn option_changes_5_2_0_3_spring_height_fade_rows_and_toggle_empty_state() {
    let mut rt = runtime(5);
    for _ in 0..60 {
        rt.frame(1.0 / 60.0);
    }
    let find = |rt: &Runtime<State>, key: &str| rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key));
    let h5 = find(&rt, "commands-content").unwrap().rect[3];
    assert_eq!(find(&rt, "empty").unwrap().opacity, 0.0);
    assert_eq!(find(&rt, "empty").unwrap().rect[3], 0.0);

    // 5 -> 2 items: height springs down and removed rows fade out.
    rt.update(|s, _| s.count = 2);
    rt.frame(1.0 / 60.0);
    let fading = find(&rt, "commands-option-cmd-4").expect("removed row stays mounted while fading out");
    assert!(fading.opacity > 0.0 && fading.opacity < 1.0, "row opacity {}", fading.opacity);
    let mid_5_to_2 = find(&rt, "commands-content").unwrap().rect[3];
    assert!(mid_5_to_2 < h5 && mid_5_to_2 > 64.0, "height springs from {h5}, got {mid_5_to_2}");
    let mut last = mid_5_to_2;
    for _ in 0..60 {
        rt.frame(1.0 / 60.0);
        let h = find(&rt, "commands-content").unwrap().rect[3];
        assert!((h - last).abs() < 25.0, "popup height jumped {last} -> {h}");
        last = h;
    }
    let h2 = last;
    assert!((h2 - 64.0).abs() < 1.0, "settled 2-item height {h2}");
    assert_eq!(find(&rt, "empty").unwrap().opacity, 0.0);

    // 2 -> 0 items: height springs to empty state and empty message becomes visible.
    rt.update(|s, _| s.count = 0);
    rt.frame(1.0 / 60.0);
    assert!(find(&rt, "empty").unwrap().opacity > 0.0);
    let mid_2_to_0 = find(&rt, "commands-content").unwrap().rect[3];
    assert!(mid_2_to_0 < h2 && mid_2_to_0 > 36.0, "height springs from {h2} toward empty, got {mid_2_to_0}");
    for _ in 0..60 {
        rt.frame(1.0 / 60.0);
    }
    let h0 = find(&rt, "commands-content").unwrap().rect[3];
    assert!((h0 - 36.0).abs() < 1.0, "settled empty height {h0}");
    assert_eq!(find(&rt, "empty").unwrap().opacity, 1.0);
    assert!((find(&rt, "empty").unwrap().rect[3] - 28.0).abs() < 1.0);

    // 0 -> 3 items: height springs back up and empty state hides.
    rt.update(|s, _| {
        s.count = 3;
        s.active = Some(0);
    });
    rt.frame(1.0 / 60.0);
    assert!(find(&rt, "empty").unwrap().opacity < 1.0);
    let mid_0_to_3 = find(&rt, "commands-content").unwrap().rect[3];
    assert!(mid_0_to_3 > h0 && mid_0_to_3 < 92.0, "height springs from {h0} toward 3 items, got {mid_0_to_3}");
    for _ in 0..60 {
        rt.frame(1.0 / 60.0);
    }
    let h3 = find(&rt, "commands-content").unwrap().rect[3];
    assert!((h3 - 92.0).abs() < 1.0, "settled 3-item height {h3}");
    assert_eq!(find(&rt, "empty").unwrap().opacity, 0.0);
}

#[test]
fn active_index_scrolls_into_view_and_updates_listbox_accessibility() {
    let mut rt = runtime(40);
    let find = |rt: &Runtime<State>, key: &str| rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key));
    let listbox = find(&rt, "viewport").unwrap();
    assert_eq!(listbox.role, "listbox");
    assert_eq!(listbox.label.as_deref(), Some("/command-00"));
    let first = find(&rt, "commands-option-cmd-0").unwrap();
    assert_eq!(first.role, "option");
    assert!(first.item_selected);

    // Moving active to the last item (via Up key in the outside field) reveals the offscreen row.
    rt.key(KeyEvent { key: Key::Up, mods: Mods::default() });
    rt.frame(0.0);
    assert_eq!(rt.state.active, Some(39));
    let last = find(&rt, "commands-option-cmd-39").expect("active virtual row is mounted and revealed");
    assert!(last.visible[3] > 0.0, "last row visible rect {:?}", last.visible);
    assert!(last.item_selected);
    let listbox = find(&rt, "viewport").unwrap();
    assert_eq!(listbox.label.as_deref(), Some("/command-39"));
}

#[test]
fn hover_fires_on_active_click_fires_on_choose_and_outside_text_edit_keeps_focus() {
    let mut rt = runtime(5);
    for _ in 0..60 {
        rt.frame(1.0 / 60.0);
    }
    // Typing while open goes straight to the outside TextEdit without moving focus into the popup.
    rt.key(KeyEvent { key: Key::Text("/re".into()), mods: Mods::default() });
    rt.frame(0.0);
    assert_eq!(rt.state.composer.text, "/re");
    assert!(rt.state.composer.focused);
    assert!(rt.elements().iter().filter(|e| e.role == "option").all(|e| !e.focusable && !e.focused));

    // Hovering option 2 fires on_active(s, 2).
    let (x2, y2) = rt.find("commands-option-cmd-2").unwrap();
    rt.pointer_move(x2, y2);
    rt.frame(0.0);
    assert_eq!(rt.state.hovered, [2]);
    assert_eq!(rt.state.active, Some(2));
    assert_eq!(
        rt.elements().into_iter().find(|e| e.key.as_deref() == Some("viewport")).unwrap().label.as_deref(),
        Some("/command-02")
    );

    // Clicking disabled option 1 neither chooses nor blurs the outside TextEdit.
    let (x1, y1) = rt.find("commands-option-cmd-1").unwrap();
    rt.pointer_down(x1, y1);
    rt.pointer_up(x1, y1);
    rt.frame(0.0);
    assert!(rt.state.chosen.is_empty());
    assert!(rt.state.composer.focused);

    // Clicking enabled option 2 fires on_choose and leaves focus in the outside TextEdit so typing continues.
    rt.pointer_down(x2, y2);
    rt.pointer_up(x2, y2);
    rt.frame(0.0);
    assert_eq!(rt.state.chosen, ["cmd-2"]);
    assert!(rt.state.composer.focused);
    rt.key(KeyEvent { key: Key::Text("view".into()), mods: Mods::default() });
    rt.frame(0.0);
    assert_eq!(rt.state.composer.text, "/review");
}

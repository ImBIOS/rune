//! Pressing anywhere on a `TextInput`, `TextField` or `TextArea` box focuses its editor,
//! places the caret at the nearest end, never blurs a focused field or starts a window drag,
//! and leaves trailing action buttons with their own presses.

use rune::runtime::ElementInfo;
use rune::{Cx, El, Env, Runtime, TextEdit, col, row, text};
use rune_kit::{IconButton, IconButtonVariant, Kit, TextArea, TextField, TextInput};

struct S {
    input: TextEdit,
    field: TextEdit,
    area: TextEdit,
    actions: usize,
    revealed: bool,
    copied: usize,
}

#[test]
fn textbox_padding_requires_exactly_one_reachable_editor() {
    struct Pair { first: TextEdit, second: TextEdit, hide_second: bool }
    fn view(s: &Pair, _: &mut Cx<Pair>) -> El {
        row().role("textbox").key("box").size(400.0, 60.0).padding(10.0)
            .child(rune::text_input(&s.first, rune::TextStyle::new(13.0, 400.0))
                .key("first").size(120.0, 30.0).on_edit(|s: &mut Pair| &mut s.first))
            .child(col().opacity(if s.hide_second { 0.0 } else { 1.0 }).child(
                rune::text_input(&s.second, rune::TextStyle::new(13.0, 400.0))
                    .key("second").size(120.0, 30.0).on_edit(|s: &mut Pair| &mut s.second)))
    }
    let mut rt = Runtime::new(Pair { first: TextEdit::new("one"), second: TextEdit::new("two"), hide_second: false },
        view, (440.0, 100.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt.pointer_down(2.0, 2.0);
    rt.pointer_up(2.0, 2.0);
    rt.frame(0.0);
    assert!(!rt.state.first.focused && !rt.state.second.focused, "padding cannot choose between two editors");
    rt.update(|s, tx| { s.hide_second = true; tx.instant(); });
    rt.frame(0.0);
    rt.pointer_down(2.0, 2.0);
    rt.pointer_up(2.0, 2.0);
    rt.frame(0.0);
    assert!(rt.state.first.focused && !rt.state.second.focused, "hidden ancestors exclude an editor");
}

#[test]
fn a_copy_button_without_a_handler_still_keeps_its_press() {
    fn view(s: &TextEdit, _: &mut Cx<TextEdit>) -> El {
        TextInput::new(s).copy_button().on_edit(|s: &mut TextEdit| s).build(&Kit::neutral(true))
    }
    let mut edit = TextEdit::new("text");
    edit.focused = true;
    edit.set_caret(1);
    let mut rt = Runtime::new(edit, view, (400.0, 100.0), Env::new(true, 1.0));
    rt.frame(0.0);
    let (x, y) = rt.find("action").unwrap();
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(0.0);
    assert!(rt.state.focused);
    assert_eq!(rt.state.range(), (1, 1), "the button press must not place the input's caret");
}

#[test]
fn padding_focus_reaches_an_editor_inside_an_auto_grow_scroll() {
    fn view(s: &TextEdit, _: &mut Cx<TextEdit>) -> El {
        TextArea::new(s).auto_grow(2, 2).on_edit(|s: &mut TextEdit| s)
            .build(&Kit::neutral(true)).key("area")
    }
    let mut rt = Runtime::new(TextEdit::new("one\ntwo\nthree\nfour"), view, (400.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt.pointer_down(2.0, 2.0);
    rt.pointer_up(2.0, 2.0);
    rt.frame(0.0);
    assert!(rt.state.focused);
    assert_eq!(rt.state.range(), (0, 0));
}

fn state() -> S {
    S {
        input: TextEdit::new("hunter2"),
        field: TextEdit::new("vault"),
        area: TextEdit::new("notes"),
        actions: 0,
        revealed: false,
        copied: 0,
    }
}

fn view(s: &S, _: &mut Cx<S>) -> El {
    let k = Kit::neutral(true);
    let accessories = row()
        .key("accessories")
        .gap(6.0)
        .px(4.0)
        .center_y()
        .child(text("pw").key("hint"))
        .child(
            IconButton::new(k.glyph.check)
                .variant(IconButtonVariant::Ghost)
                .label("Reveal")
                .on_press(|s: &mut S| s.revealed = !s.revealed)
                .build(&k)
                .key("reveal"),
        );
    col()
        .w_full()
        .h_full()
        .padding(16.0)
        .gap(16.0)
        .child(
            row().key("titlebar").w_full().h(48.0).window_drag().center_y().px(12.0).child(
                TextInput::new(&s.input)
                    .placeholder("Search")
                    .leading_icon(k.glyph.search)
                    .trailing(accessories)
                    .clear_button()
                    .on_edit(|s: &mut S| &mut s.input)
                    .on_action(|s: &mut S, _| s.actions += 1)
                    .build(&k)
                    .key("ti"),
            ),
        )
        .child(
            TextField::new("Label", &s.field)
                .copyable()
                .on_edit(|s: &mut S| &mut s.field)
                .on_action(|s: &mut S, _| s.copied += 1)
                .build(&k)
                .key("tf"),
        )
        .child(TextArea::new(&s.area).on_edit(|s: &mut S| &mut s.area).build(&k).key("ta"))
}

fn find(rt: &Runtime<S>, key: &str) -> ElementInfo {
    rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap()
}

fn click(rt: &mut Runtime<S>, x: f32, y: f32) {
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(1.0 / 60.0);
}

#[test]
fn pressing_any_part_of_a_field_focuses_without_blurring_or_dragging() {
    let mut rt = Runtime::new(state(), view, (480.0, 400.0), Env::new(true, 2.0));
    rt.frame(0.0);

    let ti = find(&rt, "ti");
    let lead = find(&rt, "leading");
    let hint = find(&rt, "hint");
    let reveal = find(&rt, "reveal");
    let clear = find(&rt, "action");

    // 1. Clicking the leading icon focuses the field, puts the caret at the start, and does not drag the window.
    click(&mut rt, lead.rect[0] + lead.rect[2] * 0.5, lead.rect[1] + lead.rect[3] * 0.5);
    assert!(rt.take_titlebar_action().is_none(), "pressing a field in a titlebar never drags the window");
    assert!(rt.state.input.focused, "leading icon focuses the field");
    assert_eq!(rt.state.input.range(), (0, 0), "leading icon places caret at start");

    // 2. While already focused, clicking the trailing accessory area (not a button) or top/bottom padding keeps focus and moves the caret to the end.
    click(&mut rt, hint.rect[0] + hint.rect[2] * 0.5, ti.rect[1] + 2.0);
    assert!(rt.take_titlebar_action().is_none());
    assert!(rt.state.input.focused, "clicking trailing accessory area or top padding must not blur");
    assert_eq!(rt.state.input.range(), (7, 7), "trailing side places caret at end");

    // Clicking the bottom-left padding moves the caret back to the start without blurring.
    click(&mut rt, ti.rect[0] + 3.0, ti.rect[1] + ti.rect[3] - 2.0);
    assert!(rt.state.input.focused, "clicking bottom-left padding must not blur");
    assert_eq!(rt.state.input.range(), (0, 0), "leading padding places caret at start");

    // 3. Trailing buttons still take their own press.
    click(&mut rt, reveal.rect[0] + reveal.rect[2] * 0.5, reveal.rect[1] + reveal.rect[3] * 0.5);
    assert!(rt.state.revealed, "trailing reveal button takes its own press");
    click(&mut rt, clear.rect[0] + clear.rect[2] * 0.5, clear.rect[1] + clear.rect[3] * 0.5);
    assert_eq!(rt.state.actions, 1, "clear button takes its own press");

    // 4. TextField: leading and trailing padding focus and place the caret; copy button takes its own press.
    let wrapper = find(&rt, "wrapper");
    let copy = find(&rt, "copy");
    click(&mut rt, wrapper.rect[0] + wrapper.rect[2] - 30.0, wrapper.rect[1] + 2.0);
    assert!(rt.state.field.focused, "TextField padding focuses the field");
    assert_eq!(rt.state.field.range(), (5, 5), "TextField trailing padding places caret at end");
    click(&mut rt, wrapper.rect[0] + 3.0, wrapper.rect[1] + wrapper.rect[3] - 2.0);
    assert!(rt.state.field.focused, "TextField leading padding keeps focus");
    assert_eq!(rt.state.field.range(), (0, 0), "TextField leading padding places caret at start");
    click(&mut rt, copy.rect[0] + copy.rect[2] * 0.5, copy.rect[1] + copy.rect[3] * 0.5);
    assert_eq!(rt.state.copied, 1, "TextField copy button takes its own press");

    // 5. TextArea: bottom/trailing padding focuses at end; leading padding focuses at start without blurring.
    let ta = find(&rt, "ta");
    click(&mut rt, ta.rect[0] + ta.rect[2] - 4.0, ta.rect[1] + ta.rect[3] - 4.0);
    assert!(rt.state.area.focused, "TextArea bottom-right padding focuses the field");
    assert_eq!(rt.state.area.range(), (5, 5), "TextArea trailing/bottom padding places caret at end");
    click(&mut rt, ta.rect[0] + 2.0, ta.rect[1] + 4.0);
    assert!(rt.state.area.focused, "TextArea leading padding keeps focus");
    assert_eq!(rt.state.area.range(), (0, 0), "TextArea leading padding places caret at start");
}

#[test]
fn a_launcher_size_style_scales_the_box_and_the_caret() {
    struct Launcher {
        plain: TextEdit,
        big: TextEdit,
        launcher: TextEdit,
        large_small_style: TextEdit,
        tight: TextEdit,
    }
    fn view(s: &Launcher, _: &mut Cx<Launcher>) -> El {
        let k = Kit::neutral(true);
        let style_24 = rune::TextStyle::new(17.0, 400.0).lh(24.0);
        col()
            .child(TextInput::new(&s.plain).on_edit(|s: &mut Launcher| &mut s.plain).build(&k).key("plain"))
            .child(
                TextInput::new(&s.big)
                    .placeholder("Search")
                    .text_style(style_24)
                    .on_edit(|s: &mut Launcher| &mut s.big)
                    .build(&k)
                    .key("big"),
            )
            .child(
                TextInput::new(&s.launcher)
                    .placeholder("Type a command")
                    .leading_icon(k.glyph.search)
                    .clear_button()
                    .text_style(style_24)
                    .height(56.0)
                    .on_edit(|s: &mut Launcher| &mut s.launcher)
                    .build(&k)
                    .key("launcher"),
            )
            .child(
                TextInput::new(&s.large_small_style)
                    .large()
                    .text_style(rune::TextStyle::new(13.0, 500.0).lh(16.0))
                    .build(&k)
                    .key("large-compact"),
            )
            .child(
                TextInput::new(&s.tight)
                    .text_style(style_24)
                    .height(10.0)
                    .build(&k)
                    .key("tight"),
            )
    }
    let state = Launcher {
        plain: TextEdit::new("hi"),
        big: TextEdit::new("hi"),
        launcher: TextEdit::new("hi"),
        large_small_style: TextEdit::new("hi"),
        tight: TextEdit::new("hi"),
    };
    let mut rt = Runtime::new(state, view, (400.0, 300.0), Env::new(true, 1.0));
    rt.frame(0.0);
    let by_path = |rt: &Runtime<Launcher>, path: &str| rt.elements().into_iter().find(|e| e.path.ends_with(path)).unwrap();
    let h = |rt: &Runtime<Launcher>, key: &str| rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap().rect[3];
    assert_eq!(
        (h(&rt, "plain"), h(&rt, "big"), h(&rt, "launcher"), h(&rt, "large-compact"), h(&rt, "tight")),
        (28.0, 36.0, 56.0, 32.0, 24.0),
        "box grows with style, keeps large's 32 floor, and clamps height to at least line_px"
    );
    let line = |rt: &Runtime<Launcher>| rt.elements().into_iter().filter(|e| e.key.as_deref() == Some("input")).map(|e| e.rect[3]).collect::<Vec<_>>();
    assert_eq!(line(&rt), [16.0, 24.0, 24.0, 16.0, 24.0], "the editor's line follows the style");

    // In the 56 pt launcher box, the 24 pt editor, leading icon, and clear button are all vertically centred.
    let box_rect = by_path(&rt, "launcher").rect;
    let input_rect = by_path(&rt, "launcher/value/input").rect;
    let lead_rect = by_path(&rt, "launcher/value/leading").rect;
    let action_rect = by_path(&rt, "launcher/action").rect;
    let box_cy = box_rect[1] + box_rect[3] * 0.5;
    for (name, r) in [("input", input_rect), ("leading", lead_rect), ("action", action_rect)] {
        assert!(((r[1] + r[3] * 0.5) - box_cy).abs() < 0.01, "{name} is vertically centred in the 56 pt box: {r:?} vs {box_rect:?}");
    }

    // Focus the 56 pt launcher field and check that its caret is 24 pt tall and vertically centred.
    let (x, y) = (box_rect[0] + 40.0, box_cy);
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(1.0 / 60.0);
    assert!(rt.state.launcher.focused);
    let caret = by_path(&rt, "launcher/value/input").caret.expect("focused launcher caret");
    assert_eq!(caret[3], 24.0, "caret height matches the 24 pt line height");
    assert!(((caret[1] + caret[3] * 0.5) - box_cy).abs() < 0.01, "caret is vertically centred in the 56 pt box: {caret:?}");
}

#[test]
fn any_unfocusable_element_can_keep_an_outside_field_focused_and_still_press() {
    struct P { field: TextEdit, pressed: usize }
    fn view(s: &P, _: &mut Cx<P>) -> El {
        col().gap(8.0)
            .child(TextInput::new(&s.field).on_edit(|s: &mut P| &mut s.field).build(&Kit::neutral(true)).key("field"))
            .child(row().key("plain").size(100.0, 30.0).on_press(|s: &mut P| s.pressed += 1))
            .child(row().key("keeper").size(100.0, 30.0).preserve_focus_on_press(true).on_press(|s: &mut P| s.pressed += 10))
    }
    let mut field = TextEdit::new("x");
    field.focused = true;
    let mut rt = Runtime::new(P { field, pressed: 0 }, view, (200.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);
    let (x, y) = rt.find("keeper").unwrap();
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.pressed, 10, "the press still reaches on_press");
    assert!(rt.state.field.focused, "the field keeps focus");
    let (x, y) = rt.find("plain").unwrap();
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(1.0 / 60.0);
    assert!(!rt.state.field.focused, "without the flag a press blurs, as before");
}

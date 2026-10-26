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

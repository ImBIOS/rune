use crate::color::Color;
use crate::el::{PillStyle, rich, text, text_input};
use crate::text::{Span, TextSystem};
use parley::Alignment;
use crate::{TextEdit, TextStyle};

const INK: Color = Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };
const CHIP: Color = Color { r: 0.0, g: 0.0, b: 1.0, a: 0.2 };

#[test]
fn pills_record_sorted_chips_and_skip_bad_ranges() {
    let body = "ask @code-reviewer about @src/main.rs — é";
    let at = |s: &str| body.find(s).unwrap();
    let agent = at("@code-reviewer")..at("@code-reviewer") + 14;
    let file = at("@src/main.rs")..at("@src/main.rs") + 12;
    let e = text(body).pills([
        (file.clone(), PillStyle::new().bg(CHIP)),
        (agent.clone(), PillStyle::new().fg(INK)),
        (agent.start + 1..agent.end + 2, PillStyle::new()),
        (5..5, PillStyle::new()),
        (body.len() - 1..body.len(), PillStyle::new()),
        (0..body.len() + 1, PillStyle::new()),
    ]);
    let t = e.text.as_ref().unwrap();
    let got: Vec<(u32, u32, Option<Color>, Option<Color>)> = t.pills.iter().map(|p| (p.start, p.end, p.bg, p.fg)).collect();
    assert_eq!(got, [(agent.start as u32, agent.end as u32, None, Some(INK)), (file.start as u32, file.end as u32, Some(CHIP), None)]);
}

#[test]
fn a_pill_colours_its_label_and_keeps_it_through_later_spans() {
    let body = "ask @reviewer to look";
    let e = rich(body, vec![Span::new(0, 3).bold()]).pill(4..13, PillStyle::new().fg(INK)).spans(vec![Span::new(14, 16).italic()]);
    let t = e.text.as_ref().unwrap();
    let ink: Vec<(u32, u32)> = t.spans.iter().filter(|s| s.color == Some(INK)).map(|s| (s.start, s.end)).collect();
    assert_eq!(ink, [(4, 13)]);
    assert!(t.spans.iter().any(|s| s.italic && (s.start, s.end) == (14, 16)));
}

#[test]
fn a_field_ignores_static_pills() {
    let e = text_input(&TextEdit::new("ask @reviewer"), TextStyle::new(14.0, 400.0)).pill(4..13, PillStyle::new().fg(INK));
    assert!(e.text.as_ref().unwrap().pills.is_empty());
}

#[test]
fn a_pill_label_stays_on_one_line_and_takes_room() {
    let body = "please ask @recursive-self-improvement about it";
    let at = body.find('@').unwrap();
    let end = at + "@recursive-self-improvement".len();
    let mut ts = TextSystem::default();
    let plain = text(body).wrap();
    let pilled = text(body).wrap().pill(at..end, PillStyle::new());
    let mut width = |e: &crate::El, w| ts.layout_for_spec(e.text.as_ref().unwrap(), 1.0, w, Alignment::Start).full_width();
    let (wide_plain, wide_pill) = (width(&plain, None), width(&pilled, None));
    assert!(wide_pill > wide_plain + 4.0, "the chip's padding takes room: {wide_plain} vs {wide_pill}");
    // Narrow enough that plain text breaks at the label's hyphens.
    let layout = ts.layout_for_spec(pilled.text.as_ref().unwrap(), 1.0, Some(150.0), Alignment::Start);
    let split = layout.lines().any(|l| {
        let r = l.text_range();
        r.start > at && r.start < end
    });
    assert!(!split, "the label never splits across lines");
}

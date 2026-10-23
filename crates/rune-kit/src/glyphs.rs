//! The small built-in glyphs components draw (check marks, chevrons, close). Geometric
//! shapes, not an icon catalog; a theme may replace any of them through `Kit::glyph`.

use rune::Icon;

use crate::kit::Glyphs;

pub static CHECK: Icon = Icon {
    name: "kit-check",
    view: 12.0,
    body: r#"<path d="M3 7.17905L5.02703 8.85135L9 3.5" stroke="currentColor" stroke-width="1.5" stroke-linecap="square"/>"#,
};
pub static CHECK_SMALL: Icon = Icon {
    name: "kit-check-small",
    view: 16.0,
    body: r#"<path d="M3.53564 8.17857L6.39279 11.75L12.4642 4.25" stroke="currentColor" stroke-width="1" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
pub static CHEVRON_DOWN: Icon = Icon {
    name: "kit-chevron-down",
    view: 16.0,
    body: r#"<path d="M5 6.5L8 9.5L11 6.5" stroke="currentColor" stroke-width="1" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
pub static CHEVRON_UP: Icon = Icon {
    name: "kit-chevron-up",
    view: 16.0,
    body: r#"<path d="M11 9.5L8 6.5L5 9.5" stroke="currentColor" stroke-width="1" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
pub static CARET_RIGHT: Icon =
    Icon { name: "kit-caret-right", view: 16.0, body: r#"<path d="M6 4L10 8L6 12V4Z" fill="currentColor"/>"# };
pub static TRIANGLE_DOWN: Icon =
    Icon { name: "kit-triangle-down", view: 16.0, body: r#"<path d="M4 6H12L8 10.5L4 6Z" fill="currentColor"/>"# };
pub static CLOSE: Icon = Icon {
    name: "kit-close",
    view: 16.0,
    body: r#"<path d="M4.25 11.75L11.75 4.25" stroke="currentColor"/><path d="M11.75 11.75L4.25 4.25" stroke="currentColor"/>"#,
};
pub static COPY: Icon = Icon {
    name: "kit-copy",
    view: 16.0,
    body: r#"<path d="M4.5 4.5V2.5H13.5V11.5H11.5M2.5 4.5H11.5V13.5H2.5V4.5Z" stroke="currentColor"/>"#,
};
pub static SEARCH: Icon = Icon {
    name: "kit-search",
    view: 16.0,
    body: r#"<path d="M14 14L10.3454 10.3454M6.88889 11.7778C9.58889 11.7778 11.7778 9.58889 11.7778 6.88889C11.7778 4.18889 9.58889 2 6.88889 2C4.18889 2 2 4.18889 2 6.88889C2 9.58889 4.18889 11.7778 6.88889 11.7778Z" stroke="currentColor"/>"#,
};
pub static CIRCLE_X: Icon = Icon {
    name: "kit-circle-x",
    view: 16.0,
    body: r#"<path d="M6 6L10 10M10 6L6 10M14.5 8C14.5 11.5899 11.5899 14.5 8 14.5C4.41015 14.5 1.5 11.5899 1.5 8C1.5 4.41015 4.41015 1.5 8 1.5C11.5899 1.5 14.5 4.41015 14.5 8Z" stroke="currentColor"/>"#,
};

pub static LOCK: Icon = Icon {
    name: "kit-lock",
    view: 16.0,
    body: r#"<rect x="3.5" y="7" width="9" height="7" rx="1" stroke="currentColor"/><path d="M5.5 7V4.5a2.5 2.5 0 0 1 5 0V7M8 10v1.5" stroke="currentColor" stroke-linecap="round"/>"#,
};
pub static EYE: Icon = Icon {
    name: "kit-eye",
    view: 16.0,
    body: r#"<path d="M1 8s2.5-4.5 7-4.5S15 8 15 8s-2.5 4.5-7 4.5S1 8 1 8Z" stroke="currentColor" stroke-linejoin="round"/><circle cx="8" cy="8" r="2" stroke="currentColor"/>"#,
};
pub static EYE_OFF: Icon = Icon {
    name: "kit-eye-off",
    view: 16.0,
    body: r#"<path d="m2 2 12 12M6.2 3.7A8 8 0 0 1 8 3.5C12.5 3.5 15 8 15 8a13 13 0 0 1-2.4 2.8M10.3 12.1a8 8 0 0 1-2.3.4C3.5 12.5 1 8 1 8a13 13 0 0 1 2.5-2.9M6.6 6.6a2 2 0 0 0 2.8 2.8" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

/// `fill-triangle-down` turned a quarter counter-clockwise (a collapsed disclosure).
pub static TRIANGLE_RIGHT: Icon = Icon {
    name: "kit-triangle-right",
    view: 16.0,
    body: r#"<path d="M6.75194 10.62376C6.41861 10.81816 6 10.57776 6 10.19186V5.8079C6 5.422 6.41861 5.1816 6.75194 5.376L10.5096 7.56797C10.8404 7.76091 10.8404 8.23881 10.5096 8.43175L6.75194 10.62376Z" fill="currentColor"/>"#,
};
pub static DASH: Icon =
    Icon { name: "kit-dash", view: 20.0, body: r#"<rect x="5" y="9.5" width="10" height="1" fill="currentColor"/>"# };

pub const DEFAULT: Glyphs = Glyphs {
    check: &CHECK,
    check_small: &CHECK_SMALL,
    chevron_down: &CHEVRON_DOWN,
    chevron_up: &CHEVRON_UP,
    caret_right: &CARET_RIGHT,
    triangle_down: &TRIANGLE_DOWN,
    triangle_right: &TRIANGLE_RIGHT,
    close: &CLOSE,
    clear: &CLOSE,
    copy: &COPY,
    search: &SEARCH,
    lock: &LOCK,
    eye: &EYE,
    eye_off: &EYE_OFF,
    circle_x: &CIRCLE_X,
    dash: &DASH,
};

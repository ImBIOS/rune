//! Sidebar icons the OpenCode catalog doesn't have, drawn in its 16 point stroke style.

use rune::Icon;

pub static SUN: Icon = Icon {
    name: "sun",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="2.75" stroke="currentColor"/><path d="M8 1.5V3M8 13V14.5M1.5 8H3M13 8H14.5M3.4 3.4L4.46 4.46M11.54 11.54L12.6 12.6M3.4 12.6L4.46 11.54M11.54 4.46L12.6 3.4" stroke="currentColor"/>"#,
};

pub static SHIELD: Icon = Icon {
    name: "shield",
    view: 16.0,
    body: r#"<path d="M8 1.75L13 3.5V7.5C13 10.6 10.9 13.1 8 14.25C5.1 13.1 3 10.6 3 7.5V3.5L8 1.75Z" stroke="currentColor" stroke-linejoin="round"/>"#,
};

pub static PERSON: Icon = Icon {
    name: "person-circle",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="6.25" stroke="currentColor"/><circle cx="8" cy="5.25" r="1" fill="currentColor"/><path d="M4.75 7H11.25M8 7V9.25M8 9.25L6.5 12M8 9.25L9.5 12" stroke="currentColor" stroke-linejoin="round"/>"#,
};

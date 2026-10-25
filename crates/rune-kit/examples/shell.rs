//! An app shell from rune-kit's `shell` components: sessions in collapsible sections in a
//! resizable sidebar, a main pane under a toolbar, and an inspector. ⇧⌘B shows and hides the
//! sidebar, ⌥⌘I the inspector; drag a pane's inner edge to resize it, double-click a session
//! to rename it. `-- virtual` swaps the sections for 5,000 sessions in a virtualised list.
//!
//! `cargo run -p rune-kit --example shell` opens the window. The stills in `demo/kit/` are
//! `-- --snap demo/kit/shell-light.png --theme light --size 1040x640` (and `dark`).
use std::borrow::Cow;
use std::collections::HashSet;

use rune::{AppSpec, Chrome, Cx, El, Icon, KeyEvent, TextEdit, Tx, col, text};
use rune_kit::shell::{self, PaneState, Sidebar, SidebarHeader, SidebarRow, SidebarSection, SplitView, Toolbar};
use rune_kit::{IconButton, IconButtonVariant, Kit};

static PLUS: Icon = Icon {
    name: "shell-plus",
    view: 16.0,
    body: r#"<path d="M8 3.5V12.5M3.5 8H12.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#,
};
static SEARCH: Icon = Icon {
    name: "shell-search",
    view: 16.0,
    body: r#"<circle cx="7" cy="7" r="3.75" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M10 10L12.75 12.75" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#,
};
static SLIDERS: Icon = Icon {
    name: "shell-sliders",
    view: 16.0,
    body: r#"<path d="M3 5H13M3 11H13" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/><circle cx="6" cy="5" r="1.6" fill="currentColor"/><circle cx="10.5" cy="11" r="1.6" fill="currentColor"/>"#,
};
static BRANCH: Icon = Icon {
    name: "shell-branch",
    view: 16.0,
    body: r#"<circle cx="5" cy="4" r="1.5" stroke="currentColor" fill="none"/><circle cx="5" cy="12" r="1.5" stroke="currentColor" fill="none"/><circle cx="11" cy="6" r="1.5" stroke="currentColor" fill="none"/><path d="M5 5.5V10.5M11 7.5C11 9.5 5 8.5 5 10.5" stroke="currentColor" fill="none"/>"#,
};

struct Session {
    id: String,
    title: String,
    section: &'static str,
    time: &'static str,
    depth: u8,
    unread: u32,
}

/// A row of the virtualised list: a section's header or a session.
enum Flat {
    Header(&'static str),
    Session(usize),
}

struct App {
    flat: Vec<Flat>,
    sidebar: PaneState,
    inspector: PaneState,
    sessions: Vec<Session>,
    selected: String,
    collapsed: HashSet<&'static str>,
    renaming: Option<String>,
    rename: TextEdit,
    virtualised: bool,
}

const SECTIONS: [&str; 4] = ["Pinned", "Today", "Yesterday", "Last week"];

fn init() -> App {
    let s = |id: &str, title: &str, section, time, depth, unread| Session { id: id.into(), title: title.into(), section, time, depth, unread };
    let mut sessions = vec![
        s("s1", "Release checklist", "Pinned", "", 0, 0),
        s("s2", "Design review notes", "Pinned", "", 0, 2),
        s("s3", "Fix scroll jank in the list", "Today", "4m", 0, 0),
        s("s4", "Profile the frame loop", "Today", "12m", 1, 0),
        s("s5", "Port the settings pane", "Today", "1h", 0, 5),
        s("s6", "Sidebar resize behaviour", "Today", "2h", 0, 0),
        s("s7", "Theme tokens for dark mode", "Yesterday", "Tue", 0, 0),
        s("s8", "Draft the changelog", "Yesterday", "Tue", 0, 0),
        s("s9", "Window restoration bug", "Last week", "Oct 18", 0, 0),
        s("s10", "Keyboard navigation audit", "Last week", "Oct 17", 0, 0),
        s("s11", "Snapshot test flakes", "Last week", "Oct 16", 0, 0),
    ];
    let virtualised = rune::extra_args().iter().any(|a| a == "virtual");
    if virtualised {
        sessions = (0..5000)
            .map(|i| s(&format!("v{i}"), &format!("Session {}", 5000 - i), SECTIONS[(i / 1250).min(3)], "", 0, 0))
            .collect();
    }
    let mut flat = Vec::new();
    for (i, x) in sessions.iter().enumerate() {
        if i == 0 || sessions[i - 1].section != x.section {
            flat.push(Flat::Header(x.section));
        }
        flat.push(Flat::Session(i));
    }
    App {
        flat,
        sidebar: PaneState::new(260.0).range(180.0, 420.0),
        inspector: PaneState::new(280.0).range(220.0, 420.0),
        selected: sessions[2.min(sessions.len() - 1)].id.clone(),
        sessions,
        collapsed: HashSet::from(["Last week"]),
        renaming: None,
        rename: TextEdit::new(""),
        virtualised,
    }
}

fn select(id: String) -> impl Fn(&mut App, &mut Tx) {
    move |s: &mut App, _| s.selected = id.clone()
}

fn session_row(s: &App, i: usize, k: &Kit) -> El {
    let session = &s.sessions[i];
    let id = session.id.clone();
    let mut r = SidebarRow::new(session.title.clone())
        .selected(s.selected == session.id)
        .depth(session.depth)
        .on_select(select(id.clone()))
        .on_double_click(move |s: &mut App, _| {
            let title = s.sessions.iter().find(|x| x.id == id).map(|x| x.title.clone()).unwrap_or_default();
            s.rename = TextEdit::new(title);
            s.rename.anchor = 0;
            s.rename.focused = true;
            s.renaming = Some(id.clone());
        });
    if session.depth > 0 {
        r = r.icon(&BRANCH);
    }
    if !session.time.is_empty() {
        r = r.detail(session.time);
    }
    if session.unread > 0 {
        r = r.badge(session.unread.to_string());
    }
    if s.renaming.as_deref() == Some(session.id.as_str()) {
        r = r.renaming(
            &s.rename,
            |s: &mut App| &mut s.rename,
            |s: &mut App, _| {
                let (id, title) = (s.renaming.take(), s.rename.text.trim().to_owned());
                if let Some(x) = s.sessions.iter_mut().find(|x| Some(&x.id) == id.as_ref()).filter(|_| !title.is_empty()) {
                    x.title = title;
                }
            },
            |s: &mut App, _| s.renaming = None,
        );
    }
    r.build(k).key(session.id.clone())
}

fn sidebar(s: &App, k: &Kit) -> El {
    let mut sb = Sidebar::new()
        .label("Sessions")
        .header(SidebarRow::new("New session").icon(&PLUS).detail("⌘N").build(k).key("new"))
        .header(SidebarRow::new("Search").icon(&SEARCH).detail("⌘K").build(k).key("search"))
        .footer(SidebarRow::new("Settings").icon(&SLIDERS).build(k).key("settings"));
    if s.virtualised {
        let kit = k.clone();
        return sb
            .virtual_rows(
                s.flat.len(),
                |s: &App, i| match s.flat[i] {
                    Flat::Header(name) => Cow::Borrowed(name),
                    Flat::Session(j) => Cow::Borrowed(s.sessions[j].id.as_str()),
                },
                move |s: &App, i| match s.flat[i] {
                    Flat::Header(name) => SidebarHeader::new(name).build(&kit),
                    Flat::Session(j) => session_row(s, j, &kit),
                },
            )
            .build(k);
    }
    for name in SECTIONS {
        let rows: Vec<El> = (0..s.sessions.len()).filter(|&i| s.sessions[i].section == name).map(|i| session_row(s, i, k)).collect();
        let mut section = SidebarSection::new(name)
            .count(rows.len())
            .collapsed(s.collapsed.contains(name))
            .on_toggle(move |s: &mut App, _| {
                if !s.collapsed.remove(name) {
                    s.collapsed.insert(name);
                }
            })
            .rows(rows);
        if name == "Pinned" {
            section = section.action(IconButton::new(&PLUS).control_size(rune_kit::ControlSize::Mini).variant(IconButtonVariant::GhostMuted).label("Pin a session").build(k));
        }
        sb = sb.section(section);
    }
    sb.build(k)
}

fn view(s: &App, cx: &mut Cx<App>) -> El {
    let k = Kit::neutral(cx.system_dark());
    let c = &k.c;
    let title = s.sessions.iter().find(|x| x.id == s.selected).map_or("", |x| x.title.as_str());
    let line = |t: &str, ink| text(t.to_owned()).font(k.ty.body).color(ink).wrap();
    let main = col()
        .key("transcript")
        .w_full()
        .padding(24.0)
        .gap(14.0)
        .child(text(title.to_owned()).key("heading").font(k.ty.body_medium).color(c.text_base))
        .child(line("The list stutters when a long session scrolls past the fold. Frames 40 to 52 spend most of their time laying out rows that never draw.", c.text_muted))
        .child(line("Virtualising the rows keeps layout to the viewport and two screens either side, so a commit costs the rows that changed.", c.text_muted))
        .child(line("Next: keep the selected row's height when its preview text grows, so the list doesn't jump under the pointer.", c.text_muted));
    let field = |label: &str, value: &str| {
        col().gap(2.0).child(text(label.to_owned()).font(k.ty.small).color(c.text_faint)).child(text(value.to_owned()).font(k.ty.ui).color(c.text_base))
    };
    let inspector = col()
        .key("details")
        .w_full()
        .padding(16.0)
        .gap(14.0)
        .child(text("Details").font(k.ty.ui_medium).color(c.text_base))
        .child(field("Model", "Neutral 2"))
        .child(field("Branch", "dev"))
        .child(field("Messages", "38"))
        .child(field("Updated", "4 minutes ago"));
    let toolbar = Toolbar::new().title(title).trailing(
        IconButton::new(&shell::INSPECTOR)
            .variant(IconButtonVariant::GhostMuted)
            .expanded(s.inspector.is_open())
            .label(if s.inspector.is_open() { "Hide inspector" } else { "Show inspector" })
            .on_press(|s: &mut App| s.inspector.toggle())
            .build(&k)
            .key("inspector-toggle"),
    );
    SplitView::new(main)
        .sidebar(&s.sidebar, |s: &mut App| &mut s.sidebar, sidebar(s, &k))
        .inspector(&s.inspector, |s: &mut App| &mut s.inspector, inspector)
        .toolbar(toolbar)
        .titlebar_inset(cx.titlebar_inset())
        .build(&k)
}

fn on_key(s: &mut App, ev: &KeyEvent, _: &mut Tx) -> bool {
    if shell::is_sidebar_toggle(ev) {
        s.sidebar.toggle();
        return true;
    }
    if ev.is_command('i') && ev.mods.alt {
        s.inspector.toggle();
        return true;
    }
    false
}

fn main() {
    // The traffic lights centred in the 48 point title bar band.
    let chrome = Chrome { full_size_content: true, traffic_lights: Some((16.0, 17.0)), drag_regions: true };
    rune::App::new(AppSpec { on_key: Some(on_key), ..AppSpec::new("Shell", (1040.0, 640.0), init, view) }).chrome(chrome).run();
}

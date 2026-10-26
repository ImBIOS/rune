//! A source-list sidebar: collapsible sections of selectable rows, with header and footer
//! slots, keyboard navigation and an optional virtualised body.

use std::borrow::Cow;

use rune::{Anim, El, GrowthSpec, Icon, Key, ScrollSpec, TextEdit, Tx, col, el, icon, row, text, text_input};

use crate::Kit;

type Apply = Box<dyn FnOnce(El) -> El>;
type Themed = Box<dyn FnOnce(&Kit) -> El>;

/// A sidebar row's height (AppKit's medium source-list row).
pub const ROW_HEIGHT: f32 = 28.0;
/// Each level of [`SidebarRow::depth`] indents this far.
pub const INDENT: f32 = 16.0;
/// Rows sit this far in from the sidebar's sides.
const INSET: f32 = 8.0;

enum Body {
    Items(Vec<Themed>),
    Virtual(Apply),
}

/// A sidebar's content: header, a scrolling list of sections and rows, and footer.
/// `Sidebar::new().header(..).section(..).footer(..).build(&kit)`. The list is one Tab stop
/// that lands on the selected row. Up and Down move the selection (it follows focus, as in a
/// source list), Home and End go to the ends, typing a row's first letters jumps to it, and
/// Return presses the focused row.
pub struct Sidebar {
    label: String,
    header: Vec<El>,
    body: Body,
    footer: Vec<El>,
}

impl Sidebar {
    pub fn new() -> Sidebar {
        Sidebar { label: "Sidebar".into(), header: Vec::new(), body: Body::Items(Vec::new()), footer: Vec::new() }
    }
    /// The list's accessible name.
    pub fn label(mut self, l: impl Into<String>) -> Self {
        self.label = l.into();
        self
    }
    /// Above the list, outside its scrolling: a "New" row or button, a search field.
    pub fn header(mut self, e: El) -> Self {
        self.header.push(e);
        self
    }
    /// Below the list: an account or settings row, a version line.
    pub fn footer(mut self, e: El) -> Self {
        self.footer.push(e);
        self
    }
    pub fn section(self, s: SidebarSection) -> Self {
        self.item(Box::new(move |k: &Kit| s.build(k)))
    }
    /// Any element in the list: a built [`SidebarRow`] or [`SidebarHeader`], a divider.
    pub fn child(self, e: El) -> Self {
        self.item(Box::new(move |_: &Kit| e))
    }
    /// A virtualised list of `count` rows for long histories: only the rows near the viewport
    /// are built. `key(s, i)` names row `i` and `build(s, i)` builds it, typically a
    /// [`SidebarRow`], or a [`SidebarHeader`] in a flattened list of sections. `El::virtual_rows`
    /// has the rules. Replaces any sections or children.
    pub fn virtual_rows<S: 'static>(
        mut self,
        count: usize,
        key: impl for<'a> Fn(&'a S, usize) -> Cow<'a, str> + 'static,
        build: impl Fn(&S, usize) -> El + 'static,
    ) -> Self {
        self.body = Body::Virtual(Box::new(move |e: El| e.virtual_rows(count, ROW_HEIGHT, key, build)));
        self
    }

    fn item(mut self, e: Themed) -> Self {
        match &mut self.body {
            Body::Items(v) => v.push(e),
            Body::Virtual(_) => self.body = Body::Items(vec![e]),
        }
        self
    }

    pub fn build(self, k: &Kit) -> El {
        let list = col().key("sidebar-list").role("list").label(self.label).focus_group().focus_follows().w_full().px(INSET);
        let list = match self.body {
            Body::Items(items) => list.pb(INSET).children(items.into_iter().map(|f| f(k))),
            Body::Virtual(rows) => rows(list.growth(GrowthSpec::default())),
        };
        let mut root = col().key("sidebar").w_full().h_full().min_h(0.0);
        if !self.header.is_empty() {
            root = root.child(col().key("sidebar-header").w_full().px(INSET).pb(INSET).gap(2.0).children(self.header));
        }
        root = root.child(
            col()
                .key("sidebar-scroll")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec { stick_bottom: false })
                .scroll_edge_fade(16.0)
                .child(list),
        );
        if !self.footer.is_empty() {
            root = root.child(col().key("sidebar-footer").w_full().px(INSET).py(INSET).gap(2.0).children(self.footer));
        }
        root
    }
}

impl Default for Sidebar {
    fn default() -> Self {
        Self::new()
    }
}

/// A section's header row: its title, an optional count, a disclosure triangle and a trailing
/// action (a small "+" button, say). Pressing it calls `on_toggle`; whether the section is
/// collapsed is the app's state. [`SidebarSection`] pairs it with its rows; use it on its own
/// in a virtualised list.
pub struct SidebarHeader {
    title: String,
    count: Option<String>,
    collapsed: Option<bool>,
    action: Option<El>,
    toggle: Option<Apply>,
}

impl SidebarHeader {
    pub fn new(title: impl Into<String>) -> SidebarHeader {
        SidebarHeader { title: title.into(), count: None, collapsed: None, action: None, toggle: None }
    }
    /// Shows a disclosure triangle for this state. Without it, or `on_toggle`, the header is
    /// a plain label.
    pub fn collapsed(mut self, c: bool) -> Self {
        self.collapsed = Some(c);
        self
    }
    pub fn count(mut self, n: usize) -> Self {
        self.count = Some(n.to_string());
        self
    }
    /// A trailing element, such as a small "+" icon button.
    pub fn action(mut self, e: El) -> Self {
        self.action = Some(e);
        self
    }
    pub fn on_toggle<S: 'static>(mut self, f: impl Fn(&mut S, &mut Tx) + 'static) -> Self {
        self.toggle = Some(Box::new(move |e: El| e.on_press_tx(f)));
        self.collapsed.get_or_insert(false);
        self
    }

    pub fn build(self, k: &Kit) -> El {
        let c = &k.c;
        let mut title = row()
            .key("section-title")
            .gap(4.0)
            .center_y()
            .shrink()
            .child(text(self.title.clone()).font(k.ty.text_xsmall_medium).color(c.text_muted).truncate().shrink());
        if let Some(n) = self.count {
            title = title.child(text(n).key("count").font(k.ty.text_xsmall_medium).color(c.text_faint));
        }
        if let Some(collapsed) = self.collapsed {
            let glyph = if collapsed { k.glyph.triangle_right } else { k.glyph.triangle_down };
            title = title.child(icon(glyph, 12.0).key("disclosure").color(c.icon_faint));
        }
        let label = match self.collapsed {
            Some(true) => format!("{}, collapsed", self.title),
            Some(false) if self.toggle.is_some() => format!("{}, expanded", self.title),
            _ => self.title,
        };
        let mut header = row()
            .key("section-header")
            .label(label)
            .w_full()
            .h(ROW_HEIGHT)
            .pt(4.0)
            .px(INSET)
            .center_y()
            .radius(k.radius.md)
            .child(title)
            .child(el().key("spacer").grow(1.0));
        if let Some(a) = self.action {
            header = header.child(a);
        }
        match self.toggle {
            // Pointer only: in the list's focus group the arrows step through rows, and with
            // selection following focus they would toggle the section as they passed.
            Some(toggle) => toggle(header)
                .role("button")
                .hover(c.hover)
                .press(c.pressed)
                .hover_motion(Anim::INSTANT, Anim::INSTANT)
                .focusable(false),
            None => header.role("heading"),
        }
    }
}

/// A titled group of rows that collapses: `SidebarSection::new("Today").collapsed(..)
/// .on_toggle(..).row(..)`. Rows below it move on the kit's disclosure spring as it opens.
pub struct SidebarSection {
    key: String,
    header: SidebarHeader,
    collapsed: bool,
    rows: Vec<El>,
}

impl SidebarSection {
    pub fn new(title: impl Into<String>) -> SidebarSection {
        let title = title.into();
        SidebarSection { key: title.clone(), header: SidebarHeader::new(title), collapsed: false, rows: Vec::new() }
    }
    /// Its key among the list's children (default: the title).
    pub fn key(mut self, k: impl Into<String>) -> Self {
        self.key = k.into();
        self
    }
    pub fn collapsed(mut self, c: bool) -> Self {
        self.collapsed = c;
        self.header = self.header.collapsed(c);
        self
    }
    pub fn count(mut self, n: usize) -> Self {
        self.header = self.header.count(n);
        self
    }
    pub fn action(mut self, e: El) -> Self {
        self.header = self.header.action(e);
        self
    }
    pub fn on_toggle<S: 'static>(mut self, f: impl Fn(&mut S, &mut Tx) + 'static) -> Self {
        self.header = self.header.on_toggle(f);
        self
    }
    /// A row: a built [`SidebarRow`], keyed by the app.
    pub fn row(mut self, e: El) -> Self {
        self.rows.push(e);
        self
    }
    pub fn rows(mut self, rows: impl IntoIterator<Item = El>) -> Self {
        self.rows.extend(rows);
        self
    }

    pub fn build(self, k: &Kit) -> El {
        let mut s = col().key(self.key).role("group").w_full().gap(2.0).motion(k.motion.disclosure).child(self.header.build(k));
        if !self.collapsed {
            s = s.children(self.rows);
        }
        s
    }
}

/// A selectable sidebar row: icon, label, then a trailing detail (a time), badge or element.
/// `SidebarRow::new("Inbox").icon(&INBOX).badge("3").selected(sel).on_select(..).build(&kit)`.
/// Key the built row by its item's id.
pub struct SidebarRow {
    label: String,
    icon: Option<&'static Icon>,
    detail: Option<String>,
    badge: Option<String>,
    trailing: Option<El>,
    selected: bool,
    depth: u8,
    rename: Option<(TextEdit, Apply)>,
    acts: Vec<Apply>,
}

impl SidebarRow {
    pub fn new(label: impl Into<String>) -> SidebarRow {
        SidebarRow {
            label: label.into(),
            icon: None,
            detail: None,
            badge: None,
            trailing: None,
            selected: false,
            depth: 0,
            rename: None,
            acts: Vec::new(),
        }
    }
    pub fn icon(mut self, i: &'static Icon) -> Self {
        self.icon = Some(i);
        self
    }
    /// Faint trailing text: a time ("2m"), a count, a shortcut.
    pub fn detail(mut self, d: impl Into<String>) -> Self {
        self.detail = Some(d.into());
        self
    }
    /// A trailing pill (an unread count).
    pub fn badge(mut self, b: impl Into<String>) -> Self {
        self.badge = Some(b.into());
        self
    }
    /// Any trailing element (a status dot, a menu button), after the detail and badge.
    pub fn trailing(mut self, e: El) -> Self {
        self.trailing = Some(e);
        self
    }
    pub fn selected(mut self, s: bool) -> Self {
        self.selected = s;
        self
    }
    /// Nesting level: each indents the row by [`INDENT`].
    pub fn depth(mut self, d: u8) -> Self {
        self.depth = d;
        self
    }
    /// Called when the row is pressed, or reached with the arrow keys.
    pub fn on_select<S: 'static>(mut self, f: impl Fn(&mut S, &mut Tx) + 'static) -> Self {
        self.acts.push(Box::new(move |e: El| e.on_press_tx(f)));
        self
    }
    /// Called on a double click (rename, open in a new window).
    pub fn on_double_click<S: 'static>(mut self, f: impl Fn(&mut S, &mut Tx) + 'static) -> Self {
        self.acts.push(Box::new(move |e: El| e.on_double_press_tx(f)));
        self
    }
    /// Edits the label in place, in the field bound to `edit` (`access` finds it in the app's
    /// state). Set `edit.focused` and its text when renaming starts. Return calls `commit`,
    /// Escape calls `cancel`; both should end the rename.
    pub fn renaming<S: 'static>(
        mut self,
        edit: &TextEdit,
        access: fn(&mut S) -> &mut TextEdit,
        commit: impl Fn(&mut S, &mut Tx) + 'static,
        cancel: impl Fn(&mut S, &mut Tx) + 'static,
    ) -> Self {
        let bind: Apply = Box::new(move |e: El| {
            e.on_edit(access).on_submit(commit).on_input_key(move |s: &mut S, ev, tx| {
                if ev.key == Key::Escape {
                    cancel(s, tx);
                    return true;
                }
                false
            })
        });
        self.rename = Some((edit.clone(), bind));
        self
    }

    pub fn build(self, k: &Kit) -> El {
        let c = &k.c;
        let renaming = self.rename.is_some();
        let (ink, glyph) = if self.selected { (c.text_base, c.icon_base) } else { (c.text_muted, c.icon_muted) };
        let mut r = row()
            .label(self.label.clone())
            .w_full()
            .h(ROW_HEIGHT)
            .pl(INSET + self.depth as f32 * INDENT)
            .pr(INSET)
            .gap(6.0)
            .center_y()
            .radius(k.radius.md)
            .motion(k.motion.control)
            .hover_group()
            .hover(c.hover)
            .press(c.pressed)
            .hover_motion(Anim::INSTANT, Anim::INSTANT)
            .focusable(!renaming)
            .focus_selected(self.selected && !renaming);
        if self.selected {
            r = r.fill(c.pressed);
        }
        if let Some(i) = self.icon {
            r = r.child(icon(i, 16.0).key("icon").color(glyph));
        }
        r = match self.rename {
            Some((edit, bind)) => r.child(bind(
                text_input(&edit, k.ty.ui)
                    .single_line()
                    .key("rename")
                    .focus_selected(true)
                    .focus_return("sidebar-list")
                    .grow(1.0)
                    .shrink()
                    .color(c.text_base)
                    .caret(c.text_base, k.selection()),
            )),
            None => r.child(text(self.label).key("label").font(k.ty.ui).color(ink).truncate().grow(1.0).shrink()),
        };
        if let Some(d) = self.detail {
            r = r.child(text(d).key("detail").font(k.ty.small).color(c.text_faint));
        }
        if let Some(b) = self.badge {
            r = r.child(
                row()
                    .key("badge")
                    .h(16.0)
                    .px(5.0)
                    .center()
                    .radius(8.0)
                    .fill(c.layer_03)
                    .child(text(b).font(k.ty.small_medium).color(c.text_muted)),
            );
        }
        if let Some(t) = self.trailing {
            r = r.child(t);
        }
        for a in self.acts {
            r = a(r);
        }
        // After the handlers, which would make it a button.
        r.role("listitem")
    }
}

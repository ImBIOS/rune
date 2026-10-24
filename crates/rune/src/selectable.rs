//! Read-only text people can select and copy (`El::selectable`), as SwiftUI's
//! `.textSelection(.enabled)` does on the Mac. It shares the text fields' machinery: Parley's
//! layout hit-tests the pointer into byte offsets, the highlight is the fields' own selection
//! boxes (`push_selection`), and copying goes through `Tx::copy`. The selection lives here, not in
//! app state, so selecting never re-runs the view or re-lays out the text.
//!
//! One text holds the selection at a time. A press elsewhere clears it, as does focus moving to
//! another element. The selection is byte offsets into the text's drawn content; a view that
//! changes that content drops it.
use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Unit {
    #[default]
    Char,
    Word,
    Paragraph,
}

#[derive(Default)]
pub(super) struct TextSelection {
    /// The text holding the selection, and its content when it was made.
    path: Option<String>,
    content: String,
    anchor: usize,
    focus: usize,
    /// What the press that made it selects by, and the range of that unit it began on, which a
    /// drag keeps whole.
    unit: Unit,
    origin: (usize, usize),
    /// The held press is extending this selection.
    pub(super) dragging: bool,
    /// The last press on selectable text: its path, time, point and click count.
    last: Option<(String, f64, f32, f32, u8)>,
    /// Shift is held (from the host's modifier events), so a press extends the selection.
    pub(super) shift: bool,
}

/// The char boundary at or before byte `i` of `text`.
fn floor_boundary(text: &str, i: usize) -> usize {
    let mut i = i.min(text.len());
    while !text.is_char_boundary(i) { i -= 1; }
    i
}

/// The char boundary before byte `i` (a boundary) of `text`.
fn prev_boundary(text: &str, i: usize) -> usize { floor_boundary(text, i.saturating_sub(1)) }

/// The range of `unit` at byte `i` of `text`: a word (or run of spaces or punctuation) by
/// Unicode word boundaries, or a paragraph with its line break. Past the end of a line (on its
/// break, or past the end of the text) it takes the line's last word or paragraph, as AppKit does.
fn unit_at(text: &str, i: usize, unit: Unit) -> (usize, usize) {
    use unicode_segmentation::UnicodeSegmentation;
    let mut i = floor_boundary(text, i);
    if i > 0 && i == text.len() && unit != Unit::Char { i = prev_boundary(text, i); }
    match unit {
        Unit::Char => (i, i),
        Unit::Word => {
            if i > 0 && text[i..].starts_with(['\n', '\r']) && !text[..i].ends_with('\n') {
                i = prev_boundary(text, i);
                if text[i..].starts_with('\r') && i > 0 && !text[..i].ends_with('\n') { i = prev_boundary(text, i); }
            }
            let mut last = (i, i);
            for (start, word) in text.split_word_bound_indices() {
                let end = start + word.len();
                last = (start, end);
                if i < end { return last; }
            }
            last
        }
        Unit::Paragraph => {
            let start = text[..i].rfind('\n').map_or(0, |n| n + 1);
            let end = text[i..].find('\n').map_or(text.len(), |n| i + n + 1);
            (start, end)
        }
    }
}

/// The selection a drag from unit range `origin` to unit range `at` makes: (anchor, focus).
fn extend(origin: (usize, usize), at: (usize, usize)) -> (usize, usize) {
    if at.0 < origin.0 { (origin.1, at.0) } else { (origin.0, at.1.max(origin.1)) }
}

/// Pushes the selection boxes of bytes `a..b` of `layout`, whose top-left draws at `origin`
/// (device px) `k` times its size. Text fields and selectable text both draw through here, so
/// their highlights match.
#[allow(clippy::too_many_arguments)]
pub(super) fn push_selection(out: &mut Vec<Instance>, layout: &Layout<Ink>, a: usize, b: usize, origin: [f32; 2], k: f32,
    fill: [f32; 4], s: f32, clip: [f32; 4], fade: [f32; 4]) {
    let sel = Selection::new(
        Cursor::from_byte_index(layout, a, Affinity::Downstream),
        Cursor::from_byte_index(layout, b, Affinity::Upstream),
    );
    for (bb, _) in sel.geometry(layout) {
        out.push(rect_inst(
            [origin[0] + bb.x0 as f32 * k, origin[1] + bb.y0 as f32 * k, (bb.x1 - bb.x0) as f32 * k, (bb.y1 - bb.y0) as f32 * k],
            fill, 2.0 * s * k, clip, fade,
        ));
    }
}

impl<S: 'static> Runtime<S> {
    pub(super) fn is_selectable(&self, i: usize) -> bool {
        let n = &self.nodes[i];
        n.input.is_none() && n.text.as_ref().is_some_and(|t| t.selectable)
    }

    /// The selected byte range of node `path`'s text, when it holds a non-empty selection made
    /// in its current content.
    pub(super) fn selected_range(&self, path: &str, content: &str) -> Option<(usize, usize)> {
        let sel = &self.text_sel;
        if sel.path.as_deref() != Some(path) || sel.content != content { return None; }
        let (a, b) = (sel.anchor.min(sel.focus), sel.anchor.max(sel.focus));
        (a < b && b <= content.len()).then_some((a, b))
    }

    /// The node holding the selection, while it's still there, still selectable and still shows
    /// the content the selection was made in.
    pub(super) fn selection_holder(&self) -> Option<usize> {
        let sel = &self.text_sel;
        let path = sel.path.as_deref()?;
        let i = (0..self.nodes.len()).find(|&i| self.nodes[i].path == path && !self.subtree_exiting(i))?;
        (self.is_selectable(i) && self.nodes[i].text.as_ref().is_some_and(|t| t.content == sel.content)).then_some(i)
    }

    /// After a commit: drop a selection whose text went away, stopped being selectable or
    /// changed its content.
    pub(super) fn prune_text_selection(&mut self) {
        if self.text_sel.path.is_some() && self.selection_holder().is_none() { self.clear_text_selection(); }
    }

    /// The text holding the selection and its selected byte range (empty after a single click),
    /// for tests and tools.
    pub fn text_selection(&self) -> Option<(&str, std::ops::Range<usize>)> {
        self.selection_holder()?;
        let sel = &self.text_sel;
        Some((sel.path.as_deref()?, sel.anchor.min(sel.focus)..sel.anchor.max(sel.focus)))
    }

    /// The selected text, as copying would put it on the clipboard (as authored, before
    /// `El::text_case`).
    pub fn selected_text(&self) -> Option<String> {
        let i = self.selection_holder()?;
        let spec = self.nodes[i].text.as_ref()?;
        let (a, b) = self.selected_range(&self.nodes[i].path, &spec.content)?;
        spec.source_slice(a as u32, b as u32).map(str::to_owned)
    }

    /// The held press is selecting text by dragging, and has selected some.
    pub(super) fn selected_by_drag(&self) -> bool {
        self.text_sel.dragging && self.press_moved && self.text_selection().is_some_and(|(_, r)| !r.is_empty())
    }

    pub(super) fn clear_text_selection(&mut self) {
        self.text_sel.path = None;
        self.text_sel.dragging = false;
    }

    /// Byte offset of node `i`'s text under window point (x, y), where it was drawn this frame.
    fn text_index_at(&self, i: usize, x: f32, y: f32) -> Option<usize> {
        let n = &self.nodes[i];
        let (spec, layout) = (n.text.as_ref()?, n.text_layout.as_ref()?);
        let s = self.env.scale;
        let z = self.drawn_scale.get(i).copied().unwrap_or(1.0).max(1e-4);
        let [ox, oy] = links::origin(n, spec, layout, self.drawn_box(i), s, z);
        let px = s / z;
        let at = Cursor::from_point(layout, (x - ox) * px, (y - oy) * px).index();
        Some(n.elided.map_or(at, |e| e.content(at)).min(spec.content.len()))
    }

    /// A primary press on `target` (None: on nothing). On selectable text away from its links it
    /// starts a selection: one click places it, two select a word, three a paragraph, and Shift
    /// extends the current one. Anywhere else it clears the selection.
    pub(super) fn selection_down(&mut self, target: Option<usize>, x: f32, y: f32) {
        let Some(i) = target.filter(|&i| self.is_selectable(i) && self.link_at(i, x, y).is_none()) else {
            self.clear_text_selection();
            self.text_sel.last = None;
            return;
        };
        let Some(at) = self.text_index_at(i, x, y) else { return };
        let path = self.nodes[i].path.clone();
        let content = self.nodes[i].text.as_ref().map(|t| t.content.clone()).unwrap_or_default();
        let now = self.now_clock();
        let clicks = match &self.text_sel.last {
            Some((p, t, px, py, n)) if *p == path && now - t <= self.double_click_interval
                && (x - px).abs() <= CLICK_SLOP && (y - py).abs() <= CLICK_SLOP => (n + 1).min(3),
            _ => 1,
        };
        self.text_sel.last = Some((path.clone(), now, x, y, clicks));
        let sel = &mut self.text_sel;
        let holds = sel.path.as_deref() == Some(path.as_str()) && sel.content == content;
        if clicks == 1 && sel.shift && holds {
            // Keep the anchor; the focus follows the pointer from here.
            sel.unit = Unit::Char;
            sel.origin = (sel.anchor, sel.anchor);
            sel.focus = at;
        } else {
            sel.unit = match clicks { 1 => Unit::Char, 2 => Unit::Word, _ => Unit::Paragraph };
            sel.origin = unit_at(&content, at, sel.unit);
            (sel.anchor, sel.focus) = sel.origin;
        }
        sel.path = Some(path);
        sel.content = content;
        sel.dragging = true;
    }

    /// The held press moved: extend the selection it started to the pointer, by its unit. The
    /// press itself may belong to an ancestor (a clickable row), so this follows the selection's
    /// own text.
    pub(super) fn selection_drag(&mut self, x: f32, y: f32) {
        if !self.text_sel.dragging { return; }
        let Some(i) = self.selection_holder() else { self.clear_text_selection(); return };
        let Some(at) = self.text_index_at(i, x, y) else { return };
        let sel = &mut self.text_sel;
        let at = unit_at(&sel.content, at, sel.unit);
        (sel.anchor, sel.focus) = extend(sel.origin, at);
    }

    /// Cmd+C (Ctrl+C elsewhere) copies a non-empty selection and Cmd+A selects all of the text
    /// holding it (a click on it makes it the holder, as it makes an AppKit text view first
    /// responder), unless a text field has the keyboard. Only the plain chord: with Shift, Option
    /// (Alt) or Control added it's the app's shortcut.
    pub(super) fn selection_key(&mut self, ev: &KeyEvent, tx: &mut Tx) -> bool {
        let crate::Key::Command(c @ ('c' | 'a')) = ev.key else { return false };
        let m = ev.mods;
        let plain = m.cmd && !m.shift && !m.alt && if cfg!(target_os = "macos") { !m.ctrl } else { !m.logo };
        if !plain || self.text_sel.path.is_none() || self.focused_input().is_some() { return false; }
        let Some(i) = self.selection_holder() else { self.clear_text_selection(); return false };
        let path = self.nodes[i].path.clone();
        if c == 'a' {
            let len = self.text_sel.content.len();
            let sel = &mut self.text_sel;
            (sel.anchor, sel.focus, sel.origin, sel.unit) = (0, len, (0, len), Unit::Char);
            self.events.push(format!("select all {path}"));
        } else {
            let Some(text) = self.selected_text() else { return false };
            self.events.push(format!("copy {path}"));
            tx.copy(text);
        }
        true
    }

    /// A secondary click on selectable text `path` without its own `on_secondary_press` opens the
    /// Copy menu, selecting the word under the pointer first when the click is outside the
    /// selection, as AppKit text does. False when the app's handler should run instead.
    pub(super) fn selection_menu(&mut self, path: &str, x: f32, y: f32) -> bool {
        let Some(i) = (0..self.nodes.len()).find(|&i| self.nodes[i].path == path && !self.subtree_exiting(i)) else { return false };
        if !self.is_selectable(i) || self.nodes[i].on_secondary_press.is_some() { return false; }
        let content = self.nodes[i].text.as_ref().map(|t| t.content.clone()).unwrap_or_default();
        if let Some(at) = self.text_index_at(i, x, y)
            && self.selected_range(path, &content).is_none_or(|(a, b)| at < a || at >= b)
        {
            let (a, b) = unit_at(&content, at, Unit::Word);
            self.text_sel = TextSelection { path: Some(path.to_owned()), content, anchor: a, focus: b, unit: Unit::Word,
                origin: (a, b), dragging: false, last: None, shift: self.text_sel.shift };
        }
        let text = self.selected_text();
        let mut copy = crate::PopupItem::new("copy", "Copy").enabled(text.is_some());
        if let Ok(shortcut) = "cmd+c".parse::<crate::Shortcut>() { copy = copy.key_equivalent(shortcut); }
        self.events.push(format!("selection menu {path}"));
        self.popups.push_back(crate::popup_menu::Request {
            menu: crate::PopupMenu::new().item(copy),
            anchor: crate::PopupAnchor::Point(x, y),
            on_pick: Box::new(move |_, picked, tx| {
                if picked == Some("copy") && let Some(text) = text { tx.copy(text); }
            }),
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{Unit, extend, unit_at};

    #[test]
    fn units_cover_words_spaces_and_paragraphs() {
        let s = "opencode --port 4096\nnext line";
        assert_eq!(unit_at(s, 3, Unit::Word), (0, 8));
        assert_eq!(unit_at(s, 8, Unit::Word), (8, 9), "a space run is its own word");
        assert_eq!(unit_at(s, 12, Unit::Word), (11, 15));
        assert_eq!(unit_at(s, s.len(), Unit::Word), (26, 30), "past the end takes the last word");
        assert_eq!(unit_at(s, 5, Unit::Paragraph), (0, 21), "a paragraph takes its line break");
        assert_eq!(unit_at(s, 23, Unit::Paragraph), (21, s.len()));
        assert_eq!(unit_at("", 0, Unit::Word), (0, 0));
    }

    #[test]
    fn line_ends_take_the_lines_last_word_and_paragraph() {
        let s = "first line\nsecond";
        assert_eq!(unit_at(s, 10, Unit::Word), (6, 10), "on the break: the line's last word");
        assert_eq!(unit_at("a\r\nb", 1, Unit::Word), (0, 1));
        assert_eq!(unit_at("first line\n", 11, Unit::Paragraph), (0, 11), "past the end: the last paragraph");
        assert_eq!(unit_at("first line\n", 11, Unit::Word), (6, 10));
        assert_eq!(unit_at("a\n\nb", 2, Unit::Word), (2, 3), "an empty line's break is its own word");
    }

    #[test]
    fn offsets_inside_a_character_stay_on_its_boundary() {
        let s = "a —";
        for i in 0..=s.len() + 2 {
            for unit in [Unit::Char, Unit::Word, Unit::Paragraph] {
                let (a, b) = unit_at(s, i, unit);
                assert!(s.is_char_boundary(a) && s.is_char_boundary(b), "{i} {unit:?}");
            }
        }
    }

    #[test]
    fn a_drag_keeps_the_unit_it_began_on_whole() {
        assert_eq!(extend((8, 12), (20, 24)), (8, 24));
        assert_eq!(extend((8, 12), (0, 4)), (12, 0));
        assert_eq!(extend((8, 12), (9, 10)), (8, 12));
        assert_eq!(extend((5, 5), (2, 2)), (5, 2));
    }
}

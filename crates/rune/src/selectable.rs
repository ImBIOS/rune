//! Read-only text people can select and copy (`El::selectable`), as SwiftUI's
//! `.textSelection(.enabled)` does on the Mac. It shares the text fields' machinery: Parley's
//! layout hit-tests the pointer into byte offsets, the highlight is the fields' own selection
//! boxes (`push_selection`), and copying goes through `Tx::copy`. The selection lives here, not in
//! app state, so selecting never re-runs the view or re-lays out the text.
//!
//! One text holds the selection at a time, or one selection group (`El::selection_group`): the
//! anchor is in one of its texts, the focus in another, and every text of the group between them
//! in tree order is selected whole. A press elsewhere clears it, as does focus moving to another
//! element. The selection is byte offsets into the texts' content; a view that changes the
//! selected part of a text drops it, while text appended after it (a message streaming in)
//! keeps it.
//!
//! Code reads the selection with `Tx::text_selection` and sets it with `Tx::select_text`,
//! `Tx::select_all_text` and `Tx::clear_text_selection`; an `El::selection_copy` element
//! decides what copying a selection inside it takes.
use super::*;
use crate::el::SelectRequest;
use std::ops::Range;

/// A selection of [`El::selectable`] text, as [`Tx::text_selection`] reads it and an
/// [`El::selection_copy`] element sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextSelection {
    texts: Vec<SelectedText>,
    text: String,
}

impl TextSelection {
    /// The selected texts in tree order (for [`El::selection_copy`], every text it could reach).
    pub fn texts(&self) -> &[SelectedText] { &self.texts }
    /// The selected text as copying takes it without [`El::selection_copy`]: as authored
    /// (before [`El::text_case`]), without [`El::uncopied`] bytes, with a newline between
    /// texts and a tab between texts side by side (table cells).
    pub fn text(&self) -> &str { &self.text }
}

/// One [`El::selectable`] text of a [`TextSelection`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectedText {
    path: String,
    key: Option<String>,
    range: Range<usize>,
    len: usize,
    whole: bool,
    text: String,
}

impl SelectedText {
    /// The text element's path ([`ElementInfo::path`]); for [`El::selection_copy`], relative to
    /// the element that asks, empty for that element itself.
    pub fn path(&self) -> &str { &self.path }
    /// The text element's own key, if it has one.
    pub fn key(&self) -> Option<&str> { self.key.as_deref() }
    /// The selected bytes of the text's content, empty when none are.
    pub fn range(&self) -> Range<usize> { self.range.clone() }
    /// The length in bytes of the text's content.
    pub fn len(&self) -> usize { self.len }
    /// The text has no content.
    pub fn is_empty(&self) -> bool { self.len == 0 }
    /// Some of the text is selected.
    pub fn is_selected(&self) -> bool { !self.range.is_empty() }
    /// All of the text is selected.
    pub fn is_whole(&self) -> bool { self.whole }
    /// The selected part as copying takes it.
    pub fn text(&self) -> &str { &self.text }
}

/// What an [`El::on_selection_change`] element hears.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionChange {
    selection: Option<Rc<TextSelection>>,
    was: bool,
}

impl SelectionChange {
    /// The whole selection while any of it is inside the element, `None` otherwise.
    pub fn selection(&self) -> Option<&TextSelection> { self.selection.as_deref() }
    /// A selection appeared inside the element, where there was none.
    pub fn started(&self) -> bool { !self.was && self.selection.is_some() }
    /// The selection cleared or left the element.
    pub fn cleared(&self) -> bool { self.was && self.selection.is_none() }
}

/// The commit and the selection's ends: what a selection read from them depends on.
type Basis = (u64, Option<String>, usize, usize, Option<String>);

/// `Tx::text_selection`'s snapshot and what it was taken from.
pub(super) struct Snapshot {
    from: Basis,
    selection: Option<Rc<TextSelection>>,
}

/// What each `El::on_selection_change` element last heard, by path, and what that was checked
/// against.
#[derive(Default)]
pub(super) struct Heard {
    from: Option<Basis>,
    by_path: HashMap<String, Rc<TextSelection>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Unit {
    #[default]
    Char,
    Word,
    Paragraph,
}

#[derive(Default)]
pub(super) struct Held {
    /// The text holding the anchor, and its content when last seen.
    path: Option<String>,
    content: String,
    anchor: usize,
    /// An offset into the anchor's text, or into `far`'s when that's set.
    focus: usize,
    /// The text holding the focus when it's another text of the anchor's selection group: its
    /// path and content when last seen.
    far: Option<(String, String)>,
    /// A hash of the texts strictly between the anchor's and the focus's (their paths and
    /// content), taken when the selection changes, so a commit that rewrites, adds or removes
    /// one drops it.
    between: u64,
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
pub(crate) fn unit_at(text: &str, i: usize, unit: Unit) -> (usize, usize) {
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
pub(crate) fn extend(origin: (usize, usize), at: (usize, usize)) -> (usize, usize) {
    if at.0 < origin.0 { (origin.1, at.0) } else { (origin.0, at.1.max(origin.1)) }
}

/// Pushes the selection boxes of bytes `a..b` of `layout`, whose top-left draws at `origin`
/// (device px) `k` times its size. Text fields and selectable text both draw through here, so
/// their highlights match.
#[allow(clippy::too_many_arguments)]
pub(super) fn push_selection(out: &mut Vec<Instance>, layout: &Layout<Ink>, a: usize, b: usize, origin: [f32; 2], k: f32,
    fill: [f32; 4], s: f32, clip: [f32; 4], fade: [f32; 4]) {
    for (bb, _) in crate::text::range_boxes(layout, a, b, true) {
        out.push(rect_inst(
            [origin[0] + bb.x0 as f32 * k, origin[1] + bb.y0 as f32 * k, (bb.x1 - bb.x0) as f32 * k, (bb.y1 - bb.y0) as f32 * k],
            fill, 2.0 * s * k, clip, fade,
        ));
    }
}

/// `now` still reads as `then` did up to byte `upto`: text added after it (streaming in) keeps
/// a selection, a change inside it doesn't. With nothing selected in it (`upto` 0) only text
/// appended keeps it.
fn kept(then: &str, now: &str, upto: usize) -> bool {
    if upto == 0 { return now.starts_with(then); }
    then.get(..upto).is_some_and(|a| now.get(..upto) == Some(a))
}

/// `r`, a word, without the [`El::uncopied`] bytes at its ends, unless that leaves nothing.
fn trim_uncopied(uncopied: &[(u32, u32)], (mut a, mut b): (usize, usize)) -> (usize, usize) {
    let r0 = (a, b);
    while let Some(&(_, e)) = uncopied.iter().find(|r| (r.0 as usize) <= a && a < r.1 as usize && a < b) { a = (e as usize).min(b); }
    while let Some(&(st, _)) = uncopied.iter().find(|r| (r.0 as usize) < b && b <= r.1 as usize && a < b) { b = (st as usize).max(a); }
    if a < b { (a, b) } else { r0 }
}

/// Node `i`'s selected bytes among `ranges` (from `selection_ranges`).
pub(super) fn range_in(ranges: &[(usize, usize, usize)], i: usize) -> Option<(usize, usize)> {
    ranges.iter().find(|r| r.0 == i).map(|r| (r.1, r.2))
}

/// How far `x` is outside box `r` (x0, y0, x1, y1) horizontally.
fn dist_x(r: [f32; 4], x: f32) -> f32 { (r[0] - x).max(x - r[2]).max(0.0) }

/// Whether `a` and `b` have the same keyed segments and the same unkeyed tail after their last
/// key, so a keyed text keeps its selection when a sibling shifts an unkeyed ancestor's index.
fn same_keyed_path(a: &str, b: &str) -> bool {
    fn split(p: &str) -> Option<(&str, &str)> {
        let last_key = p.rmatch_indices('/').find(|&(i, _)| !p[i + 1..].starts_with('#'))
            .map_or_else(|| (!p.starts_with('#')).then_some(0), |(i, _)| Some(i + 1))?;
        let end = last_key + p[last_key..].find('/').unwrap_or(p.len() - last_key);
        Some((&p[..end], &p[end..]))
    }
    let (Some((ka, ta)), Some((kb, tb))) = (split(a), split(b)) else { return false };
    ta == tb && ka.split('/').filter(|s| !s.starts_with('#')).eq(kb.split('/').filter(|s| !s.starts_with('#')))
}

impl<S: 'static> Runtime<S> {
    pub(super) fn is_selectable(&self, i: usize) -> bool {
        let n = &self.nodes[i];
        n.input.is_none() && n.text.as_ref().is_some_and(|t| t.selectable && !t.redacted)
    }

    fn live(&self, path: &str) -> Option<usize> {
        (0..self.nodes.len()).find(|&i| self.nodes[i].path == path && !self.subtree_exiting(i) && self.allowed(i))
    }

    /// A unique live selectable node at `path`'s keyed identity when an unkeyed ancestor moved.
    fn rebound(&self, path: &str, content: &str, upto: usize) -> Option<usize> {
        let mut hits = (0..self.nodes.len()).filter(|&i| {
            self.is_selectable(i) && !self.subtree_exiting(i) && self.allowed(i)
                && same_keyed_path(path, &self.nodes[i].path) && kept(content, self.content_of(i), upto)
        });
        let first = hits.next()?;
        hits.next().is_none().then_some(first)
    }

    fn content_of(&self, i: usize) -> &str {
        self.nodes[i].text.as_ref().map_or("", |t| t.content.as_str())
    }

    /// The selection group node `i` is in: its nearest `El::selection_group` ancestor.
    fn group_of(&self, i: usize) -> Option<usize> {
        std::iter::successors(self.nodes[i].parent, |&p| self.nodes[p].parent).find(|&p| self.nodes[p].selection_group)
    }

    fn same_group(&self, i: usize, j: usize) -> bool {
        self.group_of(i).is_some_and(|g| self.group_of(j) == Some(g))
    }

    /// Node `j` is a selectable text of group `g` (not of a group inside it).
    fn member(&self, j: usize, g: usize) -> bool {
        self.is_selectable(j) && self.group_of(j) == Some(g) && !self.subtree_exiting(j) && self.allowed(j)
    }

    /// Group `g`'s selectable texts, in tree order.
    fn group_texts(&self, g: usize) -> Vec<usize> {
        // Nodes are in tree order, so a subtree is the run after its root.
        let inside = |j: usize| std::iter::successors(self.nodes[j].parent, |&p| self.nodes[p].parent).any(|p| p == g);
        (g + 1..self.nodes.len()).take_while(|&j| inside(j)).filter(|&j| self.member(j, g)).collect()
    }

    /// The range of `unit` at byte `at` of node `i`'s text, a word without uncopied ends.
    fn unit_in(&self, i: usize, at: usize, unit: Unit) -> (usize, usize) {
        let r = unit_at(self.content_of(i), at, unit);
        match (unit, &self.nodes[i].text) {
            (Unit::Word, Some(t)) if !t.uncopied.is_empty() => trim_uncopied(&t.uncopied, r),
            _ => r,
        }
    }

    /// The hash `TextSelection::between` holds for the texts strictly between `h` and `f`.
    fn between_hash(&self, h: usize, f: usize) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        if let Some(g) = self.group_of(h).filter(|_| h != f) {
            for j in (h.min(f) + 1..h.max(f)).filter(|&j| self.member(j, g)) {
                (&self.nodes[j].path, self.content_of(j)).hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    /// Records the texts between the selection's ends, after the selection changed.
    fn seal(&mut self) {
        let ends = self.selection_holder().and_then(|h| Some((h, self.focus_holder(h)?)));
        self.text_sel.between = ends.map_or(0, |(h, f)| self.between_hash(h, f));
    }

    /// The node holding the anchor, while it's still there, still selectable and still reads as
    /// it did up to the selection.
    pub(super) fn selection_holder(&self) -> Option<usize> {
        let sel = &self.text_sel;
        let i = self.live(sel.path.as_deref()?)?;
        // Selected to its end when the focus is in a later text.
        let upto = match &sel.far {
            None => sel.anchor.max(sel.focus).max(sel.origin.1),
            Some((far, _)) if self.live(far).is_some_and(|j| j > i) => sel.content.len(),
            Some(_) => sel.anchor.max(sel.origin.1),
        };
        (self.is_selectable(i) && kept(&sel.content, self.content_of(i), upto)).then_some(i)
    }

    /// The node holding the focus: the holder, or another text of its group.
    fn focus_holder(&self, holder: usize) -> Option<usize> {
        let sel = &self.text_sel;
        let Some((path, content)) = &sel.far else { return Some(holder) };
        let j = self.live(path)?;
        let upto = if j > holder { sel.focus } else { content.len() };
        (self.is_selectable(j) && self.same_group(j, holder) && kept(content, self.content_of(j), upto)).then_some(j)
    }

    /// Every text the selection covers with its selected bytes, in tree order, none empty.
    pub(super) fn selection_ranges(&self) -> Vec<(usize, usize, usize)> {
        let Some(h) = self.selection_holder() else { return Vec::new() };
        let Some(f) = self.focus_holder(h) else { return Vec::new() };
        let sel = &self.text_sel;
        if f == h {
            let (a, b) = (sel.anchor.min(sel.focus), sel.anchor.max(sel.focus));
            return if a < b && b <= self.content_of(h).len() { vec![(h, a, b)] } else { Vec::new() };
        }
        let ((first, a), (last, b)) = if h < f { ((h, sel.anchor), (f, sel.focus)) } else { ((f, sel.focus), (h, sel.anchor)) };
        let Some(g) = self.group_of(h) else { return Vec::new() };
        (first..=last).filter(|&j| self.member(j, g)).filter_map(|j| {
            let len = self.content_of(j).len();
            let (a, b) = (if j == first { a.min(len) } else { 0 }, if j == last { b.min(len) } else { len });
            (a < b).then_some((j, a, b))
        }).collect()
    }

    /// After a commit: drop a selection whose texts went away, stopped being selectable or
    /// changed what it selected, or whose texts between its ends changed. Text appended since
    /// is kept, and read from now on. Content only changes in a commit, so this is the one place
    /// the texts between are checked.
    pub(super) fn prune_text_selection(&mut self) {
        if self.text_sel.path.is_none() { return; }
        let mut rebind = false;
        if self.selection_holder().is_none()
            && let Some(old_path) = self.text_sel.path.clone()
        {
            let upto = self.text_sel.anchor.max(self.text_sel.focus).max(self.text_sel.origin.1);
            if let Some(i) = self.rebound(&old_path, &self.text_sel.content, upto) {
                let old_ancs: Vec<&str> = std::iter::successors(Some(old_path.as_str()), |p| p.rsplit_once('/').map(|(a, _)| a)).collect();
                let new_ancs: Vec<usize> = std::iter::successors(Some(i), |&p| self.nodes[p].parent).collect();
                for (&old_anc, &new_anc) in old_ancs.iter().zip(&new_ancs) {
                    if let Some(v) = self.sel_heard.by_path.remove(old_anc) {
                        self.sel_heard.by_path.insert(self.nodes[new_anc].path.clone(), v);
                    }
                }
                self.text_sel.path = Some(self.nodes[i].path.clone());
                rebind = true;
            }
        }
        if let Some(h) = self.selection_holder()
            && self.focus_holder(h).is_none()
            && let Some((far, content)) = &self.text_sel.far
            && let Some(j) = self.rebound(far, content, self.text_sel.focus)
        {
            self.text_sel.far = Some((self.nodes[j].path.clone(), content.clone()));
            rebind = true;
        }
        if rebind && let Some((h, f)) = self.selection_holder().and_then(|h| Some((h, self.focus_holder(h)?))) {
            self.text_sel.between = self.between_hash(h, f);
        }
        let Some((h, f)) = self.selection_holder().and_then(|h| Some((h, self.focus_holder(h)?)))
            .filter(|&(h, f)| self.text_sel.far.is_none() || self.between_hash(h, f) == self.text_sel.between)
        else {
            self.clear_text_selection();
            return;
        };
        if self.text_sel.content != self.content_of(h) { self.text_sel.content = self.content_of(h).to_owned(); }
        if f != h && self.text_sel.far.as_ref().is_some_and(|(_, c)| c != self.content_of(f)) {
            let now = self.content_of(f).to_owned();
            if let Some((_, c)) = &mut self.text_sel.far { *c = now; }
        }
    }

    /// The text holding the selection's anchor and its selected byte range there (empty after a
    /// single click), for tests and tools.
    pub fn text_selection(&self) -> Option<(&str, std::ops::Range<usize>)> {
        let h = self.selection_holder()?;
        let sel = &self.text_sel;
        let range = match sel.far {
            None => sel.anchor.min(sel.focus)..sel.anchor.max(sel.focus),
            Some(_) => range_in(&self.selection_ranges(), h).map_or(sel.anchor..sel.anchor, |(a, b)| a..b),
        };
        Some((sel.path.as_deref()?, range))
    }

    /// The selected text as drawn, as copying puts it on the clipboard without an
    /// [`El::selection_copy`] (as authored, before `El::text_case`). Across a selection group's
    /// texts: a newline between texts, a tab between texts side by side (table cells).
    pub fn selected_text(&self) -> Option<String> {
        let out = self.joined(&self.selection_ranges());
        (!out.is_empty()).then_some(out)
    }

    /// The selection as [`Tx::text_selection`] reads it, `None` with nothing selected.
    pub fn selection(&self) -> Option<TextSelection> {
        let ranges = self.selection_ranges();
        if ranges.is_empty() { return None; }
        let texts = ranges.iter().map(|&(i, a, b)| self.selected_piece(i, a..b, None)).collect();
        Some(TextSelection { texts, text: self.joined(&ranges) })
    }

    /// [`Runtime::selection`] for a handler's `Tx`, kept until a commit or the selection changes.
    pub(super) fn selection_snapshot(&self) -> Option<Rc<TextSelection>> {
        self.text_sel.path.as_ref()?;
        {
            let cache = self.sel_snapshot.borrow();
            if let Some(s) = cache.as_ref().filter(|s| self.same_from(&s.from)) { return s.selection.clone(); }
        }
        let from = self.selection_from();
        let selection = self.selection().map(Rc::new);
        *self.sel_snapshot.borrow_mut() = Some(Snapshot { from, selection: selection.clone() });
        selection
    }

    fn same_from(&self, from: &Basis) -> bool {
        let sel = &self.text_sel;
        from.0 == self.commits
            && from.1.as_deref() == sel.path.as_deref()
            && from.2 == sel.anchor
            && from.3 == sel.focus
            && from.4.as_deref() == sel.far.as_ref().map(|f| f.0.as_str())
    }

    fn selection_from(&self) -> Basis {
        let sel = &self.text_sel;
        (self.commits, sel.path.clone(), sel.anchor, sel.focus, sel.far.as_ref().map(|f| f.0.clone()))
    }

    /// The `El::on_selection_change` calls owed since the last check: each element whose part
    /// of the selection changed, started or cleared.
    pub(super) fn selection_changes(&mut self) -> Vec<Action> {
        if self.sel_heard.from.as_ref().is_some_and(|f| self.same_from(f)) { return Vec::new(); }
        self.sel_heard.from = Some(self.selection_from());
        if self.sel_heard.by_path.is_empty() && !self.nodes.iter().any(|n| n.on_selection_change.is_some()) {
            return Vec::new();
        }
        let watchers: Vec<usize> = (0..self.nodes.len())
            .filter(|&i| self.nodes[i].on_selection_change.is_some() && !self.subtree_exiting(i) && self.allowed(i))
            .collect();
        if watchers.is_empty() && self.sel_heard.by_path.is_empty() { return Vec::new(); }
        let selected: Vec<usize> = self.selection_ranges().into_iter().filter(|r| r.1 < r.2).map(|r| r.0).collect();
        let inside = |w: usize, j: usize| std::iter::successors(Some(j), |&p| self.nodes[p].parent).any(|p| p == w);
        if self.sel_heard.by_path.is_empty() && !watchers.iter().any(|&w| selected.iter().any(|&j| inside(w, j))) {
            return Vec::new();
        }
        let sel = (!selected.is_empty()).then(|| self.selection_snapshot()).flatten();
        let (mut out, mut heard) = (Vec::new(), HashMap::new());
        for w in watchers {
            let path = self.nodes[w].path.clone();
            let now = sel.clone().filter(|_| selected.iter().any(|&j| inside(w, j)));
            let before = self.sel_heard.by_path.remove(&path).or_else(|| {
                let k = self.sel_heard.by_path.keys().find(|k| same_keyed_path(k, &path))?.clone();
                self.sel_heard.by_path.remove(&k)
            });
            let same = match (&before, &now) {
                (Some(a), Some(b)) => {
                    Rc::ptr_eq(a, b)
                        || (a.text == b.text
                            && a.texts.len() == b.texts.len()
                            && a.texts.iter().zip(&b.texts).all(|(x, y)| {
                                x.key == y.key
                                    && x.range == y.range
                                    && x.len == y.len
                                    && x.whole == y.whole
                                    && x.text == y.text
                                    && (x.path == y.path || same_keyed_path(&x.path, &y.path))
                            }))
                }
                (a, b) => a.is_none() && b.is_none(),
            };
            if !same {
                out.push(Action::SelectionChange(path.clone(), SelectionChange { selection: now.clone(), was: before.is_some() }));
            }
            if let Some(now) = now { heard.insert(path, now); }
        }
        self.sel_heard.by_path = heard;
        out
    }

    /// Node `i` as a [`SelectedText`] with `range` selected, its path relative to `base`'s.
    fn selected_piece(&self, i: usize, range: Range<usize>, base: Option<usize>) -> SelectedText {
        let n = &self.nodes[i];
        let path = match base {
            Some(b) if b == i => String::new(),
            Some(b) => n.path.strip_prefix(&self.nodes[b].path).and_then(|p| p.strip_prefix('/')).unwrap_or(&n.path).to_owned(),
            None => n.path.clone(),
        };
        let len = self.content_of(i).len();
        let (full_a, full_b) = n.text.as_ref().filter(|t| !t.uncopied.is_empty())
            .map_or((0, len), |t| trim_uncopied(&t.uncopied, (0, len)));
        let whole = !range.is_empty() && len > 0 && range.start <= full_a && range.end >= full_b;
        let text = n.text.as_ref().and_then(|t| t.copy_slice(range.start as u32, range.end as u32)).unwrap_or_default();
        SelectedText { path, key: n.key.clone(), range, len, whole, text }
    }

    /// What copying the selection puts on the clipboard: the nearest [`El::selection_copy`]
    /// around it says, else the text as drawn.
    fn copy_text(&self) -> Option<String> {
        let ranges = self.selection_ranges();
        let (&(first, ..), &(last, ..)) = (ranges.first()?, ranges.last()?);
        let drawn = self.joined(&ranges);
        let around = |a: usize, j: usize| std::iter::successors(Some(j), |&p| self.nodes[p].parent).any(|p| p == a);
        let provider = std::iter::successors(Some(first), |&p| self.nodes[p].parent)
            .find(|&p| self.nodes[p].selection_copy.is_some() && around(p, last));
        if let Some(p) = provider && let Some(f) = self.nodes[p].selection_copy.clone() {
            let g = self.group_of(first);
            let texts = (p..self.nodes.len())
                .take_while(|&j| around(p, j))
                .filter(|&j| self.is_selectable(j) && !self.subtree_exiting(j) && self.allowed(j))
                .filter(|&j| match g { Some(g) => self.group_of(j) == Some(g), None => j == first })
                .map(|j| self.selected_piece(j, range_in(&ranges, j).map_or(0..0, |(a, b)| a..b), Some(p)))
                .collect();
            if let Some(text) = f(&TextSelection { texts, text: drawn.clone() }) { return Some(text); }
        }
        (!drawn.is_empty()).then_some(drawn)
    }

    /// The texts of `ranges` joined as copying joins them.
    fn joined(&self, ranges: &[(usize, usize, usize)]) -> String {
        let mut out = String::new();
        let mut prev: Option<usize> = None;
        for &(i, a, b) in ranges {
            let Some(piece) = self.nodes[i].text.as_ref().and_then(|t| t.copy_slice(a as u32, b as u32)) else { continue };
            if let Some(p) = prev {
                let (pb, nb) = (self.drawn_box(p), self.drawn_box(i));
                if nb[1] < pb[3] - 0.5 && nb[0] >= pb[2] - 0.5 { out.push('\t'); }
                else if !out.ends_with('\n') { out.push('\n'); }
            }
            out.push_str(&piece);
            prev = Some(i);
        }
        out
    }

    /// Selects from the start of text `first` to the end of text `last`, a later text of its
    /// selection group or `first` itself.
    fn select_span(&mut self, first: usize, last: usize) {
        let (head, content) = (self.nodes[first].path.clone(), self.content_of(first).to_owned());
        let far = (first != last).then(|| (self.nodes[last].path.clone(), self.content_of(last).to_owned()));
        let end = far.as_ref().map_or(content.len(), |f| f.1.len());
        let origin = if far.is_some() { (0, 0) } else { (0, end) };
        let sel = &mut self.text_sel;
        (sel.path, sel.content, sel.anchor, sel.focus, sel.origin, sel.unit, sel.far) = (Some(head), content, 0, end, origin, Unit::Char, far);
        sel.dragging = false;
        self.seal();
    }

    /// Carries out a `Tx::select_text`, `select_all_text` or `clear_text_selection`.
    pub(super) fn select_request(&mut self, request: SelectRequest) {
        let named = |key: &str| (0..self.nodes.len()).find(|&i| self.nodes[i].named(key) && !self.subtree_exiting(i) && self.allowed(i));
        match request {
            SelectRequest::Clear => self.clear_text_selection(),
            SelectRequest::Range(key, range) => {
                let Some(i) = named(&key).filter(|&i| self.is_selectable(i)) else { return };
                let content = self.content_of(i).to_owned();
                let (a, b) = (floor_boundary(&content, range.start), floor_boundary(&content, range.end.max(range.start)));
                let path = self.nodes[i].path.clone();
                let sel = &mut self.text_sel;
                (sel.path, sel.content, sel.anchor, sel.focus, sel.origin, sel.unit, sel.far) = (Some(path), content, a, b, (a, b), Unit::Char, None);
                sel.dragging = false;
                self.seal();
            }
            SelectRequest::All(key) => {
                let Some(i) = named(&key) else { return };
                if self.nodes[i].selection_group {
                    let members = self.group_texts(i);
                    if let (Some(&first), Some(&last)) = (members.first(), members.last()) {
                        self.select_span(first, last);
                        return;
                    }
                }
                let inside = |j: usize| j == i || std::iter::successors(self.nodes[j].parent, |&p| self.nodes[p].parent).any(|p| p == i);
                let texts = || (i..self.nodes.len()).take_while(|&j| inside(j))
                    .filter(|&j| self.is_selectable(j) && !self.subtree_exiting(j) && self.allowed(j));
                let Some(first) = texts().next() else { return };
                let last = match self.group_of(first) {
                    Some(g) => texts().filter(|&j| self.group_of(j) == Some(g)).last().unwrap_or(first),
                    None => first,
                };
                self.select_span(first, last);
            }
        }
    }

    /// `select_request`, recording what an `El::on_selection_change` callback changed as already
    /// heard so it doesn't fire again on its own request.
    pub(super) fn apply_select_request(&mut self, request: SelectRequest, from_change: bool) {
        self.select_request(request);
        if from_change {
            let _ = self.selection_changes();
        }
    }

    /// The held press is selecting text by dragging, and has selected some.
    pub(super) fn selected_by_drag(&self) -> bool {
        self.text_sel.dragging && self.press_moved && !self.selection_ranges().is_empty()
    }

    pub(super) fn clear_text_selection(&mut self) {
        self.text_sel.path = None;
        self.text_sel.far = None;
        self.text_sel.dragging = false;
    }

    /// Byte offset of node `i`'s text under window point (x, y), where it was drawn this frame.
    fn text_index_at(&self, i: usize, x: f32, y: f32) -> Option<usize> {
        let n = &self.nodes[i];
        let (spec, layout) = (n.text.as_ref()?, n.text_layout.as_ref()?);
        let s = self.env.scale;
        let z = self.drawn_scale.get(i).copied().unwrap_or(1.0).max(1e-4);
        let (rect, x, y) = self.unturned_box_and_point(i, x, y);
        let [ox, oy] = links::origin(n, spec, layout, rect, s, z);
        let px = s / z;
        let at = crate::text::cursor_from_point(layout, (x - ox) * px, (y - oy) * px).index();
        Some(n.elided.map_or(at, |e| e.content(at)).min(spec.content.len()))
    }

    /// The text of group `g` a drag from `anchor` reaches at (x, y), and the byte there. Texts
    /// whose boxes overlap vertically (a table row's cells) form a band; in a band the point
    /// takes the text nearest it across. Between bands a drag down stops at the end of the band
    /// above, and a drag up at the start of the band below, so it reaches no further than the
    /// pointer has.
    fn group_point(&self, g: usize, anchor: usize, x: f32, y: f32) -> Option<(usize, usize)> {
        let mut boxes: Vec<_> = self.group_texts(g).into_iter().map(|j| (j, self.drawn_box(j))).collect();
        boxes.sort_by(|a, b| a.1[1].total_cmp(&b.1[1]));
        // Bands as (top, bottom, first text, last text), with their texts in `boxes[from..to]`.
        let mut bands: Vec<(f32, f32, usize, usize)> = Vec::new();
        for (k, &(_, r)) in boxes.iter().enumerate() {
            match bands.last_mut() {
                Some(b) if r[1] < b.1 => { b.1 = b.1.max(r[3]); b.3 = k + 1; }
                _ => bands.push((r[1], r[3], k, k + 1)),
            }
        }
        let texts = |b: &(f32, f32, usize, usize)| boxes[b.2..b.3].iter().map(|t| t.0);
        if let Some(b) = bands.iter().find(|b| y >= b.0 && y < b.1) {
            let &(j, _) = boxes[b.2..b.3].iter().min_by(|p, q| dist_x(p.1, x).total_cmp(&dist_x(q.1, x)))?;
            return Some((j, self.text_index_at(j, x, y)?));
        }
        let above = bands.iter().rev().find(|b| b.1 <= y);
        let below = bands.iter().find(|b| b.0 > y);
        match (above, below) {
            (a, Some(b)) if a.is_none() || texts(b).min()? <= anchor => Some((texts(b).min()?, 0)),
            (Some(a), _) => texts(a).max().map(|j| (j, self.content_of(j).len())),
            _ => None,
        }
    }

    /// A primary press on `target` (None: on nothing). On selectable text away from its links it
    /// starts a selection: one click places it, two select a word, three a paragraph, and Shift
    /// extends the current one, into another text of its selection group too. Anywhere else it
    /// clears the selection.
    pub(super) fn selection_down(&mut self, target: Option<usize>, x: f32, y: f32) {
        let Some(i) = target.filter(|&i| self.is_selectable(i) && self.link_at(i, x, y).is_none()) else {
            self.clear_text_selection();
            self.text_sel.last = None;
            return;
        };
        let Some(at) = self.text_index_at(i, x, y) else { return };
        let path = self.nodes[i].path.clone();
        let content = self.content_of(i).to_owned();
        let now = self.now_clock();
        let clicks = match &self.text_sel.last {
            Some((p, t, px, py, n)) if *p == path && now - t <= self.double_click_interval
                && (x - px).abs() <= CLICK_SLOP && (y - py).abs() <= CLICK_SLOP => (n + 1).min(3),
            _ => 1,
        };
        self.text_sel.last = Some((path.clone(), now, x, y, clicks));
        let holder = self.selection_holder().filter(|&h| h == i || self.same_group(h, i));
        match holder {
            Some(h) if clicks == 1 && self.text_sel.shift => {
                // Keep the anchor; the focus follows the pointer from here.
                let sel = &mut self.text_sel;
                sel.unit = Unit::Char;
                sel.origin = (sel.anchor, sel.anchor);
                sel.focus = at;
                sel.far = (h != i).then_some((path, content));
            }
            _ => {
                let unit = match clicks { 1 => Unit::Char, 2 => Unit::Word, _ => Unit::Paragraph };
                let origin = self.unit_in(i, at, unit);
                let sel = &mut self.text_sel;
                (sel.unit, sel.origin) = (unit, origin);
                (sel.anchor, sel.focus) = sel.origin;
                sel.path = Some(path);
                sel.content = content;
                sel.far = None;
            }
        }
        self.text_sel.dragging = true;
        self.seal();
    }

    /// The held press moved: extend the selection it started to the pointer, by its unit, into
    /// the other texts of its selection group. The press itself may belong to an ancestor (a
    /// clickable row), so this follows the selection's own text.
    pub(super) fn selection_drag(&mut self, x: f32, y: f32) {
        if !self.text_sel.dragging { return; }
        let Some(h) = self.selection_holder() else { self.clear_text_selection(); return };
        let Some((j, at)) = (match self.group_of(h) {
            Some(g) => self.group_point(g, h, x, y),
            None => self.text_index_at(h, x, y).map(|at| (h, at)),
        }) else { return };
        let at = self.unit_in(j, at, self.text_sel.unit);
        let (anchor, focus) = if j == h {
            extend(self.text_sel.origin, at)
        } else if j > h {
            (self.text_sel.origin.0, at.1)
        } else {
            (self.text_sel.origin.1, at.0)
        };
        let same_far = if j == h {
            self.text_sel.far.is_none()
        } else {
            self.text_sel.far.as_ref().is_some_and(|(p, _)| *p == self.nodes[j].path)
        };
        if (self.text_sel.anchor, self.text_sel.focus) == (anchor, focus) && same_far {
            return;
        }
        self.text_sel.anchor = anchor;
        self.text_sel.focus = focus;
        if j == h {
            self.text_sel.far = None;
        } else if !same_far {
            self.text_sel.far = Some((self.nodes[j].path.clone(), self.content_of(j).to_owned()));
        }
        if !same_far {
            self.seal();
        }
    }

    /// Cmd+C (Ctrl+C elsewhere) copies a non-empty selection and Cmd+A selects all of the text
    /// holding it, or all of its selection group (a click on it makes it the holder, as it makes
    /// an AppKit text view first responder), unless a text field has the keyboard. Only the plain
    /// chord: with Shift, Option (Alt) or Control added it's the app's shortcut.
    pub(super) fn selection_key(&mut self, ev: &KeyEvent, tx: &mut Tx) -> bool {
        let crate::Key::Command(c @ ('c' | 'a')) = ev.key else { return false };
        let m = ev.mods;
        let plain = m.cmd && !m.shift && !m.alt && if cfg!(target_os = "macos") { !m.ctrl } else { !m.logo };
        if !plain || self.text_sel.path.is_none() || self.focused_input().is_some() { return false; }
        let Some(i) = self.selection_holder() else { self.clear_text_selection(); return false };
        let path = self.nodes[i].path.clone();
        if c == 'a' {
            let texts = self.group_of(i).map(|g| self.group_texts(g)).unwrap_or_default();
            let (first, last) = (texts.first().copied().unwrap_or(i), texts.last().copied().unwrap_or(i));
            self.select_span(first, last);
            self.events.push(format!("select all {path}"));
        } else {
            let Some(text) = self.copy_text() else { return false };
            self.events.push(format!("copy {path}"));
            tx.copy(text);
        }
        true
    }

    /// A secondary click on selectable text `path` without its own `on_secondary_press` opens the
    /// Copy menu, selecting the word under the pointer first when the click is outside the
    /// selection, as AppKit text does. False when the app's handler should run instead.
    pub(super) fn selection_menu(&mut self, path: &str, x: f32, y: f32) -> bool {
        let Some(i) = self.live(path) else { return false };
        if !self.is_selectable(i) || self.nodes[i].on_secondary_press.is_some() { return false; }
        if let Some(at) = self.text_index_at(i, x, y)
            && range_in(&self.selection_ranges(), i).is_none_or(|(a, b)| at < a || at >= b)
        {
            let content = self.content_of(i).to_owned();
            let (a, b) = self.unit_in(i, at, Unit::Word);
            self.text_sel = Held { path: Some(path.to_owned()), content, anchor: a, focus: b, far: None, between: 0,
                unit: Unit::Word, origin: (a, b), dragging: false, last: None, shift: self.text_sel.shift };
        }
        let text = self.copy_text();
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

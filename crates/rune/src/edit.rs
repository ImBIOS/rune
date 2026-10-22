//! Single text-field editing: the model an app owns (`TextEdit`) and the key/IME events the
//! shell routes to the focused field. Movement that depends on wrapping (up/down, line start/end)
//! uses the field's current Parley layout, rebuilt by the runtime before each event.
//!
//! Invariants: `anchor` and `focus` are byte offsets on char boundaries of `text`; preedit text
//! is never part of `text` until committed.

use parley::editing::Selection;
use parley::layout::Affinity;
use parley::Layout;
use std::collections::VecDeque;
use std::sync::Arc;

use crate::text::Ink;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextEdit {
    pub text: String,
    pub anchor: usize,
    pub focus: usize,
    /// IME composition shown at the caret, with its selected range.
    pub preedit: Option<(String, Option<(usize, usize)>)>,
    pub focused: bool,
    history: Arc<History>,
}

const HISTORY_STEPS: usize = 128;
const HISTORY_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    text: Arc<str>,
    anchor: usize,
    focus: usize,
}

#[derive(Clone, Debug, PartialEq)]
struct Change {
    before: Snapshot,
    after: Snapshot,
}

impl Change {
    fn bytes(&self) -> usize { self.before.text.len() + self.after.text.len() }
}

// Rendering clones TextEdit. Share immutable history so a view never copies the undo stack.
#[derive(Clone, Debug, Default, PartialEq)]
struct History {
    undo: VecDeque<Change>,
    redo: Vec<Change>,
    bytes: usize,
    typing: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    /// Cmd on macOS, Ctrl elsewhere.
    pub cmd: bool,
    pub alt: bool,
    pub ctrl: bool,
    /// The Super/Windows key (Command on macOS, where `cmd` also reports it).
    pub logo: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    Text(String),
    Enter,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Escape,
    Tab,
    /// A letter with the command modifier ("a", "c", "v", "x", "z").
    Command(char),
    /// A function key, F1 through F24. Text fields ignore it; apps can bind it.
    Function(u8),
}

#[derive(Clone, Debug, PartialEq)]
pub struct KeyEvent {
    pub key: Key,
    pub mods: Mods,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Ime {
    Preedit(String, Option<(usize, usize)>),
    Commit(String),
}

/// What an edit asks of the host.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditOutcome {
    pub submit: bool,
    pub copy: Option<String>,
    pub blur: bool,
}

fn prev_char(s: &str, i: usize) -> usize {
    s[..i].char_indices().next_back().map_or(0, |(j, _)| j)
}

fn next_char(s: &str, i: usize) -> usize {
    s[i..].chars().next().map_or(i, |c| i + c.len_utf8())
}

fn prev_word(s: &str, i: usize) -> usize {
    let b = s.as_bytes();
    let mut j = i;
    while j > 0 && b[j - 1].is_ascii_whitespace() {
        j -= 1;
    }
    while j > 0 && !b[j - 1].is_ascii_whitespace() {
        j = prev_char(s, j);
    }
    j
}

fn next_word(s: &str, i: usize) -> usize {
    let b = s.as_bytes();
    let mut j = i;
    while j < b.len() && b[j].is_ascii_whitespace() {
        j += 1;
    }
    while j < b.len() && !b[j].is_ascii_whitespace() {
        j = next_char(s, j);
    }
    j
}

impl TextEdit {
    pub fn new(text: impl Into<String>) -> TextEdit {
        let text = text.into();
        let n = text.len();
        TextEdit { text, anchor: n, focus: n, preedit: None, focused: false, history: Arc::default() }
    }

    /// Clears the draft and its history after submission, preserving keyboard focus.
    pub fn clear(&mut self) {
        *self = Self { focused: self.focused, ..Self::new("") };
    }

    /// End a typing group on a pointer selection or focus change.
    pub fn break_history_group(&mut self) {
        if self.history.typing { Arc::make_mut(&mut self.history).typing = false; }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot { text: Arc::from(self.text.as_str()), anchor: self.anchor, focus: self.focus }
    }

    fn restore(&mut self, s: &Snapshot) {
        self.text.clear();
        self.text.push_str(&s.text);
        self.anchor = s.anchor;
        self.focus = s.focus;
        self.preedit = None;
    }

    /// Undo one typing group or discrete edit. Active IME composition is cancelled first.
    pub fn undo(&mut self) {
        self.break_history_group();
        if self.preedit.take().is_some() { return; }
        let h = Arc::make_mut(&mut self.history);
        let Some(change) = h.undo.pop_back() else { return };
        // Public model text may have been replaced by the app. Never restore stale history.
        if self.text != change.after.text.as_ref() { *h = History::default(); return; }
        self.restore(&change.before);
        Arc::make_mut(&mut self.history).redo.push(change);
    }

    pub fn redo(&mut self) {
        self.break_history_group();
        if self.preedit.take().is_some() { return; }
        let h = Arc::make_mut(&mut self.history);
        let Some(change) = h.redo.pop() else { return };
        if self.text != change.before.text.as_ref() { *h = History::default(); return; }
        self.restore(&change.after);
        Arc::make_mut(&mut self.history).undo.push_back(change);
    }

    pub fn range(&self) -> (usize, usize) {
        (self.anchor.min(self.focus), self.anchor.max(self.focus))
    }

    pub fn selected(&self) -> &str {
        let (a, b) = self.range();
        &self.text[a..b]
    }

    pub fn insert(&mut self, s: &str) {
        self.replace(self.range(), s, false);
    }

    fn replace(&mut self, (a, b): (usize, usize), s: &str, typing: bool) {
        if !typing { self.break_history_group(); }
        if &self.text[a..b] == s {
            self.anchor = a + s.len();
            self.focus = self.anchor;
            self.preedit = None;
            return;
        }
        let before = self.snapshot();
        self.text.replace_range(a..b, s);
        self.anchor = a + s.len();
        self.focus = self.anchor;
        self.preedit = None;
        let after = self.snapshot();
        let h = Arc::make_mut(&mut self.history);
        let expected = h.redo.last().map(|c| &c.before).or_else(|| h.undo.back().map(|c| &c.after));
        if expected.is_some_and(|last| last.text != before.text) { *h = History::default(); }
        h.bytes -= h.redo.iter().map(Change::bytes).sum::<usize>();
        h.redo.clear();
        if typing && h.typing && before.anchor == before.focus
            && h.undo.back().is_some_and(|c| c.after == before)
        {
            let last = h.undo.back_mut().unwrap();
            h.bytes -= last.bytes();
            last.after = after;
            h.bytes += last.bytes();
        } else {
            let change = Change { before, after };
            h.bytes += change.bytes();
            h.undo.push_back(change);
        }
        h.typing = typing;
        while h.undo.len() > HISTORY_STEPS || h.bytes > HISTORY_BYTES {
            h.bytes -= h.undo.pop_front().unwrap().bytes();
        }
    }

    fn set(&mut self, i: usize, extend: bool) {
        self.focus = i;
        if !extend {
            self.anchor = i;
        }
    }

    fn move_h(&mut self, i: usize, extend: bool) {
        let (a, b) = self.range();
        if !extend && a != b {
            // Collapse to the side of travel.
            let to = if i < self.focus { a } else { b };
            self.anchor = to;
            self.focus = to;
        } else {
            self.set(i, extend);
        }
    }

    /// Applies one key. `layout` is the field's layout for the current text (device px).
    pub fn key(&mut self, ev: &KeyEvent, layout: Option<&Layout<Ink>>) -> EditOutcome {
        let mut out = EditOutcome::default();
        let m = ev.mods;
        if !matches!(&ev.key, Key::Text(s) if !s.contains(['\r', '\n'])) { self.break_history_group(); }
        let word = m.alt || (m.ctrl && !cfg!(target_os = "macos"));
        let t = &self.text;
        match &ev.key {
            Key::Text(s) => {
                if s.chars().any(|c| !c.is_control()) {
                    self.replace(self.range(), s, !s.contains(['\r', '\n']));
                }
            }
            Key::Enter => {
                if m.shift || m.alt {
                    self.insert("\n");
                } else {
                    out.submit = true;
                }
            }
            Key::Tab => {}
            Key::Escape => out.blur = true,
            Key::Backspace => {
                let (a, b) = self.range();
                if a != b {
                    self.insert("");
                } else if a > 0 {
                    let from = if m.cmd { t[..a].rfind('\n').map_or(0, |i| i + 1) } else if word { prev_word(t, a) } else { prev_char(t, a) };
                    self.replace((from, a), "", false);
                }
            }
            Key::Delete => {
                let (a, b) = self.range();
                if a != b {
                    self.insert("");
                } else if a < t.len() {
                    let to = if word { next_word(t, a) } else { next_char(t, a) };
                    self.replace((a, to), "", false);
                }
            }
            Key::Left => {
                let i = if m.cmd { self.line_edge(layout, false) } else if word { prev_word(t, self.focus) } else { prev_char(t, self.focus) };
                self.move_h(i, m.shift);
            }
            Key::Right => {
                let i = if m.cmd { self.line_edge(layout, true) } else if word { next_word(t, self.focus) } else { next_char(t, self.focus) };
                self.move_h(i, m.shift);
            }
            Key::Home => self.set(0, m.shift),
            Key::End => {
                let n = t.len();
                self.set(n, m.shift)
            }
            Key::Up | Key::Down => {
                let down = ev.key == Key::Down;
                let i = if m.cmd {
                    if down { t.len() } else { 0 }
                } else if let Some(l) = layout {
                    let sel = Selection::from_byte_index(l, self.focus, Affinity::Downstream);
                    let moved = if down { sel.next_line(l, false) } else { sel.previous_line(l, false) };
                    let j = moved.focus().index();
                    if j == self.focus { if down { t.len() } else { 0 } } else { j }
                } else if down {
                    t.len()
                } else {
                    0
                };
                self.set(i.min(self.text.len()), m.shift);
            }
            Key::Command(c) => match c {
                'z' if m.shift => self.redo(),
                'z' => self.undo(),
                'a' => {
                    self.anchor = 0;
                    self.focus = self.text.len();
                }
                'c' => out.copy = Some(self.selected().to_owned()),
                'x' => {
                    out.copy = Some(self.selected().to_owned());
                    self.insert("");
                }
                _ => {}
            },
            Key::Function(_) => {}
        }
        out
    }

    fn line_edge(&self, layout: Option<&Layout<Ink>>, end: bool) -> usize {
        if let Some(l) = layout {
            let sel = Selection::from_byte_index(l, self.focus, Affinity::Downstream);
            let s = if end { sel.line_end(l, false) } else { sel.line_start(l, false) };
            return s.focus().index().min(self.text.len());
        }
        if end {
            self.text[self.focus..].find('\n').map_or(self.text.len(), |i| self.focus + i)
        } else {
            self.text[..self.focus].rfind('\n').map_or(0, |i| i + 1)
        }
    }

    pub fn ime(&mut self, ev: &Ime) -> EditOutcome {
        self.break_history_group();
        match ev {
            Ime::Preedit(s, cur) => {
                self.preedit = if s.is_empty() { None } else { Some((s.clone(), *cur)) };
            }
            Ime::Commit(s) => {
                self.preedit = None;
                self.insert(s);
            }
        }
        EditOutcome::default()
    }

    pub fn paste(&mut self, s: &str) -> EditOutcome {
        self.insert(&s.replace("\r\n", "\n"));
        EditOutcome::default()
    }

    /// The text to lay out: committed text with any preedit spliced in at the caret, and the
    /// byte range of the preedit within it.
    pub fn display(&self) -> (String, Option<(usize, usize)>) {
        match &self.preedit {
            Some((p, _)) => {
                let (a, b) = self.range();
                let mut s = String::with_capacity(self.text.len() + p.len());
                s.push_str(&self.text[..a]);
                s.push_str(p);
                s.push_str(&self.text[b..]);
                (s, Some((a, a + p.len())))
            }
            None => (self.text.clone(), None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(key: Key) -> KeyEvent {
        KeyEvent { key, mods: Mods::default() }
    }

    fn undo(e: &mut TextEdit, redo: bool) {
        e.key(&KeyEvent { key: Key::Command('z'), mods: Mods { cmd: true, shift: redo, ..Mods::default() } }, None);
    }

    #[test]
    fn history_groups_typing_and_restores_unicode_selection() {
        let mut e = TextEdit::new("");
        for s in ["h", "é", "🙂"] { e.key(&k(Key::Text(s.into())), None); }
        undo(&mut e, false);
        assert_eq!((&*e.text, e.anchor, e.focus), ("", 0, 0));
        undo(&mut e, true);
        assert_eq!((&*e.text, e.anchor, e.focus), ("hé🙂", 7, 7));
        e.key(&k(Key::Left), None);
        e.key(&KeyEvent { key: Key::Left, mods: Mods { shift: true, ..Mods::default() } }, None);
        assert_eq!((e.anchor, e.focus), (3, 1));
        e.paste("界\r\n日");
        assert_eq!(e.text, "h界\n日🙂");
        undo(&mut e, false);
        assert_eq!((&*e.text, e.anchor, e.focus), ("hé🙂", 3, 1));
        undo(&mut e, true);
        assert_eq!((&*e.text, e.anchor, e.focus), ("h界\n日🙂", 8, 8));
        undo(&mut e, false);
        e.key(&k(Key::Text("a".into())), None);
        undo(&mut e, true);
        assert_eq!(e.text, "ha🙂", "new edit discards the redo branch");
        undo(&mut e, false);
        assert_eq!((&*e.text, e.anchor, e.focus), ("hé🙂", 3, 1));
    }

    #[test]
    fn history_deletion_cut_and_ime_are_atomic() {
        let mut e = TextEdit::new("hé🙂");
        e.key(&k(Key::Backspace), None);
        undo(&mut e, false);
        assert_eq!((&*e.text, e.anchor, e.focus), ("hé🙂", 7, 7));
        e.key(&k(Key::Home), None);
        e.key(&k(Key::Delete), None);
        undo(&mut e, false);
        assert_eq!((&*e.text, e.anchor, e.focus), ("hé🙂", 0, 0));
        e.anchor = 3;
        e.focus = 1;
        let out = e.key(&KeyEvent { key: Key::Command('x'), mods: Mods::default() }, None);
        assert_eq!(out.copy.as_deref(), Some("é"));
        undo(&mut e, false);
        assert_eq!((e.anchor, e.focus), (3, 1));
        e.ime(&Ime::Preedit("に".into(), None));
        e.ime(&Ime::Preedit("日本".into(), None));
        e.ime(&Ime::Commit("日本".into()));
        assert_eq!(e.text, "h日本🙂");
        undo(&mut e, false);
        assert_eq!((&*e.text, e.anchor, e.focus), ("hé🙂", 3, 1));
        assert!(e.preedit.is_none());
        undo(&mut e, true);
        assert_eq!(e.text, "h日本🙂");
        e.ime(&Ime::Preedit("x".into(), None));
        undo(&mut e, false);
        assert_eq!(e.text, "h日本🙂", "undo cancels preedit without undoing committed text");
        assert!(e.preedit.is_none());
    }

    #[test]
    fn history_is_bounded_shared_and_cleared_after_send() {
        let mut e = TextEdit::new("");
        for _ in 0..HISTORY_STEPS + 12 { e.paste("é"); }
        let rendered = e.clone();
        assert!(Arc::ptr_eq(&e.history, &rendered.history));
        assert_eq!(e.history.undo.len(), HISTORY_STEPS);
        for _ in 0..HISTORY_STEPS + 1 { e.undo(); }
        assert_eq!(e.text, "é".repeat(12));
        assert_eq!(rendered.text, "é".repeat(HISTORY_STEPS + 12));
        e.paste(&"a".repeat(HISTORY_BYTES));
        assert!(e.history.bytes <= HISTORY_BYTES);
        assert!(e.history.undo.is_empty(), "one oversized edit drops history rather than retaining unbounded text");
        e.focused = true;
        e.clear();
        e.undo();
        assert!(e.focused && e.text.is_empty());
        e.paste("draft");
        e.text = "external".into();
        e.anchor = e.text.len();
        e.focus = e.anchor;
        e.undo();
        assert_eq!(e.text, "external", "external model replacement cannot restore stale history");
    }

    #[test]
    fn typing_selection_and_submit() {
        let mut e = TextEdit::default();
        e.key(&k(Key::Text("héllo world".into())), None);
        assert_eq!(e.text, "héllo world");
        e.key(&KeyEvent { key: Key::Backspace, mods: Mods { alt: true, ..Mods::default() } }, None);
        assert_eq!(e.text, "héllo ");
        e.key(&k(Key::Left), None);
        e.key(&KeyEvent { key: Key::Left, mods: Mods { shift: true, ..Mods::default() } }, None);
        assert_eq!(e.selected(), "o");
        e.key(&k(Key::Text("0".into())), None);
        assert_eq!(e.text, "héll0 ");
        e.ime(&Ime::Preedit("に".into(), None));
        assert_eq!(e.display().0, "héll0に ");
        e.ime(&Ime::Commit("日".into()));
        assert_eq!(e.text, "héll0日 ");
        assert!(e.key(&k(Key::Enter), None).submit);
        e.key(&KeyEvent { key: Key::Enter, mods: Mods { shift: true, ..Mods::default() } }, None);
        assert!(e.text.contains('\n'));
    }
}

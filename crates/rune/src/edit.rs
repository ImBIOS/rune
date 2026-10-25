//! Single and rich text-field editing: the attributed model an app owns (`TextEdit`) and the
//! key/IME events the shell routes to the focused field. Movement that depends on wrapping
//! (up/down, line start/end) uses the field's current Parley layout, rebuilt by the runtime
//! before each event.
//!
//! Invariants: `anchor` and `focus` are byte offsets on char boundaries of `text`; preedit text
//! is never part of `text` until committed; inline `marks` are non-empty char-boundary byte
//! ranges sorted by start; `blocks` has one entry per `\n`-separated line of `text`.

use parley::editing::Selection;
use parley::layout::Affinity;
use parley::Layout;
use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;
use zeroize::{Zeroize, Zeroizing};

use crate::color::{oklch, oklcha, Color};
use crate::text::{Ink, Link, Span};

mod html;

/// Maximum nesting level (`0..=MAX_LIST_LEVEL`) for [`Block::Bullet`] and [`Block::Numbered`].
pub const MAX_LIST_LEVEL: u8 = 5;

/// An inline formatting mark stored alongside [`TextEdit::text`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Mark {
    Bold,
    Italic,
    Underline,
    Strike,
    Code,
    Link(String),
}

impl Mark {
    /// Whether `self` and `other` are the same kind of mark (`Link(_)` matches any `Link(_)`).
    pub fn same_kind(&self, other: &Mark) -> bool {
        match (self, other) {
            (Mark::Bold, Mark::Bold)
            | (Mark::Italic, Mark::Italic)
            | (Mark::Underline, Mark::Underline)
            | (Mark::Strike, Mark::Strike)
            | (Mark::Code, Mark::Code)
            | (Mark::Link(_), Mark::Link(_)) => true,
            _ => false,
        }
    }

    /// The URL when `self` is [`Mark::Link`].
    pub fn link_url(&self) -> Option<&str> {
        match self {
            Mark::Link(url) => Some(url),
            _ => None,
        }
    }

    pub(crate) fn kind_order(&self) -> u8 {
        match self {
            Mark::Bold => 0,
            Mark::Italic => 1,
            Mark::Underline => 2,
            Mark::Strike => 3,
            Mark::Code => 4,
            Mark::Link(_) => 5,
        }
    }
}

/// A non-empty byte range `[start, end)` in [`TextEdit::text`] carrying an inline [`Mark`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MarkRange {
    pub start: usize,
    pub end: usize,
    pub mark: Mark,
}

impl MarkRange {
    pub fn new(range: Range<usize>, mark: Mark) -> Self {
        Self { start: range.start, end: range.end, mark }
    }

    pub fn range(&self) -> Range<usize> {
        self.start..self.end
    }
}

/// Paragraph-level block attribute for a `\n`-delimited line in [`TextEdit`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Block {
    #[default]
    Paragraph,
    /// Bullet list item (`•`, `◦`, `▪`) at nesting level `0..=MAX_LIST_LEVEL`.
    Bullet(u8),
    /// Numbered list item (`1.`, `2.`, …) at nesting level `0..=MAX_LIST_LEVEL`.
    Numbered(u8),
    /// Indented quote paragraph with a vertical left bar.
    Quote,
    /// Monospace code block line with a background fill; inline marks do not apply.
    Code,
}

impl Block {
    pub fn is_list(self) -> bool {
        matches!(self, Block::Bullet(_) | Block::Numbered(_))
    }

    pub fn list_level(self) -> Option<u8> {
        match self {
            Block::Bullet(l) | Block::Numbered(l) => Some(l),
            _ => None,
        }
    }

    pub fn with_level(self, level: u8) -> Self {
        let l = level.min(MAX_LIST_LEVEL);
        match self {
            Block::Bullet(_) => Block::Bullet(l),
            Block::Numbered(_) => Block::Numbered(l),
            other => other,
        }
    }

    /// Whether `self` and `other` share the same block kind (ignoring list nesting level).
    pub fn same_kind(self, other: Block) -> bool {
        matches!(
            (self, other),
            (Block::Paragraph, Block::Paragraph)
                | (Block::Bullet(_), Block::Bullet(_))
                | (Block::Numbered(_), Block::Numbered(_))
                | (Block::Quote, Block::Quote)
                | (Block::Code, Block::Code)
        )
    }

    /// Left indentation in points (before scale) for this block type.
    pub(crate) fn indent_pt(self, font_size: f32) -> f32 {
        let unit = (font_size * 1.45).round().max(18.0);
        match self {
            Block::Paragraph => 0.0,
            Block::Bullet(l) | Block::Numbered(l) => unit * (l.min(MAX_LIST_LEVEL) as f32 + 1.0),
            Block::Quote => (font_size * 1.1).round().max(14.0),
            Block::Code => (font_size * 0.75).round().max(10.0),
        }
    }
}

/// A paragraph slice of [`TextEdit`], returned by [`TextEdit::paragraphs`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paragraph {
    pub index: usize,
    pub range: Range<usize>,
    pub block: Block,
    /// True when this line was split from the previous line by `Shift+Return` (`<br>`).
    pub soft_break: bool,
}

/// Visual palette for rich marks and block attributes inside [`text_input`](crate::text_input).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RichStyle {
    pub code_fg: Option<Color>,
    pub code_bg: Color,
    pub code_block_bg: Color,
    pub link: Color,
    pub quote_bar: Color,
    pub marker: Option<Color>,
}

impl Default for RichStyle {
    fn default() -> Self {
        Self {
            code_fg: None,
            code_bg: oklcha(0.55, 0.02, 260.0, 0.16),
            code_block_bg: oklcha(0.5, 0.02, 260.0, 0.12),
            link: oklch(0.68, 0.16, 250.0),
            quote_bar: oklcha(0.6, 0.04, 255.0, 0.45),
            marker: None,
        }
    }
}

#[derive(Clone, Default, PartialEq)]
pub struct TextEdit {
    /// Use `replace_text` to replace a secret without leaving its old allocation behind.
    pub text: String,
    pub anchor: usize,
    pub focus: usize,
    /// IME composition shown at the caret, with its selected range.
    pub preedit: Option<(String, Option<(usize, usize)>)>,
    pub focused: bool,
    marks: Vec<MarkRange>,
    blocks: Vec<Block>,
    /// Line indices (`> 0`) created by `Shift+Return` (`<br>`): indented with their block,
    /// without starting a new list marker or incrementing list numbering.
    pub(crate) soft_breaks: Vec<u32>,
    /// Pending typing style armed at `(anchor, focus)` when toggling marks on an empty selection.
    pending: Option<((usize, usize), Vec<Mark>)>,
    history: Arc<History>,
    secure: bool,
    /// Mirrors the field's `El::single_line()`; the runtime sets it before each edit.
    single_line: bool,
}

impl std::fmt::Debug for TextEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextEdit").field("text", &if self.secure { "[redacted]" } else { &self.text })
            .field("anchor", &self.anchor).field("focus", &self.focus).field("focused", &self.focused)
            .field("marks", &self.marks).field("blocks", &self.blocks)
            .field("secure", &self.secure).finish_non_exhaustive()
    }
}

/// Hashes everything a field draws: text, selection, IME composition, focus, marks, blocks and
/// whether it's secure. Pass the edit itself in a [`memo`](crate::memo) version so the field
/// redraws as the caret or rich formatting changes. A secure field hashes its length, not its text.
impl std::hash::Hash for TextEdit {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        if self.secure { self.text.chars().count().hash(h) } else { self.text.hash(h) }
        self.anchor.hash(h);
        self.focus.hash(h);
        self.preedit.hash(h);
        self.focused.hash(h);
        self.marks.hash(h);
        self.blocks.hash(h);
        self.soft_breaks.hash(h);
        self.pending.hash(h);
        self.secure.hash(h);
    }
}

impl Drop for TextEdit {
    fn drop(&mut self) {
        self.text.zeroize();
        self.clear_preedit();
    }
}

const HISTORY_STEPS: usize = 128;
const HISTORY_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    text: Arc<Zeroizing<String>>,
    anchor: usize,
    focus: usize,
    marks: Vec<MarkRange>,
    blocks: Vec<Block>,
    soft_breaks: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq)]
struct Change {
    before: Snapshot,
    after: Snapshot,
}

impl Change {
    fn bytes(&self) -> usize {
        self.before.text.len()
            + self.after.text.len()
            + (self.before.marks.len() + self.after.marks.len()) * std::mem::size_of::<MarkRange>()
            + (self.before.blocks.len() + self.after.blocks.len()) * std::mem::size_of::<Block>()
    }
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

/// A logical key. More keys may come, so a `match` needs a `_` arm.
#[non_exhaustive]
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
    /// Page Up. Unclaimed, it scrolls the nearest scroll view up by most of a viewport.
    /// Text fields leave it alone, as AppKit's do: the view scrolls and the caret stays.
    PageUp,
    /// Page Down, as `PageUp` the other way.
    PageDown,
    Escape,
    Tab,
    /// Any character typed with Cmd (Ctrl off macOS), lowercased: a letter ("a", "c", "v"), a
    /// digit (⌘1) or punctuation (⌘,). `mods` still says whether Shift, Option or Ctrl were
    /// held too. Space is the exception: Cmd-Space arrives natively as `Text(" ")` with
    /// `mods.cmd` (scripts send `Command(' ')`); no other native `Text` comes with `mods.cmd`.
    /// [`KeyEvent::is_command`] matches either form.
    Command(char),
    /// A function key, F1 through F24. Text fields ignore it; apps can bind it.
    Function(u8),
    /// A modifier key going down (`pressed`) or up on its own, sent only where the app opts in
    /// with `App::modifier_keys` (or `Runtime::set_modifier_keys`). `mods` holds the modifiers
    /// after the change, as AppKit's flagsChanged does: Shift's press has `shift` set and its
    /// release doesn't, unless the other Shift is still down. A held modifier never repeats.
    /// Text fields, selection, focus traversal and shortcuts ignore it.
    Modifier { modifier: Modifier, pressed: bool },
}

/// A modifier key, by side. Alt is Option on macOS; Super is Command on macOS and the Windows
/// key elsewhere.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Modifier {
    ShiftLeft,
    ShiftRight,
    ControlLeft,
    ControlRight,
    AltLeft,
    AltRight,
    SuperLeft,
    SuperRight,
    /// Fn, or Globe on recent Mac keyboards. Other platforms rarely report it: their keyboards
    /// handle Fn in firmware.
    Fn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeySide {
    Left,
    Right,
}

impl Modifier {
    /// Which side of the keyboard the key is on; None for Fn.
    pub fn side(self) -> Option<KeySide> {
        use Modifier::*;
        match self {
            ShiftLeft | ControlLeft | AltLeft | SuperLeft => Some(KeySide::Left),
            ShiftRight | ControlRight | AltRight | SuperRight => Some(KeySide::Right),
            Fn => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct KeyEvent {
    pub key: Key,
    pub mods: Mods,
}

impl KeyEvent {
    /// Whether this is Cmd (Ctrl off macOS) plus `c`, whatever else is held: `Key::Command(c)`,
    /// or for `' '` the `Key::Text(" ")` a native Cmd-Space sends. Letters match either case;
    /// check `mods.shift` for ⇧⌘ chords.
    pub fn is_command(&self, c: char) -> bool {
        let c = c.to_ascii_lowercase();
        match &self.key {
            Key::Command(k) => *k == c,
            Key::Text(t) => c == ' ' && t == " " && self.mods.cmd,
            _ => false,
        }
    }

    /// Text typed with Cmd (macOS) or Ctrl held, such as an unclaimed Shift-Cmd-Space. It never
    /// inserts, matching browsers and AppKit, but still reaches key handlers. Ctrl+Alt is AltGr
    /// on Windows and Linux, so text typed with both still inserts there; Option alone types.
    pub(crate) fn is_command_chord(&self) -> bool {
        let m = self.mods;
        matches!(self.key, Key::Text(_)) && (m.cmd || m.ctrl) && (cfg!(target_os = "macos") || !m.alt)
    }
    /// A macOS Control-letter text binding (Ctrl-A, Ctrl-E, Ctrl-K, …), as in every AppKit field.
    pub(crate) fn control_binding(&self) -> Option<char> {
        let m = self.mods;
        let Key::Text(s) = &self.key else { return None };
        let c = s.chars().next().filter(|_| s.chars().count() == 1)?.to_ascii_lowercase();
        (cfg!(target_os = "macos") && m.ctrl && !m.cmd && !m.alt && "abefpndhk".contains(c)).then_some(c)
    }
}

impl Drop for KeyEvent {
    fn drop(&mut self) {
        if let Key::Text(text) = &mut self.key { text.zeroize(); }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Ime {
    Preedit(String, Option<(usize, usize)>),
    Commit(String),
}

impl Drop for Ime {
    fn drop(&mut self) {
        match self { Self::Preedit(text, _) | Self::Commit(text) => text.zeroize() }
    }
}

/// What an edit asks of the host.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditOutcome {
    pub submit: bool,
    pub copy: Option<String>,
    pub blur: bool,
}

/// The previous grapheme boundary, so a caret or backspace takes a ZWJ emoji, flag or skin
/// tone whole.
fn prev_char(s: &str, i: usize) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    s[..i].grapheme_indices(true).next_back().map_or(0, |(j, _)| j)
}

fn next_char(s: &str, i: usize) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    s[i..].graphemes(true).next().map_or(i, |g| i + g.len())
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
        let line_count = 1 + text.bytes().filter(|&b| b == b'\n').count();
        TextEdit {
            text,
            anchor: n,
            focus: n,
            preedit: None,
            focused: false,
            marks: Vec::new(),
            blocks: vec![Block::Paragraph; line_count],
            soft_breaks: Vec::new(),
            pending: None,
            history: Arc::default(),
            secure: false,
            single_line: false,
        }
    }

    /// Construct a [`TextEdit`] from plain text, inline [`MarkRange`]s, and per-line [`Block`]s.
    /// Caret starts at the end of `text`. Ranges are clamped to char boundaries and canonicalized,
    /// and `blocks` is padded or truncated to match the number of `\n`-separated lines in `text`.
    pub fn from_parts(text: impl Into<String>, marks: Vec<MarkRange>, blocks: Vec<Block>) -> TextEdit {
        let mut edit = Self::new(text);
        edit.marks = marks;
        edit.blocks = blocks;
        edit.sync_invariants();
        edit
    }

    /// Parse an HTML fragment (`<b>`, `<strong>`, `<i>`, `<em>`, `<u>`, `<s>`, `<del>`, `<strike>`,
    /// `<a href>`, `<code>`, `<pre>`, `<ul>`, `<ol>`, `<li>`, `<blockquote>`, `<p>`, `<div>`, `<br>`,
    /// and inline `style` for `font-weight`, `font-style`, `text-decoration`) into a [`TextEdit`].
    /// Unknown tags keep their text and strip the wrapper; `<script>` and `<style>` are dropped.
    pub fn from_html(html_str: &str) -> TextEdit {
        html::parse_html(html_str)
    }

    /// Serialize the entire document's text, inline marks, and paragraph blocks to safe HTML.
    pub fn to_html(&self) -> String {
        html::serialize_html(self, 0..self.text.len())
    }

    /// Serialize the current selection (`self.range()`) to HTML, or `""` when the selection is empty.
    pub fn selected_html(&self) -> String {
        let (a, b) = self.range();
        if a == b || self.secure {
            return String::new();
        }
        html::serialize_html(self, a..b)
    }

    /// Inline [`MarkRange`]s on `self.text`, sorted by start byte.
    pub fn marks(&self) -> &[MarkRange] {
        &self.marks
    }

    /// Paragraph [`Block`] attributes, one per `\n`-delimited line of `self.text`.
    pub fn blocks(&self) -> &[Block] {
        if self.blocks.is_empty() {
            &[Block::Paragraph]
        } else {
            &self.blocks
        }
    }

    /// Replace the inline [`MarkRange`]s on `self.text` and canonicalize them.
    pub fn set_marks(&mut self, marks: Vec<MarkRange>) {
        self.marks = marks;
        self.sync_invariants();
    }

    /// Replace the per-line [`Block`] list on `self.text`.
    pub fn set_blocks(&mut self, blocks: Vec<Block>) {
        self.blocks = blocks;
        self.sync_invariants();
    }

    /// Line indices (`> 0`) created by `Shift+Return` (`<br>`) rather than a hard paragraph break.
    pub fn soft_breaks(&self) -> &[u32] {
        &self.soft_breaks
    }

    /// Iterate the `\n`-delimited paragraphs of `self.text` with their byte range and [`Block`].
    pub fn paragraphs(&self) -> Vec<Paragraph> {
        let mut out = Vec::new();
        let mut line_start = 0;
        let mut idx = 0;
        for (i, b) in self.text.bytes().enumerate() {
            if b == b'\n' {
                let block = self.blocks.get(idx).copied().unwrap_or(Block::Paragraph);
                let soft_break = self.soft_breaks.binary_search(&(idx as u32)).is_ok();
                out.push(Paragraph { index: idx, range: line_start..i, block, soft_break });
                line_start = i + 1;
                idx += 1;
            }
        }
        let block = self.blocks.get(idx).copied().unwrap_or(Block::Paragraph);
        let soft_break = self.soft_breaks.binary_search(&(idx as u32)).is_ok();
        out.push(Paragraph { index: idx, range: line_start..self.text.len(), block, soft_break });
        out
    }

    /// Password editing: masked display, no undo, copy, cut or IME. All model buffers are
    /// wiped on drop; secure replacements also wipe the previous allocation before freeing it.
    pub fn secure(mut self) -> Self { self.set_secure(true); self }

    pub fn is_secure(&self) -> bool { self.secure }

    pub fn set_secure(&mut self, secure: bool) {
        self.secure = secure;
        if secure {
            self.history = Arc::default();
            self.marks.clear();
            self.blocks.clear();
            self.soft_breaks.clear();
            self.pending = None;
            self.clear_preedit();
        }
    }

    pub(crate) fn clear_preedit(&mut self) {
        if let Some((mut text, _)) = self.preedit.take() { text.zeroize(); }
    }

    pub(crate) fn clear_pending(&mut self) {
        self.pending = None;
    }

    /// Replace the whole draft and discard its history, preserving focus and password mode.
    pub fn replace_text(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.text.zeroize();
        let line_count = 1 + text.bytes().filter(|&b| b == b'\n').count();
        self.text = text;
        self.anchor = self.text.len();
        self.focus = self.anchor;
        self.marks.clear();
        self.blocks = vec![Block::Paragraph; line_count];
        self.soft_breaks.clear();
        self.pending = None;
        self.clear_preedit();
        self.history = Arc::default();
    }

    #[cfg(test)]
    pub(crate) fn has_history(&self) -> bool { !self.history.undo.is_empty() || !self.history.redo.is_empty() }

    /// For a secure field that has started exiting: wipe the secret and keep only its mask, so
    /// the field paints the same dots (no placeholder) through the exit with no plaintext.
    pub(crate) fn conceal(&mut self) {
        if !self.secure { return; }
        self.replace_text("•".repeat(self.text.chars().count()));
    }

    /// Map model bytes to the bullet layout's bytes and back. Each Unicode scalar gets one
    /// bullet, so UTF-8 length never changes caret/selection alignment.
    pub(crate) fn display_index(&self, byte: usize) -> usize {
        if self.secure { self.text[..byte.min(self.text.len())].chars().count() * '•'.len_utf8() } else { byte }
    }
    pub(crate) fn model_index(&self, byte: usize) -> usize {
        if self.secure {
            self.text.char_indices().nth(byte / '•'.len_utf8()).map_or(self.text.len(), |(i, _)| i)
        } else { byte.min(self.text.len()) }
    }

    /// Clears the draft and its history after submission, preserving keyboard focus.
    pub fn clear(&mut self) {
        self.replace_text("");
    }

    /// End a typing group on a pointer selection or focus change.
    pub fn break_history_group(&mut self) {
        if self.history.typing { Arc::make_mut(&mut self.history).typing = false; }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: Arc::new(Zeroizing::new(self.text.clone())),
            anchor: self.anchor,
            focus: self.focus,
            marks: self.marks.clone(),
            blocks: self.blocks.clone(),
            soft_breaks: self.soft_breaks.clone(),
        }
    }

    fn restore(&mut self, s: &Snapshot) {
        self.text.zeroize();
        self.text.push_str(&s.text);
        self.anchor = s.anchor;
        self.focus = s.focus;
        self.marks = s.marks.clone();
        self.blocks = s.blocks.clone();
        self.soft_breaks = s.soft_breaks.clone();
        self.pending = None;
        self.clear_preedit();
    }

    fn record_change(&mut self, before: Snapshot, typing: bool) {
        let after = self.snapshot();
        if before == after {
            return;
        }
        let h = Arc::make_mut(&mut self.history);
        let expected = h.redo.last().map(|c| &c.before).or_else(|| h.undo.back().map(|c| &c.after));
        if expected.is_some_and(|last| last.text != before.text) {
            *h = History::default();
        }
        h.bytes -= h.redo.iter().map(Change::bytes).sum::<usize>();
        h.redo.clear();
        if typing
            && h.typing
            && before.anchor == before.focus
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

    /// Undo one typing group or discrete edit. Active IME composition is cancelled first.
    pub fn undo(&mut self) {
        if self.secure { return; }
        self.break_history_group();
        if self.preedit.is_some() { self.clear_preedit(); return; }
        let h = Arc::make_mut(&mut self.history);
        let Some(change) = h.undo.pop_back() else { return };
        // Public model text may have been replaced by the app. Never restore stale history.
        if self.text != change.after.text.as_str() { *h = History::default(); return; }
        self.restore(&change.before);
        Arc::make_mut(&mut self.history).redo.push(change);
    }

    pub fn redo(&mut self) {
        if self.secure { return; }
        self.break_history_group();
        if self.preedit.is_some() { self.clear_preedit(); return; }
        let h = Arc::make_mut(&mut self.history);
        let Some(change) = h.redo.pop() else { return };
        if self.text != change.before.text.as_str() { *h = History::default(); return; }
        self.restore(&change.after);
        Arc::make_mut(&mut self.history).undo.push_back(change);
    }

    pub fn range(&self) -> (usize, usize) {
        let len = self.text.len();
        (self.anchor.min(self.focus).min(len), self.anchor.max(self.focus).min(len))
    }

    /// Move the caret to `byte` (clamped to a character boundary of `self.text`) and collapse the
    /// selection, clearing any pending typing style.
    pub fn set_caret(&mut self, byte: usize) {
        let b = Self::clamp_char_boundary(&self.text, byte);
        self.set(b, false);
    }

    /// Set the selection `anchor` and `focus` byte indices (clamped to character boundaries of
    /// `self.text`), clearing any pending typing style.
    pub fn set_selection(&mut self, anchor: usize, focus: usize) {
        self.anchor = Self::clamp_char_boundary(&self.text, anchor);
        self.focus = Self::clamp_char_boundary(&self.text, focus);
        self.pending = None;
    }

    pub fn selected(&self) -> &str {
        let (a, b) = self.range();
        &self.text[a..b]
    }

    pub fn insert(&mut self, s: &str) {
        self.replace(self.range(), s, false);
    }

    pub(crate) fn set_single_line(&mut self, single_line: bool) { self.single_line = single_line; }

    /// Line index (`0..line_count`) containing `byte`.
    pub fn line_of_byte(&self, byte: usize) -> usize {
        let clamped = byte.min(self.text.len());
        self.text.as_bytes()[..clamped].iter().filter(|&&b| b == b'\n').count()
    }

    /// Byte range (`start..end`, excluding `\n`) of 0-based `line_idx`.
    pub fn line_range(&self, line_idx: usize) -> Range<usize> {
        let mut cur = 0;
        let mut start = 0;
        for (i, b) in self.text.bytes().enumerate() {
            if b == b'\n' {
                if cur == line_idx {
                    return start..i;
                }
                start = i + 1;
                cur += 1;
            }
        }
        if cur == line_idx {
            start..self.text.len()
        } else {
            self.text.len()..self.text.len()
        }
    }

    fn clamp_char_boundary(s: &str, mut idx: usize) -> usize {
        idx = idx.min(s.len());
        while idx > 0 && !s.is_char_boundary(idx) {
            idx -= 1;
        }
        idx
    }

    /// Synchronize `blocks`, `soft_breaks`, and `marks` with `self.text`.
    pub(crate) fn sync_invariants(&mut self) {
        let line_count = 1 + self.text.bytes().filter(|&b| b == b'\n').count();
        if self.blocks.len() < line_count {
            self.blocks.resize(line_count, Block::Paragraph);
        } else if self.blocks.len() > line_count {
            self.blocks.truncate(line_count);
        }
        self.soft_breaks.sort_unstable();
        self.soft_breaks.dedup();
        self.soft_breaks.retain(|&idx| idx > 0 && (idx as usize) < line_count);
        if let Some((armed, _)) = self.pending
            && armed != (self.anchor, self.focus)
        {
            self.pending = None;
        }
        self.normalize_marks();
    }

    fn normalize_marks(&mut self) {
        if self.marks.is_empty() {
            return;
        }
        let len = self.text.len();
        for m in &mut self.marks {
            m.start = Self::clamp_char_boundary(&self.text, m.start);
            m.end = Self::clamp_char_boundary(&self.text, m.end);
        }
        self.marks.retain(|m| m.start < m.end && m.end <= len);

        // Strip inline marks from any `Block::Code` paragraph.
        if self.blocks.contains(&Block::Code) {
            let mut line_start = 0;
            let mut idx = 0;
            let mut code_ranges: Vec<Range<usize>> = Vec::new();
            for (i, b) in self.text.bytes().enumerate() {
                if b == b'\n' {
                    if self.blocks.get(idx) == Some(&Block::Code) {
                        code_ranges.push(line_start..i + 1);
                    }
                    line_start = i + 1;
                    idx += 1;
                }
            }
            if self.blocks.get(idx) == Some(&Block::Code) {
                code_ranges.push(line_start..len);
            }
            for cr in code_ranges {
                self.strip_all_marks_in_range(cr.start, cr.end);
            }
        }

        // Merge overlapping or adjacent ranges of the same mark kind.
        // For `Mark::Link`, later entries win over earlier overlapping links.
        let mut by_kind: [Vec<MarkRange>; 6] = Default::default();
        for m in self.marks.drain(..) {
            if m.start < m.end {
                by_kind[m.mark.kind_order() as usize].push(m);
            }
        }

        let mut merged = Vec::new();
        for (kind_idx, mut list) in by_kind.into_iter().enumerate() {
            if list.is_empty() {
                continue;
            }
            if kind_idx == 5 {
                // `Mark::Link`: resolve overlaps so later links overwrite earlier ones, then merge adjacent equal URLs.
                let mut resolved: Vec<MarkRange> = Vec::new();
                for link in list {
                    let mut next_resolved = Vec::with_capacity(resolved.len() + 1);
                    for old in resolved {
                        if old.end <= link.start || old.start >= link.end {
                            next_resolved.push(old);
                        } else {
                            if old.start < link.start {
                                next_resolved.push(MarkRange {
                                    start: old.start,
                                    end: link.start,
                                    mark: old.mark.clone(),
                                });
                            }
                            if old.end > link.end {
                                next_resolved.push(MarkRange {
                                    start: link.end,
                                    end: old.end,
                                    mark: old.mark,
                                });
                            }
                        }
                    }
                    next_resolved.push(link);
                    resolved = next_resolved;
                }
                resolved.sort_by_key(|m| (m.start, m.end));
                let mut iter = resolved.into_iter();
                if let Some(mut cur) = iter.next() {
                    for m in iter {
                        if m.mark == cur.mark && m.start <= cur.end {
                            cur.end = cur.end.max(m.end);
                        } else {
                            merged.push(cur);
                            cur = m;
                        }
                    }
                    merged.push(cur);
                }
            } else {
                list.sort_by_key(|m| (m.start, m.end));
                let mut iter = list.into_iter();
                if let Some(mut cur) = iter.next() {
                    for m in iter {
                        if m.start <= cur.end {
                            cur.end = cur.end.max(m.end);
                        } else {
                            merged.push(cur);
                            cur = m;
                        }
                    }
                    merged.push(cur);
                }
            }
        }

        merged.sort_by_key(|m| (m.start, m.end, m.mark.kind_order()));
        self.marks = merged;
    }

    fn strip_all_marks_in_range(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let mut out = Vec::with_capacity(self.marks.len());
        for m in self.marks.drain(..) {
            if m.end <= start || m.start >= end {
                out.push(m);
            } else {
                if m.start < start {
                    out.push(MarkRange { start: m.start, end: start, mark: m.mark.clone() });
                }
                if m.end > end {
                    out.push(MarkRange { start: end, end: m.end, mark: m.mark });
                }
            }
        }
        self.marks = out;
    }

    /// Marks inherited from the character before `pos` according to AppKit rules:
    /// typing inside (`m.start < pos < m.end`) or at the trailing edge (`m.start < pos == m.end`)
    /// of a mark inherits it; typing at the start of a mark (`pos == m.start`) does not extend it.
    fn inherited_marks_at(&self, pos: usize) -> Vec<Mark> {
        let line_idx = self.line_of_byte(pos);
        if self.blocks.get(line_idx) == Some(&Block::Code) {
            return Vec::new();
        }
        let line_start = self.line_range(line_idx).start;
        if pos <= line_start {
            return Vec::new();
        }
        let mut out = Vec::new();
        for m in &self.marks {
            if m.start < pos && pos <= m.end && !out.iter().any(|existing: &Mark| existing.same_kind(&m.mark)) {
                out.push(m.mark.clone());
            }
        }
        out
    }

    /// Whether `mark` covers the entire byte range `start..end` (`start < end`).
    fn range_has_mark(&self, start: usize, end: usize, mark: &Mark) -> bool {
        if start >= end {
            return false;
        }
        // Because `normalize_marks` merges contiguous ranges of the same mark, a mark covers
        // the entire `start..end` iff a single `MarkRange` with `m.start <= start && end <= m.end` exists
        // (for `Link("")`, any link covering each subrange, or a single link covering `start..end`).
        let match_any_link = matches!(mark, Mark::Link(u) if u.is_empty());
        self.marks.iter().any(|m| {
            let kind_matches = if match_any_link {
                matches!(m.mark, Mark::Link(_))
            } else {
                &m.mark == mark
            };
            kind_matches && m.start <= start && end <= m.end
        })
    }

    /// Active inline marks at the caret (including pending typing style) or covering the whole
    /// selection, for driving a formatting bar's pressed state.
    pub fn active_marks(&self) -> Vec<Mark> {
        let (a, b) = self.range();
        if a == b {
            let line_idx = self.line_of_byte(a);
            if self.blocks.get(line_idx) == Some(&Block::Code) {
                return Vec::new();
            }
            if let Some((armed, ref p)) = self.pending
                && armed == (self.anchor, self.focus)
            {
                return p.clone();
            }
            return self.inherited_marks_at(a);
        }
        let mut out = Vec::new();
        for m in &self.marks {
            if m.start <= a && b <= m.end && !out.iter().any(|existing: &Mark| existing == &m.mark) {
                out.push(m.mark.clone());
            }
        }
        out
    }

    /// Whether `mark` is active at the caret or covers the whole selection.
    /// Passing `Mark::Link(String::new())` checks whether *any* link is active.
    pub fn has_mark(&self, mark: &Mark) -> bool {
        let match_any_link = matches!(mark, Mark::Link(u) if u.is_empty());
        self.active_marks().iter().any(|m| {
            if match_any_link {
                matches!(m, Mark::Link(_))
            } else {
                m == mark
            }
        })
    }

    /// The URL of the active [`Mark::Link`] at the caret or covering the selection, if any.
    pub fn active_link(&self) -> Option<&str> {
        let (a, b) = self.range();
        if a == b {
            if let Some((armed, ref p)) = self.pending
                && armed == (self.anchor, self.focus)
            {
                return p.iter().find_map(Mark::link_url);
            }
            let line_idx = self.line_of_byte(a);
            if self.blocks.get(line_idx) == Some(&Block::Code) || a <= self.line_range(line_idx).start {
                return None;
            }
            return self
                .marks
                .iter()
                .find_map(|m| (m.start < a && a <= m.end).then(|| m.mark.link_url()).flatten());
        }
        self.marks
            .iter()
            .find_map(|m| (m.start <= a && b <= m.end).then(|| m.mark.link_url()).flatten())
    }

    /// Toggle `mark` on the selection, or in the pending typing style when the selection is empty.
    /// Inside a [`Block::Code`] paragraph, inline marks do not apply.
    pub fn toggle_mark(&mut self, mark: Mark) {
        if self.secure {
            return;
        }
        self.sync_invariants();
        let (a, b) = self.range();
        if a == b {
            let line_idx = self.line_of_byte(a);
            if self.blocks.get(line_idx) == Some(&Block::Code) {
                return;
            }
            let mut active = self.active_marks();
            if let Some(pos) = active.iter().position(|m| m.same_kind(&mark)) {
                if active[pos] == mark || matches!(mark, Mark::Link(ref u) if u.is_empty()) {
                    active.remove(pos);
                } else {
                    active[pos] = mark;
                }
            } else {
                active.push(mark);
            }
            self.pending = Some(((self.anchor, self.focus), active));
            return;
        }
        if self.range_has_mark(a, b, &mark) {
            self.remove_mark(&mark);
        } else {
            self.set_mark(mark);
        }
    }

    /// Apply `mark` to the current selection (or add it to pending typing style when empty).
    pub fn set_mark(&mut self, mark: Mark) {
        if self.secure {
            return;
        }
        self.sync_invariants();
        let (a, b) = self.range();
        if a == b {
            let line_idx = self.line_of_byte(a);
            if self.blocks.get(line_idx) == Some(&Block::Code) {
                return;
            }
            let mut active = self.active_marks();
            if let Some(pos) = active.iter().position(|m| m.same_kind(&mark)) {
                active[pos] = mark;
            } else {
                active.push(mark);
            }
            self.pending = Some(((self.anchor, self.focus), active));
            return;
        }
        self.break_history_group();
        let before = self.snapshot();
        self.marks.push(MarkRange { start: a, end: b, mark });
        self.pending = None;
        self.normalize_marks();
        self.record_change(before, false);
    }

    /// Remove `mark` (matching by [`Mark::same_kind`]) from the current selection, or from pending
    /// typing style when the selection is empty.
    pub fn remove_mark(&mut self, mark: &Mark) {
        if self.secure {
            return;
        }
        self.sync_invariants();
        let (a, b) = self.range();
        if a == b {
            let mut active = self.active_marks();
            active.retain(|m| !m.same_kind(mark));
            self.pending = Some(((self.anchor, self.focus), active));
            return;
        }
        self.break_history_group();
        let before = self.snapshot();
        let mut out = Vec::with_capacity(self.marks.len());
        for m in self.marks.drain(..) {
            if !m.mark.same_kind(mark) || m.end <= a || m.start >= b {
                out.push(m);
            } else {
                if m.start < a {
                    out.push(MarkRange { start: m.start, end: a, mark: m.mark.clone() });
                }
                if m.end > b {
                    out.push(MarkRange { start: b, end: m.end, mark: m.mark });
                }
            }
        }
        self.marks = out;
        self.pending = None;
        self.normalize_marks();
        self.record_change(before, false);
    }

    /// Apply or remove a [`Mark::Link`] on the selection (or pending style when empty).
    /// Passing `None` or `Some("")` removes any link on the selection.
    pub fn set_link(&mut self, url: Option<&str>) {
        match url.map(str::trim).filter(|u| !u.is_empty()) {
            Some(u) => self.set_mark(Mark::Link(u.to_owned())),
            None => self.remove_mark(&Mark::Link(String::new())),
        }
    }

    /// Line index range `first_line..=last_line` covered by the current selection.
    fn selected_line_range(&self) -> (usize, usize) {
        let (a, b) = self.range();
        let first = self.line_of_byte(a);
        let last = if b > a && self.text.as_bytes().get(b - 1) == Some(&b'\n') {
            self.line_of_byte(b - 1)
        } else {
            self.line_of_byte(b)
        };
        (first, last.max(first))
    }

    /// The [`Block`] attribute of the paragraph at the caret or selection.
    pub fn active_block(&self) -> Block {
        let (first, _) = self.selected_line_range();
        self.blocks.get(first).copied().unwrap_or(Block::Paragraph)
    }

    /// Alias for [`TextEdit::active_block`]: the [`Block`] attribute of the paragraph at the caret.
    pub fn current_block(&self) -> Block {
        self.active_block()
    }

    /// Whether any paragraph in the current selection is a bullet or numbered list item.
    pub fn selection_in_list(&self) -> bool {
        let (first, last) = self.selected_line_range();
        (first..=last).any(|i| self.blocks.get(i).copied().unwrap_or(Block::Paragraph).is_list())
    }

    /// Set the [`Block`] attribute for all paragraphs in the current selection.
    pub fn set_block(&mut self, block: Block) {
        if self.secure || self.single_line {
            return;
        }
        self.sync_invariants();
        self.break_history_group();
        let before = self.snapshot();
        let (first, last) = self.selected_line_range();
        for i in first..=last {
            if let Some(slot) = self.blocks.get_mut(i) {
                *slot = block;
            }
        }
        if block == Block::Code {
            self.pending = None;
        }
        self.normalize_marks();
        self.record_change(before, false);
    }

    /// Toggle `block` on all paragraphs in the selection: if every selected paragraph already has
    /// the same block kind as `block`, resets them to [`Block::Paragraph`]; otherwise sets `block`
    /// (preserving list nesting level when switching between bullet and numbered lists).
    pub fn toggle_block(&mut self, block: Block) {
        if self.secure || self.single_line {
            return;
        }
        self.sync_invariants();
        let (first, last) = self.selected_line_range();
        let all_match = (first..=last).all(|i| {
            self.blocks.get(i).copied().unwrap_or(Block::Paragraph).same_kind(block)
        });
        if all_match {
            self.set_block(Block::Paragraph);
        } else {
            self.break_history_group();
            let before = self.snapshot();
            for i in first..=last {
                if let Some(slot) = self.blocks.get_mut(i) {
                    *slot = match (slot.list_level(), block) {
                        (Some(lvl), Block::Bullet(0)) => Block::Bullet(lvl),
                        (Some(lvl), Block::Numbered(0)) => Block::Numbered(lvl),
                        _ => block,
                    };
                }
            }
            if block == Block::Code {
                self.pending = None;
            }
            self.normalize_marks();
            self.record_change(before, false);
        }
    }

    /// Increase list nesting level (`Tab`) for list items in the selection, up to [`MAX_LIST_LEVEL`].
    /// Returns `true` if the selection was inside a list item.
    pub fn indent_list(&mut self) -> bool {
        if self.secure || self.single_line {
            return false;
        }
        self.sync_invariants();
        if !self.selection_in_list() {
            return false;
        }
        self.break_history_group();
        let before = self.snapshot();
        let (first, last) = self.selected_line_range();
        for i in first..=last {
            if let Some(slot) = self.blocks.get_mut(i)
                && let Some(lvl) = slot.list_level()
            {
                *slot = slot.with_level((lvl + 1).min(MAX_LIST_LEVEL));
            }
        }
        self.record_change(before, false);
        true
    }

    /// Decrease list nesting level (`Shift+Tab`) for list items in the selection.
    /// Returns `true` if the selection was inside a list item.
    pub fn outdent_list(&mut self) -> bool {
        if self.secure || self.single_line {
            return false;
        }
        self.sync_invariants();
        if !self.selection_in_list() {
            return false;
        }
        self.break_history_group();
        let before = self.snapshot();
        let (first, last) = self.selected_line_range();
        for i in first..=last {
            if let Some(slot) = self.blocks.get_mut(i)
                && let Some(lvl) = slot.list_level()
            {
                *slot = slot.with_level(lvl.saturating_sub(1));
            }
        }
        self.record_change(before, false);
        true
    }

    /// Text typed, pasted or committed into this field. Secure and single-line fields drop
    /// control characters, line breaks included (stripped, as `<input type=text>` does);
    /// multi-line fields only fold CRLF to LF. Sized up front so no partial copy of a secret
    /// is left behind by a reallocation.
    fn sanitize(&self, s: &str) -> Zeroizing<String> {
        let mut out = Zeroizing::new(String::with_capacity(s.len()));
        if self.secure || self.single_line {
            out.extend(s.chars().filter(|c| !c.is_control() && !matches!(c, '\u{2028}' | '\u{2029}')));
        } else {
            let mut chars = s.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\r' && chars.peek() == Some(&'\n') { continue; }
                out.push(c);
            }
        }
        out
    }

    /// Core replacement engine updating `text`, `marks`, `blocks`, and `soft_breaks` together.
    fn apply_text_and_attr_replace(
        &mut self,
        (a, b): (usize, usize),
        s: &str,
        inserted_marks: &[Mark],
        new_line_block: Option<Block>,
        mark_new_lines_soft: bool,
    ) {
        self.sync_invariants();
        let del_len = b - a;
        let ins_len = s.len();
        let line_a = self.line_of_byte(a);
        let line_b = self.line_of_byte(b);
        let line_a_range = self.line_range(line_a);
        let line_b_range = self.line_range(line_b);

        // Determine the surviving block for `line_a` when `a..b` merges `line_a..=line_b`.
        let merged_block = if line_a < line_b
            && line_a_range.is_empty()
            && a == line_a_range.start
            && b == line_b_range.start
        {
            self.blocks.get(line_b).copied().unwrap_or(Block::Paragraph)
        } else {
            self.blocks.get(line_a).copied().unwrap_or(Block::Paragraph)
        };
        let merged_soft = if line_a < line_b
            && line_a_range.is_empty()
            && a == line_a_range.start
            && b == line_b_range.start
        {
            self.soft_breaks.binary_search(&(line_b as u32)).is_ok() && line_a > 0
        } else {
            self.soft_breaks.binary_search(&(line_a as u32)).is_ok()
        };

        let removed_lines = line_b - line_a;
        let added_lines = s.bytes().filter(|&ch| ch == b'\n').count();
        let next_line_blk = new_line_block.unwrap_or(match merged_block {
            Block::Code => Block::Code,
            _ => Block::Paragraph,
        });

        // Update `blocks` for removed and added lines.
        if removed_lines > 0 {
            self.blocks.drain(line_a + 1..=line_b);
        }
        if let Some(slot) = self.blocks.get_mut(line_a) {
            *slot = merged_block;
        }
        if added_lines > 0 {
            let insert_pos = (line_a + 1).min(self.blocks.len());
            self.blocks.splice(insert_pos..insert_pos, std::iter::repeat_n(next_line_blk, added_lines));
        }

        // Update `soft_breaks`.
        let mut next_soft = Vec::new();
        for &sb in &self.soft_breaks {
            let idx = sb as usize;
            if idx < line_a {
                next_soft.push(sb);
            } else if idx > line_b {
                let shifted = idx - removed_lines + added_lines;
                if shifted > 0 {
                    next_soft.push(shifted as u32);
                }
            }
        }
        if merged_soft && line_a > 0 {
            next_soft.push(line_a as u32);
        }
        if mark_new_lines_soft && added_lines > 0 {
            for k in 1..=added_lines {
                next_soft.push((line_a + k) as u32);
            }
        }
        self.soft_breaks = next_soft;

        // Shift and split existing inline marks around `a..b`.
        let mut updated_marks = Vec::with_capacity(self.marks.len() + inserted_marks.len());
        for m in self.marks.drain(..) {
            if m.end <= a {
                updated_marks.push(m);
            } else if m.start >= b {
                updated_marks.push(MarkRange {
                    start: m.start - del_len + ins_len,
                    end: m.end - del_len + ins_len,
                    mark: m.mark,
                });
            } else {
                // `m` overlaps `a..b` (or touches inside it).
                if m.start < a {
                    updated_marks.push(MarkRange {
                        start: m.start,
                        end: a,
                        mark: m.mark.clone(),
                    });
                }
                if m.end > b {
                    updated_marks.push(MarkRange {
                        start: a + ins_len,
                        end: m.end - del_len + ins_len,
                        mark: m.mark,
                    });
                }
            }
        }
        if ins_len > 0 {
            for im in inserted_marks {
                // Avoid wrapping a pure newline at the trailing edge of a mark unless inside the mark.
                if s == "\n" {
                    let was_inside = updated_marks.iter().any(|m| m.mark == *im && m.start >= a + ins_len);
                    if !was_inside {
                        continue;
                    }
                }
                updated_marks.push(MarkRange {
                    start: a,
                    end: a + ins_len,
                    mark: im.clone(),
                });
            }
        }
        self.marks = updated_marks;
        self.text.replace_range(a..b, s);
        self.anchor = a + ins_len;
        self.focus = self.anchor;
        self.preedit = None;
        self.sync_invariants();
    }

    fn replace(&mut self, (a, b): (usize, usize), s: &str, typing: bool) {
        if self.secure {
            let mut next = String::with_capacity(self.text.len() - (b - a) + s.len());
            next.push_str(&self.text[..a]);
            next.push_str(s);
            next.push_str(&self.text[b..]);
            self.replace_text(next);
            self.anchor = a + s.len();
            self.focus = self.anchor;
            return;
        }
        self.sync_invariants();
        if !typing { self.break_history_group(); }
        if &self.text[a..b] == s && self.pending.is_none() {
            self.anchor = a + s.len();
            self.focus = self.anchor;
            self.preedit = None;
            return;
        }
        let before = self.snapshot();
        let inserted_marks: Vec<Mark> = if s.is_empty() {
            self.pending = None;
            Vec::new()
        } else if let Some((armed, p)) = self.pending.take()
            && armed == (self.anchor, self.focus)
        {
            p
        } else if a < b {
            // Replacing a selection inherits marks that covered the first character of the selection.
            let line_idx = self.line_of_byte(a);
            if self.blocks.get(line_idx) == Some(&Block::Code) {
                Vec::new()
            } else {
                self.marks
                    .iter()
                    .filter(|m| m.start <= a && a < m.end)
                    .map(|m| m.mark.clone())
                    .collect()
            }
        } else {
            self.inherited_marks_at(a)
        };

        self.apply_text_and_attr_replace((a, b), s, &inserted_marks, None, false);
        self.record_change(before, typing);
    }

    fn insert_newline(&mut self, block_for_new_line: Block, soft: bool) {
        if self.secure || self.single_line {
            return;
        }
        self.sync_invariants();
        self.break_history_group();
        let before = self.snapshot();
        let (a, b) = self.range();
        let inserted_marks = if let Some((armed, p)) = self.pending.take()
            && armed == (self.anchor, self.focus)
        {
            p
        } else {
            self.inherited_marks_at(a)
        };
        self.apply_text_and_attr_replace((a, b), "\n", &inserted_marks, Some(block_for_new_line), soft);
        self.record_change(before, false);
    }

    fn set(&mut self, i: usize, extend: bool) {
        self.focus = i;
        if !extend {
            self.anchor = i;
        }
        self.pending = None;
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
        self.pending = None;
    }

    /// Applies one key. `layout` is the field's layout for the current text (device px).
    pub fn key(&mut self, ev: &KeyEvent, layout: Option<&Layout<Ink>>) -> EditOutcome {
        let mut out = EditOutcome::default();
        if matches!(ev.key, Key::Modifier { .. }) { return out; }
        self.sync_invariants();
        let m = ev.mods;
        if let Some(c) = ev.control_binding() {
            let (key, line) = match c {
                'a' => (Key::Left, true), 'e' => (Key::Right, true), 'b' => (Key::Left, false),
                'f' => (Key::Right, false), 'p' => (Key::Up, false), 'n' => (Key::Down, false),
                'd' => (Key::Delete, false), 'h' => (Key::Backspace, false),
                _ => {
                    // Ctrl-K deletes to the end of the line, or the line break at its end.
                    self.break_history_group();
                    let (a, b) = self.range();
                    let end = if a == b { self.line_edge(layout, true) } else { b };
                    let to = if end > a { end } else if a < self.text.len() { next_char(&self.text, a) } else { a };
                    if to > a { self.replace((a, to), "", false); }
                    return out;
                }
            };
            return self.key(&KeyEvent { key, mods: Mods { shift: m.shift, cmd: line, ..Mods::default() } }, layout);
        }
        if ev.is_command_chord() { return out; }
        if !matches!(&ev.key, Key::Text(s) if !s.contains(['\r', '\n'])) { self.break_history_group(); }
        let word = m.alt || (m.ctrl && !cfg!(target_os = "macos"));
        match &ev.key {
            Key::Text(s) => {
                if s.chars().any(|c| !c.is_control()) {
                    let s = self.sanitize(s);
                    if !s.is_empty() { self.replace(self.range(), &s, !s.contains(['\r', '\n'])); }
                }
            }
            // A composition's Return and Escape are the input method's.
            Key::Enter | Key::Escape if self.preedit.is_some() => {}
            Key::Enter => {
                if self.secure || self.single_line {
                    out.submit = true;
                } else {
                    let (a, b) = self.range();
                    let line_idx = self.line_of_byte(a);
                    let blk = self.blocks.get(line_idx).copied().unwrap_or(Block::Paragraph);
                    let line_range = self.line_range(line_idx);
                    if m.shift {
                        // Shift+Return: soft line break keeping the current paragraph's block.
                        self.insert_newline(blk, true);
                    } else if m.alt {
                        self.insert_newline(blk, false);
                    } else {
                        match blk {
                            Block::Code => {
                                // Inside a code block, Return always inserts a newline.
                                self.insert_newline(Block::Code, false);
                            }
                            Block::Bullet(_) | Block::Numbered(_) => {
                                // Return in an empty list item ends the list; otherwise continues it.
                                if a == b && line_range.is_empty() {
                                    self.set_block(Block::Paragraph);
                                } else {
                                    self.insert_newline(blk, false);
                                }
                            }
                            Block::Quote => {
                                if a == b && line_range.is_empty() {
                                    self.set_block(Block::Paragraph);
                                } else {
                                    self.insert_newline(Block::Quote, false);
                                }
                            }
                            Block::Paragraph => {
                                out.submit = true;
                            }
                        }
                    }
                }
            }
            Key::Tab => {
                if !m.cmd && !m.alt && !m.ctrl && !self.secure && !self.single_line {
                    if self.active_block() == Block::Code && !m.shift {
                        self.replace(self.range(), "\t", true);
                    } else if m.shift {
                        self.outdent_list();
                    } else {
                        self.indent_list();
                    }
                }
            }
            Key::PageUp | Key::PageDown => {}
            Key::Escape => out.blur = true,
            Key::Backspace => {
                let (a, b) = self.range();
                if a != b {
                    self.insert("");
                } else {
                    let line_idx = self.line_of_byte(a);
                    let line_range = self.line_range(line_idx);
                    let blk = self.blocks.get(line_idx).copied().unwrap_or(Block::Paragraph);
                    let is_soft = self.soft_breaks.binary_search(&(line_idx as u32)).is_ok();
                    if !m.cmd && !word && a == line_range.start && !is_soft && blk != Block::Paragraph {
                        match blk {
                            Block::Bullet(lvl) | Block::Numbered(lvl) => {
                                if lvl > 0 {
                                    self.set_block(blk.with_level(lvl - 1));
                                } else {
                                    self.set_block(Block::Paragraph);
                                }
                            }
                            Block::Quote | Block::Code => {
                                if a == 0 || line_range.is_empty() {
                                    self.set_block(Block::Paragraph);
                                } else {
                                    let from = prev_char(&self.text, a);
                                    self.replace((from, a), "", false);
                                }
                            }
                            Block::Paragraph => unreachable!(),
                        }
                    } else if a > 0 {
                        let t = &self.text;
                        let from = if m.cmd { t[..a].rfind('\n').map_or(0, |i| i + 1) } else if word { prev_word(t, a) } else { prev_char(t, a) };
                        self.replace((from, a), "", false);
                    }
                }
            }
            Key::Delete => {
                let (a, b) = self.range();
                if a != b {
                    self.insert("");
                } else if a < self.text.len() {
                    let t = &self.text;
                    let to = if word { next_word(t, a) } else { next_char(t, a) };
                    self.replace((a, to), "", false);
                }
            }
            Key::Left => {
                let t = &self.text;
                let i = if m.cmd { self.line_edge(layout, false) } else if word { prev_word(t, self.focus) } else { prev_char(t, self.focus) };
                self.move_h(i, m.shift);
            }
            Key::Right => {
                let t = &self.text;
                let i = if m.cmd { self.line_edge(layout, true) } else if word { next_word(t, self.focus) } else { next_char(t, self.focus) };
                self.move_h(i, m.shift);
            }
            Key::Home => self.set(0, m.shift),
            Key::End => {
                let n = self.text.len();
                self.set(n, m.shift)
            }
            Key::Up | Key::Down => {
                let down = ev.key == Key::Down;
                let n = self.text.len();
                let i = if m.cmd {
                    if down { n } else { 0 }
                } else if let Some(l) = layout {
                    let sel = Selection::from_byte_index(l, self.display_index(self.focus), Affinity::Downstream);
                    let moved = if down { sel.next_line(l, false) } else { sel.previous_line(l, false) };
                    let j = self.model_index(moved.focus().index());
                    if j == self.focus { if down { n } else { 0 } } else { j }
                } else if down {
                    n
                } else {
                    0
                };
                self.set(i.min(n), m.shift);
            }
            Key::Command(c) => match c {
                'z' if m.shift => self.redo(),
                'z' => self.undo(),
                'a' => {
                    self.anchor = 0;
                    self.focus = self.text.len();
                    self.pending = None;
                }
                'b' if !m.shift && !m.alt && !self.secure => self.toggle_mark(Mark::Bold),
                'i' if !m.shift && !m.alt && !self.secure => self.toggle_mark(Mark::Italic),
                'u' if !m.shift && !m.alt && !self.secure => self.toggle_mark(Mark::Underline),
                'c' if !self.secure => out.copy = Some(self.selected().to_owned()),
                'x' if !self.secure => {
                    out.copy = Some(self.selected().to_owned());
                    self.insert("");
                }
                _ => {}
            },
            Key::Function(_) | Key::Modifier { .. } => {}
        }
        out
    }

    fn line_edge(&self, layout: Option<&Layout<Ink>>, end: bool) -> usize {
        if let Some(l) = layout {
            let sel = Selection::from_byte_index(l, self.display_index(self.focus), Affinity::Downstream);
            let s = if end { sel.line_end(l, false) } else { sel.line_start(l, false) };
            return self.model_index(s.focus().index());
        }
        if end {
            self.text[self.focus..].find('\n').map_or(self.text.len(), |i| self.focus + i)
        } else {
            self.text[..self.focus].rfind('\n').map_or(0, |i| i + 1)
        }
    }

    pub fn ime(&mut self, ev: &Ime) -> EditOutcome {
        if self.secure { self.clear_preedit(); return EditOutcome::default(); }
        self.sync_invariants();
        self.break_history_group();
        match ev {
            Ime::Preedit(s, cur) => {
                self.preedit = if s.is_empty() { None } else { Some((s.clone(), *cur)) };
            }
            Ime::Commit(s) => {
                self.preedit = None;
                let s = self.sanitize(s);
                self.insert(&s);
            }
        }
        EditOutcome::default()
    }

    pub fn paste(&mut self, s: &str) -> EditOutcome {
        let s = self.sanitize(s);
        self.insert(&s);
        EditOutcome::default()
    }

    /// Paste rich HTML (`html`) with plain-text fallback (`plain`) into `self` at the current
    /// selection, recorded as a single undo step. Does not touch the OS clipboard.
    pub fn paste_html(&mut self, html_str: &str, plain: &str) -> EditOutcome {
        if self.secure {
            return self.paste(plain);
        }
        if html_str.trim().is_empty() {
            return self.paste(plain);
        }
        let frag = Self::from_html(html_str);
        if frag.text.is_empty() && !plain.is_empty() {
            return self.paste(plain);
        }
        self.sync_invariants();
        self.break_history_group();
        let before = self.snapshot();
        let (a, b) = self.range();

        if self.single_line {
            // Single-line field: strip line breaks from `frag.text` while keeping inline marks.
            let mut clean = String::with_capacity(frag.text.len());
            let mut byte_map = vec![0usize; frag.text.len() + 1];
            for (i, ch) in frag.text.char_indices() {
                byte_map[i] = clean.len();
                if !ch.is_control() && !matches!(ch, '\u{2028}' | '\u{2029}') {
                    clean.push(ch);
                } else if ch == '\n' && !clean.ends_with(' ') && !clean.is_empty() {
                    clean.push(' ');
                }
            }
            byte_map[frag.text.len()] = clean.len();
            self.apply_text_and_attr_replace((a, b), &clean, &[], None, false);
            for m in &frag.marks {
                let s_idx = a + byte_map[m.start.min(frag.text.len())];
                let e_idx = a + byte_map[m.end.min(frag.text.len())];
                if s_idx < e_idx {
                    self.marks.push(MarkRange { start: s_idx, end: e_idx, mark: m.mark.clone() });
                }
            }
            self.pending = None;
            self.normalize_marks();
            self.record_change(before, false);
            return EditOutcome::default();
        }

        // First delete `a..b` if non-empty.
        if a < b {
            self.apply_text_and_attr_replace((a, b), "", &[], None, false);
        }
        let ins_at = a;
        let line_idx = self.line_of_byte(ins_at);
        let line_range = self.line_range(line_idx);
        let line_was_empty = line_range.is_empty();
        let orig_block = self.blocks.get(line_idx).copied().unwrap_or(Block::Paragraph);

        self.apply_text_and_attr_replace((ins_at, ins_at), &frag.text, &[], None, false);

        // Apply `frag.blocks` and `frag.soft_breaks`.
        let frag_lines = frag.blocks.len();
        if frag_lines == 1 {
            if line_was_empty && orig_block == Block::Paragraph && frag.blocks[0] != Block::Paragraph {
                if let Some(slot) = self.blocks.get_mut(line_idx) {
                    *slot = frag.blocks[0];
                }
            }
        } else if frag_lines > 1 {
            if (line_was_empty || ins_at == line_range.start) && orig_block == Block::Paragraph {
                if let Some(slot) = self.blocks.get_mut(line_idx) {
                    *slot = frag.blocks[0];
                }
            }
            for k in 1..frag_lines {
                if let Some(slot) = self.blocks.get_mut(line_idx + k) {
                    *slot = frag.blocks[k];
                }
            }
            for &sb in &frag.soft_breaks {
                let target = line_idx + sb as usize;
                if target > 0 {
                    self.soft_breaks.push(target as u32);
                }
            }
        }

        // Add `frag.marks` shifted by `ins_at`.
        for m in &frag.marks {
            self.marks.push(MarkRange {
                start: ins_at + m.start,
                end: ins_at + m.end,
                mark: m.mark.clone(),
            });
        }
        self.pending = None;
        self.sync_invariants();
        self.record_change(before, false);
        EditOutcome::default()
    }

    /// Map a model byte range `start..end` (in `self.text`) onto `self.display().0` byte ranges,
    /// shifting or splitting around any active IME `preedit` splice.
    pub(crate) fn map_range_to_display(&self, start: usize, end: usize) -> Vec<(u32, u32)> {
        if self.secure || start >= end {
            return Vec::new();
        }
        let len = self.text.len();
        let s = start.min(len);
        let e = end.min(len);
        if s >= e {
            return Vec::new();
        }
        let Some((ref p, _)) = self.preedit else {
            return vec![(s as u32, e as u32)];
        };
        let (a, b) = self.range();
        let del_len = b - a;
        let ins_len = p.len();
        if e <= a {
            vec![(s as u32, e as u32)]
        } else if s >= b {
            vec![((s - del_len + ins_len) as u32, (e - del_len + ins_len) as u32)]
        } else {
            let mut out = Vec::new();
            if s < a {
                out.push((s as u32, a as u32));
            }
            if e > b {
                out.push(((a + ins_len) as u32, (e - del_len + ins_len) as u32));
            }
            out
        }
    }

    /// Build the [`Span`]s, strikethrough ranges, [`Link`]s, and per-line [`Block`]s for
    /// `self.display()` so `text_input` renders inline marks, pending IME style, and blocks.
    pub(crate) fn rich_display_specs(
        &self,
        base_style: crate::tok::TextStyle,
        style: RichStyle,
    ) -> (Vec<Span>, Vec<(u32, u32, Option<Color>)>, Vec<Link>, Vec<Block>) {
        if self.secure {
            return (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        }
        let mut spans: Vec<Span> = Vec::new();
        let mut strikes: Vec<(u32, u32, Option<Color>)> = Vec::new();
        let mut links: Vec<Link> = Vec::new();

        let push_mark_span = |r_start: u32,
                              r_end: u32,
                              mark: &Mark,
                              spans: &mut Vec<Span>,
                              strikes: &mut Vec<(u32, u32, Option<Color>)>,
                              links: &mut Vec<Link>| {
            if r_start >= r_end {
                return;
            }
            match mark {
                Mark::Bold => {
                    spans.push(Span {
                        start: r_start,
                        end: r_end,
                        weight: Some((base_style.weight + 250.0).clamp(600.0, 900.0)),
                        ..Span::default()
                    });
                }
                Mark::Italic => {
                    spans.push(Span {
                        start: r_start,
                        end: r_end,
                        italic: true,
                        ..Span::default()
                    });
                }
                Mark::Underline => {
                    spans.push(Span {
                        start: r_start,
                        end: r_end,
                        underline: Some(Color::TRANSPARENT),
                        ..Span::default()
                    });
                }
                Mark::Strike => {
                    strikes.push((r_start, r_end, None));
                }
                Mark::Code => {
                    spans.push(Span {
                        start: r_start,
                        end: r_end,
                        mono: true,
                        color: style.code_fg,
                        bg: Some(style.code_bg),
                        bg_pad: 2.0,
                        ..Span::default()
                    });
                }
                Mark::Link(url) => {
                    spans.push(Span {
                        start: r_start,
                        end: r_end,
                        color: Some(style.link),
                        underline: Some(style.link.alpha(0.65)),
                        ..Span::default()
                    });
                    links.push(Link::new(r_start as usize..r_end as usize, url.clone()));
                }
            }
        };

        for m in &self.marks {
            for (ds, de) in self.map_range_to_display(m.start, m.end) {
                push_mark_span(ds, de, &m.mark, &mut spans, &mut strikes, &mut links);
            }
        }

        // If IME preedit is active, style the preedit range with `self.active_marks()` (which
        // includes any pending typing style or inherited marks at the caret).
        if let Some((ref p, _)) = self.preedit
            && !p.is_empty()
        {
            let (a, _) = self.range();
            let ps = a as u32;
            let pe = (a + p.len()) as u32;
            for m in self.active_marks() {
                push_mark_span(ps, pe, &m, &mut spans, &mut strikes, &mut links);
            }
        }

        // Compute display blocks per `\n`-separated line of `display().0`, and add `mono: true`
        // spans for any `Block::Code` lines.
        let (disp_text, _) = self.display();
        let display_blocks = self.display_blocks(&disp_text);
        let mut line_start = 0usize;
        let mut line_idx = 0usize;
        for (i, b) in disp_text.bytes().enumerate() {
            if b == b'\n' {
                if display_blocks.get(line_idx) == Some(&Block::Code) && line_start < i {
                    spans.push(Span {
                        start: line_start as u32,
                        end: i as u32,
                        mono: true,
                        color: style.code_fg,
                        ..Span::default()
                    });
                }
                line_start = i + 1;
                line_idx += 1;
            }
        }
        if display_blocks.get(line_idx) == Some(&Block::Code) && line_start < disp_text.len() {
            spans.push(Span {
                start: line_start as u32,
                end: disp_text.len() as u32,
                mono: true,
                color: style.code_fg,
                ..Span::default()
            });
        }

        (spans, strikes, links, display_blocks)
    }

    fn display_blocks(&self, disp_text: &str) -> Vec<Block> {
        let line_count = 1 + disp_text.bytes().filter(|&b| b == b'\n').count();
        if self.blocks.is_empty() {
            return vec![Block::Paragraph; line_count];
        }
        if self.preedit.is_none() {
            let mut out = self.blocks.clone();
            out.resize(line_count, Block::Paragraph);
            return out;
        }
        let (a, b) = self.range();
        let line_a = self.line_of_byte(a);
        let line_b = self.line_of_byte(b);
        let mut out = self.blocks.clone();
        if line_b > line_a && line_b < out.len() {
            out.drain(line_a + 1..=line_b);
        }
        out.resize(line_count, out.get(line_a).copied().unwrap_or(Block::Paragraph));
        out
    }

    /// The text to lay out: committed text with any preedit spliced in at the caret, and the
    /// byte range of the preedit within it.
    pub fn display(&self) -> (String, Option<(usize, usize)>) {
        if self.secure { return ("•".repeat(self.text.chars().count()), None); }
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

    #[test]
    fn is_command_matches_any_character_and_both_forms_of_cmd_space() {
        let cmd = Mods { cmd: true, ..Mods::default() };
        let ev = |key, mods| KeyEvent { key, mods };
        assert!(ev(Key::Command(','), cmd).is_command(','));
        assert!(ev(Key::Command('1'), cmd).is_command('1'));
        assert!(ev(Key::Command('k'), cmd).is_command('K'));
        assert!(ev(Key::Command('z'), Mods { shift: true, ..cmd }).is_command('z'));
        assert!(!ev(Key::Command('k'), cmd).is_command('j'));
        assert!(ev(Key::Text(" ".into()), cmd).is_command(' '));
        assert!(ev(Key::Command(' '), cmd).is_command(' '));
        assert!(!ev(Key::Text(" ".into()), Mods::default()).is_command(' '));
        assert!(!ev(Key::Text("k".into()), Mods::default()).is_command('k'));
    }

    fn undo(e: &mut TextEdit, redo: bool) {
        e.key(&KeyEvent { key: Key::Command('z'), mods: Mods { cmd: true, shift: redo, ..Mods::default() } }, None);
    }

    #[test]
    fn secure_unicode_editing_has_no_history_or_export() {
        let mut e = TextEdit::new("hé🙂");
        e.insert("x");
        e.set_secure(true);
        assert!(e.history.undo.is_empty());
        assert_eq!(e.display().0, "••••");
        for (model, display) in [(0, 0), (1, 3), (3, 6), (7, 9), (8, 12)] {
            assert_eq!(e.display_index(model), display);
            assert_eq!(e.model_index(display), model);
        }
        e.anchor = 1;
        e.focus = 7;
        for command in ['c', 'x', 'z'] {
            assert!(e.key(&k(Key::Command(command)), None).copy.is_none());
            assert_eq!(e.text, "hé🙂x");
        }
        e.paste("界");
        assert_eq!((&*e.text, e.anchor, e.focus), ("h界x", 4, 4));
        e.ime(&Ime::Preedit("composition".into(), None));
        e.ime(&Ime::Commit("commit".into()));
        assert!(e.preedit.is_none());
        assert_eq!(e.text, "h界x");
        e.undo(); e.redo();
        assert_eq!(e.text, "h界x");
        assert!(e.history.undo.is_empty() && e.history.redo.is_empty());
        assert!(!format!("{e:?}").contains("h界x"));
        e.clear();
        assert!(e.is_secure());
    }

    /// Arrows and backspace treat a ZWJ family, a flag and a skin-toned hand as one character.
    #[test]
    fn caret_and_backspace_step_over_whole_emoji() {
        let (family, flag, wave) = ("👨\u{200D}👩\u{200D}👧", "🇯🇵", "👋🏽");
        let text = format!("a{family}{flag}{wave}");
        let mut e = TextEdit::new(&text);
        e.key(&k(Key::Home), None);
        let mut stops = vec![e.focus];
        for _ in 0..5 { e.key(&k(Key::Right), None); stops.push(e.focus); }
        let ends = [0, 1, 1 + family.len(), 1 + family.len() + flag.len(), text.len(), text.len()];
        assert_eq!(stops, ends);
        for want in [format!("a{family}{flag}"), format!("a{family}"), "a".into()] {
            e.key(&k(Key::Backspace), None);
            assert_eq!(*e.text, want);
        }
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

    fn chord(s: &str, mods: Mods) -> KeyEvent { KeyEvent { key: Key::Text(s.into()), mods } }

    #[test]
    fn command_chords_type_nothing() {
        let mut e = TextEdit::new("pw").secure();
        e.key(&chord(" ", Mods { cmd: true, shift: true, logo: true, ..Mods::default() }), None);
        e.key(&chord("a", Mods { ctrl: true, ..Mods::default() }), None);
        assert_eq!(e.text, "pw");
        let mut plain = TextEdit::new("");
        plain.key(&chord(" ", Mods { cmd: true, logo: true, ..Mods::default() }), None);
        plain.key(&chord("a", Mods { ctrl: true, cmd: !cfg!(target_os = "macos"), ..Mods::default() }), None);
        assert_eq!(plain.text, "");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn altgr_text_still_types() {
        let mut e = TextEdit::new("");
        e.key(&chord("€", Mods { ctrl: true, cmd: true, alt: true, ..Mods::default() }), None);
        assert_eq!(e.text, "€");
    }

    #[test]
    fn option_text_still_types() {
        let mut e = TextEdit::new("").secure();
        e.key(&chord("é", Mods { alt: true, ..Mods::default() }), None);
        e.key(&chord("ø", Mods { alt: true, shift: true, ..Mods::default() }), None);
        assert_eq!(e.text, "éø");
    }

    #[test]
    fn paste_keeps_single_line_fields_on_one_line() {
        let mut secure = TextEdit::new("").secure();
        let mut single = TextEdit::new("");
        single.set_single_line(true);
        let mut multi = TextEdit::new("");
        for e in [&mut secure, &mut single, &mut multi] { e.paste("a\tb\r\nc\u{2028}d"); }
        assert_eq!(secure.text, "abcd");
        assert_eq!(single.text, "abcd");
        assert_eq!(multi.text, "a\tb\nc\u{2028}d");
        single.ime(&Ime::Commit("x\ny".into()));
        assert_eq!(single.text, "abcdxy");
    }

    #[test]
    fn shift_enter_in_a_single_line_field_submits_without_a_line_break() {
        let shift_enter = KeyEvent { key: Key::Enter, mods: Mods { shift: true, ..Mods::default() } };
        let mut single = TextEdit::new("ab");
        single.set_single_line(true);
        let mut secure = TextEdit::new("ab").secure();
        for e in [&mut single, &mut secure] {
            assert!(e.key(&shift_enter, None).submit);
            e.key(&k(Key::Text("\n".into())), None);
            e.key(&k(Key::Text("\r\nc".into())), None);
            assert_eq!(e.text, "abc");
        }
    }

    #[test]
    fn command_chord_reaches_on_key() {
        use crate::{text_input, Cx, El, Env, Runtime, TextStyle};
        struct Form { pw: TextEdit, keys: Vec<Key> }
        fn view(s: &Form, _: &mut Cx<Form>) -> El {
            text_input(&s.pw, TextStyle::new(14.0, 400.0)).single_line().key("pw").size(200.0, 40.0)
                .on_edit(|s: &mut Form| &mut s.pw)
        }
        let mut pw = TextEdit::new("").secure();
        pw.focused = true;
        let mut rt = Runtime::new(Form { pw, keys: vec![] }, view, (200.0, 40.0), Env::new(true, 1.0));
        rt.on_key = Some(|s, ev, _| { s.keys.push(ev.key.clone()); true });
        rt.frame(0.0);
        rt.key(chord(" ", Mods { cmd: true, shift: true, logo: true, ..Mods::default() })); rt.frame(0.0);
        rt.key(chord("x", Mods::default())); rt.frame(0.0);
        assert_eq!(rt.state.pw.text, "x");
        assert_eq!(rt.state.keys, [Key::Text(" ".into())]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn control_letters_edit_like_appkit_fields() {
        let ctrl = |c: &str| KeyEvent { key: Key::Text(c.into()), mods: Mods { ctrl: true, ..Mods::default() } };
        let mut e = TextEdit::new("one two\nthree");
        e.set(4, false);
        e.key(&ctrl("a"), None); assert_eq!(e.focus, 0);
        e.key(&ctrl("e"), None); assert_eq!(e.focus, 7);
        e.key(&ctrl("b"), None); e.key(&ctrl("b"), None); assert_eq!(e.focus, 5);
        e.key(&ctrl("f"), None); assert_eq!(e.focus, 6);
        e.key(&ctrl("h"), None); assert_eq!(e.text, "one to\nthree");
        e.key(&ctrl("d"), None); assert_eq!(e.text, "one t\nthree");
        e.set(0, false);
        e.key(&ctrl("k"), None); assert_eq!(e.text, "\nthree");
        e.key(&ctrl("k"), None); assert_eq!(e.text, "three", "at a line's end Ctrl-K joins the next line");
        e.key(&ctrl("n"), None); assert_eq!(e.focus, 5);
        e.key(&ctrl("x"), None); assert_eq!(e.text, "three", "other Control letters never type");
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

    #[test]
    fn inline_marks_shift_split_inherit_and_respect_boundaries() {
        let mut e = TextEdit::new("hello world");
        // Apply bold to "hello" (0..5).
        e.anchor = 0;
        e.focus = 5;
        e.toggle_mark(Mark::Bold);
        assert!(e.has_mark(&Mark::Bold));
        assert_eq!(e.marks(), &[MarkRange::new(0..5, Mark::Bold)]);

        // Caret at right edge of bold (index 5): inherits Bold on typing!
        e.set(5, false);
        assert!(e.has_mark(&Mark::Bold));
        e.key(&k(Key::Text("!".into())), None);
        assert_eq!(e.text, "hello! world");
        assert_eq!(e.marks(), &[MarkRange::new(0..6, Mark::Bold)]);

        // Caret at index 0 (start of bold run): does NOT inherit Bold.
        e.set(0, false);
        assert!(!e.has_mark(&Mark::Bold));
        e.key(&k(Key::Text(">".into())), None);
        assert_eq!(e.text, ">hello! world");
        assert_eq!(e.marks(), &[MarkRange::new(1..7, Mark::Bold)]);

        // Split Bold by toggling it off on "ll" (3..5).
        e.anchor = 3;
        e.focus = 5;
        assert!(e.has_mark(&Mark::Bold));
        e.toggle_mark(Mark::Bold);
        assert!(!e.has_mark(&Mark::Bold));
        assert_eq!(
            e.marks(),
            &[MarkRange::new(1..3, Mark::Bold), MarkRange::new(5..7, Mark::Bold)]
        );

        // Selecting across the gap (1..7) reports Bold is not active on the whole range,
        // so toggling Bold applies it to the whole selection and merges the runs.
        e.anchor = 1;
        e.focus = 7;
        assert!(!e.has_mark(&Mark::Bold));
        e.toggle_mark(Mark::Bold);
        assert!(e.has_mark(&Mark::Bold));
        assert_eq!(e.marks(), &[MarkRange::new(1..7, Mark::Bold)]);

        // Code and Link marks do NOT inherit at the start of their run (index 8).
        e.anchor = 8;
        e.focus = 13; // "world"
        e.toggle_mark(Mark::Code);
        e.set_link(Some("https://example.com"));
        // Inside "world" (index 10): both Code and Link are active and inherited.
        e.set(10, false);
        assert!(e.has_mark(&Mark::Code));
        assert_eq!(e.active_link(), Some("https://example.com"));
        // At start of link/code run (index 8): neither Code nor Link is inherited.
        e.set(8, false);
        assert!(!e.has_mark(&Mark::Code));
        assert_eq!(e.active_link(), None);
        e.key(&k(Key::Text("?".into())), None);
        assert_eq!(e.text, ">hello! ?world");
        assert_eq!(
            e.marks(),
            &[
                MarkRange::new(1..7, Mark::Bold),
                MarkRange::new(9..14, Mark::Code),
                MarkRange::new(9..14, Mark::Link("https://example.com".into())),
            ]
        );
    }

    #[test]
    fn pending_typing_style_and_command_shortcuts() {
        let mut e = TextEdit::new("");
        // Empty selection: Cmd+B sets pending Bold, Cmd+I sets pending Italic.
        e.key(&KeyEvent { key: Key::Command('b'), mods: Mods { cmd: true, ..Mods::default() } }, None);
        e.key(&KeyEvent { key: Key::Command('i'), mods: Mods { cmd: true, ..Mods::default() } }, None);
        assert!(e.has_mark(&Mark::Bold));
        assert!(e.has_mark(&Mark::Italic));

        e.key(&k(Key::Text("hi".into())), None);
        assert_eq!(e.text, "hi");
        assert_eq!(
            e.marks(),
            &[MarkRange::new(0..2, Mark::Bold), MarkRange::new(0..2, Mark::Italic)]
        );

        // Toggle Bold off on empty selection at index 2, then type " there".
        e.key(&KeyEvent { key: Key::Command('b'), mods: Mods { cmd: true, ..Mods::default() } }, None);
        assert!(!e.has_mark(&Mark::Bold));
        assert!(e.has_mark(&Mark::Italic));
        e.key(&k(Key::Text(" there".into())), None);
        assert_eq!(e.text, "hi there");
        assert_eq!(
            e.marks(),
            &[MarkRange::new(0..2, Mark::Bold), MarkRange::new(0..8, Mark::Italic)]
        );

        // Moving the caret clears pending typing style.
        e.toggle_mark(Mark::Underline);
        assert!(e.has_mark(&Mark::Underline));
        e.key(&k(Key::Left), None);
        assert!(!e.has_mark(&Mark::Underline));
    }

    #[test]
    fn unicode_emoji_cjk_and_combining_marks_across_boundaries() {
        let mut e = TextEdit::new("🦀a\u{0301}日本語");
        // "🦀" is 0..4, "a\u{0301}" is 4..7, "日" is 7..10, "本" is 10..13, "語" is 13..16.
        e.anchor = 0;
        e.focus = 4;
        e.toggle_mark(Mark::Bold);
        e.anchor = 4;
        e.focus = 7;
        e.toggle_mark(Mark::Italic);
        e.anchor = 7;
        e.focus = 13;
        e.toggle_mark(Mark::Strike);
        e.anchor = 10;
        e.focus = 16;
        e.toggle_mark(Mark::Underline);

        // Insert CJK right at boundary between combining mark and CJK (byte 7).
        e.set(7, false);
        e.key(&k(Key::Text("★".into())), None); // 3 bytes, inherits Italic from 4..7
        assert_eq!(e.text, "🦀a\u{0301}★日本語");
        assert_eq!(
            e.marks(),
            &[
                MarkRange::new(0..4, Mark::Bold),
                MarkRange::new(4..10, Mark::Italic),
                MarkRange::new(10..16, Mark::Strike),
                MarkRange::new(13..19, Mark::Underline),
            ]
        );

        // Delete across boundary ("★日", bytes 7..13).
        e.anchor = 7;
        e.focus = 13;
        e.key(&k(Key::Backspace), None);
        assert_eq!(e.text, "🦀a\u{0301}本語");
        assert_eq!(
            e.marks(),
            &[
                MarkRange::new(0..4, Mark::Bold),
                MarkRange::new(4..7, Mark::Italic),
                MarkRange::new(7..10, Mark::Strike),
                MarkRange::new(7..13, Mark::Underline),
            ]
        );

        // Round-trip through HTML preserves exact Unicode mark boundaries.
        let html = e.to_html();
        let rt = TextEdit::from_html(&html);
        assert_eq!(rt.text, e.text);
        assert_eq!(rt.marks(), e.marks());
    }

    #[test]
    fn block_attributes_lists_quotes_code_and_keys() {
        let mut e = TextEdit::new("first");
        e.set_block(Block::Bullet(0));
        assert_eq!(e.current_block(), Block::Bullet(0));

        // Return at end of non-empty list item creates a new Bullet(0) item.
        e.set(5, false);
        assert!(!e.key(&k(Key::Enter), None).submit);
        assert_eq!(e.text, "first\n");
        assert_eq!(e.blocks(), &[Block::Bullet(0), Block::Bullet(0)]);

        // Tab indents the second list item to Bullet(1).
        e.key(&k(Key::Tab), None);
        assert_eq!(e.blocks(), &[Block::Bullet(0), Block::Bullet(1)]);
        e.key(&k(Key::Text("nested".into())), None);

        // Shift+Return inserts a soft break inside the same list item.
        let shift_enter = KeyEvent { key: Key::Enter, mods: Mods { shift: true, ..Mods::default() } };
        e.key(&shift_enter, None);
        e.key(&k(Key::Text("cont".into())), None);
        assert_eq!(e.soft_breaks(), &[2]);
        assert_eq!(e.blocks(), &[Block::Bullet(0), Block::Bullet(1), Block::Bullet(1)]);

        // Return at end of "cont" creates a new empty Bullet(1) item on line 3.
        e.key(&k(Key::Enter), None);
        assert_eq!(e.blocks().len(), 4);
        assert_eq!(e.current_block(), Block::Bullet(1));

        // Return on the empty Bullet(1) item ends the list item (converts to Paragraph) without adding a newline.
        let len_before = e.text.len();
        e.key(&k(Key::Enter), None);
        assert_eq!(e.text.len(), len_before);
        assert_eq!(e.current_block(), Block::Paragraph);

        // Backspace at start of a nested list item outdents first, then exits list.
        e.set_block(Block::Numbered(1));
        e.key(&k(Key::Backspace), None);
        assert_eq!(e.current_block(), Block::Numbered(0));
        e.key(&k(Key::Backspace), None);
        assert_eq!(e.current_block(), Block::Paragraph);

        // Inside a Code block: Return inserts a newline, Tab inserts '\t', and inline marks do not apply.
        e.set_block(Block::Code);
        e.toggle_mark(Mark::Bold);
        assert!(!e.has_mark(&Mark::Bold));
        e.key(&k(Key::Tab), None);
        e.key(&k(Key::Text("let x = 1;".into())), None);
        assert!(!e.key(&k(Key::Enter), None).submit);
        e.key(&k(Key::Text("let y = 2;".into())), None);
        assert!(e.text.ends_with("\tlet x = 1;\nlet y = 2;"));
        assert_eq!(e.current_block(), Block::Code);
        assert!(e.marks().is_empty());
    }

    #[test]
    fn undo_redo_and_ime_preserve_marks_blocks_and_spans() {
        let mut e = TextEdit::new("fn main() {}");
        e.anchor = 0;
        e.focus = 2;
        e.toggle_mark(Mark::Bold);
        e.set_block(Block::Quote);

        // Undo block change, then undo mark change, then redo both.
        undo(&mut e, false);
        assert_eq!(e.current_block(), Block::Paragraph);
        assert_eq!(e.marks(), &[MarkRange::new(0..2, Mark::Bold)]);
        undo(&mut e, false);
        assert!(e.marks().is_empty());
        undo(&mut e, true);
        assert_eq!(e.marks(), &[MarkRange::new(0..2, Mark::Bold)]);
        undo(&mut e, true);
        assert_eq!(e.current_block(), Block::Quote);

        // IME preedit with pending typing style and caller-supplied span mapping.
        e.set(3, false); // right before "main"
        e.toggle_mark(Mark::Italic);
        // Suppose caller supplies a syntax span covering "main" (3..7 in model).
        assert_eq!(e.map_range_to_display(3, 7), vec![(3, 7)]);
        e.ime(&Ime::Preedit("日".into(), Some((0, 3))));
        // During preedit ("fn 日main() {}"), "main" in display is shifted by 3 bytes to 6..10!
        assert_eq!(e.display().0, "fn 日main() {}");
        assert_eq!(e.map_range_to_display(3, 7), vec![(6, 10)]);
        // And the preedit itself has the pending Italic mark in rich_display_specs.
        let (spans, _, _, _) = e.rich_display_specs(crate::TextStyle::new(14.0, 400.0), RichStyle::default());
        assert!(spans.iter().any(|s| s.start == 3 && s.end == 6 && s.italic));

        // Commit IME preedit: Italic mark is committed on "日" (3..6).
        e.ime(&Ime::Commit("日".into()));
        assert_eq!(e.text, "fn 日main() {}");
        assert!(e.marks().contains(&MarkRange::new(3..6, Mark::Italic)));

        // Undo restores "fn main() {}" and its original marks.
        undo(&mut e, false);
        assert_eq!(e.text, "fn main() {}");
        assert_eq!(e.marks(), &[MarkRange::new(0..2, Mark::Bold)]);
        assert_eq!(e.map_range_to_display(3, 7), vec![(3, 7)]);
    }

    #[test]
    fn html_import_export_and_paste_safe_subset() {
        let html = concat!(
            "<p>Hello <b>bold</b>, <strong style=\"font-style: italic\">both</strong>, ",
            "<u>under</u>, <del>struck</del>, <code>inline()</code>, and ",
            "<a href=\"https://opencode.ai\">link</a>",
            "<a href=\"javascript:alert(1)\">bad</a></p>",
            "<ul><li>first<ul><li>nested<br>soft</li></ul></li></ul>",
            "<ol><li>one</li><li>two</li></ol>",
            "<blockquote>quoted</blockquote>",
            "<pre><code>fn add(a: i32) -&gt; i32 {\n\ta + 1\n}</code></pre>",
            "<script>evil()</script><style>body{display:none}</style>",
        );
        let e = TextEdit::from_html(html);
        assert!(!e.text.contains("evil"));
        assert!(!e.text.contains("display:none"));
        assert!(e.marks().iter().any(|m| m.mark == Mark::Bold));
        assert!(e.marks().iter().any(|m| m.mark == Mark::Italic));
        assert!(e.marks().iter().any(|m| m.mark == Mark::Underline));
        assert!(e.marks().iter().any(|m| m.mark == Mark::Strike));
        assert!(e.marks().iter().any(|m| m.mark == Mark::Code));
        assert!(e.marks().iter().any(|m| m.mark == Mark::Link("https://opencode.ai".into())));
        assert!(!e.marks().iter().any(|m| matches!(&m.mark, Mark::Link(u) if u.starts_with("javascript:"))));
        assert_eq!(
            e.blocks(),
            &[
                Block::Paragraph,
                Block::Bullet(0),
                Block::Bullet(1),
                Block::Bullet(1),
                Block::Numbered(0),
                Block::Numbered(0),
                Block::Quote,
                Block::Code,
                Block::Code,
                Block::Code,
            ]
        );
        assert_eq!(e.soft_breaks(), &[3]);

        // Round-trip via to_html() -> from_html().
        let exported = e.to_html();
        let round = TextEdit::from_html(&exported);
        assert_eq!(round.text, e.text);
        assert_eq!(round.marks(), e.marks());
        assert_eq!(round.blocks(), e.blocks());
        assert_eq!(round.soft_breaks(), e.soft_breaks());

        // paste_html into an existing editor splices text, marks, and blocks cleanly.
        let mut dest = TextEdit::new("prefix  suffix");
        dest.set(7, false);
        dest.paste_html("<b>rich</b> <i>paste</i>", "rich paste");
        assert_eq!(dest.text, "prefix rich paste suffix");
        assert_eq!(
            dest.marks(),
            &[
                MarkRange::new(7..11, Mark::Bold),
                MarkRange::new(12..17, Mark::Italic),
            ]
        );
    }

    #[test]
    fn spans_on_text_input_hold_through_ime_and_undo() {
        use crate::{Cx, El, Env, Runtime, Span, TextStyle, text_input};
        struct App {
            edit: TextEdit,
        }
        fn view(s: &App, _: &mut Cx<App>) -> El {
            // Highlight the first 3 bytes ("let") with a caller-supplied Span on plain text_input.
            let end = s.edit.text.len().min(3);
            let spans = if end > 0 {
                vec![Span::new(0, end).color(crate::Color::WHITE).bold()]
            } else {
                vec![]
            };
            text_input(&s.edit, TextStyle::new(14.0, 400.0))
                .key("ed")
                .size(240.0, 60.0)
                .spans(spans)
                .on_edit(|s: &mut App| &mut s.edit)
        }
        let mut edit = TextEdit::new("let x = 1;");
        edit.focused = true;
        edit.set(0, false);
        let mut rt = Runtime::new(App { edit }, view, (240.0, 60.0), Env::new(true, 1.0));
        rt.frame(0.0);

        // Start IME preedit at byte 0: display text becomes "あlet x = 1;", so caller's 0..3
        // span on `edit.text` shifts automatically to 3..6 in the rendered TextSpec!
        rt.ime(Ime::Preedit("あ".into(), Some((0, 3))));
        rt.frame(0.0);
        let spec_spans = &rt.nodes.iter().find(|n| n.key.as_deref() == Some("ed")).unwrap().text.as_ref().unwrap().spans;
        assert_eq!(spec_spans.len(), 1);
        assert_eq!((spec_spans[0].start, spec_spans[0].end), (3, 6));

        // Commit IME preedit, then undo back to "let x = 1;": span returns to 0..3.
        rt.ime(Ime::Commit("あ".into()));
        rt.frame(0.0);
        rt.key(KeyEvent { key: Key::Command('z'), mods: Mods { cmd: true, ..Mods::default() } });
        rt.frame(0.0);
        assert_eq!(rt.state.edit.text, "let x = 1;");
        let spec_spans = &rt.nodes.iter().find(|n| n.key.as_deref() == Some("ed")).unwrap().text.as_ref().unwrap().spans;
        assert_eq!(spec_spans.len(), 1);
        assert_eq!((spec_spans[0].start, spec_spans[0].end), (0, 3));
    }

    #[test]
    fn runtime_routes_mark_shortcuts_list_tabs_and_renders_decorations() {
        use crate::{Cx, El, Env, Runtime, TextStyle, col, text_input};
        struct App {
            edit: TextEdit,
            other: TextEdit,
        }
        fn view(s: &App, _: &mut Cx<App>) -> El {
            col()
                .size(320.0, 200.0)
                .child(
                    text_input(&s.edit, TextStyle::new(14.0, 400.0))
                        .key("ed")
                        .size(300.0, 140.0)
                        .on_edit(|s: &mut App| &mut s.edit),
                )
                .child(
                    text_input(&s.other, TextStyle::new(14.0, 400.0))
                        .key("other")
                        .size(300.0, 40.0)
                        .on_edit(|s: &mut App| &mut s.other),
                )
        }
        let mut edit = TextEdit::from_html(
            "<ul><li>item</li></ul><ol><li><del>struck</del></li></ol><blockquote>quote</blockquote><pre><code>let x = 1;</code></pre>",
        );
        edit.focused = true;
        edit.anchor = 0;
        edit.focus = 4; // "item"
        let mut rt = Runtime::new(
            App { edit, other: TextEdit::new("next") },
            view,
            (320.0, 200.0),
            Env::new(true, 1.0),
        );
        rt.frame(0.0);
        let base_instances = rt.instances.len();
        assert!(base_instances > 10);

        // Cmd+B, Cmd+I, Cmd+U toggle marks on the selection through Runtime::key.
        rt.key(KeyEvent { key: Key::Command('b'), mods: Mods { cmd: true, ..Mods::default() } });
        rt.key(KeyEvent { key: Key::Command('i'), mods: Mods { cmd: true, ..Mods::default() } });
        rt.key(KeyEvent { key: Key::Command('u'), mods: Mods { cmd: true, ..Mods::default() } });
        rt.frame(0.0);
        assert!(rt.state.edit.has_mark(&Mark::Bold));
        assert!(rt.state.edit.has_mark(&Mark::Italic));
        assert!(rt.state.edit.has_mark(&Mark::Underline));

        // Tab inside a list item indents the list item instead of moving focus to "other"!
        rt.key(KeyEvent { key: Key::Tab, mods: Mods::default() });
        rt.frame(0.0);
        assert!(rt.state.edit.focused);
        assert!(!rt.state.other.focused);
        assert_eq!(rt.state.edit.blocks()[0], Block::Bullet(1));

        // Shift+Tab outdents the list item back to Bullet(0).
        rt.key(KeyEvent { key: Key::Tab, mods: Mods { shift: true, ..Mods::default() } });
        rt.frame(0.0);
        assert_eq!(rt.state.edit.blocks()[0], Block::Bullet(0));
    }
}

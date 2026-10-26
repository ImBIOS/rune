//! Single and rich text-field editing: the attributed model an app owns (`TextEdit`) and the
//! key/IME events the shell routes to the focused field. Movement that depends on wrapping
//! (up/down, line start/end) uses the field's current Parley layout, rebuilt by the runtime
//! before each event.
//!
//! Invariants: `anchor` and `focus` are byte offsets on char boundaries of `text`; preedit text
//! is never part of `text` until committed; inline `marks` are non-empty char-boundary byte
//! ranges sorted by start; `blocks` has one entry per `\n`-separated line of `text`.

use parley::Layout;
use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;
use zeroize::{Zeroize, Zeroizing};

use crate::color::{oklch, oklcha, Color};
use crate::text::{Ink, Link, Span};

mod html;

/// Maximum nesting level (`0..=MAX_LIST_LEVEL`) for [`Block::Bullet`] and [`Block::Numbered`].
pub const MAX_LIST_LEVEL: u8 = 3;

/// An inline formatting mark stored alongside [`TextEdit::text`]. More marks may come, so a
/// `match` needs a `_` arm.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Mark {
    Bold,
    Italic,
    Underline,
    Strike,
    Code,
    Link(String),
    /// An atomic inline object (an `@file` or `@agent` mention, an attachment) carrying the
    /// app's id; its text is the label. The caret steps over it, deleting takes it whole, a
    /// selection that touches it covers it, and typing never extends it. A field draws it as a
    /// chip ([`El::pill_colors`](crate::El::pill_colors), [`El::pill_icon`](crate::El::pill_icon)). Insert one with
    /// [`TextEdit::insert_pill`].
    Pill(String),
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
            | (Mark::Link(_), Mark::Link(_))
            | (Mark::Pill(_), Mark::Pill(_)) => true,
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

    /// The app's id when `self` is [`Mark::Pill`].
    pub fn pill_id(&self) -> Option<&str> {
        match self {
            Mark::Pill(id) => Some(id),
            _ => None,
        }
    }

    pub(crate) fn is_pill(&self) -> bool {
        matches!(self, Mark::Pill(_))
    }

    pub(crate) fn kind_order(&self) -> u8 {
        match self {
            Mark::Bold => 0,
            Mark::Italic => 1,
            Mark::Underline => 2,
            Mark::Strike => 3,
            Mark::Code => 4,
            Mark::Link(_) => 5,
            Mark::Pill(_) => 6,
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

    /// This block's kind as one bit, for [`TextEdit::allow_blocks`].
    fn kind_bit(self) -> u8 {
        1 << match self {
            Block::Paragraph => 0,
            Block::Bullet(_) => 1,
            Block::Numbered(_) => 2,
            Block::Quote => 3,
            Block::Code => 4,
        }
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
#[non_exhaustive]
pub struct ParagraphInfo {
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
    /// Set inside [`TextEdit::undo_group`]: edits record nothing until the outermost group ends.
    group: Option<(u32, Snapshot)>,
    secure: bool,
    /// Mirrors the field's `El::single_line()`; the runtime sets it before each edit.
    single_line: bool,
    /// Formatting keys and rich paste are on (see [`TextEdit::rich`]).
    rich: bool,
    /// The mark kinds this field keeps, one bit per [`Mark::kind_order`]; `None` keeps all.
    allowed_marks: Option<u16>,
    /// The block kinds this field keeps ([`Block::kind_bit`]); `None` keeps all.
    allowed_blocks: Option<u8>,
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
pub(crate) struct History {
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

impl TextEdit {
    pub fn new(text: impl Into<String>) -> TextEdit {
        let text = text.into();
        let n = text.len();
        let line_count = 1 + count_newlines(text.as_bytes());
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
            group: None,
            secure: false,
            single_line: false,
            rich: false,
            allowed_marks: None,
            allowed_blocks: None,
        }
    }

    /// Construct a [`TextEdit`] from plain text, inline [`MarkRange`]s, and per-line [`Block`]s.
    /// Caret starts at the end of `text`. Ranges are clamped to char boundaries and canonicalized,
    /// and `blocks` is padded or truncated to match the number of `\n`-separated lines in `text`.
    pub fn from_parts(text: impl Into<String>, marks: Vec<MarkRange>, blocks: Vec<Block>) -> TextEdit {
        let mut edit = Self::new(text).rich();
        edit.marks = marks;
        edit.blocks = blocks;
        edit.clamp_list_levels();
        edit.sync_invariants();
        edit
    }

    fn clamp_list_levels(&mut self) {
        for b in &mut self.blocks {
            *b = b.with_level(b.list_level().unwrap_or(0));
        }
    }

    /// Rich editing: `⌘B` / `⌘I` / `⌘U` toggle marks, `Shift+Return` inserts a soft break and
    /// [`paste_html`](Self::paste_html) keeps formatting. Off by default, so a plain field leaves
    /// those keys to the app and pastes plain text. [`from_html`](Self::from_html) and
    /// [`from_parts`](Self::from_parts) turn it on. The mark and block methods work either way.
    pub fn rich(mut self) -> Self {
        self.rich = true;
        self
    }

    pub fn set_rich(&mut self, rich: bool) {
        self.rich = rich;
    }

    pub fn is_rich(&self) -> bool {
        self.rich
    }

    /// Keep only these kinds of inline mark, for a field whose storage can't hold the rest (an
    /// app that saves Markdown has no underline). A disallowed mark's shortcut (`⌘U`) goes on
    /// to the app's key handlers, `toggle_mark` and `set_mark` ignore it, and paste drops it.
    /// Kinds match by [`Mark::same_kind`], so any [`Mark::Link`] allows links. The field never
    /// holds a disallowed mark: marks it already has, or that `set_marks` adds, are dropped.
    /// Every kind is allowed by default, and [`Mark::Pill`] always is: pills are the app's own
    /// tokens, not formatting.
    ///
    /// ```ignore
    /// TextEdit::new("").rich().allow_marks([Mark::Bold, Mark::Italic, Mark::Strike, Mark::Code, Mark::Link(String::new())])
    /// ```
    pub fn allow_marks(mut self, marks: impl IntoIterator<Item = Mark>) -> Self {
        let pill = 1 << Mark::Pill(String::new()).kind_order();
        self.allowed_marks = Some(marks.into_iter().fold(pill, |bits, m| bits | 1 << m.kind_order()));
        self.sync_invariants();
        self
    }

    /// Keep only these kinds of paragraph block, as [`allow_marks`](Self::allow_marks) does for
    /// marks: `toggle_block` and `set_block` ignore the rest and pasted ones become paragraphs.
    /// [`Block::Paragraph`] is always allowed, and list levels don't matter (`Block::Bullet(0)`
    /// allows every bullet).
    pub fn allow_blocks(mut self, blocks: impl IntoIterator<Item = Block>) -> Self {
        self.allowed_blocks = Some(blocks.into_iter().fold(Block::Paragraph.kind_bit(), |bits, b| bits | b.kind_bit()));
        self.sync_invariants();
        self
    }

    /// Whether this field keeps marks of `mark`'s kind (see [`allow_marks`](Self::allow_marks)),
    /// to hide a formatting bar's button for one it doesn't (`Mark::Link(String::new())` checks
    /// links).
    pub fn allows_mark(&self, mark: &Mark) -> bool {
        self.allowed_marks.is_none_or(|bits| bits & 1 << mark.kind_order() != 0)
    }

    /// Whether this field keeps blocks of `block`'s kind (see [`allow_blocks`](Self::allow_blocks)).
    pub fn allows_block(&self, block: Block) -> bool {
        self.allowed_blocks.is_none_or(|bits| bits & block.kind_bit() != 0)
    }

    /// The mark a rich field's `⌘B`, `⌘I` or `⌘U` toggles, if this field takes that chord.
    pub(crate) fn format_chord(&self, ev: &KeyEvent) -> Option<Mark> {
        let Key::Command(c) = ev.key else { return None };
        if ev.mods.shift || ev.mods.alt || !self.rich || self.secure {
            return None;
        }
        let mark = match c {
            'b' => Mark::Bold,
            'i' => Mark::Italic,
            'u' => Mark::Underline,
            _ => return None,
        };
        self.allows_mark(&mark).then_some(mark)
    }

    /// `frag` without the marks and blocks this field doesn't allow.
    fn keep_allowed(&self, mut frag: TextEdit) -> TextEdit {
        frag.allowed_marks = self.allowed_marks;
        frag.allowed_blocks = self.allowed_blocks;
        frag.sync_invariants();
        frag
    }

    /// No marks, pending style or non-paragraph blocks: draws exactly as plain text.
    pub(crate) fn is_plain(&self) -> bool {
        self.marks.is_empty() && self.pending.is_none() && self.blocks.iter().all(|&b| b == Block::Paragraph)
    }

    /// Parse an HTML fragment (`<b>`, `<strong>`, `<i>`, `<em>`, `<u>`, `<s>`, `<del>`, `<strike>`,
    /// `<a href>`, `<code>`, `<pre>`, `<ul>`, `<ol>`, `<li>`, `<blockquote>`, `<p>`, `<div>`, `<br>`,
    /// and inline `style` for `font-weight`, `font-style`, `text-decoration`) into a [`TextEdit`].
    /// Unknown tags keep their text and strip the wrapper; `<script>` and `<style>` are dropped.
    pub fn from_html(html_str: &str) -> TextEdit {
        html::parse_html(html_str).rich()
    }

    /// Serialize the entire document's text, inline marks, and paragraph blocks to safe HTML.
    pub fn to_html(&self) -> String {
        let e = self.synced();
        html::serialize_html(&e, 0..e.text.len())
    }

    /// `self` with marks, blocks and caret fitted to `text`, which an app may have assigned
    /// directly.
    fn synced(&self) -> TextEdit {
        let mut e = self.clone();
        e.sync_invariants();
        e
    }

    /// Serialize the current selection (`self.range()`) to HTML, or `""` when the selection is empty.
    pub fn selected_html(&self) -> String {
        let (a, b) = self.range();
        if a == b || self.secure {
            return String::new();
        }
        html::serialize_html(&self.synced(), a..b)
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
        self.clamp_list_levels();
        self.sync_invariants();
    }

    /// Line indices (`> 0`) created by `Shift+Return` (`<br>`) rather than a hard paragraph break.
    pub fn soft_breaks(&self) -> &[u32] {
        &self.soft_breaks
    }

    /// `self` with soft breaks before lines `lines` (indices into `\n`-delimited lines, as
    /// [`soft_breaks`](Self::soft_breaks) returns them): each of those lines continues the
    /// paragraph, list item or quote above it, as `Shift+Return` makes, so a draft rebuilt with
    /// [`from_parts`](Self::from_parts) keeps them. Line 0 and lines past the end are dropped.
    ///
    /// ```ignore
    /// // "one" then "two" on a new line of the same bullet.
    /// TextEdit::from_parts("one\ntwo", vec![], vec![Block::Bullet(0); 2]).with_soft_breaks(vec![1])
    /// ```
    pub fn with_soft_breaks(mut self, lines: Vec<u32>) -> Self {
        self.set_soft_breaks(lines);
        self
    }

    /// Replace the soft breaks (see [`with_soft_breaks`](Self::with_soft_breaks)). Like
    /// `set_marks` and `set_blocks`, this isn't an undo step.
    pub fn set_soft_breaks(&mut self, lines: Vec<u32>) {
        self.soft_breaks = lines;
        self.sync_invariants();
    }

    /// Iterate the `\n`-delimited paragraphs of `self.text` with their byte range and [`Block`].
    pub fn paragraphs(&self) -> Vec<ParagraphInfo> {
        let mut out = Vec::new();
        let mut line_start = 0;
        let mut idx = 0;
        for (i, b) in self.text.bytes().enumerate() {
            if b == b'\n' {
                let block = self.blocks.get(idx).copied().unwrap_or(Block::Paragraph);
                let soft_break = self.soft_breaks.binary_search(&(idx as u32)).is_ok();
                out.push(ParagraphInfo { index: idx, range: line_start..i, block, soft_break });
                line_start = i + 1;
                idx += 1;
            }
        }
        let block = self.blocks.get(idx).copied().unwrap_or(Block::Paragraph);
        let soft_break = self.soft_breaks.binary_search(&(idx as u32)).is_ok();
        out.push(ParagraphInfo { index: idx, range: line_start..self.text.len(), block, soft_break });
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
        let line_count = 1 + count_newlines(text.as_bytes());
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

    pub(crate) fn same_model(&self, other: &TextEdit) -> bool {
        Arc::ptr_eq(&self.history, &other.history)
    }

    pub(crate) fn weak_history(&self) -> std::sync::Weak<History> {
        Arc::downgrade(&self.history)
    }

    pub(crate) fn matches_history(&self, weak: &std::sync::Weak<History>) -> bool {
        weak.ptr_eq(&Arc::downgrade(&self.history))
    }

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

    /// Drop the undo and redo stacks, keeping text, marks, blocks, soft breaks and the selection
    /// (after loading a sent message back into a composer to edit it, say).
    pub fn clear_history(&mut self) {
        self.history = Arc::default();
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
        self.sync_invariants();
    }

    fn record_change(&mut self, before: Snapshot, typing: bool) {
        if self.group.is_some() {
            return;
        }
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

    /// Run `f`, whose edits become one undo step: one undo reverts all of them and one redo
    /// restores them, with the selection from before the first and after the last. Groups
    /// nest and flatten into the outermost. A group that changes nothing records no step, and
    /// `undo` and `redo` do nothing inside one. Typing after the group starts a new step.
    ///
    /// Picking a mention is one step, trailing space included:
    ///
    /// ```
    /// # use rune::TextEdit;
    /// let mut e = TextEdit::default();
    /// e.insert("hi @ag");
    /// e.undo_group(|e| {
    ///     e.replace_with_pill(3..6, "agent:1", "@agent");
    ///     e.insert(" ");
    /// });
    /// assert_eq!(e.text, "hi @agent ");
    /// e.undo();
    /// assert_eq!(e.text, "hi @ag");
    /// ```
    pub fn undo_group<R>(&mut self, f: impl FnOnce(&mut TextEdit) -> R) -> R {
        if let Some((depth, _)) = &mut self.group {
            *depth += 1;
            let r = f(self);
            if let Some((depth, _)) = &mut self.group { *depth -= 1; }
            return r;
        }
        self.break_history_group();
        self.group = Some((0, self.snapshot()));
        let r = f(self);
        if let Some((_, before)) = self.group.take() {
            self.break_history_group();
            if !self.secure { self.record_change(before, false); }
        }
        r
    }

    /// Undo one typing group or discrete edit. Active IME composition is cancelled first.
    pub fn undo(&mut self) {
        if self.secure || self.group.is_some() { return; }
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
        if self.secure || self.group.is_some() { return; }
        self.break_history_group();
        if self.preedit.is_some() { self.clear_preedit(); return; }
        let h = Arc::make_mut(&mut self.history);
        let Some(change) = h.redo.pop() else { return };
        if self.text != change.before.text.as_str() { *h = History::default(); return; }
        self.restore(&change.after);
        Arc::make_mut(&mut self.history).undo.push_back(change);
    }

    /// The selection as ordered byte offsets, clamped to character boundaries of `text` (an app
    /// may have replaced `text` without moving the caret).
    pub fn range(&self) -> (usize, usize) {
        let t = &self.text;
        (Self::clamp_char_boundary(t, self.anchor.min(self.focus)), Self::clamp_char_boundary(t, self.anchor.max(self.focus)))
    }

    /// Move the caret to `byte` (clamped to a character boundary of `self.text`) and collapse the
    /// selection, clearing any pending typing style.
    pub fn set_caret(&mut self, byte: usize) {
        let b = Self::clamp_char_boundary(&self.text, byte);
        self.anchor = b;
        self.focus = b;
        self.snap_to_pills();
        self.pending = None;
    }

    /// Set the selection `anchor` and `focus` byte indices (clamped to character boundaries of
    /// `self.text`), clearing any pending typing style. A selection that ends inside a
    /// [`Mark::Pill`] grows to cover it.
    pub fn set_selection(&mut self, anchor: usize, focus: usize) {
        self.anchor = Self::clamp_char_boundary(&self.text, anchor);
        self.focus = Self::clamp_char_boundary(&self.text, focus);
        self.snap_to_pills();
        self.pending = None;
    }

    /// The [`Mark::Pill`]s in `self.text`, in order, with their byte ranges and ids.
    pub fn pills(&self) -> impl Iterator<Item = (Range<usize>, &str)> + '_ {
        let t = &self.text;
        self.marks.iter().filter_map(move |m| {
            let id = m.mark.pill_id()?;
            (m.start < m.end && m.end <= t.len() && t.is_char_boundary(m.start) && t.is_char_boundary(m.end) && !t.as_bytes()[m.start..m.end].contains(&b'\n'))
                .then(|| (m.range(), id))
        })
    }

    fn prev_word(&self, i: usize) -> usize {
        let s = &self.text;
        let b = s.as_bytes();
        let mut j = i.min(b.len());
        if let Some(m) = self.marks.iter().find(|m| m.mark.is_pill() && m.start < j && j <= m.end) {
            return m.start;
        }
        while j > 0 && b[j - 1].is_ascii_whitespace() {
            if let Some(m) = self.marks.iter().find(|m| m.mark.is_pill() && m.start < j && j <= m.end) {
                return m.start;
            }
            j -= 1;
        }
        if let Some(m) = self.marks.iter().find(|m| m.mark.is_pill() && m.start < j && j <= m.end) {
            return m.start;
        }
        while j > 0 && !b[j - 1].is_ascii_whitespace() {
            if self.marks.iter().any(|m| m.mark.is_pill() && m.end == j) {
                break;
            }
            j = prev_char(s, j);
        }
        j
    }

    fn next_word(&self, i: usize) -> usize {
        let s = &self.text;
        let b = s.as_bytes();
        let mut j = i.min(b.len());
        if let Some(m) = self.marks.iter().find(|m| m.mark.is_pill() && m.start <= j && j < m.end) {
            return m.end;
        }
        while j < b.len() && b[j].is_ascii_whitespace() {
            if let Some(m) = self.marks.iter().find(|m| m.mark.is_pill() && m.start <= j && j < m.end) {
                return m.end;
            }
            j += 1;
        }
        if let Some(m) = self.marks.iter().find(|m| m.mark.is_pill() && m.start <= j && j < m.end) {
            return m.end;
        }
        while j < b.len() && !b[j].is_ascii_whitespace() {
            if self.marks.iter().any(|m| m.mark.is_pill() && m.start == j) {
                break;
            }
            j = next_char(s, j);
        }
        j
    }

    /// Replace the selection with `label` as one [`Mark::Pill`] carrying `id`, and put the
    /// caret after it: one undo step. Line breaks in `label` become spaces.
    pub fn insert_pill(&mut self, id: impl Into<String>, label: &str) {
        let (a, b) = self.range();
        self.replace_with_pill(a..b, id, label);
    }

    /// Replace byte range `range` with `label` as one [`Mark::Pill`] carrying `id` (an `@query`
    /// the user typed, say), and put the caret after it: one undo step. The range grows to
    /// cover any pill it cuts into. Line breaks in `label` become spaces; an empty label or a
    /// secure field inserts nothing.
    pub fn replace_with_pill(&mut self, range: Range<usize>, id: impl Into<String>, label: &str) {
        let label: String = label.chars().map(|c| if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') { ' ' } else { c }).collect();
        if self.secure || label.is_empty() {
            return;
        }
        self.sync_invariants();
        self.break_history_group();
        let before = self.snapshot();
        let a = Self::clamp_char_boundary(&self.text, range.start.min(range.end));
        let b = Self::clamp_char_boundary(&self.text, range.end.max(range.start));
        let (a, b) = self.cover_pills(a, b);
        self.pending = None;
        self.apply_text_and_attr_replace((a, b), &label, &[], None, false);
        // Code blocks keep no inline marks: there the label stays plain text.
        self.marks.push(MarkRange { start: a, end: a + label.len(), mark: Mark::Pill(id.into()) });
        self.normalize_marks();
        self.record_change(before, false);
    }

    /// The pill strictly containing byte `i` (`start < i < end`).
    fn pill_around(&self, i: usize) -> Option<(usize, usize)> {
        self.marks.iter().find(|m| m.mark.is_pill() && m.start < i && i < m.end).map(|m| (m.start, m.end))
    }

    /// `i` moved out of any pill it lands inside, to its end when `forward`, else its start.
    fn out_of_pill(&self, i: usize, forward: bool) -> usize {
        self.pill_around(i).map_or(i, |(s, e)| if forward { e } else { s })
    }

    /// `a..b` grown to cover any pill either end cuts into; an empty range inside one moves
    /// to its end.
    fn cover_pills(&self, a: usize, b: usize) -> (usize, usize) {
        if a == b {
            let i = self.out_of_pill(a, true);
            return (i, i);
        }
        (self.out_of_pill(a, false), self.out_of_pill(b, true))
    }

    /// Keeps the caret off a pill's inside: a caret goes to the nearer edge, a selection grows
    /// to cover the pill.
    pub(crate) fn snap_to_pills(&mut self) {
        if !self.marks.iter().any(|m| m.mark.is_pill()) {
            return;
        }
        if self.anchor == self.focus {
            if let Some((s, e)) = self.pill_around(self.focus) {
                let to = if self.focus - s < e - self.focus { s } else { e };
                (self.anchor, self.focus) = (to, to);
            }
        } else {
            let forward = self.focus > self.anchor;
            self.anchor = self.out_of_pill(self.anchor, !forward);
            self.focus = self.out_of_pill(self.focus, forward);
        }
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
        count_newlines(&self.text.as_bytes()[..clamped])
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
        self.anchor = Self::clamp_char_boundary(&self.text, self.anchor);
        self.focus = Self::clamp_char_boundary(&self.text, self.focus);
        let line_count = 1 + count_newlines(self.text.as_bytes());
        if self.blocks.len() < line_count {
            self.blocks.resize(line_count, Block::Paragraph);
        } else if self.blocks.len() > line_count {
            self.blocks.truncate(line_count);
        }
        if let Some(bits) = self.allowed_blocks {
            for b in &mut self.blocks {
                if bits & b.kind_bit() == 0 {
                    *b = Block::Paragraph;
                }
            }
        }
        self.soft_breaks.sort_unstable();
        self.soft_breaks.dedup();
        self.soft_breaks.retain(|&idx| idx > 0 && (idx as usize) < line_count);
        self.normalize_marks();
        self.snap_to_pills();
        let anchor_line = self.line_of_byte(self.anchor);
        if let Some((armed, ref mut marks)) = self.pending {
            if armed != (self.anchor, self.focus) || self.blocks.get(anchor_line) == Some(&Block::Code) {
                self.pending = None;
            } else if let Some(bits) = self.allowed_marks {
                marks.retain(|m| bits & 1 << m.kind_order() != 0);
            }
        }
    }

    fn normalize_marks(&mut self) {
        if let Some(bits) = self.allowed_marks {
            self.marks.retain(|m| bits & 1 << m.mark.kind_order() != 0);
        }
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
            // One pass: cut each mark around the (sorted, disjoint) code ranges it overlaps.
            let mut out = Vec::with_capacity(self.marks.len());
            for m in self.marks.drain(..) {
                let mut start = m.start;
                let first = code_ranges.partition_point(|cr| cr.end <= start);
                for cr in &code_ranges[first..] {
                    if cr.start >= m.end {
                        break;
                    }
                    if start < cr.start {
                        out.push(MarkRange { start, end: cr.start, mark: m.mark.clone() });
                    }
                    start = start.max(cr.end);
                }
                if start < m.end {
                    out.push(MarkRange { start, end: m.end, mark: m.mark });
                }
            }
            self.marks = out;
        }

        // Merge overlapping or adjacent ranges of the same mark kind.
        // For `Mark::Link`, later entries win over earlier overlapping links.
        let mut by_kind: [Vec<MarkRange>; 7] = Default::default();
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
            if kind_idx == 6 {
                // `Mark::Pill`: whole objects on one line that never merge; the first of two
                // overlapping pills wins.
                list.sort_by_key(|m| (m.start, m.end));
                let mut end = 0;
                for m in list {
                    if m.start >= end && !self.text.as_bytes()[m.start..m.end].contains(&b'\n') {
                        end = m.end;
                        merged.push(m);
                    }
                }
            } else if kind_idx == 5 {
                // `Mark::Link`: resolve overlaps so later links overwrite earlier ones, then merge adjacent equal URLs.
                let mut bounds: Vec<(usize, usize)> = list.iter().map(|m| (m.start, m.end)).collect();
                bounds.sort_unstable();
                let overlapping = bounds.windows(2).any(|w| w[1].0 < w[0].1);
                let mut resolved: Vec<MarkRange> = Vec::new();
                if !overlapping {
                    resolved = std::mem::take(&mut list);
                }
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

    /// Marks inherited from the character before `pos` according to AppKit rules:
    /// typing inside (`m.start < pos < m.end`) or at the trailing edge (`m.start < pos == m.end`)
    /// of a mark inherits it; typing at the start of a mark (`pos == m.start`) does not extend it.
    fn inherited_marks_at(&self, pos: usize) -> Vec<Mark> {
        if self.marks.is_empty() {
            return Vec::new();
        }
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
            if m.start < pos && pos <= m.end && !m.mark.is_pill() && !out.iter().any(|existing: &Mark| existing.same_kind(&m.mark)) {
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
    /// Inside a [`Block::Code`] paragraph, inline marks do not apply, nor does a mark the field
    /// doesn't [allow](Self::allow_marks).
    pub fn toggle_mark(&mut self, mark: Mark) {
        if self.secure || !self.allows_mark(&mark) {
            return;
        }
        self.sync_invariants();
        let (a, b) = self.range();
        if a == b {
            if mark.is_pill() {
                return;
            }
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
        if self.secure || !self.allows_mark(&mark) {
            return;
        }
        self.sync_invariants();
        let (a, b) = self.range();
        if a == b {
            if mark.is_pill() {
                return;
            }
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
        if mark.is_pill() {
            self.marks.retain(|m| !m.mark.is_pill() || m.end <= a || m.start >= b);
        }
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
            } else if m.mark.is_pill() {
                // A pill goes whole: its label stays as plain text.
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

    /// Set the [`Block`] attribute for all paragraphs in the current selection, unless the field
    /// doesn't [allow](Self::allow_blocks) it.
    pub fn set_block(&mut self, block: Block) {
        if self.secure || self.single_line || !self.allows_block(block) {
            return;
        }
        let block = block.with_level(block.list_level().unwrap_or(0));
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
        if self.secure || self.single_line || !self.allows_block(block) {
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
        // Callers sync first; a second whole-text pass here would show on every keystroke.
        let del_len = b - a;
        let ins_len = s.len();
        if self.marks.is_empty()
            && inserted_marks.is_empty()
            && self.soft_breaks.is_empty()
            && !mark_new_lines_soft
            && new_line_block.is_none_or(|b| b == Block::Paragraph)
            && self.blocks.iter().all(|&b| b == Block::Paragraph)
        {
            // Plain text: no per-line passes, so a keystroke in a long plain field costs what
            // it did before blocks and marks existed.
            let removed = count_newlines(&self.text.as_bytes()[a..b]);
            let added = count_newlines(s.as_bytes());
            self.text.replace_range(a..b, s);
            self.blocks.resize(self.blocks.len() + added - removed, Block::Paragraph);
            self.anchor = a + ins_len;
            self.focus = self.anchor;
            self.preedit = None;
            return;
        }
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
        let added_lines = count_newlines(s.as_bytes());
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
            } else if m.mark.is_pill() {
                // An edit that cuts into a pill leaves its remains as plain text.
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
            // Typed or replacing text never becomes a pill.
            for im in inserted_marks.iter().filter(|m| !m.is_pill()) {
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
        // Backspace or Delete next to a pill, or a selection into one, takes it whole.
        let (a, b) = self.cover_pills(a, b);
        if !typing { self.break_history_group(); }
        if &self.text[a..b] == s
            && self.pending.is_none()
            && !self.marks.iter().any(|m| m.mark.is_pill() && m.start < b && a < m.end)
        {
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
        // A caret moving into a pill steps over it, the way it was going.
        let i = self.out_of_pill(i, i > self.focus);
        self.focus = i;
        if !extend {
            self.anchor = i;
        }
        self.snap_to_pills();
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
                    if m.shift && self.rich {
                        // Shift+Return: soft line break keeping the current paragraph's block.
                        self.insert_newline(blk, true);
                    } else if m.shift || m.alt {
                        self.insert_newline(blk, false);
                    } else {
                        match blk {
                            Block::Code => {
                                // Return on a blank last line leaves the block, as on an empty
                                // list item; anywhere else it inserts a newline.
                                if a == b && line_range.is_empty() && self.blocks.get(line_idx + 1) != Some(&Block::Code) {
                                    self.set_block(Block::Paragraph);
                                } else {
                                    self.insert_newline(Block::Code, false);
                                }
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
                        let from = if m.cmd { self.text[..a].rfind('\n').map_or(0, |i| i + 1) } else if word { self.prev_word(a) } else { prev_char(&self.text, a) };
                        self.replace((from, a), "", false);
                    }
                }
            }
            Key::Delete => {
                let (a, b) = self.range();
                if a != b {
                    self.insert("");
                } else if a < self.text.len() {
                    let to = if word { self.next_word(a) } else { next_char(&self.text, a) };
                    self.replace((a, to), "", false);
                }
            }
            Key::Left => {
                let i = if m.cmd { self.line_edge(layout, false) } else if word { self.prev_word(self.focus) } else { prev_char(&self.text, self.focus) };
                self.move_h(i, m.shift);
            }
            Key::Right if !m.shift && !m.cmd && !m.alt && !m.ctrl && self.at_end_of_trailing_block() => self.leave_trailing_block(),
            Key::Down if !m.shift && !m.cmd && !m.alt && !m.ctrl && self.at_end_of_trailing_block() => self.leave_trailing_block(),
            Key::Right => {
                let i = if m.cmd { self.line_edge(layout, true) } else if word { self.next_word(self.focus) } else { next_char(&self.text, self.focus) };
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
                    match crate::text::move_line(l, self.display_index(self.focus), down) {
                        Some(j) => {
                            let j = self.model_index(j);
                            if j == self.focus { if down { n } else { 0 } } else { j }
                        }
                        None => n,
                    }
                } else if down {
                    n
                } else {
                    0
                };
                let i = i.min(n);
                if m.shift {
                    self.focus = i;
                    self.snap_to_pills();
                    self.pending = None;
                } else {
                    self.set_caret(i);
                }
            }
            Key::Command(c) => match c {
                'z' if m.shift => self.redo(),
                'z' => self.undo(),
                'a' => {
                    self.anchor = 0;
                    self.focus = self.text.len();
                    self.pending = None;
                }
                'b' | 'i' | 'u' => {
                    if let Some(mark) = self.format_chord(ev) {
                        self.toggle_mark(mark);
                    }
                }
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

    /// The caret is at the end of the text, in a code block or quote there that it can't move
    /// out of by itself.
    fn at_end_of_trailing_block(&self) -> bool {
        let n = self.text.len();
        !self.secure
            && !self.single_line
            && self.preedit.is_none()
            && (self.anchor, self.focus) == (n, n)
            && matches!(self.blocks.last(), Some(Block::Code | Block::Quote))
    }

    /// Adds an empty paragraph after a trailing code block or quote and puts the caret in it:
    /// one undo step.
    fn leave_trailing_block(&mut self) {
        self.sync_invariants();
        self.break_history_group();
        let before = self.snapshot();
        let n = self.text.len();
        self.pending = None;
        self.apply_text_and_attr_replace((n, n), "\n", &[], Some(Block::Paragraph), false);
        self.record_change(before, false);
    }

    fn line_edge(&self, layout: Option<&Layout<Ink>>, end: bool) -> usize {
        if let Some(l) = layout {
            return self.model_index(crate::text::line_edge(l, self.display_index(self.focus), end));
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
    /// selection, recorded as a single undo step. Does not touch the OS clipboard. A plain field,
    /// or HTML with no marks or blocks this model keeps, pastes `plain` instead. Pills
    /// (`<span data-pill="id">`) paste as their plain labels: a field restores them only from
    /// its app's own copies, and an app's paste hook can with
    /// [`paste_html_with_pills`](Self::paste_html_with_pills).
    pub fn paste_html(&mut self, html_str: &str, plain: &str) -> EditOutcome {
        self.paste_rich(html_str, plain, false)
    }

    /// [`paste_html`](Self::paste_html), keeping the HTML's [`Mark::Pill`]s.
    pub fn paste_html_with_pills(&mut self, html_str: &str, plain: &str) -> EditOutcome {
        self.paste_rich(html_str, plain, true)
    }

    fn paste_rich(&mut self, html_str: &str, plain: &str, pills: bool) -> EditOutcome {
        if self.secure || html_str.trim().is_empty() {
            return self.paste(plain);
        }
        let mut frag = Self::from_html(html_str);
        if !pills {
            frag.marks.retain(|m| !m.mark.is_pill());
        }
        // HTML that only colours or lays out text (a code editor's copy) would collapse the
        // plain text's whitespace for nothing.
        let frag = self.keep_allowed(frag);
        let unformatted = frag.marks.is_empty() && frag.blocks.iter().all(|&b| b == Block::Paragraph);
        if !plain.is_empty() && (frag.text.is_empty() || unformatted) {
            return self.paste(plain);
        }
        if !self.rich {
            return self.paste(if plain.is_empty() { frag.text.as_str() } else { plain });
        }
        self.sync_invariants();
        // A code block takes the plain text, whose whitespace HTML would have collapsed.
        if !self.single_line {
            let (a, _) = self.range();
            if self.blocks.get(self.line_of_byte(a)) == Some(&Block::Code) {
                return self.paste(if plain.is_empty() { frag.text.as_str() } else { plain });
            }
        }
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
        let s = Self::clamp_char_boundary(&self.text, start);
        let e = Self::clamp_char_boundary(&self.text, end);
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
                // The chip and its colours come from `display_pills`.
                Mark::Pill(_) => {}
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
            for m in self.active_marks().iter().filter(|m| !m.is_pill()) {
                push_mark_span(ps, pe, m, &mut spans, &mut strikes, &mut links);
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

    /// Each [`Mark::Pill`]'s byte range in `self.display().0`, with its id.
    pub(crate) fn display_pills(&self) -> Vec<(u32, u32, &str)> {
        if self.secure {
            return Vec::new();
        }
        self.marks
            .iter()
            .filter_map(|m| {
                let id = m.mark.pill_id()?;
                match self.map_range_to_display(m.start, m.end)[..] {
                    [(s, e)] if (e - s) as usize == m.end - m.start => Some((s, e, id)),
                    _ => None,
                }
            })
            .collect()
    }

    fn display_blocks(&self, disp_text: &str) -> Vec<Block> {
        let line_count = 1 + count_newlines(disp_text.as_bytes());
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

/// `\n` bytes in `s`. Counts into `u8` lanes so it vectorizes: `filter().count()` widens every
/// byte to `usize` and runs several times slower on a long field.
fn count_newlines(s: &[u8]) -> usize {
    s.chunks(255).map(|c| c.iter().fold(0u8, |n, &b| n + (b == b'\n') as u8) as usize).sum()
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
        let mut e = TextEdit::new("").rich();
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
        let mut e = TextEdit::new("first").rich();
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
        let mut dest = TextEdit::new("prefix  suffix").rich();
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

    fn cmd(c: char) -> KeyEvent {
        KeyEvent { key: Key::Command(c), mods: Mods { cmd: true, ..Mods::default() } }
    }

    /// The text under each range of `mark`.
    fn marked<'a>(e: &'a TextEdit, mark: &Mark) -> Vec<&'a str> {
        e.marks().iter().filter(|m| m.mark == *mark).map(|m| &e.text[m.start..m.end]).collect()
    }

    /// What every edit path must leave behind.
    fn assert_invariants(e: &TextEdit, ctx: &str) {
        let t = &e.text;
        let lines = 1 + t.bytes().filter(|&b| b == b'\n').count();
        assert_eq!(e.blocks().len(), lines, "{ctx}: one block per line");
        assert!(e.blocks().iter().all(|b| b.list_level().is_none_or(|l| l <= MAX_LIST_LEVEL)), "{ctx}: list levels");
        assert!(e.soft_breaks().windows(2).all(|w| w[0] < w[1]), "{ctx}: soft breaks sorted");
        assert!(e.soft_breaks().iter().all(|&i| i > 0 && (i as usize) < lines), "{ctx}: soft breaks in range");
        assert!(t.is_char_boundary(e.anchor) && t.is_char_boundary(e.focus), "{ctx}: caret on a char boundary");
        let ms = e.marks();
        assert!(ms.windows(2).all(|w| w[0].start <= w[1].start), "{ctx}: marks sorted");
        for m in ms {
            assert!(m.start < m.end && m.end <= t.len(), "{ctx}: mark {m:?} in range");
            assert!(t.is_char_boundary(m.start) && t.is_char_boundary(m.end), "{ctx}: mark {m:?} on char boundaries");
            for line in e.line_of_byte(m.start)..=e.line_of_byte(m.end - 1) {
                assert_ne!(e.blocks()[line], Block::Code, "{ctx}: mark {m:?} reaches code line {line}");
            }
        }
        for (i, a) in ms.iter().enumerate() {
            for b in &ms[i + 1..] {
                if a.mark.same_kind(&b.mark) {
                    assert!(b.start >= a.end, "{ctx}: same-kind marks {a:?} {b:?} overlap");
                    assert!(!(b.start == a.end && a.mark == b.mark), "{ctx}: equal marks {a:?} {b:?} not merged");
                }
            }
        }
    }

    #[test]
    fn plain_edit_leaves_rich_keys_alone_and_pastes_plain_text() {
        let shift_enter = KeyEvent { key: Key::Enter, mods: Mods { shift: true, ..Mods::default() } };
        let mut e = TextEdit::new("hello");
        e.set_selection(0, 5);
        e.key(&cmd('b'), None);
        e.key(&cmd('i'), None);
        assert!(e.marks().is_empty() && e.is_plain());
        e.set(5, false);
        e.key(&shift_enter, None);
        assert_eq!((e.text.as_str(), e.soft_breaks()), ("hello\n", &[][..]));
        e.paste_html("<b>x</b><ul><li>y</li></ul>", "x\ny");
        assert_eq!(e.text, "hello\nx\ny");
        assert!(e.is_plain(), "a plain field pastes the plain text");

        let mut r = TextEdit::new("hello").rich();
        r.set_selection(0, 5);
        r.key(&cmd('b'), None);
        assert_eq!(marked(&r, &Mark::Bold), ["hello"]);
        r.set(5, false);
        r.key(&shift_enter, None);
        assert_eq!(r.soft_breaks(), &[1]);
        assert!(TextEdit::from_html("<b>a</b>").is_rich() && TextEdit::from_parts("a", vec![], vec![]).is_rich());
    }

    #[test]
    fn plain_field_leaves_format_chords_to_the_app() {
        use crate::{text_input, Cx, El, Env, Runtime, TextStyle};
        struct Form { f: TextEdit, keys: Vec<Key> }
        fn view(s: &Form, _: &mut Cx<Form>) -> El {
            text_input(&s.f, TextStyle::new(14.0, 400.0)).key("f").size(200.0, 40.0).on_edit(|s: &mut Form| &mut s.f)
        }
        let mut f = TextEdit::new("hello");
        f.focused = true;
        f.set_selection(0, 5);
        let mut rt = Runtime::new(Form { f, keys: vec![] }, view, (200.0, 40.0), Env::new(true, 1.0));
        rt.on_key = Some(|s, ev, _| { s.keys.push(ev.key.clone()); true });
        rt.frame(0.0);
        for c in ['b', 'i', 'u'] { rt.key(cmd(c)); rt.frame(0.0); }
        assert_eq!(rt.state.keys, [Key::Command('b'), Key::Command('i'), Key::Command('u')]);
        assert!(rt.state.f.marks().is_empty());
        // A field with no formatting lays out as plain text: no spans, blocks or soft breaks.
        let n = rt.nodes.iter().find(|n| n.key.as_deref() == Some("f")).unwrap();
        let t = n.text.as_ref().unwrap();
        assert!(t.spans.is_empty() && t.blocks.is_empty() && t.soft_breaks.is_empty() && t.strikes.is_empty());
    }

    #[test]
    fn direct_text_assignment_never_panics() {
        use crate::{text_input, Cx, El, Env, Runtime, TextStyle};
        let fresh = || {
            let mut e = TextEdit::from_html("<ul><li><b>bold</b> <a href=\"https://a.b\">item</a></li></ul><pre><code>a\nb</code></pre><p>x<br>y</p>");
            e.set(e.text.len(), false);
            e
        };
        // The app shrinks `text` under marks, blocks, soft breaks and a caret that sit past its
        // end or inside a character.
        let stale = |e: &mut TextEdit| {
            e.text = "é🦀".into();
            e.anchor = 1;
            e.focus = 3;
        };
        let mut e = fresh();
        stale(&mut e);
        let _ = (e.selected(), e.display(), e.paragraphs(), e.active_block(), e.active_marks(), e.has_mark(&Mark::Bold));
        let _ = (e.to_html(), e.selected_html());
        e.key(&k(Key::Left), None);
        assert_invariants(&e, "after Left");
        for ev in [k(Key::Text("z".into())), k(Key::Backspace), k(Key::Enter), cmd('b'), k(Key::Tab)] {
            let mut e = fresh();
            stale(&mut e);
            e.key(&ev, None);
            assert_invariants(&e, &format!("after {ev:?}"));
        }
        let mut e = fresh();
        stale(&mut e);
        e.ime(&Ime::Preedit("に".into(), None));
        e.ime(&Ime::Commit("日".into()));
        assert_invariants(&e, "after IME");
        let mut e = fresh();
        stale(&mut e);
        e.paste_html("<i>p</i>", "p");
        assert_invariants(&e, "after paste_html");
        let mut e = fresh();
        stale(&mut e);
        e.toggle_mark(Mark::Italic);
        e.set_block(Block::Quote);
        assert_invariants(&e, "after toggle_mark and set_block");

        struct App { e: TextEdit }
        fn view(s: &App, _: &mut Cx<App>) -> El {
            text_input(&s.e, TextStyle::new(14.0, 400.0)).key("e").size(200.0, 120.0).on_edit(|s: &mut App| &mut s.e)
        }
        let mut e = fresh();
        e.focused = true;
        let mut rt = Runtime::new(App { e }, view, (200.0, 120.0), Env::new(true, 1.0));
        rt.frame(0.0);
        rt.update(|s, _| stale(&mut s.e));
        rt.frame(0.0);
        rt.ime(Ime::Preedit("あ".into(), None));
        rt.frame(0.0);
        rt.key(k(Key::Right));
        rt.ime(Ime::Commit("あ".into()));
        rt.frame(0.0);
        assert_invariants(&rt.state.e, "runtime");
    }

    #[test]
    fn count_newlines_matches_a_plain_count() {
        for len in [0, 1, 254, 255, 256, 600, 70_000] {
            let t: Vec<u8> = (0..len).map(|i| if i % 3 == 0 || i > 1000 { b'\n' } else { b'x' }).collect();
            assert_eq!(count_newlines(&t), t.iter().filter(|&&b| b == b'\n').count(), "{len}");
        }
    }

    #[test]
    fn list_levels_stop_at_max_list_level() {
        let mut e = TextEdit::new("a").rich();
        e.set_block(Block::Bullet(0));
        for _ in 0..10 { e.key(&k(Key::Tab), None); }
        assert_eq!(e.blocks(), &[Block::Bullet(MAX_LIST_LEVEL)]);
        e.set_block(Block::Numbered(9));
        assert_eq!(e.blocks(), &[Block::Numbered(MAX_LIST_LEVEL)]);
        assert_eq!(TextEdit::from_parts("a", vec![], vec![Block::Bullet(200)]).blocks(), &[Block::Bullet(MAX_LIST_LEVEL)]);
        let deep = TextEdit::from_html(&format!("{}a", "<ul><li>".repeat(8)));
        assert_eq!(deep.blocks().last(), Some(&Block::Bullet(MAX_LIST_LEVEL)));
    }

    #[test]
    fn paste_html_without_formatting_keeps_the_plain_whitespace() {
        // VS Code's copy: colour spans in a `white-space: pre` div.
        let html = concat!(
            "<meta charset='utf-8'><div style=\"color: #d4d4d4;white-space: pre;\">",
            "<div><span style=\"color: #569cd6;\">fn</span><span>&nbsp;a()&nbsp;{</span></div>",
            "<div><span>&nbsp; &nbsp; b</span></div><div><span>}</span></div></div>",
        );
        let mut e = TextEdit::new("").rich();
        e.paste_html(html, "fn a() {\n    b\n}");
        assert_eq!(e.text, "fn a() {\n    b\n}");
        let mut e = TextEdit::new("").rich();
        e.paste_html("<p>a  <b>b</b></p>", "a  b");
        assert_eq!((e.text.as_str(), marked(&e, &Mark::Bold)), ("a b", vec!["b"]), "formatted HTML still wins");
    }

    #[test]
    fn paste_html_into_a_code_block_keeps_the_plain_whitespace() {
        let mut e = TextEdit::new("").rich();
        e.set_block(Block::Code);
        e.paste_html("<p>fn  a()</p><p>b</p>", "fn  a() {\n\tb\n}");
        assert_eq!(e.text, "fn  a() {\n\tb\n}");
        assert_eq!(e.blocks(), &[Block::Code; 3]);
        assert!(e.marks().is_empty());
    }

    #[test]
    fn soft_breaks_can_be_set_on_a_rebuilt_draft() {
        let mut e = TextEdit::from_parts("one\ntwo\nthree", vec![], vec![Block::Bullet(0); 3]).with_soft_breaks(vec![5, 1, 0, 1]);
        assert!(!e.has_history(), "with_soft_breaks on from_parts adds no undo step");
        assert_eq!(e.soft_breaks(), &[1], "line 0, duplicates and lines past the end are dropped");
        assert_eq!(e.to_html().matches("<li>").count(), 2, "\"two\" continues the first bullet");
        assert_invariants(&e, "with_soft_breaks");
        e.set_soft_breaks(vec![2]);
        assert!(!e.has_history());
        assert_eq!(e.soft_breaks(), &[2]);
        let round = TextEdit::from_parts(e.text.clone(), e.marks().to_vec(), e.blocks().to_vec()).with_soft_breaks(e.soft_breaks().to_vec());
        assert_eq!(round.to_html(), e.to_html());
    }

    #[test]
    fn clear_history_drops_undo_and_redo_while_keeping_formatting_and_selection() {
        let mut e = TextEdit::from_parts("one\ntwo", vec![MarkRange::new(0..3, Mark::Bold)], vec![Block::Bullet(0); 2])
            .with_soft_breaks(vec![1]);
        e.insert("!");
        e.undo();
        assert!(e.has_history());
        e.redo();
        e.set_selection(1, 4);
        e.clear_history();
        assert!(!e.has_history());
        e.undo();
        e.redo();
        assert_eq!((e.text.as_str(), e.anchor, e.focus), ("one\ntwo!", 1, 4));
        assert_eq!(e.marks(), &[MarkRange::new(0..3, Mark::Bold)]);
        assert_eq!(e.blocks(), &[Block::Bullet(0); 2]);
        assert_eq!(e.soft_breaks(), &[1]);
    }

    #[test]
    fn return_on_a_blank_last_line_leaves_a_code_block_in_one_undo_step() {
        let mut e = TextEdit::new("").rich();
        e.set_block(Block::Code);
        e.key(&k(Key::Text("let x = 1;".into())), None);
        e.key(&k(Key::Enter), None);
        assert_eq!((e.text.as_str(), e.blocks()), ("let x = 1;\n", &[Block::Code; 2][..]));
        // Composing on a blank code line and pressing Return to commit stays in the block.
        e.ime(&Ime::Preedit("に".into(), None));
        assert!(!e.key(&k(Key::Enter), None).submit);
        e.ime(&Ime::Commit("日".into()));
        assert_eq!((e.text.as_str(), e.blocks()), ("let x = 1;\n日", &[Block::Code; 2][..]));
        undo(&mut e, false);
        assert_eq!((e.text.as_str(), e.blocks()), ("let x = 1;\n", &[Block::Code; 2][..]));

        let out = e.key(&k(Key::Enter), None);
        assert!(!out.submit);
        assert_eq!((e.text.as_str(), e.blocks()), ("let x = 1;\n", &[Block::Code, Block::Paragraph][..]), "the blank line left the block");
        assert_eq!((e.anchor, e.focus), (11, 11));
        undo(&mut e, false);
        assert_eq!((e.text.as_str(), e.blocks()), ("let x = 1;\n", &[Block::Code; 2][..]), "leaving is one undo step");
        undo(&mut e, true);
        assert_eq!((e.text.as_str(), e.blocks()), ("let x = 1;\n", &[Block::Code, Block::Paragraph][..]), "redo leaves the block again");
        e.key(&k(Key::Text("after".into())), None);
        assert_eq!(e.blocks(), &[Block::Code, Block::Paragraph]);
        assert_invariants(&e, "left code");
        undo(&mut e, false);
        undo(&mut e, false);
        assert_eq!((e.text.as_str(), e.blocks()), ("let x = 1;\n", &[Block::Code; 2][..]));

        // A blank line with more code below it is the code's own.
        let mut e = TextEdit::from_parts("a\n\nb", vec![], vec![Block::Code; 3]);
        e.anchor = 2;
        e.focus = 2;
        e.key(&k(Key::Enter), None);
        assert_eq!((e.text.as_str(), e.blocks()), ("a\n\n\nb", &[Block::Code; 4][..]));
        // So is a non-blank last line, and a blank line under a selection.
        let mut e = TextEdit::from_parts("a\n", vec![], vec![Block::Code; 2]);
        e.anchor = 0;
        e.focus = 2;
        e.key(&k(Key::Enter), None);
        assert_eq!((e.text.as_str(), e.blocks()), ("\n", &[Block::Code; 2][..]));
    }

    #[test]
    fn down_or_right_at_the_end_of_a_trailing_code_block_or_quote_adds_a_paragraph() {
        for (block, key) in [(Block::Code, Key::Down), (Block::Code, Key::Right), (Block::Quote, Key::Down), (Block::Quote, Key::Right)] {
            let mut e = TextEdit::from_parts("x\ny", vec![], vec![block; 2]);
            let ctx = format!("{block:?} {key:?}");
            e.anchor = 1;
            e.focus = 1;
            e.key(&k(key.clone()), None);
            assert_eq!(e.text, "x\ny", "{ctx}: not at the end, the caret just moves");
            e.anchor = 3;
            e.focus = 3;
            e.key(&k(key.clone()), None);
            assert_eq!((e.text.as_str(), e.blocks()), ("x\ny\n", &[block, block, Block::Paragraph][..]), "{ctx}");
            assert_eq!((e.anchor, e.focus), (4, 4), "{ctx}");
            assert_invariants(&e, &ctx);
            e.key(&k(key.clone()), None);
            assert_eq!(e.text, "x\ny\n", "{ctx}: a paragraph at the end stays put");
            undo(&mut e, false);
            assert_eq!((e.text.as_str(), e.blocks()), ("x\ny", &[block; 2][..]), "{ctx}: one undo step");
            undo(&mut e, true);
            assert_eq!((e.text.as_str(), e.blocks()), ("x\ny\n", &[block, block, Block::Paragraph][..]), "{ctx}: redo restores it");
        }
        // Does not fire with a selection at the end, during IME, with modifiers, or in a plain field.
        for key in [Key::Down, Key::Right] {
            let mut e = TextEdit::from_parts("x", vec![], vec![Block::Code]);
            e.set_selection(0, 1);
            e.key(&k(key.clone()), None);
            assert_eq!((e.text.as_str(), e.blocks()), ("x", &[Block::Code][..]), "selection ending at end");

            let mut e = TextEdit::from_parts("x", vec![], vec![Block::Code]);
            e.ime(&Ime::Preedit("に".into(), None));
            e.key(&k(key.clone()), None);
            assert_eq!((e.text.as_str(), e.blocks()), ("x", &[Block::Code][..]), "during IME preedit");

            let mut e = TextEdit::from_parts("x", vec![], vec![Block::Code]);
            e.key(&KeyEvent { key: key.clone(), mods: Mods { shift: true, ..Mods::default() } }, None);
            e.key(&KeyEvent { key: key.clone(), mods: Mods { alt: true, ..Mods::default() } }, None);
            assert_eq!((e.text.as_str(), e.blocks()), ("x", &[Block::Code][..]), "modified arrow");

            let mut plain = TextEdit::new("x");
            plain.key(&k(key), None);
            assert_eq!((plain.text.as_str(), plain.blocks()), ("x", &[Block::Paragraph][..]), "plain field");
        }
    }

    #[test]
    fn a_field_keeps_only_the_marks_and_blocks_it_allows() {
        let mut e = TextEdit::from_parts("ab", vec![MarkRange { start: 0, end: 1, mark: Mark::Underline }], vec![Block::Paragraph])
            .allow_marks([Mark::Bold, Mark::Strike, Mark::Code, Mark::Link(String::new())])
            .allow_blocks([Block::Bullet(0), Block::Quote]);
        assert!(e.marks().is_empty(), "marks it held are dropped");
        assert!(e.allows_mark(&Mark::Link("https://example.com".into())) && !e.allows_mark(&Mark::Italic));
        assert!(e.allows_block(Block::Bullet(3)) && e.allows_block(Block::Paragraph) && !e.allows_block(Block::Code));
        e.anchor = 0;
        e.focus = 2;
        assert_eq!(e.format_chord(&cmd('u')), None, "⌘U is the app's");
        e.key(&cmd('u'), None);
        e.key(&cmd('i'), None);
        e.toggle_mark(Mark::Italic);
        e.set_mark(Mark::Underline);
        assert!(e.marks().is_empty());
        e.key(&cmd('b'), None);
        assert_eq!(marked(&e, &Mark::Bold), ["ab"]);
        e.toggle_block(Block::Code);
        e.set_block(Block::Numbered(0));
        assert_eq!(e.blocks(), &[Block::Paragraph]);
        e.toggle_block(Block::Bullet(0));
        assert_eq!(e.blocks(), &[Block::Bullet(0)]);
        e.set_marks(vec![MarkRange { start: 0, end: 2, mark: Mark::Italic }]);
        assert!(e.marks().is_empty(), "set_marks can't add one either");
        e.set_blocks(vec![Block::Code]);
        assert_eq!(e.blocks(), &[Block::Paragraph], "set_blocks can't add one either");

        // Undo/redo of an edit recorded before allow_marks/allow_blocks was set, or a pending
        // mark armed before allow_marks, cannot smuggle a disallowed mark or block back in.
        let mut pre = TextEdit::new("ab").rich();
        pre.set_selection(0, 2);
        pre.set_mark(Mark::Underline);
        pre.set_block(Block::Code);
        pre.set_caret(2);
        pre.insert("!");
        pre.toggle_mark(Mark::Italic);
        let mut pre = pre.allow_marks([Mark::Bold]).allow_blocks([Block::Quote]);
        assert!(!pre.has_mark(&Mark::Italic), "pending disallowed mark is stripped");
        undo(&mut pre, false);
        assert!(pre.marks().is_empty() && pre.blocks() == &[Block::Paragraph], "undo filters snapshot");
        undo(&mut pre, true);
        assert!(pre.marks().is_empty() && pre.blocks() == &[Block::Paragraph], "redo filters snapshot");

        // Pills are the app's tokens, kept whatever the allow-list says.
        let mut p = TextEdit::new("hi @maya").rich().allow_marks([Mark::Bold]);
        assert!(p.allows_mark(&Mark::Pill(String::new())));
        p.replace_with_pill(3..8, "user:maya", "@maya");
        assert_eq!(p.pills().count(), 1, "replace_with_pill works under an allow-list");
        let p = p.allow_marks(Vec::<Mark>::new());
        assert_eq!(p.pills().count(), 1, "an allow-list set later keeps existing pills");

        let mut e = TextEdit::new("").rich().allow_marks([Mark::Bold]).allow_blocks([Block::Quote]);
        e.paste_html("<ul><li><b>b</b> <i>i</i> <u>u</u></li></ul><blockquote>q</blockquote><pre>c</pre>", "");
        assert_eq!(e.text, "b i u\nq\nc");
        assert_eq!(e.marks().iter().map(|m| &m.mark).collect::<Vec<_>>(), [&Mark::Bold]);
        assert_eq!(e.blocks(), &[Block::Paragraph, Block::Quote, Block::Paragraph]);
        assert_invariants(&e, "filtered paste");
        // HTML whose only formatting the field drops pastes as plain text, keeping its spacing.
        let mut e = TextEdit::new("").rich().allow_marks([Mark::Bold]);
        e.paste_html("<p><i>a  b</i></p>", "a  b");
        assert_eq!(e.text, "a  b");
    }

    #[test]
    fn flatten_spans_matches_the_naive_merge_and_later_spans_win() {
        use crate::text::{flatten_spans, Span};
        use crate::Color;
        fn naive(spans: &[Span]) -> Vec<Span> {
            let mut bounds: Vec<u32> = spans.iter().filter(|s| s.start < s.end).flat_map(|s| [s.start, s.end]).collect();
            bounds.sort_unstable();
            bounds.dedup();
            let mut out: Vec<Span> = Vec::new();
            for w in bounds.windows(2) {
                let mut m: Option<Span> = None;
                for s in spans.iter().filter(|s| s.start < s.end && s.start <= w[0] && w[1] <= s.end) {
                    let m = m.get_or_insert_with(|| Span::new(w[0] as usize, w[1] as usize));
                    if s.weight.is_some() { m.weight = s.weight; }
                    if s.color.is_some() { m.color = s.color; }
                    if s.bg.is_some() { m.bg = s.bg; m.bg_pad = s.bg_pad; }
                    if s.underline.is_some() { m.underline = s.underline; }
                    if s.size.is_some() { m.size = s.size; }
                    m.mono |= s.mono;
                    m.italic |= s.italic;
                }
                if let Some(m) = m {
                    match out.last_mut() {
                        Some(last) if last.end == m.start && Span { start: m.start, end: m.end, ..last.clone() } == m => last.end = m.end,
                        _ => out.push(m),
                    }
                }
            }
            out
        }
        let red = Color::WHITE;
        let blue = Color::BLACK;
        assert_eq!(
            flatten_spans(&[Span::new(2, 5).color(blue), Span::new(0, 10).color(red)]),
            [Span::new(0, 10).color(red)],
            "a later span wins even where it starts first"
        );
        assert_eq!(
            flatten_spans(&[Span::new(0, 10).color(red), Span::new(2, 5).color(blue).bold()]),
            [Span::new(0, 2).color(red), Span::new(2, 5).color(blue).bold(), Span::new(5, 10).color(red)]
        );
        let mut seed = 7u64;
        let mut next = |n: u64| { seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); (seed >> 33) % n };
        for _ in 0..300 {
            let spans: Vec<Span> = (0..next(12)).map(|_| {
                let a = next(40) as usize;
                let mut s = Span::new(a, a + next(15) as usize);
                match next(5) {
                    0 => s = s.color(if next(2) == 0 { red } else { blue }),
                    1 => s = s.bold(),
                    2 => s = s.italic(),
                    3 => s = s.bg(red, next(3) as f32),
                    _ => s = s.mono().underline(blue),
                }
                s
            }).collect();
            assert_eq!(flatten_spans(&spans), naive(&spans), "{spans:?}");
        }
    }

    #[test]
    fn html_parsing_is_linear_on_hostile_input() {
        let n = 200_000;
        let cases = [
            "<b>".repeat(n) + "x",
            "<script>".to_owned() + &"</script".repeat(n),
            "<p>".to_owned() + &"&".repeat(n * 4),
            "<div><span>".repeat(n / 2) + &"</div>".repeat(n / 2),
            "</b>".repeat(n),
            "<ul><li>".repeat(n / 2) + "x",
            "a<br>".repeat(n),
            "<a href=\"".to_owned() + &"&#x6a;".repeat(n) + "\">x</a>",
        ];
        for html in &cases {
            let t = std::time::Instant::now();
            let e = TextEdit::from_html(html);
            assert_invariants(&e, &html[..20]);
            let _ = e.to_html();
            // Debug builds parse these in well under a second; a quadratic path takes minutes.
            assert!(t.elapsed().as_secs() < 10, "{} took {:?}", &html[..20], t.elapsed());
        }
    }

    #[test]
    fn html_links_keep_only_safe_schemes() {
        for bad in [
            "javascript:alert(1)", "JaVaScRiPt:alert(1)", " javascript:x", "java\tscript:x", "java\nscript:x",
            "java&#x09;script:x", "&#106;avascript:x", "javascript&#58;x", "javascript&colon;x", "java&Tab;script:x",
            "jav&#x0A;ascript:x", "\u{1}javascript:x", "vbscript:x", "data:text/html,x", "file:///etc/passwd",
            "blob:https://a.b/x", "x-app-scheme:do",
        ] {
            let e = TextEdit::from_html(&format!("<a href=\"{bad}\">x</a><a href='{bad}'>y</a>"));
            assert!(e.marks().iter().all(|m| !matches!(m.mark, Mark::Link(_))), "{bad:?} kept: {:?}", e.marks());
            assert_eq!(e.text, "xy");
        }
        for (good, url) in [
            ("https://a.b/c?d=1&amp;e=2", "https://a.b/c?d=1&e=2"),
            ("HTTP://A.B", "HTTP://A.B"),
            ("mailto:a@b.c", "mailto:a@b.c"),
            ("tel:+1555", "tel:+1555"),
            ("/relative/path", "/relative/path"),
            ("#frag", "#frag"),
        ] {
            let e = TextEdit::from_html(&format!("<a href=\"{good}\">x</a>"));
            assert_eq!(e.marks(), &[MarkRange::new(0..1, Mark::Link(url.into()))], "{good}");
        }
        let e = TextEdit::from_html("a&#0;b&#x1;c<pre>d&#0;\te</pre>");
        assert_eq!(e.text, "abc\nd\te", "entities never decode to control characters");
    }

    #[test]
    fn html_from_browsers_notes_slack_and_docs_round_trips() {
        let round_trip = |e: &TextEdit, ctx: &str| {
            let back = TextEdit::from_html(&e.to_html());
            assert_eq!(
                (&back.text, back.marks(), back.blocks(), back.soft_breaks()),
                (&e.text, e.marks(), e.blocks(), e.soft_breaks()),
                "{ctx} round trip"
            );
            assert_invariants(e, ctx);
        };

        let chrome = TextEdit::from_html(concat!(
            "<meta charset='utf-8'><span style=\"color: rgb(0, 0, 0); font-family: Arial; font-weight: 700;\">Hello</span>",
            "<span style=\"color: rgb(0, 0, 0);\"> world</span>",
        ));
        assert_eq!(chrome.text, "Hello world");
        assert_eq!(marked(&chrome, &Mark::Bold), ["Hello"]);
        round_trip(&chrome, "browser");

        let notes = TextEdit::from_html(concat!(
            "<!DOCTYPE html PUBLIC \"-//W3C//DTD HTML 4.01//EN\">\n<html>\n<head>\n",
            "<meta http-equiv=\"Content-Type\" content=\"text/html; charset=UTF-8\">\n",
            "<style type=\"text/css\">\np.p1 {margin: 0.0px 0.0px 0.0px 0.0px}\n</style>\n</head>\n<body>\n",
            "<p class=\"p1\"><b>Bold</b> text</p>\n<p class=\"p1\"><br></p>\n",
            "<ul class=\"ul1\">\n<li class=\"li1\">item one</li>\n<li class=\"li1\"><i>item two</i></li>\n</ul>\n",
            "</body>\n</html>\n",
        ));
        assert_eq!(notes.text, "Bold text\n\nitem one\nitem two");
        assert_eq!(notes.blocks(), &[Block::Paragraph, Block::Paragraph, Block::Bullet(0), Block::Bullet(0)]);
        assert_eq!((marked(&notes, &Mark::Bold), marked(&notes, &Mark::Italic)), (vec!["Bold"], vec!["item two"]));
        round_trip(&notes, "notes");

        let slack = TextEdit::from_html(concat!(
            "<meta charset='utf-8'><b data-stringify-type=\"bold\" style=\"color: rgb(29, 28, 29);\">bold</b>",
            "<span style=\"color: rgb(29, 28, 29);\"> and </span><i data-stringify-type=\"italic\">it</i><span> </span>",
            "<code data-stringify-type=\"code\" class=\"c-mrkdwn__code\">x()</code><br style=\"color: rgb(29, 28, 29);\">",
            "<a target=\"_blank\" class=\"c-link\" data-stringify-link=\"https://a.b/c?d=1&amp;e=2\" ",
            "href=\"https://a.b/c?d=1&amp;e=2\" rel=\"noopener noreferrer\">link</a>",
        ));
        assert_eq!(slack.text, "bold and it x()\nlink");
        assert_eq!(slack.soft_breaks(), &[1]);
        assert_eq!(marked(&slack, &Mark::Code), ["x()"]);
        assert_eq!(marked(&slack, &Mark::Link("https://a.b/c?d=1&e=2".into())), ["link"]);
        round_trip(&slack, "slack");

        let docs = TextEdit::from_html(concat!(
            "<meta charset=\"utf-8\"><b style=\"font-weight:normal;\" id=\"docs-internal-guid-1\">",
            "<p dir=\"ltr\" style=\"line-height:1.38;margin-top:0pt;\"><span style=\"font-size:11pt;font-weight:700;\">Bold</span>",
            "<span style=\"font-size:11pt;font-weight:400;\"> plain</span></p>",
            "<ul style=\"margin-top:0;\"><li dir=\"ltr\" style=\"list-style-type:disc;\"><p dir=\"ltr\" role=\"presentation\">",
            "<span style=\"font-style:italic;\">item</span></p></li></ul></b>",
        ));
        assert_eq!(docs.text, "Bold plain\nitem");
        assert_eq!(docs.blocks(), &[Block::Paragraph, Block::Bullet(0)]);
        assert_eq!((marked(&docs, &Mark::Bold), marked(&docs, &Mark::Italic)), (vec!["Bold"], vec!["item"]));
        round_trip(&docs, "google docs");

        let loose = TextEdit::from_html("<p>A</p>B<p>C<br></p>");
        assert_eq!(loose.text, "A\nB\nC", "loose text gets its own line; a trailing <br> draws nothing");
        round_trip(&loose, "loose");
    }

    #[test]
    fn random_edits_keep_ranges_blocks_and_soft_breaks_consistent() {
        const PIECES: [&str; 10] = ["a", "b ", "é", "🦀", "e\u{301}", "👩\u{200d}👩\u{200d}👧", "日本", "\n", "\t", " "];
        for seed in 0..40u64 {
            let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
            let mut next = |n: u64| { state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); (state >> 33) % n };
            let mut e = TextEdit::from_html("<p>start <b>bold</b></p><ul><li>item</li></ul>");
            let (mut clip, mut clip_html) = (String::new(), String::new());
            for step in 0..300 {
                let op = next(22);
                let mods = Mods { shift: next(3) == 0, alt: next(4) == 0, ..Mods::default() };
                match op {
                    0..=3 => { let p = PIECES[next(PIECES.len() as u64) as usize]; e.key(&k(Key::Text(p.into())), None); }
                    4 => { e.key(&KeyEvent { key: Key::Enter, mods }, None); }
                    5 => { e.key(&KeyEvent { key: Key::Backspace, mods }, None); }
                    6 => { e.key(&KeyEvent { key: Key::Delete, mods }, None); }
                    7 => { e.key(&KeyEvent { key: if next(2) == 0 { Key::Left } else { Key::Right }, mods }, None); }
                    8 => { e.key(&KeyEvent { key: if next(2) == 0 { Key::Home } else { Key::End }, mods }, None); }
                    9 => { e.key(&KeyEvent { key: Key::Tab, mods: Mods { shift: mods.shift, ..Mods::default() } }, None); }
                    10 => { e.key(&cmd(['b', 'i', 'u'][next(3) as usize]), None); }
                    11 => e.toggle_mark([Mark::Code, Mark::Strike, Mark::Link(format!("https://e.x/{}", next(3)))][next(3) as usize].clone()),
                    12 => {
                        let l = next(6) as u8;
                        e.set_block([Block::Paragraph, Block::Bullet(l), Block::Numbered(l), Block::Quote, Block::Code][next(5) as usize]);
                    }
                    13 => undo(&mut e, next(2) == 0),
                    14 => {
                        clip_html = e.selected_html();
                        if let Some(c) = e.key(&cmd(if next(2) == 0 { 'x' } else { 'c' }), None).copy { clip = c; }
                    }
                    15 => { e.paste(&clip.clone()); }
                    16 => { e.paste_html(&clip_html.clone(), &clip.clone()); }
                    17 => {
                        let p = PIECES[next(6) as usize];
                        e.ime(&Ime::Preedit(p.into(), None));
                        if next(2) == 0 { e.ime(&Ime::Commit(p.into())); }
                    }
                    18 => { let a = next(e.text.len() as u64 + 2) as usize; let f = next(e.text.len() as u64 + 2) as usize; e.set_selection(a, f); }
                    19 => {
                        // An app writes `text` and the caret directly, then the user types.
                        if next(2) == 0 { let cut = next(e.text.len() as u64 + 1) as usize; let cut = TextEdit::clamp_char_boundary(&e.text, cut); e.text.truncate(cut); }
                        else { e.text.push_str(PIECES[next(PIECES.len() as u64) as usize]); }
                        e.anchor = next(e.text.len() as u64 + 3) as usize;
                        e.focus = e.anchor;
                        e.key(&k(Key::Text("z".into())), None);
                    }
                    20 => { e.key(&cmd('a'), None); }
                    _ => { e.key(&KeyEvent { key: if next(2) == 0 { Key::Up } else { Key::Down }, mods }, None); }
                }
                assert_invariants(&e, &format!("seed {seed} step {step} op {op} text {:?}", e.text));
                let _ = (e.to_html(), e.display(), e.paragraphs());
            }
        }
    }

    #[test]
    fn copy_from_one_rich_field_pastes_marks_and_blocks_into_another() {
        use crate::{Cx, El, Env, Runtime, TextStyle, col, text_input};
        struct App { a: TextEdit, b: TextEdit, plain: TextEdit }
        fn view(s: &App, _: &mut Cx<App>) -> El {
            let st = TextStyle::new(14.0, 400.0);
            col().size(320.0, 360.0)
                .child(text_input(&s.a, st).key("a").size(300.0, 140.0).on_edit(|s: &mut App| &mut s.a))
                .child(text_input(&s.b, st).key("b").size(300.0, 140.0).on_edit(|s: &mut App| &mut s.b))
                .child(text_input(&s.plain, st).key("plain").size(300.0, 40.0).on_edit(|s: &mut App| &mut s.plain))
        }
        let mut a = TextEdit::from_html(
            "<p>Hello <b>bold</b> <a href=\"https://example.com\">link</a></p><ul><li>one</li><li><i>two</i></li></ul><blockquote>q</blockquote>",
        );
        a.focused = true;
        a.set_selection(0, a.text.len());
        let m = crate::clipboard::memory();
        let mut rt = Runtime::new(App { a, b: TextEdit::new("").rich(), plain: TextEdit::new("") }, view, (320.0, 360.0), Env::new(true, 1.0));
        rt.use_clipboard(&m);
        rt.frame(0.0);
        rt.key(cmd('c'));
        rt.frame(0.0);
        let copied = m.content().unwrap();
        assert_eq!(copied.text(), rt.state.a.text);
        assert!(copied.html().is_some_and(|h| h.contains("<strong>bold</strong>") && h.contains("<ul>")), "{copied:?}");

        rt.update(|s, _| { s.a.focused = false; s.b.focused = true; });
        rt.frame(0.0);
        rt.update(|_, tx| tx.request_paste());
        rt.frame(0.0);
        let (a, b) = (&rt.state.a, &rt.state.b);
        assert_eq!((&b.text, b.marks(), b.blocks()), (&a.text, a.marks(), a.blocks()));

        // Cut carries the formatting it removes.
        rt.update(|s, _| s.b.set_selection(6, 10));
        rt.key(cmd('x'));
        rt.frame(0.0);
        assert_eq!(rt.state.b.text, format!("Hello  link{}", &rt.state.a.text[15..]));
        assert!(m.content().unwrap().html().is_some_and(|h| h.contains("<strong>bold</strong>")));

        // A plain field pastes the text only and copies no HTML.
        rt.update(|s, _| { s.b.focused = false; s.plain.focused = true; });
        rt.frame(0.0);
        rt.update(|_, tx| tx.request_paste());
        rt.frame(0.0);
        assert_eq!(rt.state.plain.text, "bold");
        assert!(rt.state.plain.is_plain());
        rt.update(|s, _| s.plain.set_selection(0, 4));
        rt.key(cmd('c'));
        rt.frame(0.0);
        assert_eq!(m.content().unwrap().html(), None);
    }

    /// `"a @x-agent b"` with the label a pill carrying `agent:x`, the caret after it.
    fn with_pill() -> TextEdit {
        let mut e = TextEdit::new("a  b").rich();
        e.set_caret(2);
        e.insert_pill("agent:x", "@x-agent");
        e
    }

    fn pills_of(e: &TextEdit) -> Vec<(Range<usize>, String)> {
        e.pills().map(|(r, id)| (r, id.to_owned())).collect()
    }

    #[test]
    fn a_pill_replaces_a_typed_query_and_lists_with_its_id() {
        let mut e = TextEdit::new("").rich();
        e.insert("see @ma");
        let at = e.text.find('@').unwrap();
        e.replace_with_pill(at..e.text.len(), "file:src/main.rs", "@main.rs\n");
        assert_eq!(e.text, "see @main.rs ", "line breaks in a label become spaces");
        assert_eq!(pills_of(&e), [(4..13, "file:src/main.rs".to_owned())]);
        assert_eq!(e.range(), (13, 13));
        e.key(&k(Key::Text("x".into())), None);
        assert_eq!(pills_of(&e), [(4..13, "file:src/main.rs".to_owned())], "typing after a pill never extends it");
        e.set_caret(4);
        e.key(&k(Key::Text("y".into())), None);
        assert_eq!((e.text.as_str(), pills_of(&e)), ("see y@main.rs x", vec![(5..14, "file:src/main.rs".to_owned())]));
        let mut secure = TextEdit::new("").secure();
        secure.insert_pill("a", "@a");
        assert_eq!((secure.text.as_str(), secure.pills().count()), ("", 0));
    }

    #[test]
    fn the_caret_steps_over_a_pill_in_one_move() {
        let mut e = with_pill();
        assert_eq!((e.text.as_str(), e.range()), ("a @x-agent b", (10, 10)));
        e.key(&k(Key::Left), None);
        assert_eq!(e.range(), (2, 2));
        e.key(&k(Key::Left), None);
        assert_eq!(e.range(), (1, 1));
        e.key(&k(Key::Right), None);
        e.key(&k(Key::Right), None);
        assert_eq!(e.range(), (10, 10));
        e.set_caret(2);
        e.key(&KeyEvent { key: Key::Right, mods: Mods { alt: true, ..Mods::default() } }, None);
        assert_eq!(e.range(), (10, 10), "a word move that stops inside the label steps over it");
        e.set_caret(2);
        e.key(&KeyEvent { key: Key::Right, mods: Mods { shift: true, ..Mods::default() } }, None);
        assert_eq!((e.anchor, e.focus), (2, 10), "Shift+Right selects the whole pill");
        e.set_caret(5);
        assert_eq!(e.range(), (2, 2), "a click inside goes to the nearer edge");
        e.set_caret(8);
        assert_eq!(e.range(), (10, 10));
        // A caret written straight into a pill moves out before the next edit.
        e.anchor = 6;
        e.focus = 6;
        e.insert("Z");
        assert_eq!((e.text.as_str(), pills_of(&e)), ("a @x-agentZ b", vec![(2..10, "agent:x".to_owned())]));
    }

    #[test]
    fn an_undo_group_is_one_step_with_its_selection_and_nests() {
        let mut e = TextEdit::new("hi @ag").rich();
        e.set_caret(6);
        e.undo_group(|e| {
            e.replace_with_pill(3..6, "agent:1", "@agent");
            e.undo_group(|e| e.insert(" "));
        });
        assert_eq!((e.text.as_str(), e.range(), e.pills().count()), ("hi @agent ", (10, 10), 1));
        undo(&mut e, false);
        assert_eq!((e.text.as_str(), e.range(), e.pills().count()), ("hi @ag", (6, 6), 0));
        undo(&mut e, true);
        assert_eq!((e.text.as_str(), e.range(), pills_of(&e)), ("hi @agent ", (10, 10), vec![(3..9, "agent:1".to_owned())]));
        undo(&mut e, false);
        assert_eq!(e.text, "hi @ag");
        assert!(e.history.undo.is_empty(), "the group was a single step");
    }

    #[test]
    fn undo_groups_keep_typing_coalescing_and_record_nothing_when_empty() {
        let mut e = TextEdit::default();
        e.key(&k(Key::Text("a".into())), None);
        e.key(&k(Key::Text("b".into())), None);
        e.undo_group(|_| {});
        e.undo_group(|e| e.set_caret(2));
        assert_eq!(e.history.undo.len(), 1, "typing coalesces and empty groups add no step");
        e.key(&k(Key::Text("c".into())), None);
        e.undo_group(|e| { e.insert("X"); e.insert("Y"); });
        e.key(&k(Key::Text("d".into())), None);
        let steps = e.history.undo.len();
        undo(&mut e, false);
        undo(&mut e, false);
        assert_eq!(e.history.undo.len(), steps - 2, "typing after a group is its own step");
        assert_eq!(e.text, "abc");
    }

    #[test]
    fn an_undo_group_leaves_ime_composition_alone() {
        let mut e = TextEdit::new("x").rich();
        e.set_caret(1);
        e.ime(&Ime::Preedit("に".into(), None));
        e.undo_group(|e| e.ime(&Ime::Commit("日本".into())));
        assert_eq!((e.text.as_str(), e.preedit.is_none()), ("x日本", true));
        undo(&mut e, false);
        assert_eq!(e.text, "x");
        e.ime(&Ime::Preedit("a".into(), None));
        e.undo_group(|e| e.undo());
        assert!(e.preedit.is_some(), "undo inside a group does nothing");
    }

    #[test]
    fn backspace_and_delete_take_a_pill_whole_and_undo_restores_its_id() {
        let mut e = with_pill();
        e.key(&k(Key::Backspace), None);
        assert_eq!((e.text.as_str(), e.range(), e.pills().count()), ("a  b", (2, 2), 0));
        undo(&mut e, false);
        assert_eq!((e.text.as_str(), pills_of(&e)), ("a @x-agent b", vec![(2..10, "agent:x".to_owned())]));
        e.set_caret(2);
        e.key(&k(Key::Delete), None);
        assert_eq!((e.text.as_str(), e.pills().count()), ("a  b", 0));
        undo(&mut e, false);
        e.set_caret(11);
        e.key(&k(Key::Backspace), None);
        assert_eq!((e.text.as_str(), pills_of(&e)), ("a @x-agentb", vec![(2..10, "agent:x".to_owned())]));
        e.set_caret(10);
        e.key(&KeyEvent { key: Key::Backspace, mods: Mods { alt: true, ..Mods::default() } }, None);
        assert_eq!((e.text.as_str(), e.pills().count()), ("a b", 0), "a word delete into a pill takes it whole");
        undo(&mut e, false);
        undo(&mut e, true);
        undo(&mut e, false);
        assert_eq!(pills_of(&e), [(2..10, "agent:x".to_owned())]);
    }

    #[test]
    fn a_selection_into_a_pill_covers_it() {
        let mut e = with_pill();
        e.set_selection(0, 5);
        assert_eq!((e.anchor, e.focus), (0, 10));
        e.set_selection(7, 12);
        assert_eq!((e.anchor, e.focus), (2, 12));
        e.set_selection(11, 4);
        assert_eq!((e.anchor, e.focus), (11, 2));
        e.set_selection(0, 5);
        e.insert("z");
        assert_eq!((e.text.as_str(), e.pills().count()), ("z b", 0));
        // Formatting a pill restyles its label and keeps it whole.
        let mut e = with_pill();
        e.set_selection(0, 6);
        e.toggle_mark(Mark::Bold);
        assert_eq!(pills_of(&e), [(2..10, "agent:x".to_owned())]);
        assert!(e.marks().iter().any(|m| m.mark == Mark::Bold && m.range() == (0..10)));
    }

    #[test]
    fn a_composition_never_splits_a_pill() {
        let mut e = with_pill();
        e.anchor = 4;
        e.focus = 4;
        e.ime(&Ime::Preedit("に".into(), None));
        let (shown, pre) = e.display();
        let (pa, _) = pre.unwrap();
        assert!(pa <= 2 || pa >= 10, "preedit at {pa} in {shown:?}");
        assert_eq!(e.display_pills().len(), 1);
        e.ime(&Ime::Commit("日本".into()));
        assert_eq!(pills_of(&e).len(), 1);
        assert_eq!(&e.text[pills_of(&e)[0].0.clone()], "@x-agent");
        assert!(e.text == "a 日本@x-agent b" || e.text == "a @x-agent日本 b", "{}", e.text);
    }

    #[test]
    fn pills_round_trip_through_html_and_outside_html_makes_none() {
        let mut e = with_pill();
        e.set_selection(0, 5);
        e.toggle_mark(Mark::Italic);
        e.set_selection(0, e.text.len());
        let html = e.selected_html();
        assert!(html.contains("<span data-pill=\"agent:x\">"), "{html}");
        assert_eq!(e.selected(), "a @x-agent b", "the plain copy is the label");
        let mut other = TextEdit::new("").rich();
        other.paste_html_with_pills(&html, e.selected());
        assert_eq!((other.text.as_str(), pills_of(&other)), ("a @x-agent b", vec![(2..10, "agent:x".to_owned())]));
        let mut outside = TextEdit::new("").rich();
        outside.paste_html(&html, e.selected());
        assert_eq!((outside.text.as_str(), outside.pills().count()), ("a @x-agent b", 0));
        assert!(outside.marks().iter().any(|m| m.mark == Mark::Italic), "the rest of the formatting stays");
        // A pill split by a mark's edge still serializes as one span.
        let one = TextEdit::from_html("<p><span data-pill=\"f&quot;1\">@f<b>il</b>e</span></p>");
        assert_eq!(pills_of(&one), [(0..5, "f\"1".to_owned())]);
        assert!(one.to_html().contains("<span data-pill=\"f&quot;1\">@f<strong>il</strong>e</span>"), "{}", one.to_html());
        // A pill can't span lines.
        let broken = TextEdit::from_parts("a\nb c", vec![MarkRange::new(0..3, Mark::Pill("x".into())), MarkRange::new(4..5, Mark::Pill("c".into()))], Vec::new());
        assert_eq!(pills_of(&broken), [(4..5, "c".to_owned())]);
    }

    #[test]
    fn a_pill_copied_from_one_rich_field_pastes_whole_into_another() {
        use crate::{ClipboardContent, Cx, El, Env, Runtime, TextStyle, col, text_input};
        struct App { a: TextEdit, b: TextEdit, hooked: TextEdit }
        fn view(s: &App, _: &mut Cx<App>) -> El {
            let st = TextStyle::new(14.0, 400.0);
            col().size(320.0, 360.0)
                .child(text_input(&s.a, st).key("a").size(300.0, 100.0).on_edit(|s: &mut App| &mut s.a))
                .child(text_input(&s.b, st).key("b").size(300.0, 100.0).on_edit(|s: &mut App| &mut s.b))
                .child(text_input(&s.hooked, st).key("hooked").size(300.0, 100.0).on_edit(|s: &mut App| &mut s.hooked)
                    .on_input_paste(|s: &mut App, content, _| {
                        let Some(html) = content.html() else { return false };
                        s.hooked.paste_html_with_pills(html, content.text());
                        true
                    }))
        }
        let mut a = with_pill();
        a.focused = true;
        a.set_selection(0, a.text.len());
        let m = crate::clipboard::memory();
        let mut rt = Runtime::new(App { a, b: TextEdit::new("").rich(), hooked: TextEdit::new("").rich() }, view, (320.0, 360.0), Env::new(true, 1.0));
        rt.use_clipboard(&m);
        rt.frame(0.0);
        rt.key(cmd('c'));
        rt.frame(0.0);
        let copied = m.content().unwrap();
        assert_eq!(copied.text(), "a @x-agent b");
        assert!(copied.html().is_some_and(|h| h.contains("<span data-pill=\"agent:x\">@x-agent</span>")), "{copied:?}");
        let paste_into = |rt: &mut Runtime<App>, which: fn(&mut App) -> &mut TextEdit| {
            rt.update(move |s, _| { s.a.focused = false; s.b.focused = false; s.hooked.focused = false; which(s).focused = true; });
            rt.frame(0.0);
            rt.update(|_, tx| tx.request_paste());
            rt.frame(0.0);
        };
        paste_into(&mut rt, |s| &mut s.b);
        assert_eq!(pills_of(&rt.state.b), [(2..10, "agent:x".to_owned())], "its own copy pastes the pill back");

        // The same HTML from outside (another app's copy) pastes plain labels, even with identical plain text, unless a hook keeps them.
        m.set_content(ClipboardContent::new("a @x-agent b").with_html("a <span data-pill=\"agent:x\">@x-agent</span> b"));
        rt.update(|s, _| s.b.set_selection(0, s.b.text.len()));
        paste_into(&mut rt, |s| &mut s.b);
        assert_eq!((rt.state.b.text.as_str(), rt.state.b.pills().count()), ("a @x-agent b", 0));
        m.set_content(ClipboardContent::new("x @x-agent").with_html("x <span data-pill=\"agent:x\">@x-agent</span>"));
        rt.update(|s, _| s.b.set_selection(0, s.b.text.len()));
        paste_into(&mut rt, |s| &mut s.b);
        assert_eq!((rt.state.b.text.as_str(), rt.state.b.pills().count()), ("x @x-agent", 0));
        paste_into(&mut rt, |s| &mut s.hooked);
        assert_eq!(pills_of(&rt.state.hooked), [(2..10, "agent:x".to_owned())]);
    }

    #[test]
    fn pill_edge_cases_words_lines_bidi_and_direct_text_mutation() {
        // Touching pills, punctuation after a pill, and spaces inside a pill label.
        let mut e = TextEdit::new("").rich();
        e.insert_pill("u:1", "@maya");
        e.insert_pill("u:2", "@noah");
        e.insert(", ");
        e.insert_pill("u:3", "@Maya Lin");
        assert_eq!(e.text, "@maya@noah, @Maya Lin");
        let alt_left = KeyEvent { key: Key::Left, mods: Mods { alt: true, ..Mods::default() } };
        let alt_right = KeyEvent { key: Key::Right, mods: Mods { alt: true, ..Mods::default() } };
        let alt_bs = KeyEvent { key: Key::Backspace, mods: Mods { alt: true, ..Mods::default() } };
        e.key(&alt_left, None);
        assert_eq!(e.range(), (12, 12), "steps over @Maya Lin as one word");
        e.key(&alt_left, None);
        assert_eq!(e.range(), (10, 10), "stops between @noah and comma");
        e.key(&alt_left, None);
        assert_eq!(e.range(), (5, 5), "steps over @noah without eating @maya");
        e.key(&alt_left, None);
        assert_eq!(e.range(), (0, 0), "steps over @maya");
        e.key(&alt_right, None);
        assert_eq!(e.range(), (5, 5));
        e.key(&alt_right, None);
        assert_eq!(e.range(), (10, 10));
        e.key(&alt_bs, None);
        assert_eq!((e.text.as_str(), pills_of(&e)), ("@maya, @Maya Lin", vec![(0..5, "u:1".to_owned()), (7..16, "u:3".to_owned())]));

        // Vertical movement and line_edge across wrapped lines ending or starting with pills.
        let mut ts = crate::text::TextSystem::default();
        let style = crate::tok::TextStyle::new(14.0, 400.0);
        let mut w = TextEdit::new("hello world ").rich();
        w.set_caret(w.text.len());
        w.insert_pill("p:1", "@first-pill");
        w.insert(" ");
        w.insert_pill("p:2", "@second-pill");
        w.insert(" tail");
        let chips: Vec<crate::text::PillChip> = w
            .display_pills()
            .into_iter()
            .map(|(start, end, _)| crate::text::PillChip { start, end, icon: None, bg: None, fg: None })
            .collect();
        let layout = ts.layout_blocks(&w.text, &[], w.blocks(), &chips, style, 2.0, Some(220.0), parley::Alignment::Start);
        assert!(layout.len() >= 3, "wrapped into {} lines", layout.len());
        // Caret at end of line 0 ("hello world "), press Down onto the pill line: snaps to a pill edge on line 1, never jumps to EOF.
        w.set_caret(11);
        w.key(&k(Key::Down), Some(&layout));
        assert!(w.focus < w.text.len(), "Down onto a pill line stays on that line: {}", w.focus);
        assert!(w.pill_around(w.focus).is_none(), "caret never lands inside a pill: {}", w.focus);
        // Shift+Down across a pill line covers the pill whole.
        w.set_caret(0);
        w.key(&KeyEvent { key: Key::Down, mods: Mods { shift: true, ..Mods::default() } }, Some(&layout));
        assert!(w.pill_around(w.focus).is_none());

        // IME preedit right after a pill does not bleed its underline box into the pill's chip.
        let mut ime_e = with_pill();
        ime_e.set_caret(10);
        ime_e.ime(&Ime::Preedit("に".into(), None));
        let (disp, pre) = ime_e.display();
        let (pa, pb) = pre.unwrap();
        let p_chips: Vec<crate::text::PillChip> = ime_e
            .display_pills()
            .into_iter()
            .map(|(start, end, _)| crate::text::PillChip { start, end, icon: None, bg: None, fg: None })
            .collect();
        let ime_layout = ts.layout_blocks(&disp, &[], &[Block::Paragraph], &p_chips, style, 2.0, None, parley::Alignment::Start);
        let (mut decos, mut icons) = (Vec::new(), Vec::new());
        crate::text::pill_chips(&ime_layout, &p_chips, 14.0, 2.0, [0.0, 0.0], Color::BLACK, &mut decos, &mut icons);
        let boxes = crate::text::range_boxes(&ime_layout, pa, pb, false);
        assert_eq!(boxes.len(), 1);
        assert!(boxes[0].0.x0 as f32 >= decos[0].rect[0] + decos[0].rect[2] - 0.5, "preedit box {:?} overlaps chip {:?}", boxes[0].0, decos[0].rect);

        // RTL pill and mixed RTL+LTR pill chips have positive width.
        for (txt, start, end) in [("דוד", 0u32, 6u32), ("שלום@maya", 8, 13), ("@e\u{0301}", 0, 4)] {
            let pc = [crate::text::PillChip { start, end, icon: None, bg: None, fg: None }];
            let l = ts.layout_blocks(txt, &[], &[Block::Paragraph], &pc, style, 2.0, None, parley::Alignment::Start);
            let (mut d, mut ic) = (Vec::new(), Vec::new());
            crate::text::pill_chips(&l, &pc, 14.0, 2.0, [0.0, 0.0], Color::BLACK, &mut d, &mut ic);
            assert_eq!(d.len(), 1, "{txt:?}");
            assert!(d[0].rect[2] > 8.0, "chip for {txt:?} has width {}", d[0].rect[2]);
        }

        // Direct text assignment on a field holding pills never panics or exposes invalid pill ranges.
        let mut direct = with_pill();
        direct.text = "é".into();
        assert_eq!(direct.pills().count(), 0);
        assert!(direct.display_pills().is_empty());
        direct.set_caret(99);
        assert_eq!(direct.range(), (2, 2));

        // Mark::Pill at collapsed caret does not arm pending; set_mark replaces an existing pill; plain insert strips a pill.
        let mut m = with_pill();
        m.set_caret(0);
        m.toggle_mark(Mark::Pill("x".into()));
        m.set_mark(Mark::Pill("x".into()));
        assert!(m.pending.is_none());
        m.set_selection(2, 10);
        m.set_mark(Mark::Pill("agent:y".into()));
        assert_eq!(pills_of(&m), [(2..10, "agent:y".to_owned())]);
        m.set_selection(2, 10);
        m.insert("@x-agent");
        assert_eq!((m.text.as_str(), m.pills().count()), ("a @x-agent b", 0));
    }
}

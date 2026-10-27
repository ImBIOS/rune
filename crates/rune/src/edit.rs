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
mod ime;
pub use ime::Ime;
mod keys;
pub use keys::{Key, KeyEvent, KeySide, Modifier, Mods};

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
    /// chip ([`El::pill_style`](crate::El::pill_style), [`El::pill_colors`](crate::El::pill_colors),
    /// [`El::pill_icon`](crate::El::pill_icon)). Insert one with [`TextEdit::insert_pill`].
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


/// Fixed mask shown by a hidden multi-line secure field when non-empty, hiding both byte length
/// and line count.
pub(crate) const MULTILINE_SECURE_MASK: &str = "••••••••••••";

/// Mirrors whether the bound [`text_input`](crate::text_input) has [`El::single_line`](crate::El::single_line).
/// Before a field mounts, standalone [`TextEdit::secure`] defaults to single-line.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum LineMode {
    #[default]
    Default,
    Single,
    Multi,
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
    group: ActiveGroup,
    secure: bool,
    revealed: bool,
    /// Mirrors the field's `El::single_line()`; the runtime sets it before each edit.
    line_mode: LineMode,
    /// Formatting keys and rich paste are on (see [`TextEdit::rich`]).
    rich: bool,
    /// The mark kinds this field keeps, one bit per [`Mark::kind_order`]; `None` keeps all.
    allowed_marks: Option<u16>,
    /// The block kinds this field keeps ([`Block::kind_bit`]); `None` keeps all.
    allowed_blocks: Option<u8>,
    /// Whether a user has edited the value since `replace_text`.
    edited_tag: EditedTag,
}

impl std::fmt::Debug for TextEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextEdit").field("text", &if self.secure { "[redacted]" } else { &self.text })
            .field("anchor", &self.anchor).field("focus", &self.focus).field("focused", &self.focused)
            .field("marks", &self.marks).field("blocks", &self.blocks)
            .field("secure", &self.secure).field("revealed", &self.revealed).finish_non_exhaustive()
    }
}

/// Hashes everything a field draws: text, selection, IME composition, focus, marks, blocks and
/// whether it's secure or revealed. Pass the edit itself in a [`memo`](crate::memo) version so
/// the field redraws as the caret or rich formatting changes. A hidden secure field hashes its
/// length (single-line) or non-empty state (multi-line), not its text.
impl std::hash::Hash for TextEdit {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        if self.is_hidden_multiline() {
            (!self.text.is_empty()).hash(h);
            (self.anchor != self.focus).hash(h);
        } else if self.secure && !self.revealed {
            self.text.chars().count().hash(h);
            self.anchor.hash(h);
            self.focus.hash(h);
        } else {
            self.text.hash(h);
            self.anchor.hash(h);
            self.focus.hash(h);
        }
        self.preedit.hash(h);
        self.focused.hash(h);
        self.marks.hash(h);
        if !self.secure {
            self.blocks.hash(h);
        }
        self.soft_breaks.hash(h);
        self.pending.hash(h);
        self.secure.hash(h);
        self.revealed.hash(h);
        self.is_single_line().hash(h);
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

impl Snapshot {
    fn same_doc(&self, other: &Self) -> bool {
        self.text == other.text
            && self.marks == other.marks
            && self.blocks == other.blocks
            && self.soft_breaks == other.soft_breaks
    }
}

/// Active [`TextEdit::undo_group`] baseline (`Some(None)` on a secure field). Never cloned or
/// compared so cloning or comparing a [`TextEdit`] mid-group stays clean.
#[derive(Default)]
struct ActiveGroup(Option<Option<Snapshot>>);

impl Clone for ActiveGroup {
    fn clone(&self) -> Self {
        Self(None)
    }
}

impl PartialEq for ActiveGroup {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

/// Non-content-derived edit state. Never retain a fingerprint of a secret.
#[derive(Clone, Copy, Default)]
struct EditedTag(bool);

impl PartialEq for EditedTag {
    fn eq(&self, _: &Self) -> bool {
        true
    }
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
            + (self.before.soft_breaks.len() + self.after.soft_breaks.len()) * std::mem::size_of::<u32>()
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
            group: ActiveGroup::default(),
            secure: false,
            revealed: false,
            line_mode: LineMode::Default,
            rich: false,
            allowed_marks: None,
            allowed_blocks: None,
            edited_tag: EditedTag::default(),
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
            if self.group.0.is_some() {
                self.group.0 = Some(None);
            }
            self.marks.clear();
            self.blocks.clear();
            self.soft_breaks.clear();
            self.pending = None;
            self.clear_preedit();
        } else if self.group.0 == Some(None) {
            self.sync_invariants();
            self.group.0 = Some(Some(self.snapshot_now()));
        }
    }

    /// Show the plain characters of a [`secure`](Self::secure) field while keeping all other
    /// secure protections: no undo, copy, cut, IME or accessibility text, redacted [`Debug`],
    /// and zeroized buffers.
    /// Rune-owned source buffers are wiped; third-party shaping scratch used while revealed
    /// is released after shaping but is not guaranteed zeroized.
    pub fn revealed(mut self, revealed: bool) -> Self { self.set_revealed(revealed); self }

    pub fn is_revealed(&self) -> bool { self.revealed }

    pub fn set_revealed(&mut self, revealed: bool) { self.revealed = revealed; }

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
        let line_count = if self.secure { 0 } else { 1 + count_newlines(text.as_bytes()) };
        self.text = text;
        self.anchor = self.text.len();
        self.focus = self.anchor;
        self.marks.clear();
        self.blocks = vec![Block::Paragraph; line_count];
        self.soft_breaks.clear();
        self.pending = None;
        self.clear_preedit();
        self.edited_tag = EditedTag(false);
        self.history = Arc::default();
        if self.group.0.is_some() {
            self.group.0 = Some((!self.secure).then(|| self.snapshot_now()));
        }
    }

    /// Whether `self.text` is non-empty and still holds the value the app last set, with no user
    /// edit since [`TextEdit::new`] or [`Self::replace_text`]. An empty field is never pristine.
    /// Use `replace_text` for app replacements, even with the same value. Direct mutation of
    /// the public `text` string cannot reset this state and does not zeroize its old allocation.
    ///
    /// While a hidden multi-line [`secure`](Self::secure) field is pristine, the first insertion
    /// replaces the entire value with the new text and the first deletion empties it, regardless
    /// of caret or selection.
    pub fn is_pristine(&self) -> bool {
        !self.text.is_empty() && !self.edited_tag.0
    }

    /// Mark the current `self.text` as user-edited (`!self.is_pristine()`).
    pub fn mark_edited(&mut self) {
        self.edited_tag.0 = true;
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

    pub(crate) fn is_single_line(&self) -> bool {
        match self.line_mode {
            LineMode::Default => self.secure,
            LineMode::Single => true,
            LineMode::Multi => false,
        }
    }

    /// Whether this is a multi-line secure field in hidden mode (`secure && !revealed && !single_line`).
    pub(crate) fn is_hidden_multiline(&self) -> bool {
        self.secure && !self.revealed && !self.is_single_line()
    }

    /// For a secure field that has started exiting: wipe the secret and keep only its mask, so
    /// the field paints the same dots (no placeholder) through the exit with no plaintext.
    pub(crate) fn conceal(&mut self) {
        if !self.secure { return; }
        self.revealed = false;
        let mask = if self.text.is_empty() {
            String::new()
        } else if self.is_single_line() {
            "•".repeat(self.text.chars().count())
        } else {
            MULTILINE_SECURE_MASK.to_owned()
        };
        self.replace_text(mask);
    }

    /// Map model bytes to the bullet layout's bytes and back. Each Unicode scalar gets one
    /// bullet in single-line mode so UTF-8 length never changes caret/selection alignment;
    /// hidden multi-line fields pin the visual caret to the end of the fixed mask and map
    /// pointer hits to the end of the model.
    pub(crate) fn display_index(&self, byte: usize) -> usize {
        if self.is_hidden_multiline() {
            if self.text.is_empty() { 0 } else { MULTILINE_SECURE_MASK.len() }
        } else if self.secure && !self.revealed {
            self.text[..byte.min(self.text.len())].chars().count() * '•'.len_utf8()
        } else {
            byte.min(self.text.len())
        }
    }
    pub(crate) fn model_index(&self, byte: usize) -> usize {
        if self.is_hidden_multiline() {
            self.text.len()
        } else if self.secure && !self.revealed {
            self.text.char_indices().nth(byte / '•'.len_utf8()).map_or(self.text.len(), |(i, _)| i)
        } else {
            byte.min(self.text.len())
        }
    }
    pub(crate) fn display_selection_range(&self, a: usize, b: usize) -> (usize, usize) {
        if self.is_hidden_multiline() {
            if a == b || self.text.is_empty() { (0, 0) } else { (0, MULTILINE_SECURE_MASK.len()) }
        } else {
            (self.display_index(a), self.display_index(b))
        }
    }

    // A fixed mask has no visible interior positions. Preserve the revealed selection until
    // an edit occurs, then treat it as either the whole secret or an insertion at its end.
    fn normalize_hidden_selection(&mut self) {
        if self.is_hidden_multiline() {
            let selected = self.anchor != self.focus;
            self.focus = self.text.len();
            self.anchor = if selected { 0 } else { self.focus };
        }
    }

    /// Clears the draft and its history after submission, preserving keyboard focus.
    pub fn clear(&mut self) {
        self.replace_text("");
    }

    /// Drop the undo and redo stacks, keeping text, marks, blocks, soft breaks and the selection
    /// (after loading a sent message back into a composer to edit it, say).
    pub fn clear_history(&mut self) {
        self.history = Arc::default();
        if self.group.0.is_some() {
            self.sync_invariants();
            self.group.0 = Some((!self.secure).then(|| self.snapshot_now()));
        }
    }

    /// End a typing group on a pointer selection or focus change.
    pub fn break_history_group(&mut self) {
        if self.history.typing { Arc::make_mut(&mut self.history).typing = false; }
    }

    fn snapshot_now(&self) -> Snapshot {
        Snapshot {
            text: Arc::new(Zeroizing::new(self.text.clone())),
            anchor: self.anchor,
            focus: self.focus,
            marks: self.marks.clone(),
            blocks: self.blocks.clone(),
            soft_breaks: self.soft_breaks.clone(),
        }
    }

    fn snapshot(&self) -> Option<Snapshot> {
        (!self.secure && self.group.0.is_none()).then(|| self.snapshot_now())
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

    fn record_change(&mut self, before: Option<Snapshot>, typing: bool) {
        let Some(before) = before.filter(|_| !self.secure && self.group.0.is_none()) else {
            return;
        };
        let after = self.snapshot_now();
        if before.same_doc(&after) {
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
        if self.group.0.is_some() {
            return f(self);
        }
        self.sync_invariants();
        self.break_history_group();
        self.group.0 = Some((!self.secure).then(|| self.snapshot_now()));
        struct Reset<'a>(&'a mut TextEdit);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                self.0.group.0 = None;
            }
        }
        let g = Reset(self);
        let r = f(g.0);
        let before = g.0.group.0.take().flatten();
        drop(g);
        self.sync_invariants();
        self.break_history_group();
        self.record_change(before, false);
        r
    }

    /// Undo one typing group or discrete edit. Active IME composition is cancelled first.
    pub fn undo(&mut self) {
        if self.secure || self.group.0.is_some() { return; }
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
        if self.secure || self.group.0.is_some() { return; }
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
        if self.secure && !self.revealed {
            return 0;
        }
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
        if self.secure && !self.revealed {
            return self.text.len();
        }
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

    pub(crate) fn set_single_line(&mut self, single_line: bool) {
        self.line_mode = if single_line { LineMode::Single } else { LineMode::Multi };
    }

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
        if self.secure {
            self.marks.clear();
            self.blocks.clear();
            self.soft_breaks.clear();
            self.pending = None;
            return;
        }
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
        if self.secure || self.is_single_line() || !self.allows_block(block) {
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
        if self.secure || self.is_single_line() || !self.allows_block(block) {
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
        if self.secure || self.is_single_line() {
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
        if self.secure || self.is_single_line() {
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

    /// Text typed, pasted or committed into this field. Single-line fields drop control
    /// characters, line breaks included (stripped, as `<input type=text>` does); multi-line
    /// fields fold CRLF to LF (and multi-line secure fields also drop control characters other
    /// than `\n` and `\t`). Sized up front so no partial copy of a secret is left behind by a
    /// reallocation.
    fn sanitize(&self, s: &str) -> Zeroizing<String> {
        let mut out = Zeroizing::new(String::with_capacity(s.len()));
        if self.is_single_line() {
            out.extend(s.chars().filter(|c| !c.is_control() && !matches!(c, '\u{2028}' | '\u{2029}')));
        } else if self.secure {
            let mut chars = s.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\r' {
                    if chars.peek() != Some(&'\n') {
                        out.push('\n');
                    }
                    continue;
                }
                if (!c.is_control() || matches!(c, '\n' | '\t')) && !matches!(c, '\u{2028}' | '\u{2029}') {
                    out.push(c);
                }
            }
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
        self.mark_edited();
    }

    fn replace(&mut self, (mut a, mut b): (usize, usize), s: &str, typing: bool) {
        if self.is_hidden_multiline() && self.is_pristine() {
            (a, b) = (0, self.text.len());
        }
        if self.secure {
            let mut next = String::with_capacity(self.text.len() - (b - a) + s.len());
            next.push_str(&self.text[..a]);
            next.push_str(s);
            next.push_str(&self.text[b..]);
            self.replace_text(next);
            self.anchor = a + s.len();
            self.focus = self.anchor;
            self.mark_edited();
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
            self.mark_edited();
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
        if self.secure || self.is_single_line() {
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
        self.normalize_hidden_selection();
        if self.is_hidden_multiline() && matches!(ev.key, Key::Left | Key::Right | Key::Up | Key::Down | Key::Home | Key::End) {
            self.focus = self.text.len();
            self.anchor = if m.shift { 0 } else { self.focus };
            return out;
        }
        if let Some(c) = ev.control_binding() {
            let (key, line) = match c {
                'a' => (Key::Left, true), 'e' => (Key::Right, true), 'b' => (Key::Left, false),
                'f' => (Key::Right, false), 'p' => (Key::Up, false), 'n' => (Key::Down, false),
                'd' => (Key::Delete, false), 'h' => (Key::Backspace, false),
                _ => {
                    // Ctrl-K deletes to the end of the line, or the line break at its end.
                    self.break_history_group();
                    if self.is_hidden_multiline() && self.is_pristine() {
                        self.insert("");
                        return out;
                    }
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
                if s.chars().any(|c| !c.is_control() || (!self.is_single_line() && matches!(c, '\n' | '\r' | '\t'))) {
                    let s = self.sanitize(s);
                    if !s.is_empty() { self.replace(self.range(), &s, !s.contains(['\r', '\n'])); }
                }
            }
            // A composition's Return and Escape are the input method's.
            Key::Enter | Key::Escape if self.preedit.is_some() => {}
            Key::Enter => {
                if self.is_single_line() {
                    out.submit = true;
                } else if self.secure {
                    if m.cmd || m.ctrl {
                        out.submit = true;
                    } else {
                        self.replace(self.range(), "\n", false);
                    }
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
                if !m.cmd && !m.alt && !m.ctrl && !self.secure && !self.is_single_line() {
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
                if a != b || (self.is_hidden_multiline() && self.is_pristine()) {
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
                        let from = if m.cmd {
                            if self.secure && !self.revealed { 0 } else { self.text[..a].rfind('\n').map_or(0, |i| i + 1) }
                        } else if word {
                            self.prev_word(a)
                        } else {
                            prev_char(&self.text, a)
                        };
                        self.replace((from, a), "", false);
                    }
                }
            }
            Key::Delete => {
                let (a, b) = self.range();
                if a != b || (self.is_hidden_multiline() && self.is_pristine()) {
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
                let i = if m.cmd || (self.secure && !self.revealed) {
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
                }
                .min(n);
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
            Key::Function(_) | Key::ContextMenu | Key::Modifier { .. } => {}
        }
        out
    }

    /// The caret is at the end of the text, in a code block or quote there that it can't move
    /// out of by itself.
    fn at_end_of_trailing_block(&self) -> bool {
        let n = self.text.len();
        !self.secure
            && !self.is_single_line()
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
        if self.secure && !self.revealed {
            return if end { self.text.len() } else { 0 };
        }
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
            Ime::Enabled | Ime::Disabled => {}
        }
        EditOutcome::default()
    }

    pub fn paste(&mut self, s: &str) -> EditOutcome {
        self.normalize_hidden_selection();
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
        if !self.is_single_line() {
            let (a, _) = self.range();
            if self.blocks.get(self.line_of_byte(a)) == Some(&Block::Code) {
                return self.paste(if plain.is_empty() { frag.text.as_str() } else { plain });
            }
        }
        self.break_history_group();
        let before = self.snapshot();
        let (a, b) = self.range();

        if self.is_single_line() {
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
        if (self.secure && !self.revealed) || start >= end {
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
        if self.preedit.is_none() {
            let t = &self.text;
            return self
                .marks
                .iter()
                .filter_map(|m| {
                    let id = m.mark.pill_id()?;
                    let s = Self::clamp_char_boundary(t, m.start);
                    let e = Self::clamp_char_boundary(t, m.end);
                    (s < e && e - s == m.end - m.start).then_some((s as u32, e as u32, id))
                })
                .collect();
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
        if self.secure {
            let text = if self.revealed {
                self.text.clone()
            } else if self.is_single_line() {
                "•".repeat(self.text.chars().count())
            } else if self.text.is_empty() {
                String::new()
            } else {
                MULTILINE_SECURE_MASK.to_owned()
            };
            return (text, None);
        }
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
#[path = "edit/tests.rs"]
mod tests;

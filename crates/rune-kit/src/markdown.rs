//! Markdown (pulldown-cmark): headings, paragraphs, emphasis, inline code, code blocks, lists
//! (with GFM task checkboxes; see [`Markdown::on_task`]), block quotes, rules, links, GFM tables,
//! ```` ```diff ```` fences and (with the `mermaid` feature) ```` ```mermaid ```` diagrams,
//! rendered from a [`MarkdownStyle`]. Fences in rust,
//! js, ts, tsx, json, python, go, swift, sh, sql, html, css and yaml are syntax-coloured by
//! [`crate::syntax`], or by an app's own highlighter ([`Markdown::highlight`]). Partial input
//! while streaming is fine: an unclosed fence is a code block that keeps growing.
//!
//! ```ignore
//! let view = Markdown::new(&message.text).reveal(0.18).on_link(|s: &mut App, url| s.open(url)).build(&kit);
//! // Without a Kit:
//! let view = Markdown::new(&message.text).render(&MarkdownStyle::neutral(dark));
//! ```
//!
//! **Cost.** [`parse_cached`] keeps the parse of every source string the last two commits drew
//! (and others up to a memory budget), so re-rendering unchanged text (every frame of an
//! animation, a transcript of any length) parses nothing. A growing stream parses only from the
//! start of its last top-level block each time it grows.
//!
//! **Keys.** Top-level blocks are keyed `b{index}` (or by [`Markdown::block_keys`]), list items
//! `i{index}` and their blocks by `b{index}` inside the item. Appending to a stream only ever
//! changes the last block or adds blocks after it, so earlier blocks keep their keys and never
//! remount. Give the `Markdown` root its own key when more than one is on screen.
//!
//! **Tables** size each column to its content. A table that doesn't fit shrinks its columns in
//! proportion to their content (wrapping cell text), down to each column's longest word capped at
//! [`MarkdownStyle::table_min_column`]; past that it clips. While a stream is inside a table (the
//! last row has no newline yet and [`Markdown::streaming`] is on), the row still arriving doesn't
//! widen columns, so columns move at most once per finished row. Otherwise the text is taken as
//! whole, and a table ending it sizes to every row. A trailing `| a | b |` line is already drawn
//! as the header it is about to become.
//!
//! **Diagrams** (`mermaid` feature) render once their fence closes; until then, and for anything
//! the subset doesn't cover, the source shows as a code block.

use std::cell::{Cell, RefCell};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use rune::{Align, Color, Cursor, El, Family, Icon, Link, LinkPaint, Span, Spring, TextHighlight, TextStyle, Tx, col, el, icon, rich, row, text};

use crate::{Kit, PaletteSeed};

mod table;

use crate::budget::Budget;
use crate::syntax::{Highlighter, SyntaxTheme};
pub use crate::syntax::Syntax;

/// An inline style run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Style {
    Code,
    /// The narrow no-break spaces around inline code (CSS's `padding: 0 4px`).
    CodePad,
    Strong,
    Em,
    Strike,
    Link,
}

/// Text with styled and linked byte ranges.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Inline {
    pub text: String,
    /// Byte ranges into `text`.
    pub spans: Vec<(usize, usize, Style)>,
    /// Byte ranges into `text` and the destination of each link.
    pub links: Vec<(usize, usize, String)>,
}

/// A block of the parsed document.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Para(Inline),
    Heading(u8, Inline),
    Code(String),
    List { start: Option<u64>, items: Vec<Vec<Block>> },
    Quote(Vec<Block>),
    Rule,
    /// A GFM table.
    Table(Table),
    /// A ```` ```diff ```` (or `patch`) fence.
    Diff(String),
    /// A closed ```` ```mermaid ```` fence (an unclosed one is still [`Block::Code`]).
    Mermaid(String),
}

/// A GFM table. Rows may have fewer cells than there are columns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Table {
    /// One per column: `:--` and none are `Start`, `:-:` is `Center`, `--:` is `End`.
    pub align: Vec<Align>,
    pub head: Vec<Inline>,
    pub rows: Vec<Vec<Inline>>,
    /// The source ends inside the last row (or the header), so it may still grow.
    pub open: bool,
}

/// Maps source UTF-8 byte ranges to rendered text UTF-8 byte ranges.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct SpanMap(Vec<(Range<usize>, Range<usize>)>);

impl SpanMap {
    fn push(&mut self, src: Range<usize>, out: Range<usize>) {
        if src.start >= src.end || out.start >= out.end {
            return;
        }
        if let Some((last_src, last_out)) = self.0.last_mut()
            && last_src.end == src.start
            && last_out.end == out.start
            && last_src.len() == last_out.len()
            && src.len() == out.len()
        {
            last_src.end = src.end;
            last_out.end = out.end;
            return;
        }
        self.0.push((src, out));
    }

    fn truncate_out(&mut self, len: usize) {
        self.0.retain_mut(|(src, out)| {
            if out.start >= len {
                return false;
            }
            if out.end > len {
                let cut = out.end - len;
                if src.len() == out.len() {
                    src.end = src.end.saturating_sub(cut);
                }
                out.end = len;
            }
            src.start < src.end && out.start < out.end
        });
    }

    fn shift_src(&mut self, offset: usize) {
        if offset == 0 {
            return;
        }
        for (src, _) in &mut self.0 {
            src.start += offset;
            src.end += offset;
        }
    }

    fn map_range(&self, query: Range<usize>, out_text: &str) -> Vec<Range<usize>> {
        if query.start >= query.end || self.0.is_empty() || out_text.is_empty() {
            return Vec::new();
        }
        let mut mapped: Vec<Range<usize>> = Vec::new();
        for (src, out) in &self.0 {
            if query.end <= src.start || src.end <= query.start {
                continue;
            }
            let s0 = query.start.max(src.start);
            let s1 = query.end.min(src.end);
            if s0 >= s1 {
                continue;
            }
            let (mut o0, mut o1) = if src.len() == out.len() {
                (out.start + (s0 - src.start), out.start + (s1 - src.start))
            } else {
                let a = if s0 <= src.start {
                    out.start
                } else {
                    out.start + ((s0 - src.start) * out.len()) / src.len()
                };
                let b = if s1 >= src.end {
                    out.end
                } else {
                    (out.start + ((s1 - src.start) * out.len()).div_ceil(src.len())).max(a + 1)
                };
                (a, b)
            };
            o0 = out_text.floor_char_boundary(o0.clamp(out.start, out.end).min(out_text.len()));
            o1 = out_text.ceil_char_boundary(o1.clamp(out.start, out.end).min(out_text.len()));
            if o0 >= o1 {
                continue;
            }
            if let Some(prev) = mapped.last_mut()
                && (prev.end == o0 || out_text.get(prev.end..o0) == Some(PAD))
            {
                prev.end = o1;
            } else {
                mapped.push(o0..o1);
            }
        }
        mapped
    }
}

/// Source-to-rendered byte mappings parallel to [`Block`], kept beside `Block` so `Block` keeps
/// its shape.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) enum BlockMap {
    Inline(SpanMap),
    Code(SpanMap),
    List(Vec<Vec<BlockMap>>),
    Quote(Vec<BlockMap>),
    Table { head: Vec<SpanMap>, rows: Vec<Vec<SpanMap>> },
    #[default]
    None,
}

impl BlockMap {
    fn shift_src(mut self, offset: usize) -> Self {
        if offset == 0 {
            return self;
        }
        match &mut self {
            BlockMap::Inline(m) | BlockMap::Code(m) => m.shift_src(offset),
            BlockMap::List(items) => {
                for item in items {
                    for b in item {
                        *b = std::mem::take(b).shift_src(offset);
                    }
                }
            }
            BlockMap::Quote(inner) => {
                for b in inner {
                    *b = std::mem::take(b).shift_src(offset);
                }
            }
            BlockMap::Table { head, rows } => {
                for m in head {
                    m.shift_src(offset);
                }
                for row in rows {
                    for m in row {
                        m.shift_src(offset);
                    }
                }
            }
            BlockMap::None => {}
        }
        self
    }
}

struct Builder {
    /// Open containers: (kind, blocks so far, maps so far).
    stack: Vec<(Container, Vec<Block>, Vec<BlockMap>)>,
    inline: Option<(InlineKind, Inline, Vec<Open>, SpanMap)>,
    code: Option<(String, Fence, SpanMap)>,
    /// The source range of the last paragraph started at the root.
    last_root_para: Option<Range<usize>>,
    sources: Vec<Source>,
    /// One entry per [`Tag::Item`] in document order (`Some(checked)` for GFM task items).
    tasks: Vec<Option<(bool, usize)>>,
    block_starts: Vec<usize>,
    root_start: usize,
    cuts: Vec<Cut>,
    autolink: bool,
    image_starts: Vec<usize>,
    image_ranges: Vec<(usize, usize)>,
}

/// Where a top-level block starts: the start of its first line, and how many top-level blocks,
/// [`Source`]s and list items come before it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Cut {
    at: usize,
    blocks: usize,
    sources: usize,
    tasks: usize,
}

/// What colours a [`Block::Code`], one per block in document order (the order the renderer
/// meets them). Kept beside the blocks so `Block` keeps its shape.
#[derive(Clone, Debug, PartialEq)]
struct Source {
    /// The fence's language as written (its info string's first word), if it names one.
    lang: Option<Rc<str>>,
    /// The code's length, to check this belongs to the block it is matched with.
    len: usize,
    /// How many leading bytes get colour: all of them, or while an unclosed fence streams in,
    /// the complete lines (the line still arriving draws plain until its newline).
    coloured: usize,
}

#[derive(Clone, PartialEq)]
enum Fence {
    /// A code block; its language as written, if it names one, and whether its closing fence
    /// has arrived.
    Plain(Option<Rc<str>>, bool),
    Diff,
    /// A mermaid fence; whether its closing fence has arrived.
    Mermaid(bool),
}

/// An emphasis, strong, strike or link run that has started and not yet ended.
struct Open {
    start: usize,
    style: Style,
    url: Option<String>,
}

enum Container {
    Root,
    Quote,
    List(Option<u64>, Vec<Vec<Block>>, Vec<Vec<BlockMap>>),
    Item(usize),
    Table(Table, Vec<Inline>, Vec<SpanMap>, Vec<Vec<SpanMap>>, Vec<SpanMap>),
}

#[derive(Clone, Copy)]
enum InlineKind {
    Para,
    Heading(u8),
    Cell,
}

impl Builder {
    fn push(&mut self, b: Block, map: BlockMap) {
        if self.stack.len() == 1 {
            self.block_starts.push(self.root_start);
        }
        let top = self.stack.last_mut().unwrap();
        top.1.push(b);
        top.2.push(map);
    }

    /// Pushes a code block's text: without its final newline, or at the end of input without
    /// any trailing newlines.
    fn push_code(&mut self, mut text: String, fence: Fence, mut map: SpanMap, eof: bool) {
        let line_ended = text.ends_with('\n');
        if eof {
            text.truncate(text.trim_end_matches('\n').len());
        } else if line_ended {
            text.pop();
        }
        map.truncate_out(text.len());
        let block = code_block(text, &fence);
        if let Block::Code(code) = &block {
            let (lang, closed) = match fence {
                Fence::Plain(lang, closed) => (lang, closed),
                // A mermaid fence still arriving.
                _ => (Some("mermaid".into()), false),
            };
            let coloured = if closed || line_ended { code.len() } else { code.rfind('\n').unwrap_or(0) };
            self.sources.push(Source { lang, len: code.len(), coloured });
        }
        self.push(block, BlockMap::Code(map));
    }

    fn inline(&mut self) -> &mut (InlineKind, Inline, Vec<Open>, SpanMap) {
        // Tight list items have text without a paragraph.
        self.inline.get_or_insert_with(|| (InlineKind::Para, Inline::default(), Vec::new(), SpanMap::default()))
    }

    fn flush_inline(&mut self) {
        if let Some((kind, mut inl, open, mut map)) = self.inline.take() {
            for o in open {
                end_run(&mut inl, o);
            }
            let trimmed = inl.text.trim_end().len();
            inl.text.truncate(trimmed);
            map.truncate_out(trimmed);
            inl.spans.retain_mut(|s| {
                s.1 = s.1.min(trimmed);
                s.0 < s.1
            });
            inl.links.retain_mut(|s| {
                s.1 = s.1.min(trimmed);
                s.0 < s.1
            });
            if self.autolink {
                autolink_inline(&mut inl, &self.image_ranges);
            }
            self.image_ranges.clear();
            self.image_starts.clear();
            if let InlineKind::Cell = kind {
                // Empty cells still hold their column.
                if let Some((Container::Table(_, cells, _, _, cell_maps), _, _)) = self.stack.last_mut() {
                    cells.push(inl);
                    cell_maps.push(map);
                }
                return;
            }
            if inl.text.is_empty() {
                return;
            }
            self.push(
                match kind {
                    InlineKind::Para | InlineKind::Cell => Block::Para(inl),
                    InlineKind::Heading(l) => Block::Heading(l, inl),
                },
                BlockMap::Inline(map),
            );
        }
    }

    /// Closes the innermost container into its parent.
    fn pop_container(&mut self) {
        self.flush_inline();
        let Some((c, blocks, maps)) = self.stack.pop() else { return };
        match c {
            Container::Item(_) => {
                if let Some((Container::List(_, items, item_maps), _, _)) = self.stack.last_mut() {
                    items.push(blocks);
                    item_maps.push(maps);
                }
            }
            Container::List(start, items, item_maps) => self.push(Block::List { start, items }, BlockMap::List(item_maps)),
            Container::Quote => self.push(Block::Quote(blocks), BlockMap::Quote(maps)),
            Container::Table(mut t, cells, head, mut rows, cell_maps) => {
                if !cells.is_empty() {
                    t.rows.push(cells);
                    rows.push(cell_maps);
                }
                self.push(Block::Table(t), BlockMap::Table { head, rows });
            }
            Container::Root => {}
        }
    }

    fn table(&mut self) -> Option<(&mut Table, &mut Vec<Inline>, &mut Vec<SpanMap>, &mut Vec<Vec<SpanMap>>, &mut Vec<SpanMap>)> {
        match self.stack.last_mut() {
            Some((Container::Table(t, cells, head, rows, cell_maps), _, _)) => Some((t, cells, head, rows, cell_maps)),
            _ => None,
        }
    }
}

const PAD: &str = "\u{202f}";

/// Source byte range of the inner code text inside a backtick code span `src[range]`.
fn code_span_content_range(src: &str, range: Range<usize>, rendered: &str) -> Range<usize> {
    let Some(raw) = src.get(range.clone()) else { return range };
    let open = raw.bytes().take_while(|&b| b == b'`').count();
    let close = raw[open..].bytes().rev().take_while(|&b| b == b'`').count();
    let mut start = range.start + open;
    let mut end = range.end.saturating_sub(close);
    if !rendered.chars().all(|c| c == ' ')
        && src.as_bytes().get(start).is_some_and(|b| matches!(b, b' ' | b'\r' | b'\n'))
        && src.as_bytes().get(end.saturating_sub(1)).is_some_and(|b| matches!(b, b' ' | b'\r' | b'\n'))
    {
        start += 1;
        end -= 1;
    }
    if start < end { start..end } else { range }
}

/// Code spans normalize newlines and omit continuation-line container prefixes. Map the
/// actual surviving characters, not a ratio of the source and displayed byte lengths.
fn map_code_span(src: &str, range: Range<usize>, rendered: &str, offset: usize, map: &mut SpanMap) {
    if &src[range.clone()] == rendered {
        map.push(range, offset..offset + rendered.len());
        return;
    }
    let mut out = 0;
    let mut at = range.start;
    for line in src[range].split_inclusive(['\r', '\n']) {
        let body = line.trim_end_matches(['\r', '\n']);
        let remaining = &rendered[out..];
        let matched = body.char_indices().find_map(|(skip, _)| {
            let mut chars = body[skip..].char_indices().peekable();
            let mut consumed = 0;
            let mut pairs = Vec::new();
            while let Some((i, mut c)) = chars.next() {
                let mut end = i + c.len_utf8();
                if c == '\\' && chars.peek().is_some_and(|(_, c)| *c == '|') && remaining[consumed..].starts_with('|') {
                    let (j, next) = chars.next().unwrap();
                    c = next;
                    end = j + c.len_utf8();
                }
                if !remaining[consumed..].starts_with(c) { return None; }
                pairs.push((at + skip + i..at + skip + end, offset + out + consumed..offset + out + consumed + c.len_utf8()));
                consumed += c.len_utf8();
            }
            Some((consumed, pairs))
        });
        if let Some((consumed, pairs)) = matched {
            for (src, dest) in pairs { map.push(src, dest); }
            out += consumed;
        }
        if line.ends_with(['\r', '\n']) && rendered[out..].starts_with(' ') {
            map.push(at + body.len()..at + line.len(), offset + out..offset + out + 1);
            out += 1;
        }
        at += line.len();
    }
}

/// Scans `inl.text` outside inline code and existing links for bare GFM autolinks (`https://…`,
/// `http://…`, `www.…`) and wires them into `inl.links` and `inl.spans`.
fn autolink_inline(inl: &mut Inline, image_ranges: &[(usize, usize)]) {
    if !inl.text.contains("://") && !inl.text.contains("www.") {
        return;
    }
    let mut excluded: Vec<(usize, usize)> = inl
        .spans
        .iter()
        .filter(|(_, _, st)| matches!(st, Style::Code | Style::CodePad | Style::Link))
        .map(|&(a, b, _)| (a, b))
        .chain(image_ranges.iter().copied())
        .collect();
    excluded.sort_unstable_by_key(|&(a, _)| a);

    let mut found: Vec<(usize, usize, String)> = Vec::new();
    let mut cursor = 0;
    for (ex_a, ex_b) in excluded {
        if cursor < ex_a {
            scan_bare_urls(&inl.text, cursor, ex_a, &mut found);
        }
        cursor = cursor.max(ex_b);
    }
    if cursor < inl.text.len() {
        scan_bare_urls(&inl.text, cursor, inl.text.len(), &mut found);
    }
    if found.is_empty() {
        return;
    }
    let had_links = !inl.links.is_empty();
    for (start, end, dest) in found {
        inl.links.push((start, end, dest));
        inl.spans.push((start, end, Style::Link));
    }
    if had_links {
        inl.links.sort_by_key(|l| l.0);
        // Keep `Style::Link` spans in `inl.spans` ordered by start byte to match `inl.links`.
        let mut link_ranges: Vec<(usize, usize)> = inl.links.iter().map(|&(a, b, _)| (a, b)).collect();
        let mut k = 0;
        for sp in &mut inl.spans {
            if matches!(sp.2, Style::Link) {
                (sp.0, sp.1) = link_ranges[k];
                k += 1;
            }
        }
        let _ = &mut link_ranges;
    }
}

fn scan_bare_urls(text: &str, seg_start: usize, seg_end: usize, out: &mut Vec<(usize, usize, String)>) {
    let bytes = text.as_bytes();
    let mut i = seg_start;
    while i < seg_end {
        let rest = &text[i..seg_end];
        let (prefix_len, is_www) = if rest.get(..8).is_some_and(|s| s.eq_ignore_ascii_case("https://")) {
            (8, false)
        } else if rest.get(..7).is_some_and(|s| s.eq_ignore_ascii_case("http://")) {
            (7, false)
        } else if rest.starts_with("www.") {
            (4, true)
        } else {
            i += rest.chars().next().map_or(1, char::len_utf8);
            continue;
        };
        let valid_prev = i == 0
            || matches!(bytes[i - 1], b' ' | b'\t' | b'\n' | b'*' | b'_' | b'~' | b'[')
            || (bytes[i - 1] == b'(' && (i < 2 || bytes[i - 2] != b']'));
        if !valid_prev {
            i += prefix_len;
            continue;
        }
        let raw_len = rest
            .find(|c: char| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>'))
            .unwrap_or(rest.len());
        let mut end = i + raw_len;
        trim_gfm_autolink(text, i, prefix_len, &mut end);
        let matched = &text[i..end];
        if valid_autolink_domain(&matched[prefix_len..], is_www) {
            let dest = if is_www { format!("http://{matched}") } else { matched.to_owned() };
            out.push((i, end, dest));
            i = end;
        } else {
            i += prefix_len;
        }
    }
}

/// GFM 6.9 extended autolink path trailing-punctuation trimming.
fn trim_gfm_autolink(text: &str, start: usize, prefix_len: usize, end: &mut usize) {
    loop {
        let slice = &text[start..*end];
        if slice.len() <= prefix_len {
            break;
        }
        let Some(last) = slice.chars().next_back() else { break };
        if matches!(last, '?' | '!' | '.' | ',' | ':' | '*' | '_' | '~' | '\'' | '"') {
            *end -= last.len_utf8();
            continue;
        }
        if last == ')' {
            let body = &slice[prefix_len..];
            let opens = body.bytes().filter(|&b| b == b'(').count();
            let closes = body.bytes().filter(|&b| b == b')').count();
            if closes > opens {
                *end -= 1;
                continue;
            }
            break;
        }
        if last == ']' {
            let body = &slice[prefix_len..];
            let opens = body.bytes().filter(|&b| b == b'[').count();
            let closes = body.bytes().filter(|&b| b == b']').count();
            if closes > opens {
                *end -= 1;
                continue;
            }
            break;
        }
        if last == ';' {
            let before_semi = &slice[..slice.len() - 1];
            if let Some(amp) = before_semi.rfind('&') {
                let entity = &before_semi[amp + 1..];
                if !entity.is_empty() && entity.bytes().all(|b| b.is_ascii_alphanumeric()) && amp >= prefix_len {
                    *end = start + amp;
                    continue;
                }
            }
            *end -= 1;
            continue;
        }
        break;
    }
}

/// Validates the authority/domain portion of a bare URL after `https://`, `http://`, or `www.`.
fn valid_autolink_domain(after_prefix: &str, is_www: bool) -> bool {
    let host_end = after_prefix.find(['/', '?', '#']).unwrap_or(after_prefix.len());
    let mut host = &after_prefix[..host_end];
    if let Some((h, port)) = host.rsplit_once(':')
        && !port.is_empty()
        && port.bytes().all(|b| b.is_ascii_digit())
    {
        host = h;
    }
    if host.is_empty()
        || !host.as_bytes()[0].is_ascii_alphanumeric()
        || host.ends_with('.')
        || host.ends_with('-')
        || !host.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return false;
    }
    if is_www && !host.contains('.') {
        return false;
    }
    let segments: Vec<&str> = host.split('.').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return false;
    }
    // GFM 6.9: the last two domain segments must not contain underscores.
    if segments.len() >= 2 && segments[segments.len() - 2..].iter().any(|s| s.contains('_')) {
        return false;
    }
    true
}

/// Parses `src` into blocks. Unclosed constructs are closed at the end of input.
pub fn parse(src: &str) -> Vec<Block> {
    parse_doc(src, true).0
}

fn parse_doc(src: &str, autolink: bool) -> (Vec<Block>, Vec<Source>, Vec<Option<(bool, usize)>>, Vec<Cut>, Vec<usize>, Vec<BlockMap>) {
    let mut b = Builder {
        stack: vec![(Container::Root, Vec::new(), Vec::new())],
        inline: None,
        code: None,
        last_root_para: None,
        sources: Vec::new(),
        tasks: Vec::new(),
        block_starts: Vec::new(),
        root_start: 0,
        cuts: Vec::new(),
        autolink,
        image_starts: Vec::new(),
        image_ranges: Vec::new(),
    };
    let opts = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    for (ev, range) in Parser::new_ext(src, opts).into_offset_iter() {
        let starts_block = matches!(
            ev,
            Event::Start(
                Tag::Paragraph | Tag::Heading { .. } | Tag::CodeBlock(_) | Tag::BlockQuote(_) | Tag::List(_) | Tag::Table(_) | Tag::HtmlBlock
            ) | Event::Rule
        );
        if starts_block && b.stack.len() == 1 && b.code.is_none() {
            b.flush_inline();
            let at = src[..range.start].rfind('\n').map_or(0, |i| i + 1);
            b.root_start = at;
            b.cuts.push(Cut { at, blocks: b.stack[0].1.len(), sources: b.sources.len(), tasks: b.tasks.len() });
        }
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    b.flush_inline();
                    if b.stack.len() == 1 {
                        b.last_root_para = Some(range);
                    }
                    b.inline = Some((InlineKind::Para, Inline::default(), Vec::new(), SpanMap::default()));
                }
                Tag::Heading { level, .. } => {
                    b.flush_inline();
                    let l = match level {
                        HeadingLevel::H1 => 1,
                        HeadingLevel::H2 => 2,
                        HeadingLevel::H3 => 3,
                        _ => 4,
                    };
                    b.inline = Some((InlineKind::Heading(l), Inline::default(), Vec::new(), SpanMap::default()));
                }
                Tag::CodeBlock(kind) => {
                    b.flush_inline();
                    let fence = match &kind {
                        CodeBlockKind::Fenced(info) => {
                            // `rust,ignore` and `ts title="a.ts"` name their language first.
                            let word = info.split(|c: char| c.is_whitespace() || c == ',').next().unwrap_or("");
                            match word.to_ascii_lowercase().as_str() {
                                "diff" | "patch" => Fence::Diff,
                                "mermaid" => Fence::Mermaid(fence_closed(&src[range])),
                                _ => Fence::Plain((!word.is_empty()).then(|| word.into()), fence_closed(&src[range])),
                            }
                        }
                        CodeBlockKind::Indented => Fence::Plain(None, true),
                    };
                    b.code = Some((String::new(), fence, SpanMap::default()));
                }
                Tag::BlockQuote(_) => {
                    b.flush_inline();
                    b.stack.push((Container::Quote, Vec::new(), Vec::new()));
                }
                Tag::List(start) => {
                    b.flush_inline();
                    b.stack.push((Container::List(start, Vec::new(), Vec::new()), Vec::new(), Vec::new()));
                }
                Tag::Item => {
                    b.flush_inline();
                    let idx = b.tasks.len();
                    b.tasks.push(None);
                    b.stack.push((Container::Item(idx), Vec::new(), Vec::new()));
                }
                Tag::Table(aligns) => {
                    b.flush_inline();
                    let align = aligns
                        .iter()
                        .map(|a| match a {
                            Alignment::Center => Align::Center,
                            Alignment::Right => Align::End,
                            Alignment::None | Alignment::Left => Align::Start,
                        })
                        .collect();
                    b.stack.push((
                        Container::Table(Table { align, ..Table::default() }, Vec::new(), Vec::new(), Vec::new(), Vec::new()),
                        Vec::new(),
                        Vec::new(),
                    ));
                }
                Tag::TableHead | Tag::TableRow => {
                    // A row is still arriving while its line has no newline yet.
                    let open = !src[range.start..].contains('\n');
                    if let Some((t, _, _, _, _)) = b.table() {
                        t.open = open;
                    }
                }
                Tag::TableCell => {
                    b.flush_inline();
                    b.inline = Some((InlineKind::Cell, Inline::default(), Vec::new(), SpanMap::default()));
                }
                Tag::Strong => open(&mut b, Style::Strong, None),
                Tag::Emphasis => open(&mut b, Style::Em, None),
                Tag::Strikethrough => open(&mut b, Style::Strike, None),
                Tag::Link { dest_url, .. } => open(&mut b, Style::Link, Some(dest_url.into_string())),
                Tag::Image { .. } => {
                    let start = b.inline().1.text.len();
                    b.image_starts.push(start);
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::TableCell => b.flush_inline(),
                TagEnd::CodeBlock => {
                    if let Some((text, fence, map)) = b.code.take() {
                        b.push_code(text, fence, map, false);
                    }
                }
                TagEnd::TableHead => {
                    b.flush_inline();
                    if let Some((t, cells, head, _, cell_maps)) = b.table() {
                        t.head = std::mem::take(cells);
                        *head = std::mem::take(cell_maps);
                    }
                }
                TagEnd::TableRow => {
                    b.flush_inline();
                    if let Some((t, cells, _, rows, cell_maps)) = b.table() {
                        t.rows.push(std::mem::take(cells));
                        rows.push(std::mem::take(cell_maps));
                    }
                }
                TagEnd::BlockQuote(_) | TagEnd::Item | TagEnd::List(_) | TagEnd::Table => b.pop_container(),
                TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough | TagEnd::Link => close(&mut b),
                TagEnd::Image => {
                    if let Some(start) = b.image_starts.pop() {
                        let end = b.inline().1.text.len();
                        b.image_ranges.push((start, end));
                    }
                }
                _ => {}
            },
            Event::Text(t) => {
                if let Some((code, _, map)) = &mut b.code {
                    let start = code.len();
                    code.push_str(&t);
                    map.push(range, start..code.len());
                } else {
                    let inl = b.inline();
                    let start = inl.1.text.len();
                    inl.1.text.push_str(&t);
                    inl.3.push(range, start..inl.1.text.len());
                }
            }
            Event::Code(t) => {
                let code_src = code_span_content_range(src, range, &t);
                let inl = b.inline();
                let a = inl.1.text.len();
                inl.1.text.push_str(PAD);
                let start = inl.1.text.len();
                inl.1.text.push_str(&t);
                let end = inl.1.text.len();
                inl.1.text.push_str(PAD);
                inl.1.spans.push((a, start, Style::CodePad));
                inl.1.spans.push((start, end, Style::Code));
                inl.1.spans.push((end, inl.1.text.len(), Style::CodePad));
                map_code_span(src, code_src, &t, start, &mut inl.3);
            }
            Event::SoftBreak => {
                let inl = b.inline();
                let start = inl.1.text.len();
                inl.1.text.push(' ');
                inl.3.push(range, start..inl.1.text.len());
            }
            Event::HardBreak => {
                let inl = b.inline();
                let start = inl.1.text.len();
                inl.1.text.push('\n');
                inl.3.push(range, start..inl.1.text.len());
            }
            Event::TaskListMarker(checked) => {
                if let Some(idx) = b.stack.iter().rev().find_map(|(c, _, _)| match c {
                    Container::Item(idx) => Some(*idx),
                    _ => None,
                }) {
                    b.tasks[idx] = Some((checked, range.start));
                }
            }
            Event::Rule => {
                b.flush_inline();
                b.push(Block::Rule, BlockMap::None);
            }
            _ => {}
        }
    }
    b.flush_inline();
    if let Some((text, fence, map)) = b.code.take() {
        b.push_code(text, fence, map, true);
    }
    // Unwind containers left open by a partial stream.
    while b.stack.len() > 1 {
        b.pop_container();
    }
    let last_para = b.last_root_para.take();
    let (_, mut blocks, mut maps) = b.stack.pop().unwrap();
    if let (Some(Block::Para(_)), Some(range)) = (blocks.last(), last_para)
        && let Some((t, map)) = pending_table(&src[range.start..], range.start, autolink)
    {
        *blocks.last_mut().unwrap() = Block::Table(t);
        *maps.last_mut().unwrap() = map;
    }
    (blocks, b.sources, b.tasks, b.cuts, b.block_starts, maps)
}

fn code_block(text: String, fence: &Fence) -> Block {
    match fence {
        Fence::Plain(..) | Fence::Mermaid(false) => Block::Code(text),
        Fence::Diff => Block::Diff(text),
        Fence::Mermaid(true) => Block::Mermaid(text),
    }
}

/// Whether a fenced code block's source (from its opening fence) has its closing fence.
fn fence_closed(block: &str) -> bool {
    fn strip(l: &str) -> &str {
        l.trim_start_matches(|c: char| c == '>' || c.is_whitespace()).trim_end()
    }
    let mut lines = block.lines();
    let Some(first) = lines.next().map(strip) else { return false };
    let Some(ch) = first.chars().next() else { return false };
    let n = first.chars().take_while(|&c| c == ch).count();
    lines.next_back().map(strip).is_some_and(|l| l.len() >= n && l.chars().all(|c| c == ch))
}

/// The header (and partial delimiter row) of a table still streaming in, from the source of the
/// document's last paragraph: `| a | b |` alone, or followed by a delimiter row in progress.
fn pending_table(src: &str, base_offset: usize, autolink: bool) -> Option<(Table, BlockMap)> {
    let mut lines = src.trim_end().lines();
    let raw_head = lines.next()?;
    let lead_ws = raw_head.len() - raw_head.trim_start().len();
    let head = raw_head.trim();
    let delim = lines.next().map(str::trim);
    if lines.next().is_some() || !head.starts_with('|') {
        return None;
    }
    let cells = split_row(head, base_offset + lead_ws);
    if cells.len() < 2 || delim.is_some_and(|d| !d.starts_with('|') || !d.chars().all(|c| "|:- ".contains(c))) {
        return None;
    }
    let mut align = vec![Align::Start; cells.len()];
    if let Some(d) = delim {
        for (a, (_, spec)) in align.iter_mut().zip(split_row(d, 0)) {
            *a = match (spec.starts_with(':'), spec.len() > 1 && spec.ends_with(':')) {
                (true, true) => Align::Center,
                (false, true) => Align::End,
                _ => Align::Start,
            };
        }
    }
    let mut head_inlines = Vec::with_capacity(cells.len());
    let mut head_maps = Vec::with_capacity(cells.len());
    for (cell_off, c) in &cells {
        let (inl, map) = inline_of(c, *cell_off, autolink);
        head_inlines.push(inl);
        head_maps.push(map);
    }
    Some((
        Table { align, head: head_inlines, rows: Vec::new(), open: true },
        BlockMap::Table { head: head_maps, rows: Vec::new() },
    ))
}

/// The cells of a `| a | b |` line (a trailing pipe is optional; `\|` doesn't split, and stays
/// escaped for [`inline_of`] to unescape), with each trimmed cell's starting byte offset.
fn split_row(line: &str, base_offset: usize) -> Vec<(usize, String)> {
    let pipe_off = usize::from(line.starts_with('|'));
    let inner = &line[pipe_off..];
    let mut cells: Vec<(usize, String)> = vec![(base_offset + pipe_off, String::new())];
    let mut chars = inner.char_indices().peekable();
    while let Some((idx, c)) = chars.next() {
        match c {
            '\\' if chars.peek().is_some_and(|&(_, next)| next == '|') => {
                cells.last_mut().unwrap().1.extend(['\\', chars.next().unwrap().1]);
            }
            '|' => cells.push((base_offset + pipe_off + idx + 1, String::new())),
            c => cells.last_mut().unwrap().1.push(c),
        }
    }
    if cells.last().is_some_and(|(_, c)| c.trim().is_empty()) && inner.trim_end().ends_with('|') {
        cells.pop();
    }
    cells
        .into_iter()
        .map(|(off, c)| {
            let lead = c.len() - c.trim_start().len();
            (off + lead, c.trim().to_owned())
        })
        .collect()
}

fn inline_of(src: &str, offset: usize, autolink: bool) -> (Inline, SpanMap) {
    let (blocks, _, _, _, _, maps) = parse_doc(src, autolink);
    match (blocks.into_iter().next(), maps.into_iter().next()) {
        (Some(Block::Para(i)), Some(BlockMap::Inline(mut m))) => {
            m.shift_src(offset);
            (i, m)
        }
        _ => {
            let mut m = SpanMap::default();
            m.push(offset..offset + src.len(), 0..src.len());
            (Inline { text: src.to_owned(), ..Inline::default() }, m)
        }
    }
}

fn open(b: &mut Builder, style: Style, url: Option<String>) {
    let inl = b.inline();
    let start = inl.1.text.len();
    inl.2.push(Open { start, style, url });
}

fn close(b: &mut Builder) {
    let (_, inl, open, _) = b.inline();
    if let Some(o) = open.pop() {
        end_run(inl, o);
    }
}

/// Ends run `o` at the current end of the text.
fn end_run(inl: &mut Inline, o: Open) {
    let end = inl.text.len();
    if let Some(url) = o.url {
        inl.links.push((o.start, end, url));
    }
    inl.spans.push((o.start, end, o.style));
}
/// Estimated bytes of parsed documents kept beyond those on screen (see [`budget`]).
const CACHE_BYTES: usize = 16 << 20;

type Doc = (Rc<Vec<Block>>, Rc<[Source]>, Rc<[Option<(bool, usize)>]>, Rc<[usize]>, Rc<[BlockMap]>);

#[derive(Clone)]
struct Parsed {
    doc: Doc,
    cuts: Rc<[Cut]>,
}

struct Cache {
    docs: Budget<Rc<str>, Parsed>,
    /// The source parsed last, which a stream's next string extends.
    last: Option<Rc<str>>,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache { docs: Budget::new(CACHE_BYTES), last: None });
}

#[cfg(test)]
thread_local! {
    /// (whole documents parsed, streams parsed from their last block).
    static PARSES: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}

/// [`parse`] memoised by source on this thread. Every document the last two commits drew stays
/// cached however many there are; past a memory budget the least recently drawn of the rest go.
pub fn parse_cached(src: &str) -> Rc<Vec<Block>> {
    doc_cached(src).0
}

fn doc_cached(src: &str) -> Doc {
    CACHE.with_borrow_mut(|c| {
        if let Some(hit) = c.docs.get(src) {
            return hit.doc.clone();
        }
        let grown = c.last.clone().filter(|last| src.len() > last.len() && src.starts_with(&**last));
        let parsed = grown.as_ref().and_then(|last| extend(c.docs.peek(&**last)?, last.len(), src)).unwrap_or_else(|| {
            #[cfg(test)]
            PARSES.with(|p| p.set((p.get().0 + 1, p.get().1)));
            let (blocks, sources, tasks, cuts, starts, maps) = parse_doc(src, true);
            Parsed { doc: (Rc::new(blocks), sources.into(), tasks.into(), starts.into(), maps.into()), cuts: cuts.into() }
        });
        // A stream's earlier string is dead once it grows, unless something drew it this
        // commit; left in, a long stream would fill the budget with its prefixes.
        if let Some(last) = grown
            && !c.docs.used_now(&*last)
        {
            c.docs.remove(&*last);
        }
        let key: Rc<str> = src.into();
        // Parsed text is about as long as its source, plus each block's bookkeeping.
        let bytes = 2 * src.len() + 64 * parsed.cuts.len() + 128;
        c.docs.insert(key.clone(), parsed.clone(), bytes);
        c.last = Some(key);
        parsed.doc
    })
}

/// The parse of `src`, which extends the `base_len` bytes `base` parsed, reusing every
/// top-level block before `base`'s last: appended text can change only the last block or add
/// blocks after it. Whether a line starts a block can depend on the rest of that line (`|`
/// ending a table that `| 1 |` would continue), so the reparse starts at the last block that
/// starts on a finished line. A reference definition (`[x]: url`) can change links anywhere,
/// so a source that may hold one parses whole.
fn extend(base: &Parsed, base_len: usize, src: &str) -> Option<Parsed> {
    let i = base.cuts.iter().rposition(|c| src[c.at..base_len].contains('\n'))?;
    let cut = base.cuts[i];
    if cut.at == 0 || cut.blocks >= base.doc.0.len() || src.contains("]:") {
        return None;
    }
    #[cfg(test)]
    PARSES.with(|p| p.set((p.get().0, p.get().1 + 1)));
    let (tail, sources, tasks, cuts, starts, maps) = parse_doc(&src[cut.at..], true);
    let mut blocks = base.doc.0[..cut.blocks].to_vec();
    blocks.extend(tail);
    let sources: Rc<[Source]> = base.doc.1[..cut.sources].iter().cloned().chain(sources).collect();
    let tasks = base.doc.2[..cut.tasks].iter().copied()
        .chain(tasks.into_iter().map(|t| t.map(|(checked, at)| (checked, at + cut.at)))).collect();
    let starts = base.doc.3[..cut.blocks].iter().copied().chain(starts.into_iter().map(|at| at + cut.at)).collect();
    let maps = base.doc.4[..cut.blocks].iter().cloned()
        .chain(maps.into_iter().map(|m| m.shift_src(cut.at))).collect();
    let cuts = base.cuts[..i]
        .iter()
        .copied()
        .chain(cuts.into_iter().map(|c| Cut {
            at: c.at + cut.at,
            blocks: c.blocks + cut.blocks,
            sources: c.sources + cut.sources,
            tasks: c.tasks + cut.tasks,
        }))
        .collect();
    Some(Parsed { doc: (Rc::new(blocks), sources, tasks, starts, maps), cuts })
}

/// How markdown looks. [`MarkdownStyle::from_kit`] takes every colour and font from the kit's
/// tokens, [`MarkdownStyle::neutral`] and [`MarkdownStyle::from_seed`] need no kit; each builder
/// overrides one thing.
#[derive(Clone, Debug)]
pub struct MarkdownStyle {
    text: Color,
    muted: Color,
    accent: Color,
    link_underline: Color,
    code_bg: Color,
    code_block_fill: Color,
    code_block_border: Color,
    quote_rule: Color,
    rule: Color,
    body: TextStyle,
    headings: [TextStyle; 3],
    code_block: TextStyle,
    inline_code_scale: f32,
    inline_code_weight: f32,
    strong_weight: f32,
    table_outline: Option<Color>,
    table_rule: Color,
    table_head_fill: Color,
    table_head_text: Color,
    table_head_rule: Option<Color>,
    table_head_weight: Option<f32>,
    table_min_column: f32,
    table_tabular: bool,
    /// Cells' font before tabular figures and the header's weight (`None`: the body's).
    table_font: Option<TextStyle>,
    /// Cell padding (x, y), and whether the first column has none on its left.
    table_pad: [f32; 2],
    table_flush_left: bool,
    diff_add: Color,
    diff_remove: Color,
    diff_add_text: Color,
    diff_remove_text: Color,
    diff_hues: [Color; 2],
    diff_hunk: Color,
    syntax: SyntaxTheme,
    highlight: bool,
    selection: Color,
    highlight_bg: Color,
    highlight_fg: Option<Color>,
    highlight_ring: Option<(Color, f32)>,
    highlight_current_bg: Color,
    highlight_current_fg: Option<Color>,
    highlight_current_ring: Option<(Color, f32)>,
    task: TaskPaint,
    pub(crate) diagram: DiagramPaint,
}

/// Colours, icon and motion of a GFM task list checkbox (matching [`crate::Checkbox`]).
#[derive(Clone, Copy, Debug)]
struct TaskPaint {
    fill_on: Color,
    border_on: Color,
    border_off: Color,
    hover: Color,
    focus_ring: Color,
    mark: Color,
    check: &'static Icon,
    radius: f32,
    motion: Spring,
}

/// Colours and sizes diagrams draw with (see [`MarkdownStyle::diagram`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DiagramPaint {
    pub text: Color,
    pub muted: Color,
    pub surface: Color,
    pub surface_border: Color,
    pub node_fill: Color,
    pub node_border: Color,
    pub line: Color,
    pub cluster_fill: Color,
    pub note_fill: Color,
    pub note_border: Color,
    pub font: TextStyle,
    pub max_width: f32,
}

/// `t` of the way from `base` toward `hue` in Oklab, at `base`'s alpha: a quiet wash that keeps
/// `base`'s lightness close in both themes.
fn tint(base: Color, hue: Color, t: f32) -> Color {
    let [a, b] = [base.to_oklab(), hue.to_oklab()];
    Color::from_oklab([0, 1, 2, 3].map(|i| if i == 3 { a[3] } else { a[i] + (b[i] - a[i]) * t }))
}

impl MarkdownStyle {
    pub fn from_kit(k: &Kit) -> MarkdownStyle {
        let c = &k.c;
        let body = TextStyle { line_height: 22.4, ..k.ty.body };
        let heading = |size, lh| TextStyle { size, weight: 600.0, line_height: lh, ..body };
        MarkdownStyle {
            text: c.text_base,
            muted: c.text_muted,
            accent: c.text_accent,
            link_underline: k.alpha(c.text_accent, 0.4),
            code_bg: k.alpha(c.text_base, 0.08),
            code_block_fill: c.layer_01,
            code_block_border: c.border_muted,
            quote_rule: c.layer_03,
            rule: c.border_base,
            body,
            headings: [heading(17.0, 24.0), heading(15.0, 24.0), heading(13.0, 20.0)],
            code_block: TextStyle { size: 13.0, line_height: 20.0, ..k.ty.mono },
            inline_code_scale: 0.9,
            inline_code_weight: 400.0,
            strong_weight: 670.0,
            table_outline: Some(c.border_muted),
            table_rule: c.border_muted,
            table_head_fill: c.layer_01,
            table_head_text: c.text_base,
            table_head_rule: None,
            table_head_weight: None,
            table_min_column: 120.0,
            table_tabular: true,
            table_font: None,
            table_pad: [10.0, 6.0],
            table_flush_left: false,
            diff_add: tint(c.layer_01, c.success.icon, if k.dark { 0.11 } else { 0.14 }),
            diff_remove: tint(c.layer_01, c.danger.icon, if k.dark { 0.11 } else { 0.14 }),
            diff_add_text: c.text_base,
            diff_remove_text: c.text_base,
            diff_hues: [c.success.icon, c.danger.icon],
            diff_hunk: c.text_muted,
            syntax: SyntaxTheme::from_kit(k),
            highlight: true,
            selection: k.selection(),
            highlight_bg: if k.dark { k.alpha(c.warning.icon, 0.24) } else { rune::oklcha(0.86, 0.15, 85.0, 0.42) },
            highlight_fg: None,
            highlight_ring: None,
            highlight_current_bg: if k.dark { k.alpha(c.warning.icon, 0.40) } else { rune::oklcha(0.82, 0.17, 75.0, 0.58) },
            highlight_current_fg: None,
            highlight_current_ring: Some((if k.dark { c.warning.icon } else { rune::oklch(0.66, 0.16, 70.0) }, 1.0)),
            task: TaskPaint {
                fill_on: c.surface_weak,
                border_on: c.std_border,
                border_off: c.border_weak,
                hover: c.surface_hover,
                focus_ring: c.border_active,
                mark: c.std_icon,
                check: k.glyph.check,
                radius: k.radius.sm,
                motion: k.motion.control,
            },
            diagram: DiagramPaint {
                text: c.text_base,
                muted: c.text_muted,
                surface: c.layer_01,
                surface_border: c.border_muted,
                node_fill: c.bg_base,
                node_border: k.alpha(c.text_base, 0.32),
                line: k.alpha(c.text_base, 0.55),
                cluster_fill: k.alpha(c.text_base, 0.04),
                note_fill: c.warning.bg,
                note_border: c.warning.border,
                font: TextStyle { size: 13.0, line_height: 18.0, ..k.ty.body },
                max_width: 680.0,
            },
        }
    }
    /// Rune's neutral light or dark look ([`Kit::neutral`]'s tokens), for apps without a kit.
    pub fn neutral(dark: bool) -> MarkdownStyle {
        MarkdownStyle::from_kit(&Kit::neutral(dark))
    }
    /// Derived from three colours, the way [`Kit::from_palette`] derives a kit.
    pub fn from_seed(seed: PaletteSeed) -> MarkdownStyle {
        MarkdownStyle::from_kit(&Kit::from_palette(seed))
    }
    /// Body, heading (levels 1 to 3 and the muted level 4+) and emphasis colour.
    pub fn text(mut self, c: Color) -> Self {
        self.text = c;
        self
    }
    /// Level 4+ headings and list markers.
    pub fn muted(mut self, c: Color) -> Self {
        self.muted = c;
        self
    }
    /// Link text; the underline is the same colour at 40% unless set with [`Self::link_underline`].
    pub fn link(mut self, c: Color) -> Self {
        self.accent = c;
        self.link_underline = c.alpha(0.4);
        self
    }
    pub fn link_underline(mut self, c: Color) -> Self {
        self.link_underline = c;
        self
    }
    /// The chip behind inline code.
    pub fn inline_code_bg(mut self, c: Color) -> Self {
        self.code_bg = c;
        self
    }
    pub fn code_block(mut self, fill: Color, border: Color) -> Self {
        self.code_block_fill = fill;
        self.code_block_border = border;
        self
    }
    pub fn quote_rule(mut self, c: Color) -> Self {
        self.quote_rule = c;
        self
    }
    pub fn rule(mut self, c: Color) -> Self {
        self.rule = c;
        self
    }
    /// Paragraphs, list items and list markers.
    pub fn body_font(mut self, f: TextStyle) -> Self {
        self.body = f;
        self
    }
    /// The font of heading `level` (1 to 3; level 4 and deeper share 3).
    pub fn heading_font(mut self, level: u8, f: TextStyle) -> Self {
        self.headings[(level.clamp(1, 3) - 1) as usize] = f;
        self
    }
    /// Code blocks and diffs. Inline code is set in the app's mono slot, so keep this on
    /// `Family::Mono` for the two to match; for the platform's monospaced font (SF Mono) in
    /// both, name `ui-monospace` in the mono slot of `AppSpec::families`.
    pub fn code_block_font(mut self, f: TextStyle) -> Self {
        self.code_block = f;
        self
    }
    /// Inline code's size as a multiple of the surrounding text, and its weight (the family is
    /// always the mono slot's).
    pub fn inline_code_font(mut self, scale: f32, weight: f32) -> Self {
        self.inline_code_scale = scale;
        self.inline_code_weight = weight;
        self
    }
    pub fn strong_weight(mut self, w: f32) -> Self {
        self.strong_weight = w;
        self
    }
    /// A table's outline and row rules, and its header row's fill.
    pub fn table(mut self, border: Color, header_fill: Color) -> Self {
        self.table_outline = Some(border);
        self.table_rule = border;
        self.table_head_fill = header_fill;
        self
    }
    /// The rounded outline around a table, or `None` for none; a rule then runs under the
    /// last row too, as on a web page.
    pub fn table_outline(mut self, outline: Option<Color>) -> Self {
        self.table_outline = outline;
        self
    }
    /// The hairline under the header and between rows.
    pub fn table_rules(mut self, c: Color) -> Self {
        self.table_rule = c;
        self
    }
    /// The header row's text colour (the body text colour by default).
    pub fn table_header_text(mut self, c: Color) -> Self {
        self.table_head_text = c;
        self
    }
    /// The header row's fill alone (`table` sets it with the border).
    pub fn table_header_fill(mut self, c: Color) -> Self {
        self.table_head_fill = c;
        self
    }
    /// The hairline under the header row, when it should stand out from the row rules, as a
    /// web `th { border-bottom }` in a stronger colour than the `td`s'. The row rule colour
    /// by default.
    pub fn table_header_rule(mut self, c: Color) -> Self {
        self.table_head_rule = Some(c);
        self
    }
    /// The header row's weight (the strong weight by default), for a medium-weight `th` beside
    /// bold `strong` text.
    pub fn table_header_weight(mut self, w: f32) -> Self {
        self.table_head_weight = Some(w);
        self
    }
    /// Whether cells set figures at one width (OpenType `tnum`) so numbers line up down a
    /// column. On by default; only digits change.
    pub fn table_tabular_figures(mut self, on: bool) -> Self {
        self.table_tabular = on;
        self
    }
    /// The narrowest a column shrinks to when its longest word is wider (120 points by default).
    pub fn table_min_column(mut self, points: f32) -> Self {
        self.table_min_column = points.max(0.0);
        self
    }
    /// The cells' font (the body font by default). The header takes it at the strong weight,
    /// and inline code scales from its size. A web table at `font-size: 0.93em` is the body
    /// style at 0.93 of its size.
    pub fn table_font(mut self, f: TextStyle) -> Self {
        self.table_font = Some(f);
        self
    }
    /// Each cell's padding: `x` on the left and right, `y` above and below (10 and 6 points by
    /// default).
    pub fn table_cell_padding(mut self, x: f32, y: f32) -> Self {
        self.table_pad = [x.max(0.0), y.max(0.0)];
        self
    }
    /// Whether the first column starts at the table's left edge, with no left padding, so its
    /// text lines up with the paragraphs around it, as a web table whose first cells have
    /// `padding-left: 0`. Off by default.
    pub fn table_flush_left(mut self, on: bool) -> Self {
        self.table_flush_left = on;
        self
    }
    /// Tables that read like a data [`Table`](crate::Table) beside them: its cell font, cell
    /// padding (sized so a one-line row is the data table's row height), rules and muted header
    /// text. The outline, when there is one, takes the rule colour.
    pub fn table_like(mut self, t: &crate::TableStyle) -> Self {
        self.table_font = Some(t.font);
        self.table_pad = [t.cell_pad, ((t.row_height - t.font.line_height) / 2.0).max(0.0)];
        self.table_rule = t.rule;
        self.table_head_text = t.header_text;
        self.table_outline = self.table_outline.map(|_| t.rule);
        self
    }
    /// The tints behind a diff's added and removed lines (full width).
    pub fn diff(mut self, added: Color, removed: Color) -> Self {
        self.diff_add = added;
        self.diff_remove = removed;
        self
    }
    /// The text colours of a diff's added and removed lines (the body text colour by default).
    pub fn diff_text(mut self, added: Color, removed: Color) -> Self {
        self.diff_add_text = added;
        self.diff_remove_text = removed;
        self
    }
    /// Tints added and removed lines' text `amount` (0 to 1) of the way toward the theme's green
    /// and red, from the text colour set so far; around 0.5 reads clearly in both themes.
    pub fn diff_text_tint(mut self, amount: f32) -> Self {
        let t = amount.clamp(0.0, 1.0);
        self.diff_add_text = tint(self.text, self.diff_hues[0], t);
        self.diff_remove_text = tint(self.text, self.diff_hues[1], t);
        self
    }
    /// The text colour of a diff's `@@` hunk headers and file header lines.
    pub fn diff_hunk(mut self, c: Color) -> Self {
        self.diff_hunk = c;
        self
    }
    /// The colour of one kind of token in highlighted code blocks.
    pub fn syntax(mut self, kind: Syntax, c: Color) -> Self {
        self.syntax = self.syntax.with(kind, c);
        self
    }
    /// Every token colour at once (by default [`SyntaxTheme::from_kit`]).
    pub fn syntax_theme(mut self, theme: SyntaxTheme) -> Self {
        self.syntax = theme;
        self
    }
    /// Whether code blocks are coloured (on by default): fences in a language the highlighter
    /// knows (see [`Markdown::highlight`]). Off, every code block draws in the text colour.
    pub fn syntax_highlight(mut self, on: bool) -> Self {
        self.highlight = on;
        self
    }
    /// The highlight of [`Markdown::selectable`] text (the kit's field selection by default).
    pub fn selection(mut self, c: Color) -> Self {
        self.selection = c;
        self
    }
    /// Diagram nodes (and sequence participants): their fill and outline, and the colour of
    /// edges, arrows and lifelines.
    pub fn diagram(mut self, node_fill: Color, node_border: Color, line: Color) -> Self {
        self.diagram.node_fill = node_fill;
        self.diagram.node_border = node_border;
        self.diagram.line = line;
        self
    }
    /// The card a diagram sits on, and the tint behind flowchart subgraphs.
    pub fn diagram_surface(mut self, fill: Color, border: Color, subgraph: Color) -> Self {
        self.diagram.surface = fill;
        self.diagram.surface_border = border;
        self.diagram.cluster_fill = subgraph;
        self
    }
    /// Sequence diagram notes.
    pub fn diagram_note(mut self, fill: Color, border: Color) -> Self {
        self.diagram.note_fill = fill;
        self.diagram.note_border = border;
        self
    }
    /// The font of diagram labels (13 points of the body face by default).
    pub fn diagram_font(mut self, f: TextStyle) -> Self {
        self.diagram.font = f;
        self
    }
    /// The widest a diagram draws, in points (680 by default). A wider left-to-right flowchart
    /// is laid out top-down instead; anything still wider is drawn smaller, down to 60%.
    pub fn diagram_max_width(mut self, points: f32) -> Self {
        self.diagram.max_width = points.max(120.0);
        self
    }
    /// Background fill of standard [`Markdown::highlights`] matches.
    pub fn highlight_bg(mut self, c: Color) -> Self {
        self.highlight_bg = c;
        self
    }
    /// Optional glyph foreground colour of standard [`Markdown::highlights`] matches.
    pub fn highlight_fg(mut self, c: Option<Color>) -> Self {
        self.highlight_fg = c;
        self
    }
    /// Optional inset border ring `(color, width_pt)` of standard [`Markdown::highlights`] matches.
    pub fn highlight_ring(mut self, ring: Option<(Color, f32)>) -> Self {
        self.highlight_ring = ring;
        self
    }
    /// Background fill of [`Highlight::current`] matches.
    pub fn highlight_current_bg(mut self, c: Color) -> Self {
        self.highlight_current_bg = c;
        self
    }
    /// Optional glyph foreground colour of [`Highlight::current`] matches.
    pub fn highlight_current_fg(mut self, c: Option<Color>) -> Self {
        self.highlight_current_fg = c;
        self
    }
    /// Optional inset border ring `(color, width_pt)` of [`Highlight::current`] matches.
    pub fn highlight_current_ring(mut self, ring: Option<(Color, f32)>) -> Self {
        self.highlight_current_ring = ring;
        self
    }
}

/// How a source byte range in [`Markdown::highlights`] is painted.
///
/// [`Highlight::default()`] (or [`Highlight::new()`]) draws a standard match background from
/// [`MarkdownStyle`]; [`Highlight::current()`] draws the active match with a stronger background
/// and an inset border ring. Either may be customized with [`Highlight::bg`], [`Highlight::fg`],
/// [`Highlight::ring`] and [`Highlight::key`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Highlight {
    pub(crate) current: bool,
    pub(crate) bg: Option<Color>,
    pub(crate) fg: Option<Color>,
    pub(crate) ring: Option<(Color, f32)>,
    pub(crate) key: Option<String>,
}

impl Highlight {
    /// A standard match highlight using the theme's [`MarkdownStyle::highlight_bg`].
    pub fn new() -> Highlight {
        Highlight::default()
    }
    /// An active/current match highlight using the theme's [`MarkdownStyle::highlight_current_bg`]
    /// and [`MarkdownStyle::highlight_current_ring`].
    pub fn current() -> Highlight {
        Highlight { current: true, ..Highlight::default() }
    }
    /// Alias for [`Highlight::current`].
    pub fn active() -> Highlight {
        Highlight::current()
    }
    /// Sets whether this highlight uses the active/current match style.
    pub fn current_match(mut self, current: bool) -> Self {
        self.current = current;
        self
    }
    /// Whether this highlight uses the active/current match style.
    pub fn is_current(&self) -> bool {
        self.current
    }
    /// Overrides the rounded background colour behind the highlighted glyphs.
    pub fn bg(mut self, c: Color) -> Self {
        self.bg = Some(c);
        self
    }
    /// Overrides the glyph foreground colour over the highlighted range.
    pub fn fg(mut self, c: Color) -> Self {
        self.fg = Some(c);
        self
    }
    /// Alias for [`Highlight::fg`].
    pub fn color(self, c: Color) -> Self {
        self.fg(c)
    }
    /// Overrides the inset border ring `(color, width_pt)` inside the rounded background.
    pub fn ring(mut self, color: Color, width: f32) -> Self {
        self.ring = (width > 0.0 && width.is_finite()).then_some((color, width));
        self
    }
    /// Sets a key for [`Tx::scroll_into_view`](rune::Tx::scroll_into_view),
    /// [`Cx::layout`](rune::Cx::layout) and [`Runtime::find`](rune::Runtime::find). When unset,
    /// the highlight is reachable under [`Markdown::highlight_key`]`(index)`.
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }
}

/// An item accepted by [`Markdown::highlights`]: either `(Range<usize>, Highlight)` by value or
/// `&(Range<usize>, Highlight)` from a slice.
pub trait HighlightItem {
    fn into_highlight(self) -> (Range<usize>, Highlight);
}

impl HighlightItem for (Range<usize>, Highlight) {
    fn into_highlight(self) -> (Range<usize>, Highlight) {
        self
    }
}

impl HighlightItem for &(Range<usize>, Highlight) {
    fn into_highlight(self) -> (Range<usize>, Highlight) {
        self.clone()
    }
}

type LinkWire = Rc<dyn Fn(El) -> El>;
type LinkLook = Rc<dyn Fn(&str) -> Option<(Span, LinkPaint)>>;
type TaskWire = Rc<dyn Fn(El, Rc<str>, usize, bool) -> El>;
type LinkRing = Rc<dyn Fn(&str) -> Option<(Color, f32)>>;

/// Markdown source as an element. See the module docs for cost and keys.
pub struct Markdown {
    src: String,
    style: Option<MarkdownStyle>,
    block_keys: Vec<(usize, String)>,
    highlights: Vec<(Range<usize>, Highlight)>,
    autolink: bool,
    reveal: f32,
    streaming: bool,
    on_link: Option<LinkWire>,
    link_look: Option<LinkLook>,
    on_task: Option<TaskWire>,
    tasks_read_only: bool,
    link_ring: Option<LinkRing>,
    selectable: bool,
    scroll_wide: bool,
    highlighter: Highlighter,
}

impl Markdown {
    pub fn new(src: impl Into<String>) -> Markdown {
        Markdown { src: src.into(), style: None, block_keys: Vec::new(), highlights: Vec::new(), autolink: true, reveal: 0.0, streaming: false, on_link: None, link_look: None, link_ring: None,
            on_task: None, tasks_read_only: false, selectable: false, scroll_wide: false, highlighter: Highlighter::builtin() }
    }
    /// Highlight UTF-8 byte ranges of the Markdown source string.
    ///
    /// Ranges refer to the source passed to [`Markdown::new`] (as `pulldown_cmark`'s offset
    /// iterator reports them) and map across inline formatting, code blocks, headings, lists,
    /// blockquotes and tables without reshaping text or shifting layout. Each highlight is
    /// reachable by [`Tx::scroll_into_view`](rune::Tx::scroll_into_view) and
    /// [`Cx::layout`](rune::Cx::layout) under its [`Highlight::key`], or under
    /// [`Markdown::highlight_key`]`(i)` when no key was set on it.
    /// Backgrounds, rings and foreground colours are paint-only. Matches add no layout nodes;
    /// [`Runtime::elements`](rune::Runtime::elements) reports a `mark` per rendered fragment.
    /// Empty/out-of-source ranges paint nothing; partial UTF-8 characters expand to boundaries.
    pub fn highlights(mut self, highlights: impl IntoIterator<Item = impl HighlightItem>) -> Self {
        self.highlights = highlights.into_iter().map(HighlightItem::into_highlight).collect();
        self
    }
    /// Default key (`"md-mark-{index}"`) for the `index`-th highlight in [`Self::highlights`]
    /// when [`Highlight::key`] was not set on it.
    pub fn highlight_key(index: usize) -> String {
        format!("md-mark-{index}")
    }
    /// Whether bare `https://…`, `http://…` and `www.…` URLs in plain text are autolinked
    /// (`true` by default).
    /// This is URL-only: bare email addresses remain text; `<name@example.org>` still links.
    pub fn autolink(mut self, on: bool) -> Self {
        self.autolink = on;
        self
    }
    /// Stable keys as `(source_byte_offset, key)` pairs. The offset is the start of the first
    /// source line of a rendered top-level block (including any indentation), not its ordinal.
    /// Recompute offsets against the current source after edits; keep the IDs themselves stable.
    /// Unmatched offsets are ignored: differing parser boundaries cannot shift later IDs.
    /// Missing, empty or duplicate IDs fall back to an unused `b{index}`. For duplicate offsets,
    /// the first pair wins. Nested blocks keep index keys beneath their top-level parent's key.
    /// Retained blocks spring to new positions and sizes; additions enter and removals stay
    /// mounted during their exit transition.
    pub fn block_keys<I, K>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = (usize, K)>,
        K: Into<String>,
    {
        self.block_keys = keys.into_iter().map(|(at, key)| (at, key.into())).collect();
        self
    }
    /// Replaces the kit-derived style in [`Self::build`].
    pub fn style(mut self, style: MarkdownStyle) -> Self {
        self.style = Some(style);
        self
    }
    /// Streaming word reveal in seconds (see [`El::reveal`]); 0 shows words at once.
    pub fn reveal(mut self, word_fade: f32) -> Self {
        self.reveal = word_fade;
        self
    }
    /// The source is still arriving: a table's last row, with no newline after it yet, is left
    /// out of the column widths until it ends. Off, the text is whole.
    pub fn streaming(mut self, on: bool) -> Self {
        self.streaming = on;
        self
    }
    /// Called with a link's destination when it is clicked, or pressed with Enter or Space
    /// while focused. With a handler, links show a hand and a solid underline under the
    /// pointer and are Tab stops; without one they are styled text only. Shares its slot with
    /// [`Self::on_link_tx`] (the later call wins).
    pub fn on_link<S: 'static>(self, f: impl Fn(&mut S, &str) + 'static) -> Self {
        self.on_link_tx(move |s: &mut S, url, _| f(s, url))
    }
    /// [`Self::on_link`] with the [`Tx`] of the press, for a handler that opens a screen, scrolls
    /// or copies in the same commit (an app's own `scheme:id` links, say). Replaces any earlier
    /// `on_link` or `on_link_tx` handler (the later call wins).
    pub fn on_link_tx<S: 'static>(mut self, f: impl Fn(&mut S, &str, &mut Tx) + 'static) -> Self {
        let f = Rc::new(f);
        self.on_link = Some(Rc::new(move |e: El| {
            let f = f.clone();
            e.on_link(move |s: &mut S, url, tx| f(s, url, tx))
        }));
        self
    }
    /// A look for some links by destination. `f` returns `None` for the default look
    /// ([`MarkdownStyle::link`] with its underline, and an accent underline on hover), or
    /// `Some((resting, hover))`: the [`Span`] drawn over the link's text (colour, weight,
    /// `bg` with its padding, underline; its range is ignored, and emphasis, strong and inline
    /// code inside the link keep their styles over it) and the [`LinkPaint`] over it while
    /// hovered. Applies with or without a handler.
    pub fn link_style(mut self, f: impl Fn(&str) -> Option<(Span, LinkPaint)> + 'static) -> Self {
        self.link_look = Some(Rc::new(f));
        self
    }
    /// Called when a GFM task list item (`- [ ]` or `- [x]`) is clicked, or pressed with Space
    /// while focused: `(state, block_key, marker_offset, checked)`. Enter does not toggle.
    /// `block_key` is the top-level block's key ([`Self::block_keys`], or `b{index}`),
    /// `marker_offset` is the byte offset of `[` in the source's `[ ]`/`[x]` marker, including
    /// nested items, and `checked` is the new state. The offset refers to the source used to
    /// build this view, so the caller can replace `source[offset + 1..offset + 2]`.
    /// Without a handler, or with [`Self::tasks_read_only`], task checkboxes
    /// are read-only. Shares its slot with [`Self::on_task_tx`] (the later call wins).
    pub fn on_task<S: 'static>(self, f: impl Fn(&mut S, &str, usize, bool) + 'static) -> Self {
        self.on_task_tx(move |s: &mut S, key, item, checked, _| f(s, key, item, checked))
    }
    /// [`Self::on_task`] with the [`Tx`] of the press. Replaces any earlier `on_task` or
    /// `on_task_tx` handler (the later call wins).
    pub fn on_task_tx<S: 'static>(mut self, f: impl Fn(&mut S, &str, usize, bool, &mut Tx) + 'static) -> Self {
        let f = Rc::new(f);
        self.on_task = Some(Rc::new(move |e: El, key: Rc<str>, item: usize, checked: bool| {
            let f = f.clone();
            e.on_press_tx(move |s: &mut S, tx| f(s, &key, item, checked, tx))
        }));
        self
    }
    /// Keeps GFM task list checkboxes read-only (unfocusable and unpressable), even when an
    /// [`Self::on_task`] or [`Self::on_task_tx`] handler was set.
    pub fn tasks_read_only(mut self) -> Self {
        self.tasks_read_only = true;
        self
    }
    /// An inset border ring `(color, width_pt)` inside the rounded [`Span::bg`] of chosen links
    /// by destination (`None`: no ring). Stays on hover unless the link's hover [`LinkPaint`]
    /// sets its own [`LinkPaint::ring`]. Applies with or without a handler.
    /// Links without a background draw no resting ring; use [`Self::link_style`] to set one.
    pub fn link_ring(mut self, f: impl Fn(&str) -> Option<(Color, f32)> + 'static) -> Self {
        self.link_ring = Some(Rc::new(f));
        self
    }
    /// Lets people select and copy the text (see [`El::selectable`]) as one selection across
    /// paragraphs, headings, list items, quotes, code blocks and table cells
    /// ([`El::selection_group`]): Cmd+A selects the whole block, and Cmd+C copies the text as
    /// drawn, a newline between blocks and a tab between table cells, without list markers or
    /// code fences. Links still open on a click. The highlight is [`MarkdownStyle::selection`].
    pub fn selectable(mut self, on: bool) -> Self {
        self.selectable = on;
        self
    }

    /// Code blocks keep their lines whole and tables their columns' natural widths, each
    /// scrolling sideways in its own [`El::scroll_x`] viewport when wider than the text column,
    /// with soft left and right edges on code. Off (the default), code wraps and columns shrink.
    pub fn scroll_wide(mut self, on: bool) -> Self {
        self.scroll_wide = on;
        self
    }

    /// Colours code blocks with `f` instead of the built-in scanners: it gets a fence's language
    /// as written (its info string's first word; `None` for an indented block or a bare fence)
    /// and the code, and returns byte ranges of the code with a token kind, which the style's
    /// [`SyntaxTheme`] colours, or a colour of its own. See [`Highlighter`] for caching, and
    /// [`syntax::builtin`](crate::syntax::builtin) to fall back on for other languages.
    ///
    /// ```ignore
    /// Markdown::new(src).highlight(|lang, code| match lang {
    ///     Some("kotlin") => my_kotlin(code),
    ///     _ => rune_kit::syntax::builtin(lang, code),
    /// })
    /// ```
    pub fn highlight(self, f: impl Fn(Option<&str>, &str) -> Vec<crate::syntax::Highlight> + 'static) -> Self {
        self.highlighter(Highlighter::new(f))
    }

    /// Colours code blocks with `h` (by default [`Highlighter::builtin`]).
    pub fn highlighter(mut self, h: Highlighter) -> Self {
        self.highlighter = h;
        self
    }

    pub fn build(mut self, k: &Kit) -> El {
        let style = self.style.take().unwrap_or_else(|| MarkdownStyle::from_kit(k));
        self.render(&style)
    }

    /// Builds with `style`, for apps without a [`Kit`] (see [`MarkdownStyle::neutral`] and
    /// [`MarkdownStyle::from_seed`]).
    pub fn render(self, style: &MarkdownStyle) -> El {
        let (blocks, sources, tasks, starts, maps) = if self.autolink {
            doc_cached(&self.src)
        } else {
            let (b, s, t, _, st, m) = parse_doc(&self.src, false);
            (Rc::new(b), s.into(), t.into(), st.into(), m.into())
        };
        let keys = if self.block_keys.is_empty() { Vec::new() } else {
            starts.iter().map(|at| self.block_keys.iter().find(|(offset, _)| offset == at)
                .map_or_else(String::new, |(_, key)| key.clone())).collect::<Vec<_>>()
        };
        let r = Renderer { s: style.clone(), highlights: self.highlights, reveal: self.reveal, streaming: self.streaming, on_link: self.on_link, link_look: self.link_look, link_ring: self.link_ring,
            on_task: self.on_task, tasks_read_only: self.tasks_read_only, selectable: self.selectable, scroll_wide: self.scroll_wide,
            highlighter: self.highlighter, sources, next_source: Cell::new(0), tasks, next_task: Cell::new(0),
            current_block_key: RefCell::new("b0".into()) };
        r.blocks(&blocks, &maps, &keys, true).selection_group(self.selectable)
    }
}

struct Renderer {
    s: MarkdownStyle,
    highlights: Vec<(Range<usize>, Highlight)>,
    reveal: f32,
    streaming: bool,
    on_link: Option<LinkWire>,
    link_look: Option<LinkLook>,
    on_task: Option<TaskWire>,
    tasks_read_only: bool,
    link_ring: Option<LinkRing>,
    selectable: bool,
    /// `Markdown::scroll_wide`.
    scroll_wide: bool,
    highlighter: Highlighter,
    sources: Rc<[Source]>,
    /// The next of `sources`, advanced at each [`Block::Code`] in document order.
    next_source: Cell<usize>,
    tasks: Rc<[Option<(bool, usize)>]>,
    /// The next of `tasks`, advanced at each list item in document order.
    next_task: Cell<usize>,
    current_block_key: RefCell<Rc<str>>,
}

fn block_key(i: usize, keys: &[String], used: &mut Vec<String>) -> String {
    if let Some(k) = keys.get(i).filter(|k| !k.is_empty() && !used.iter().any(|u| u == *k)) {
        used.push(k.clone());
        return k.clone();
    }
    let taken = |cand: &str, used: &[String]| {
        used.iter().any(|u| u == cand) || keys.get(i + 1..).is_some_and(|rest| rest.iter().any(|k| k == cand))
    };
    let mut cand = format!("b{i}");
    let mut n = 0usize;
    while taken(&cand, used) {
        cand = format!("b{n}");
        n += 1;
    }
    used.push(cand.clone());
    cand
}

/// (margin-top, margin-bottom) per block, collapsed like CSS margins.
fn margins(b: &Block) -> (f32, f32) {
    match b {
        Block::Para(_) => (0.0, 12.0),
        Block::Heading(1, _) => (28.0, 12.0),
        Block::Heading(2, _) => (24.0, 10.0),
        Block::Heading(..) => (20.0, 8.0),
        Block::Code { .. } | Block::Diff(_) | Block::Mermaid(_) => (12.0, 24.0),
        Block::List { .. } => (8.0, 12.0),
        Block::Quote(_) => (16.0, 16.0),
        Block::Rule => (32.0, 32.0),
        Block::Table(_) => (12.0, 16.0),
    }
}

/// Code as rich text with `spans` and `highlights` (byte ranges of `code`). Text layout widens
/// tabs in a monospace `style` to four-column stops itself; in any other family it draws a tab
/// as one space, so each tab is padded with uncopied spaces to the next stop: lines line up and a
/// copy still gets the tab.
fn tabbed(code: &str, mut spans: Vec<Span>, mut highlights: Vec<TextHighlight>, style: TextStyle) -> El {
    if !code.contains('\t') || matches!(style.family, Family::Mono | Family::SystemMono) {
        return rich(code.to_owned(), spans).text_highlights(highlights);
    }
    let mut shown = String::with_capacity(code.len() + code.len() / 8);
    // (source byte, shown byte, pad, padding so far including this tab's)
    let mut tabs: Vec<(usize, usize, usize, usize)> = Vec::new();
    let mut col = 0;
    for (i, c) in code.char_indices() {
        if c == '\t' {
            let pad = 3 - col % 4;
            let total = tabs.last().map_or(0, |t| t.3) + pad;
            tabs.push((i, shown.len(), pad, total));
            shown.push('\t');
            shown.extend(std::iter::repeat_n(' ', pad));
            col += pad + 1;
            continue;
        }
        col = if c == '\n' { 0 } else { col + 1 };
        shown.push(c);
    }
    let shift = |b: u32| match tabs.partition_point(|t| (t.0 as u32) < b) {
        0 => b,
        n => b + tabs[n - 1].3 as u32,
    };
    for sp in &mut spans {
        (sp.start, sp.end) = (shift(sp.start), shift(sp.end));
    }
    for hl in &mut highlights {
        let r = hl.range();
        *hl = hl.clone().with_range(shift(r.start as u32) as usize..shift(r.end as u32) as usize);
    }
    tabs.iter().fold(rich(shown, spans).text_highlights(highlights), |el, t| el.uncopied(t.1 + 1..t.1 + 1 + t.2))
}

impl Renderer {
    fn link_span(&self, a: usize, b: usize, inl: &Inline) -> Span {
        let s = &self.s;
        let url = inl.links.iter().find(|l| (l.0, l.1) == (a, b)).map(|l| l.2.as_str());
        if let Some((own, _)) = self.link_look.as_ref().and_then(|look| url.and_then(|u| look(u))) {
            Span { start: a as u32, end: b as u32, ..own }
        } else {
            Span { color: Some(s.accent), underline: Some(s.link_underline), ..Span::new(a, b) }
        }
    }

    fn spans(&self, inl: &Inline, base: f32) -> Vec<Span> {
        let s = &self.s;
        let links: Vec<Span> = if inl.links.is_empty() {
            Vec::new()
        } else {
            inl.spans.iter().filter(|s| matches!(s.2, Style::Link)).map(|&(a, b, _)| self.link_span(a, b, inl)).collect()
        };
        let mut next_link = 0;
        let mut out = Vec::with_capacity(inl.spans.len());
        for &(a, b, st) in &inl.spans {
            if matches!(st, Style::Link) {
                let sp = links[next_link].clone();
                next_link += 1;
                let at = out.iter().position(|p: &Span| p.start < sp.end && sp.start < p.end).unwrap_or(out.len());
                out.insert(at, sp);
                continue;
            }
            if matches!(st, Style::Strike) {
                continue;
            }
            let in_link = links.iter().find(|l| l.start <= a as u32 && b as u32 <= l.end);
            let mut sp = Span::new(a, b);
            match st {
                Style::Code => {
                    let link_base = in_link.and_then(|l| l.size).unwrap_or(base);
                    sp.mono = true;
                    sp.weight = Some(s.inline_code_weight);
                    sp.size = Some(link_base * s.inline_code_scale);
                    if in_link.is_none_or(|l| l.bg.is_none()) {
                        sp.bg = Some(s.code_bg);
                    }
                }
                Style::CodePad => {
                    if in_link.is_none_or(|l| l.bg.is_none()) {
                        sp.bg = Some(s.code_bg);
                    }
                }
                Style::Strong => sp.weight = Some(s.strong_weight),
                Style::Em => sp.italic = true,
                Style::Strike | Style::Link => unreachable!(),
            }
            out.push(sp);
        }
        out
    }

    /// Resolves `hl` against [`MarkdownStyle`] into a [`TextHighlight`] for `range`.
    fn make_text_highlight(&self, idx: usize, range: Range<usize>, hl: &Highlight) -> TextHighlight {
        let s = &self.s;
        let bg = hl.bg.unwrap_or(if hl.current { s.highlight_current_bg } else { s.highlight_bg });
        let fg = hl.fg.or(if hl.current { s.highlight_current_fg } else { s.highlight_fg });
        let ring = hl.ring.or(if hl.current { s.highlight_current_ring } else { s.highlight_ring });
        let key = hl.key.clone().unwrap_or_else(|| Markdown::highlight_key(idx));
        let mut th = TextHighlight::new(range, bg).key(key);
        if let Some(c) = fg {
            th = th.color(c);
        }
        if let Some((rc, rw)) = ring {
            th = th.ring(rc, rw);
        }
        th
    }

    fn text_highlights_for(&self, text: &str, map: Option<&SpanMap>) -> Vec<TextHighlight> {
        let Some(map) = map.filter(|_| !self.highlights.is_empty()) else { return Vec::new() };
        let mut out = Vec::new();
        for (idx, (src_range, hl)) in self.highlights.iter().enumerate() {
            for r in map.map_range(src_range.clone(), text) {
                out.push(self.make_text_highlight(idx, r, hl));
            }
        }
        out
    }

    /// The single place a link press is connected: each of `inl.links` becomes a pressable
    /// span of `e` (the text element holding it) reporting its destination.
    fn wire_link(&self, mut e: El, inl: &Inline) -> El {
        if let Some(ring) = self.link_ring.as_ref().filter(|_| !inl.links.is_empty()) {
            for (a, b, url) in &inl.links {
                if let Some((color, width)) = ring(url) {
                    e = e.span_ring(*a, *b, color, width);
                }
            }
        }
        let Some(on_link) = self.on_link.as_ref().filter(|_| !inl.links.is_empty()) else { return e };
        let default = LinkPaint::new().underline(self.s.accent);
        let hover = |url: &str| self.link_look.as_ref().and_then(|f| f(url)).map_or(default, |l| l.1);
        let e = inl.links.iter().fold(e, |e, (a, b, url)| e.link(Link::new(*a..*b, url.clone()).hover(hover(url))));
        on_link(e)
    }

    /// `e`, a text, selectable when the markdown is (after its links, so it shows the I-beam).
    fn select(&self, e: El) -> El {
        if self.selectable { e.selectable(true).selection_color(self.s.selection) } else { e }
    }

    /// `e`, the text of `inl`, with its strikethroughs, highlights, links and selectable, copying
    /// without inline code's padding.
    pub(super) fn select_inline(&self, mut e: El, inl: &Inline, map: Option<&SpanMap>) -> El {
        for &(a, b, st) in &inl.spans {
            if matches!(st, Style::Strike) {
                e = e.strike(a, b, None);
            }
        }
        e = e.text_highlights(self.text_highlights_for(&inl.text, map));
        let e = self.select(self.wire_link(e, inl));
        if !self.selectable { return e; }
        inl.spans.iter().filter(|s| matches!(s.2, Style::CodePad)).fold(e, |e, s| e.uncopied(s.0..s.1))
    }

    fn para(&self, inl: &Inline, map: Option<&SpanMap>, style: TextStyle, color: Color) -> El {
        let e = rich(inl.text.clone(), self.spans(inl, style.size)).font(style).color(color).reveal(self.reveal).w_full();
        self.select_inline(e, inl, map)
    }

    fn blocks(&self, blocks: &[Block], maps: &[BlockMap], keys: &[String], top_level: bool) -> El {
        let mut c = col().w_full();
        let mut prev_mb = 0.0f32;
        let mut used: Vec<String> = Vec::new();
        let empty = BlockMap::None;
        for (i, b) in blocks.iter().enumerate() {
            let (mt, mb) = margins(b);
            let top = if i == 0 { 0.0 } else { mt.max(prev_mb) };
            let key = if keys.is_empty() { format!("b{i}") } else { block_key(i, keys, &mut used) };
            if top_level {
                self.current_block_key.replace(key.clone().into());
            }
            c = c.child(self.block(b, maps.get(i).unwrap_or(&empty)).key(key).mt(top));
            prev_mb = mb;
        }
        c
    }

    fn code(&self, code: &str, source: Option<&Source>, map: Option<&SpanMap>) -> El {
        let s = &self.s;
        let highlights = self.text_highlights_for(code, map);
        let body = tabbed(code, self.syntax_spans(code, source), highlights, s.code_block).font(s.code_block).color(s.text).reveal(self.reveal);
        let frame = col().w_full().radius(6.0).fill(s.code_block_fill).border(0.5, s.code_block_border);
        if self.scroll_wide {
            // The text sizes to its longest line inside the viewport, which scrolls under the
            // padding so lines run to the frame's edge.
            return frame.child(col().key("scroll").w_full().scroll_x().scroll_edge_fade(12.0)
                .child(col().padding(12.0).child(self.select(body))));
        }
        frame.padding(12.0).child(self.select(body.w_full()))
    }

    /// Colour-only spans, so highlighting never moves a glyph: a block streaming in draws its
    /// newest line plain and colours it in place once the line ends.
    fn syntax_spans(&self, code: &str, source: Option<&Source>) -> Vec<Span> {
        let Some(src) = source.filter(|_| self.s.highlight) else { return Vec::new() };
        let lang = src.lang.as_deref();
        // The built-in scanners leave other languages plain; skip the cache for those.
        if self.highlighter.is_builtin() && !lang.is_some_and(crate::syntax::known) {
            return Vec::new();
        }
        let Some(prefix) = code.get(..src.coloured) else { return Vec::new() };
        self.s.syntax.spans(&self.highlighter.highlight(lang, prefix), 0)
    }

    /// One row per line, so added and removed lines are tinted edge to edge.
    fn diff(&self, src: &str, map: Option<&SpanMap>) -> El {
        let s = &self.s;
        let all_hl = self.text_highlights_for(src, map);
        let mut c = col().w_full().py(12.0).radius(6.0).fill(s.code_block_fill).border(0.5, s.code_block_border);
        let mut line_start = 0usize;
        for (n, line) in src.split('\n').enumerate() {
            let line_end = line_start + line.len();
            let line_hl: Vec<TextHighlight> = all_hl
                .iter()
                .filter_map(|h| {
                    let r = h.range();
                    let a = r.start.max(line_start);
                    let b = r.end.min(line_end);
                    (a < b).then(|| h.clone().with_range((a - line_start)..(b - line_start)))
                })
                .collect();
            line_start = line_end + 1;
            let header = ["@@", "diff ", "index ", "--- ", "+++ "].iter().any(|p| line.starts_with(p));
            let (tint, color) = match line.as_bytes().first() {
                _ if header => (None, s.diff_hunk),
                Some(b'+') => (Some(s.diff_add), s.diff_add_text),
                Some(b'-') => (Some(s.diff_remove), s.diff_remove_text),
                _ => (None, s.text),
            };
            let shown = if line.is_empty() { " " } else { line };
            let mut r = row()
                .key(format!("l{n}"))
                .w_full()
                .px(12.0)
                .child(self.select(tabbed(shown, Vec::new(), line_hl, s.code_block).font(s.code_block).color(color).wrap().reveal(self.reveal).w_full()));
            if let Some(t) = tint {
                r = r.fill(t);
            }
            c = c.child(r);
        }
        c
    }

    #[cfg_attr(not(feature = "mermaid"), allow(clippy::unused_self))]
    fn mermaid(&self, src: &str, map: Option<&SpanMap>) -> El {
        #[cfg(feature = "mermaid")]
        {
            match crate::mermaid::diagram(src, &self.s.diagram) {
                Some(d) => d,
                None => col()
                    .w_full()
                    .gap(6.0)
                    .child(self.code(src, None, map).key("source"))
                    .child(text("Diagram not supported").font(TextStyle { size: 12.0, line_height: 16.0, ..self.s.body }).color(self.s.muted).key("hint")),
            }
        }
        #[cfg(not(feature = "mermaid"))]
        self.code(src, None, map)
    }

    fn task_item(&self, k: usize, item: &[Block], item_maps: &[BlockMap], item_idx: usize, checked: bool) -> El {
        let t = &self.s.task;
        let interactive = self.on_task.is_some() && !self.tasks_read_only;
        let (fill, border) = if checked {
            (t.fill_on, t.border_on)
        } else {
            (Color::TRANSPARENT, t.border_off)
        };
        let mut control = el()
            .key("control")
            .size(16.0, 16.0)
            .ml(8.0)
            .mr(8.0)
            .mt(((self.s.body.line_height - 16.0) * 0.5).max(0.0))
            .center()
            .radius(t.radius)
            .fill(fill)
            .border(1.0, border)
            .motion(t.motion)
            .focus_ring(1.0, 0.0, t.focus_ring)
            .child(icon(t.check, 10.0).key("mark").color(t.mark).opacity(if checked { 1.0 } else { 0.0 }));
        if interactive {
            control = control.hover(t.hover);
        }
        let mut r = row()
            .key(format!("i{k}"))
            .role("checkbox")
            .label(item_label(item))
            .focus_selected(checked)
            .focusable(interactive)
            .w_full()
            .child(control)
            .child(self.blocks(item, item_maps, &[], false).grow(1.0).shrink());
        if let Some(on_task) = self.on_task.as_ref().filter(|_| !self.tasks_read_only) {
            let block_key = self.current_block_key.borrow().clone();
            r = on_task(r.cursor(Cursor::Pointer), block_key, item_idx, !checked);
        }
        r
    }

    fn block(&self, b: &Block, map: &BlockMap) -> El {
        let s = &self.s;
        let inl_map = match map {
            BlockMap::Inline(m) => Some(m),
            _ => None,
        };
        let code_map = match map {
            BlockMap::Code(m) => Some(m),
            _ => None,
        };
        match b {
            Block::Para(inl) => self.para(inl, inl_map, s.body, s.text),
            Block::Heading(l, inl) => {
                self.para(inl, inl_map, s.headings[(*l as usize - 1).min(2)], if *l >= 4 { s.muted } else { s.text })
            }
            Block::Code(code) => {
                let n = self.next_source.get();
                self.next_source.set(n + 1);
                self.code(code, self.sources.get(n).filter(|src| src.len == code.len()), code_map)
            }
            Block::Diff(src) => self.diff(src, code_map),
            Block::Mermaid(src) => self.mermaid(src, code_map),
            Block::Table(t) => {
                let (head_maps, row_maps) = match map {
                    BlockMap::Table { head, rows } => (head.as_slice(), rows.as_slice()),
                    _ => (&[][..], &[][..]),
                };
                self.table(t, head_maps, row_maps)
            }
            Block::List { start, items } => {
                let item_maps = match map {
                    BlockMap::List(m) => m.as_slice(),
                    _ => &[][..],
                };
                let mut c = col().w_full().gap(4.0);
                for (k, item) in items.iter().enumerate() {
                    let imap = item_maps.get(k).map_or(&[][..], Vec::as_slice);
                    let task_idx = self.next_task.get();
                    self.next_task.set(task_idx + 1);
                    if let Some((checked, marker_offset)) = self.tasks.get(task_idx).copied().flatten() {
                        c = c.child(self.task_item(k, item, imap, marker_offset, checked));
                        continue;
                    }
                    let marker = match start {
                        Some(n) => format!("{}.", n + k as u64),
                        None => "•".into(),
                    };
                    c = c.child(
                        row()
                            .key(format!("i{k}"))
                            .w_full()
                            .child(
                                text(marker).font(s.body).color(s.muted).w(32.0).align(Align::Center).reveal(self.reveal),
                            )
                            .child(self.blocks(item, imap, &[], false).grow(1.0).shrink()),
                    );
                }
                c
            }
            Block::Quote(inner) => {
                let inner_maps = match map {
                    BlockMap::Quote(m) => m.as_slice(),
                    _ => &[][..],
                };
                row()
                    .w_full()
                    .child(el().w(2.0).h_full().radius(1.0).fill(s.quote_rule))
                    .child(self.blocks(inner, inner_maps, &[], false).grow(1.0).shrink().pl(12.0))
            }
            Block::Rule => el().w_full().h(0.5).fill(s.rule),
        }
    }
}

fn item_label(blocks: &[Block]) -> String {
    let mut out = String::new();
    for b in blocks {
        let text = match b {
            Block::Para(inl) | Block::Heading(_, inl) => inl.text.replace(PAD, ""),
            Block::Code(s) | Block::Diff(s) | Block::Mermaid(s) => s.clone(),
            _ => continue,
        };
        if !text.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&text);
        }
    }
    out
}

fn hash_of(v: impl Hash) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::text::{GlyphQuad, TextSystem};

    /// Each code block's syntax spans, in document order, rendered with `style`.
    fn spans(src: &str, style: &MarkdownStyle) -> Vec<(String, Vec<Span>)> {
        spans_with(src, style, Highlighter::builtin())
    }

    /// [`spans`] with a highlighter.
    fn spans_with(src: &str, style: &MarkdownStyle, highlighter: Highlighter) -> Vec<(String, Vec<Span>)> {
        fn codes<'a>(blocks: &'a [Block], out: &mut Vec<&'a str>) {
            for b in blocks {
                match b {
                    Block::Code(c) => out.push(c),
                    Block::Quote(inner) => codes(inner, out),
                    Block::List { items, .. } => items.iter().for_each(|i| codes(i, out)),
                    _ => {}
                }
            }
        }
        let (blocks, sources, tasks, _, _) = doc_cached(src);
        let r = Renderer { s: style.clone(), highlights: Vec::new(), reveal: 0.0, streaming: false, on_link: None, link_look: None, link_ring: None, on_task: None,
            tasks_read_only: false, selectable: false, scroll_wide: false, highlighter, sources: sources.clone(),
            next_source: Cell::new(0), tasks, next_task: Cell::new(0), current_block_key: RefCell::new("b0".into()) };
        let mut found = Vec::new();
        codes(&blocks, &mut found);
        assert_eq!(found.len(), sources.len(), "one source per code block");
        found
            .into_iter()
            .zip(sources.iter())
            .map(|(code, s)| {
                assert_eq!(s.len, code.len(), "sources line up with blocks in document order");
                (code.to_owned(), r.syntax_spans(code, Some(s)))
            })
            .collect()
    }

    fn coloured<'a>(code: &'a str, spans: &[Span], c: Color) -> Vec<&'a str> {
        spans.iter().filter(|s| s.color == Some(c)).map(|s| &code[s.start as usize..s.end as usize]).collect()
    }

    #[test]
    fn known_languages_are_coloured_and_others_stay_plain() {
        let style = MarkdownStyle::neutral(true);
        let src = "```ts\nconst a = 1\n```\n\n```brainfuck\n+[-]\n```\n\n    indented\n\n```\nnone\n```\n\n- ```rust,ignore\n  fn f() {}\n  ```\n\n> ```SQL\n> select 1\n> ```";
        let got = spans(src, &style);
        assert_eq!(got.len(), 6);
        let (ts, s) = &got[0];
        assert_eq!(coloured(ts, s, style.syntax.color(Syntax::Keyword)), ["const"]);
        assert_eq!(coloured(ts, s, style.syntax.color(Syntax::Number)), ["1"]);
        assert!(s.iter().all(|s| s.color.is_some() && s.weight.is_none() && !s.italic && !s.mono && s.size.is_none() && s.bg.is_none()));
        for (code, s) in &got[1..4] {
            assert!(s.is_empty(), "{code:?} stays plain");
        }
        assert_eq!(coloured(&got[4].0, &got[4].1, style.syntax.color(Syntax::Function)), ["f"]);
        assert_eq!(coloured(&got[5].0, &got[5].1, style.syntax.color(Syntax::Keyword)), ["select"]);
    }

    #[test]
    fn a_streaming_fence_colours_only_its_finished_lines() {
        let style = MarkdownStyle::neutral(false);
        let last_end = |src: &str| spans(src, &style)[0].1.last().map_or(0, |s| s.end as usize);
        assert_eq!(last_end("```ts\nconst"), 0, "the first line is still arriving");
        assert_eq!(last_end("```ts\nconst a = 1\nconst b"), "const a = 1".len());
        assert_eq!(last_end("```ts\nconst a = 1\nconst b = 2\n"), "const a = 1\nconst b = 2".len());
        assert_eq!(last_end("```ts\nconst a = 1\nlet b = 2\n```"), "const a = 1\nlet b = 2".len());
    }

    #[test]
    fn syntax_builders_override_and_switch_off_colour() {
        let red = Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };
        let src = "```rust\nlet x = 1;\n```";
        let style = MarkdownStyle::neutral(true).syntax(Syntax::Keyword, red);
        let (code, s) = &spans(src, &style)[0];
        assert_eq!(coloured(code, s, red), ["let"]);
        assert!(spans(src, &style.syntax_highlight(false))[0].1.is_empty());
    }

    #[test]
    fn a_highlighter_gets_each_fence_language_as_written() {
        use crate::syntax::Paint;
        thread_local!(static SEEN: RefCell<Vec<(Option<String>, String)>> = const { RefCell::new(Vec::new()) });
        let red = Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };
        let h = Highlighter::new(move |lang: Option<&str>, code: &str| {
            SEEN.with_borrow_mut(|s| s.push((lang.map(str::to_owned), code.to_owned())));
            vec![(2..3, Paint::Color(red)), (0..1, Paint::Kind(Syntax::Keyword))]
        });
        let style = MarkdownStyle::neutral(true);
        let src = "```Kotlin title=\"a.kt\"\nval x = 1\n```\n\n```\nbare fence\n```\n\n    indented code\n\n```rust,ignore\nfn f() {}\n```\n\n```diff\n+a\n```";
        let got = spans_with(src, &style, h);
        let want = [(Some("Kotlin"), "val x = 1"), (None, "bare fence"), (None, "indented code"), (Some("rust"), "fn f() {}")];
        assert_eq!(SEEN.take(), want.map(|(l, c)| (l.map(str::to_owned), c.to_owned())), "diff fences keep their own look");
        for (code, s) in &got {
            assert_eq!(coloured(code, s, style.syntax.color(Syntax::Keyword)), [&code[0..1]]);
            assert_eq!(coloured(code, s, red), [&code[2..3]]);
        }
    }

    /// Apps that don't pass a highlighter get exactly the built-in one's colours, and with colour
    /// off nothing runs.
    #[test]
    fn without_a_highlighter_the_output_is_the_builtin_ones() {
        let style = MarkdownStyle::neutral(false);
        let src = "```ts\nconst a = 1;\n```\n\n```\nplain\n```\n\n```kotlin\nval x = 1\n```";
        let got = spans(src, &style);
        let k = |s| style.syntax.color(s);
        let ts: Vec<_> = got[0].1.iter().map(|s| (s.start, s.end, s.color.unwrap())).collect();
        assert_eq!(ts, [(0, 5, k(Syntax::Keyword)), (8, 9, k(Syntax::Punctuation)), (10, 11, k(Syntax::Number)), (11, 12, k(Syntax::Punctuation))]);
        assert!(got[1].1.is_empty() && got[2].1.is_empty());
        let h = Highlighter::new(|_, _| panic!("colour is off"));
        assert!(spans_with(src, &style.clone().syntax_highlight(false), h).iter().all(|(_, s)| s.is_empty()));
    }

    /// Rebuilding a view highlights nothing: a highlighter runs once per block however many
    /// commits draw it.
    #[test]
    fn rebuilding_a_view_runs_the_highlighter_once_per_block() {
        thread_local!(static RUNS: Cell<usize> = const { Cell::new(0) });
        fn view(_: &(), _: &mut rune::Cx<()>) -> El {
            let style = MarkdownStyle::neutral(true);
            Markdown::new("```py\nx = 1\n```\n\n```go\nvar y = 2\n```")
                .highlight(|lang, code| {
                    RUNS.set(RUNS.get() + 1);
                    crate::syntax::builtin(lang, code)
                })
                .render(&style)
        }
        let mut rt = rune::Runtime::new((), view, (400.0, 400.0), rune::Env::new(true, 1.0));
        rt.frame(0.0);
        for _ in 0..3 {
            rt.invalidate();
            rt.frame(1.0 / 60.0);
        }
        assert_eq!(RUNS.get(), 2);
    }

    /// Colour is the only thing a highlight changes: every glyph lands in the same place, wrapped
    /// or not, as in the plain block and in the block mid-stream.
    #[test]
    fn highlighting_never_moves_a_glyph() {
        const INTER: &[u8] = include_bytes!("../../../themes/opencode/assets/fonts/InterVariable.ttf");
        const PLEX: &[u8] = include_bytes!("../../../themes/opencode/assets/fonts/IBMPlexMono-Regular.ttf");
        let mut ts = TextSystem::new(&[INTER, PLEX], ("Inter", "IBM Plex Mono"));
        let style = MarkdownStyle::neutral(true);
        let font = style.code_block;
        let mut quads = |code: &str, spans: &[Span], width: Option<f32>| {
            let l = ts.layout_rich(code, spans, font, 2.0, width);
            let mut out: Vec<GlyphQuad> = Vec::new();
            ts.glyphs_for_layout(&l, 2.0, [0.0, 0.0], true, &mut out);
            out.iter().map(|q| (q.rect.map(f32::to_bits), q.uv.map(f32::to_bits), q.byte)).collect::<Vec<_>>()
        };
        for (lang, code) in [
            ("tsx", "export function Card({ title }: { title: string }) {\n  return <div className=\"card\">{title.toUpperCase()}</div>; // done\n}"),
            ("rust", "fn main() { let v: Vec<u8> = vec![1, 2, 3]; println!(\"{v:?} → ok\"); }"),
            ("sh", "FOO=1 npm run build --if-present && echo \"$HOME/${X}\" | tee out.txt # café"),
            ("yaml", "name: rune\nlist: [a, 1, {b: c}]\nscript: |\n  echo hi"),
        ] {
            let closed = format!("```{lang}\n{code}\n```");
            let (shown, full) = spans(&closed, &style).remove(0);
            assert_eq!(shown, code);
            assert!(!full.is_empty(), "{lang} gets colour");
            let cut = code.find('\n').map_or(code.len() / 2, |n| n + 4);
            let cut = (cut..).find(|&i| code.is_char_boundary(i)).unwrap();
            let (_, streaming) = spans(&format!("```{lang}\n{}", &code[..cut]), &style).remove(0);
            for width in [None, Some(900.0), Some(420.0), Some(260.0)] {
                let plain = quads(code, &[], width);
                assert_eq!(plain, quads(code, &full, width), "{lang} at {width:?}");
                assert_eq!(plain, quads(code, &streaming, width), "{lang} mid-stream at {width:?}");
            }
        }
    }

    fn parses() -> (usize, usize) {
        PARSES.get()
    }

    /// A transcript longer than any fixed entry count re-renders without parsing.
    #[test]
    fn a_column_of_forty_documents_parses_each_once() {
        fn view(_: &(), _: &mut rune::Cx<()>) -> El {
            let style = MarkdownStyle::neutral(true);
            col().children((0..40).map(|i| Markdown::new(format!("para {i}\n\n- item")).render(&style).key(format!("m{i}"))))
        }
        let before = parses();
        let mut rt = rune::Runtime::new((), view, (400.0, 800.0), rune::Env::default());
        rt.frame(0.0);
        assert_eq!(parses().0 - before.0, 40);
        for _ in 0..3 {
            rt.invalidate();
            rt.frame(1.0 / 60.0);
        }
        assert_eq!(parses().0 - before.0, 40, "later commits parse nothing");
    }

    /// Feeding a document in a few bytes at a time parses only from its last block, and gives
    /// exactly what parsing each string whole gives.
    #[test]
    fn a_stream_reparses_from_its_last_block_and_matches_a_whole_parse() {
        let docs = [
            "# Title\n\nFirst para with **bold** and `code`.\nSetext\n---\n\n- one\n- two\n\n  more of two\n\n- [ ] todo\n- [x] done\n\n1. a\n2. b\n\nAfter the list.\n\n> quoted\n> lines\n\n```rust\nfn main() {}\n```\n\n    indented code\n\n***\n\n| a | b |\n|:-|-:|\n| 1 | 2 |\n\ntail text <span>html</span>\n\n<div>\nblock\n</div>\n\nlast *words*",
            "para one\n\npara two\n```ts\nconst a = 1\n```\n- item\nlazy line\n\n| x | y |\n| - | - |\nend",
            "---\ntitle: Example\n---\n\nPréface\n\n- plain\n  - [ ] nested\n    - [x] deeper\n- [ ] sibling\n\n<div>hidden</div>\n\nAfter\n\n- [x] final task\n\npartial",
        ];
        for doc in docs {
            let before = parses();
            for end in (1..=doc.len()).filter(|&i| doc.is_char_boundary(i)) {
                let src = &doc[..end];
                let (blocks, sources, tasks, starts, maps) = doc_cached(src);
                let (want, want_sources, want_tasks, want_cuts, want_starts, want_maps) = parse_doc(src, true);
                assert_eq!(*blocks, want, "blocks of {src:?}");
                assert_eq!(&*sources, &want_sources[..], "sources of {src:?}");
                assert_eq!(&*tasks, &want_tasks[..], "tasks of {src:?}");
                assert_eq!(&*starts, &want_starts[..], "block starts of {src:?}");
                assert_eq!(&*maps, &want_maps[..], "maps of {src:?}");
                let cuts = CACHE.with_borrow(|c| c.docs.peek(src).unwrap().cuts.clone());
                assert_eq!(&*cuts, &want_cuts[..], "cuts of {src:?}");
            }
            let (whole, tail) = (parses().0 - before.0, parses().1 - before.1);
            assert!(tail > whole * 2, "{doc:?}: {whole} whole parses, {tail} from the last block");
        }
    }

    /// A long stream keeps only its newest string, so it never pushes other documents out.
    #[test]
    fn a_stream_drops_the_strings_it_grew_from() {
        let mut rt = rune::Runtime::new((), |_: &(), _: &mut rune::Cx<()>| rune::el(), (10.0, 10.0), rune::Env::default());
        let mut commit = || {
            rt.invalidate();
            rt.frame(0.01);
        };
        commit();
        let docs: Vec<String> = (0..40).map(|i| format!("doc {i}\n\n- item {i}\n- another item")).collect();
        for doc in &docs {
            doc_cached(doc);
        }
        let held = CACHE.with_borrow(|c| c.docs.bytes());
        let mut stream = String::new();
        for i in 0.. {
            stream += &format!("Line {i} of a streamed answer, with a few more words.\n\n");
            if stream.len() >= 50_000 { break; }
        }
        for end in (50..=stream.len()).step_by(50).chain([stream.len()]) {
            commit();
            doc_cached(&stream[..end]);
        }
        CACHE.with_borrow(|c| {
            let last = c.docs.peek(stream.as_str()).expect("the whole stream is cached");
            let bytes = 2 * stream.len() + 64 * last.cuts.len() + 128;
            assert_eq!(c.docs.bytes(), held + bytes, "only the newest string of the stream is held");
            for doc in &docs {
                assert!(c.docs.peek(doc.as_str()).is_some(), "{doc:?} was evicted");
            }
        });
    }

    #[test]
    fn a_reference_definition_parses_whole() {
        let before = parses();
        doc_cached("[x]\n\nsome text");
        doc_cached("[x]\n\nsome text\n\n[x]: https://example.com");
        assert_eq!(parses().0 - before.0, 2);
        assert_eq!(parse_cached("[x]\n\nsome text\n\n[x]: https://example.com")[0], parse("[x]\n\nsome text\n\n[x]: https://example.com")[0]);
    }

    #[test]
    fn link_spans_come_before_inner_inline_styles_so_bold_em_and_code_keep_theirs() {
        let style = MarkdownStyle::neutral(true);
        let bg = Color { r: 1.0, g: 0.0, b: 0.5, a: 1.0 };
        let look: LinkLook = Rc::new(move |url| {
            url.starts_with("session:").then(|| (Span::new(0, 0).color(Color::WHITE).weight(500.0).bg(bg, 3.0), LinkPaint::new()))
        });
        let (blocks, sources, tasks, _, _) = doc_cached("[**bold** *em* `code` plain](session:1)");
        let Block::Para(inl) = &blocks[0] else { panic!() };
        let r = Renderer { s: style.clone(), highlights: Vec::new(), reveal: 0.0, streaming: false, on_link: None, link_look: Some(look), link_ring: None, on_task: None,
            tasks_read_only: false, selectable: false, scroll_wide: false, highlighter: Highlighter::builtin(), sources,
            next_source: Cell::new(0), tasks, next_task: Cell::new(0), current_block_key: RefCell::new("b0".into()) };
        let spans = r.spans(inl, style.body.size);
        // The link span is first so `flatten_spans` applies inner Strong/Em/Code over it.
        assert_eq!((spans[0].start, spans[0].end, spans[0].color, spans[0].weight, spans[0].bg, spans[0].bg_pad), (0, inl.text.len() as u32, Some(Color::WHITE), Some(500.0), Some(bg), 3.0));
        assert!(spans.iter().any(|s| &inl.text[s.start as usize..s.end as usize] == "bold" && s.weight == Some(style.strong_weight)));
        assert!(spans.iter().any(|s| &inl.text[s.start as usize..s.end as usize] == "em" && s.italic && s.color.is_none()));
        assert!(spans.iter().any(|s| &inl.text[s.start as usize..s.end as usize] == "code" && s.mono && s.size == Some(style.body.size * style.inline_code_scale) && s.bg.is_none()));
    }
}

//! Markdown (pulldown-cmark): headings, paragraphs, emphasis, inline code, code blocks, lists,
//! block quotes, rules, links, GFM tables, ```` ```diff ```` fences and (with the `mermaid`
//! feature) ```` ```mermaid ```` diagrams, rendered from a [`MarkdownStyle`]. Partial input while
//! streaming is fine: an unclosed fence is a code block that keeps growing.
//!
//! ```ignore
//! let view = Markdown::new(&message.text).reveal(0.18).on_link(|s: &mut App, url| s.open(url)).build(&kit);
//! // Without a Kit:
//! let view = Markdown::new(&message.text).render(&MarkdownStyle::neutral(dark));
//! ```
//!
//! **Cost.** [`parse_cached`] keeps the parse of the last few source strings, so re-rendering
//! unchanged text (every frame of an animation, a list of finished messages) parses nothing.
//! A growing stream parses once per distinct string, which is the unavoidable minimum.
//!
//! **Keys.** Top-level blocks are keyed `b{index}`, list items `i{index}` and their blocks by
//! the same rule inside the item. Appending to a stream only ever changes the last block or
//! adds blocks after it, so earlier blocks keep their keys and never remount. Give the
//! `Markdown` root its own key when more than one is on screen.
//!
//! **Tables** size each column to its content. A table that doesn't fit shrinks its columns in
//! proportion to their content (wrapping cell text), down to each column's longest word capped at
//! [`MarkdownStyle::table_min_column`]; past that it clips. While a stream is inside a table, the
//! row still arriving doesn't widen columns, so columns move at most once per finished row, and a
//! trailing `| a | b |` line is already drawn as the header it is about to become.
//!
//! **Diagrams** (`mermaid` feature) render once their fence closes; until then, and for anything
//! the subset doesn't cover, the source shows as a code block.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use rune::{Align, Color, El, Link, LinkPaint, Span, TextStyle, col, el, rich, row, text};

use crate::{Kit, PaletteSeed};

mod table;

/// An inline style run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Style {
    Code,
    /// The narrow no-break spaces around inline code (CSS's `padding: 0 4px`).
    CodePad,
    Strong,
    Em,
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

struct Builder {
    /// Open containers: (kind, blocks so far).
    stack: Vec<(Container, Vec<Block>)>,
    inline: Option<(InlineKind, Inline, Vec<Open>)>,
    code: Option<(String, Fence)>,
    /// The source range of the last paragraph started at the root.
    last_root_para: Option<Range<usize>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Fence {
    Plain,
    Diff,
    /// A mermaid fence; whether its closing fence has arrived.
    Mermaid(bool),
}

/// An emphasis, strong or link run that has started and not yet ended.
struct Open {
    start: usize,
    style: Style,
    url: Option<String>,
}

enum Container {
    Root,
    Quote,
    List(Option<u64>, Vec<Vec<Block>>),
    Item,
    Table(Table, Vec<Inline>),
}

#[derive(Clone, Copy)]
enum InlineKind {
    Para,
    Heading(u8),
    Cell,
}

impl Builder {
    fn push(&mut self, b: Block) {
        self.stack.last_mut().unwrap().1.push(b);
    }

    fn inline(&mut self) -> &mut (InlineKind, Inline, Vec<Open>) {
        // Tight list items have text without a paragraph.
        self.inline.get_or_insert_with(|| (InlineKind::Para, Inline::default(), Vec::new()))
    }

    fn flush_inline(&mut self) {
        if let Some((kind, mut inl, open)) = self.inline.take() {
            for o in open {
                end_run(&mut inl, o);
            }
            let trimmed = inl.text.trim_end().len();
            inl.text.truncate(trimmed);
            inl.spans.retain_mut(|s| {
                s.1 = s.1.min(trimmed);
                s.0 < s.1
            });
            inl.links.retain_mut(|s| {
                s.1 = s.1.min(trimmed);
                s.0 < s.1
            });
            if let InlineKind::Cell = kind {
                // Empty cells still hold their column.
                if let Some((Container::Table(_, cells), _)) = self.stack.last_mut() {
                    cells.push(inl);
                }
                return;
            }
            if inl.text.is_empty() {
                return;
            }
            self.push(match kind {
                InlineKind::Para | InlineKind::Cell => Block::Para(inl),
                InlineKind::Heading(l) => Block::Heading(l, inl),
            });
        }
    }

    /// Closes the innermost container into its parent.
    fn pop_container(&mut self) {
        self.flush_inline();
        let Some((c, blocks)) = self.stack.pop() else { return };
        match c {
            Container::Item => {
                if let Some((Container::List(_, items), _)) = self.stack.last_mut() {
                    items.push(blocks);
                }
            }
            Container::List(start, items) => self.push(Block::List { start, items }),
            Container::Quote => self.push(Block::Quote(blocks)),
            Container::Table(mut t, cells) => {
                if !cells.is_empty() {
                    t.rows.push(cells);
                }
                self.push(Block::Table(t));
            }
            Container::Root => {}
        }
    }

    fn table(&mut self) -> Option<(&mut Table, &mut Vec<Inline>)> {
        match self.stack.last_mut() {
            Some((Container::Table(t, cells), _)) => Some((t, cells)),
            _ => None,
        }
    }
}

const PAD: &str = "\u{202f}";

/// Parses `src` into blocks. Unclosed constructs are closed at the end of input.
pub fn parse(src: &str) -> Vec<Block> {
    let mut b = Builder { stack: vec![(Container::Root, Vec::new())], inline: None, code: None, last_root_para: None };
    let opts = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    for (ev, range) in Parser::new_ext(src, opts).into_offset_iter() {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    b.flush_inline();
                    if b.stack.len() == 1 {
                        b.last_root_para = Some(range);
                    }
                    b.inline = Some((InlineKind::Para, Inline::default(), Vec::new()));
                }
                Tag::Heading { level, .. } => {
                    b.flush_inline();
                    let l = match level {
                        HeadingLevel::H1 => 1,
                        HeadingLevel::H2 => 2,
                        HeadingLevel::H3 => 3,
                        _ => 4,
                    };
                    b.inline = Some((InlineKind::Heading(l), Inline::default(), Vec::new()));
                }
                Tag::CodeBlock(kind) => {
                    b.flush_inline();
                    let fence = match &kind {
                        CodeBlockKind::Fenced(info) => {
                            match info.split_whitespace().next().unwrap_or("").to_ascii_lowercase().as_str() {
                                "diff" | "patch" => Fence::Diff,
                                "mermaid" => Fence::Mermaid(fence_closed(&src[range])),
                                _ => Fence::Plain,
                            }
                        }
                        CodeBlockKind::Indented => Fence::Plain,
                    };
                    b.code = Some((String::new(), fence));
                }
                Tag::BlockQuote(_) => {
                    b.flush_inline();
                    b.stack.push((Container::Quote, Vec::new()));
                }
                Tag::List(start) => {
                    b.flush_inline();
                    b.stack.push((Container::List(start, Vec::new()), Vec::new()));
                }
                Tag::Item => {
                    b.flush_inline();
                    b.stack.push((Container::Item, Vec::new()));
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
                    b.stack.push((Container::Table(Table { align, ..Table::default() }, Vec::new()), Vec::new()));
                }
                Tag::TableHead | Tag::TableRow => {
                    // A row is still arriving while its line has no newline yet.
                    let open = !src[range.start..].contains('\n');
                    if let Some((t, _)) = b.table() {
                        t.open = open;
                    }
                }
                Tag::TableCell => {
                    b.flush_inline();
                    b.inline = Some((InlineKind::Cell, Inline::default(), Vec::new()));
                }
                Tag::Strong => open(&mut b, Style::Strong, None),
                Tag::Emphasis => open(&mut b, Style::Em, None),
                Tag::Link { dest_url, .. } => open(&mut b, Style::Link, Some(dest_url.into_string())),
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::TableCell => b.flush_inline(),
                TagEnd::CodeBlock => {
                    if let Some((mut text, fence)) = b.code.take() {
                        if text.ends_with('\n') {
                            text.pop();
                        }
                        b.push(code_block(text, fence));
                    }
                }
                TagEnd::TableHead => {
                    b.flush_inline();
                    if let Some((t, cells)) = b.table() {
                        t.head = std::mem::take(cells);
                    }
                }
                TagEnd::TableRow => {
                    b.flush_inline();
                    if let Some((t, cells)) = b.table() {
                        t.rows.push(std::mem::take(cells));
                    }
                }
                TagEnd::BlockQuote(_) | TagEnd::Item | TagEnd::List(_) | TagEnd::Table => b.pop_container(),
                TagEnd::Strong | TagEnd::Emphasis | TagEnd::Link => close(&mut b),
                _ => {}
            },
            Event::Text(t) => {
                if let Some((code, _)) = &mut b.code {
                    code.push_str(&t);
                } else {
                    b.inline().1.text.push_str(&t);
                }
            }
            Event::Code(t) => {
                let inl = &mut b.inline().1;
                let a = inl.text.len();
                inl.text.push_str(PAD);
                let start = inl.text.len();
                inl.text.push_str(&t);
                let end = inl.text.len();
                inl.text.push_str(PAD);
                inl.spans.push((a, start, Style::CodePad));
                inl.spans.push((start, end, Style::Code));
                inl.spans.push((end, inl.text.len(), Style::CodePad));
            }
            Event::SoftBreak => b.inline().1.text.push(' '),
            Event::HardBreak => b.inline().1.text.push('\n'),
            Event::Rule => {
                b.flush_inline();
                b.push(Block::Rule);
            }
            _ => {}
        }
    }
    b.flush_inline();
    if let Some((text, fence)) = b.code.take() {
        b.push(code_block(text.trim_end_matches('\n').to_owned(), fence));
    }
    // Unwind containers left open by a partial stream.
    while b.stack.len() > 1 {
        b.pop_container();
    }
    let last_para = b.last_root_para.take();
    let mut blocks = b.stack.pop().unwrap().1;
    if let (Some(Block::Para(_)), Some(range)) = (blocks.last(), last_para)
        && let Some(t) = pending_table(&src[range.start..])
    {
        *blocks.last_mut().unwrap() = Block::Table(t);
    }
    blocks
}

fn code_block(text: String, fence: Fence) -> Block {
    match fence {
        Fence::Plain | Fence::Mermaid(false) => Block::Code(text),
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
fn pending_table(src: &str) -> Option<Table> {
    let mut lines = src.trim_end().lines();
    let head = lines.next()?.trim();
    let delim = lines.next().map(str::trim);
    if lines.next().is_some() || !head.starts_with('|') {
        return None;
    }
    let cells = split_row(head);
    if cells.len() < 2 || delim.is_some_and(|d| !d.starts_with('|') || !d.chars().all(|c| "|:- ".contains(c))) {
        return None;
    }
    let mut align = vec![Align::Start; cells.len()];
    if let Some(d) = delim {
        for (a, spec) in align.iter_mut().zip(split_row(d)) {
            *a = match (spec.starts_with(':'), spec.len() > 1 && spec.ends_with(':')) {
                (true, true) => Align::Center,
                (false, true) => Align::End,
                _ => Align::Start,
            };
        }
    }
    let head = cells.iter().map(|c| inline_of(c)).collect();
    Some(Table { align, head, rows: Vec::new(), open: true })
}

/// The cells of a `| a | b |` line (a trailing pipe is optional; `\|` doesn't split).
fn split_row(line: &str) -> Vec<String> {
    let inner = line.strip_prefix('|').unwrap_or(line);
    let mut cells = vec![String::new()];
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => cells.last_mut().unwrap().push(chars.next().unwrap()),
            '|' => cells.push(String::new()),
            c => cells.last_mut().unwrap().push(c),
        }
    }
    if cells.last().is_some_and(|c| c.trim().is_empty()) && inner.trim_end().ends_with('|') {
        cells.pop();
    }
    cells.into_iter().map(|c| c.trim().to_owned()).collect()
}

fn inline_of(src: &str) -> Inline {
    match parse(src).into_iter().next() {
        Some(Block::Para(i)) => i,
        _ => Inline { text: src.to_owned(), ..Inline::default() },
    }
}

fn open(b: &mut Builder, style: Style, url: Option<String>) {
    let inl = b.inline();
    let start = inl.1.text.len();
    inl.2.push(Open { start, style, url });
}

fn close(b: &mut Builder) {
    let (_, inl, open) = b.inline();
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
const CACHE_LEN: usize = 32;

#[derive(Default)]
struct Cache {
    map: HashMap<String, Rc<Vec<Block>>>,
    order: VecDeque<String>,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
}

/// [`parse`] memoised by source on this thread (the last 32 distinct strings).
pub fn parse_cached(src: &str) -> Rc<Vec<Block>> {
    CACHE.with_borrow_mut(|c| {
        if let Some(hit) = c.map.get(src) {
            return hit.clone();
        }
        let blocks = Rc::new(parse(src));
        if c.order.len() >= CACHE_LEN
            && let Some(old) = c.order.pop_front()
        {
            c.map.remove(&old);
        }
        c.order.push_back(src.to_owned());
        c.map.insert(src.to_owned(), blocks.clone());
        blocks
    })
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
    table_border: Color,
    table_head_fill: Color,
    table_min_column: f32,
    diff_add: Color,
    diff_remove: Color,
    diff_hunk: Color,
    pub(crate) diagram: DiagramPaint,
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

/// `t` of the way from `base` toward `hue` in Oklab, opaque: a quiet wash that keeps `base`'s
/// lightness close in both themes.
fn tint(base: Color, hue: Color, t: f32) -> Color {
    let [a, b] = [base.to_oklab(), hue.to_oklab()];
    Color::from_oklab([0, 1, 2, 3].map(|i| if i == 3 { 1.0 } else { a[i] + (b[i] - a[i]) * t }))
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
            table_border: c.border_muted,
            table_head_fill: c.layer_01,
            table_min_column: 120.0,
            diff_add: tint(c.layer_01, c.success.icon, if k.dark { 0.11 } else { 0.14 }),
            diff_remove: tint(c.layer_01, c.danger.icon, if k.dark { 0.11 } else { 0.14 }),
            diff_hunk: c.text_muted,
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
    pub fn code_block_font(mut self, f: TextStyle) -> Self {
        self.code_block = f;
        self
    }
    /// Inline code's size as a multiple of the surrounding text, and its weight (the family is
    /// always the mono face).
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
        self.table_border = border;
        self.table_head_fill = header_fill;
        self
    }
    /// The narrowest a column shrinks to when its longest word is wider (120 points by default).
    pub fn table_min_column(mut self, points: f32) -> Self {
        self.table_min_column = points.max(0.0);
        self
    }
    /// The tints behind a diff's added and removed lines (full width).
    pub fn diff(mut self, added: Color, removed: Color) -> Self {
        self.diff_add = added;
        self.diff_remove = removed;
        self
    }
    /// The text colour of a diff's `@@` hunk headers and file header lines.
    pub fn diff_hunk(mut self, c: Color) -> Self {
        self.diff_hunk = c;
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
}

type LinkWire = Rc<dyn Fn(El) -> El>;

/// Markdown source as an element. See the module docs for cost and keys.
pub struct Markdown {
    src: String,
    style: Option<MarkdownStyle>,
    reveal: f32,
    on_link: Option<LinkWire>,
}

impl Markdown {
    pub fn new(src: impl Into<String>) -> Markdown {
        Markdown { src: src.into(), style: None, reveal: 0.0, on_link: None }
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
    /// Called with a link's destination when it is clicked, or pressed with Enter or Space
    /// while focused. With a handler, links show a hand and a solid underline under the
    /// pointer and are Tab stops; without one they are styled text only.
    pub fn on_link<S: 'static>(mut self, f: impl Fn(&mut S, &str) + 'static) -> Self {
        let f = Rc::new(f);
        self.on_link = Some(Rc::new(move |e: El| {
            let f = f.clone();
            e.on_link(move |s: &mut S, url, _| f(s, url))
        }));
        self
    }

    pub fn build(mut self, k: &Kit) -> El {
        let style = self.style.take().unwrap_or_else(|| MarkdownStyle::from_kit(k));
        self.render(&style)
    }

    /// Builds with `style`, for apps without a [`Kit`] (see [`MarkdownStyle::neutral`] and
    /// [`MarkdownStyle::from_seed`]).
    pub fn render(self, style: &MarkdownStyle) -> El {
        let r = Renderer { s: style.clone(), reveal: self.reveal, on_link: self.on_link };
        r.blocks(&parse_cached(&self.src))
    }
}

struct Renderer {
    s: MarkdownStyle,
    reveal: f32,
    on_link: Option<LinkWire>,
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

impl Renderer {
    fn spans(&self, inl: &Inline, base: f32) -> Vec<Span> {
        let s = &self.s;
        inl.spans
            .iter()
            .map(|&(a, b, st)| {
                let mut sp = Span::new(a, b);
                match st {
                    Style::Code => {
                        sp.mono = true;
                        sp.weight = Some(s.inline_code_weight);
                        sp.size = Some(base * s.inline_code_scale);
                        sp.bg = Some(s.code_bg);
                    }
                    Style::CodePad => sp.bg = Some(s.code_bg),
                    Style::Strong => sp.weight = Some(s.strong_weight),
                    Style::Em => {
                        sp.color = Some(s.text);
                        sp.italic = true;
                    }
                    Style::Link => {
                        sp.color = Some(s.accent);
                        sp.underline = Some(s.link_underline);
                    }
                }
                sp
            })
            .collect()
    }

    /// The single place a link press is connected: each of `inl.links` becomes a pressable
    /// span of `e` (the text element holding it) reporting its destination.
    fn wire_link(&self, e: El, inl: &Inline) -> El {
        let Some(on_link) = self.on_link.as_ref().filter(|_| !inl.links.is_empty()) else { return e };
        let hover = LinkPaint::new().underline(self.s.accent);
        let e = inl.links.iter().fold(e, |e, (a, b, url)| e.link(Link::new(*a..*b, url.clone()).hover(hover)));
        on_link(e)
    }

    fn para(&self, inl: &Inline, style: TextStyle, color: Color) -> El {
        let e = rich(inl.text.clone(), self.spans(inl, style.size)).font(style).color(color).reveal(self.reveal).w_full();
        self.wire_link(e, inl)
    }

    fn blocks(&self, blocks: &[Block]) -> El {
        let mut c = col().w_full();
        let mut prev_mb = 0.0f32;
        for (i, b) in blocks.iter().enumerate() {
            let (mt, mb) = margins(b);
            let top = if i == 0 { 0.0 } else { mt.max(prev_mb) };
            c = c.child(self.block(b).key(format!("b{i}")).mt(top));
            prev_mb = mb;
        }
        c
    }

    fn code(&self, code: &str) -> El {
        let s = &self.s;
        col()
            .w_full()
            .padding(12.0)
            .radius(6.0)
            .fill(s.code_block_fill)
            .border(0.5, s.code_block_border)
            .child(text(code.to_owned()).font(s.code_block).color(s.text).wrap().reveal(self.reveal).w_full())
    }

    /// One row per line, so added and removed lines are tinted edge to edge.
    fn diff(&self, src: &str) -> El {
        let s = &self.s;
        let mut c = col().w_full().py(12.0).radius(6.0).fill(s.code_block_fill).border(0.5, s.code_block_border);
        for (n, line) in src.split('\n').enumerate() {
            let header = ["@@", "diff ", "index ", "--- ", "+++ "].iter().any(|p| line.starts_with(p));
            let (tint, color) = match line.as_bytes().first() {
                _ if header => (None, s.diff_hunk),
                Some(b'+') => (Some(s.diff_add), s.text),
                Some(b'-') => (Some(s.diff_remove), s.text),
                _ => (None, s.text),
            };
            let shown = if line.is_empty() { " " } else { line };
            let mut r = row()
                .key(format!("l{n}"))
                .w_full()
                .px(12.0)
                .child(text(shown.to_owned()).font(s.code_block).color(color).wrap().reveal(self.reveal).w_full());
            if let Some(t) = tint {
                r = r.fill(t);
            }
            c = c.child(r);
        }
        c
    }

    fn mermaid(&self, src: &str) -> El {
        self.code(src)
    }

    fn block(&self, b: &Block) -> El {
        let s = &self.s;
        match b {
            Block::Para(inl) => self.para(inl, s.body, s.text),
            Block::Heading(l, inl) => {
                self.para(inl, s.headings[(*l as usize - 1).min(2)], if *l >= 4 { s.muted } else { s.text })
            }
            Block::Code(code) => self.code(code),
            Block::Diff(src) => self.diff(src),
            Block::Mermaid(src) => self.mermaid(src),
            Block::Table(t) => self.table(t),
            Block::List { start, items } => {
                let mut c = col().w_full().gap(4.0);
                for (k, item) in items.iter().enumerate() {
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
                            .child(self.blocks(item).grow(1.0).shrink()),
                    );
                }
                c
            }
            Block::Quote(inner) => row()
                .w_full()
                .child(el().w(2.0).h_full().radius(1.0).fill(s.quote_rule))
                .child(self.blocks(inner).grow(1.0).shrink().pl(12.0)),
            Block::Rule => el().w_full().h(0.5).fill(s.rule),
        }
    }
}

fn hash_of(v: impl Hash) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

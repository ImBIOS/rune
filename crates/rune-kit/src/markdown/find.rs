//! Rendered-text search over Markdown documents, returning source UTF-8 byte ranges for
//! [`Markdown::highlights`].

use std::ops::Range;

use super::{Block, BlockMap, Highlight, HighlightItem, Markdown, PAD, SpanMap, Table, doc_cached};

/// Case sensitivity for [`find`] (and [`Markdown::find`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Case {
    /// Case-insensitive matching using per-character Unicode lowercase folding
    /// ([`char::to_lowercase`], with Greek final `'ς'` folded to `'σ'`).
    #[default]
    Insensitive,
    /// Exact code-point matching.
    Sensitive,
}

impl Case {
    /// [`Case::Insensitive`].
    pub const fn insensitive() -> Self {
        Self::Insensitive
    }
    /// [`Case::Sensitive`].
    pub const fn sensitive() -> Self {
        Self::Sensitive
    }
}

/// Finds non-overlapping matches of `query` in the rendered text of `src`, in document order.
///
/// Each outer entry is one match over the rendered text of a single block (paragraph, heading,
/// list item block, table cell, code block or diff block; matches never cross block boundaries).
/// Each inner [`Vec`] lists the Markdown source UTF-8 byte ranges covered by that match, in
/// order and merged where adjacent, skipping inline formatting markers (`**`, `*`, `~~`, `` ` ``),
/// link and image destinations, and container prefixes.
///
/// [`Case::Insensitive`] folds each character with [`char::to_lowercase`] (and Greek final `'ς'`
/// to `'σ'`) while tracking character boundaries in the rendered and source text, so characters
/// whose byte length changes when lowercased (such as `'İ'` or `'ẞ'`/`'ß'`) keep valid UTF-8
/// byte ranges. An empty `query` returns an empty [`Vec`].
///
/// The returned matches feed straight into [`Markdown::highlights`]:
///
/// ```ignore
/// let view = Markdown::new(src).highlights(markdown::find(src, query, Case::Insensitive)).build(&kit);
/// ```
pub fn find(src: &str, query: &str, case: Case) -> Vec<Vec<Range<usize>>> {
    if src.is_empty() || query.is_empty() {
        return Vec::new();
    }
    let (blocks, _, _, _, maps) = doc_cached(src);
    let needle = Needle::new(query, case);
    let mut out = Vec::new();
    find_in_blocks(src, &blocks, &maps, &needle, &mut out);
    out
}

impl Markdown {
    /// Finds non-overlapping matches of `query` in the rendered text of `src` (see [`find`]).
    pub fn find(src: &str, query: &str, case: Case) -> Vec<Vec<Range<usize>>> {
        find(src, query, case)
    }
}

enum Needle<'a> {
    Sensitive(&'a str),
    Insensitive { folded: String, ascii: bool },
}

impl<'a> Needle<'a> {
    fn new(query: &'a str, case: Case) -> Self {
        match case {
            Case::Sensitive => Needle::Sensitive(query),
            Case::Insensitive => {
                let ascii = query.is_ascii();
                let folded = if ascii {
                    query.to_ascii_lowercase()
                } else {
                    let mut s = String::with_capacity(query.len());
                    for c in query.chars() {
                        push_folded(&mut s, c);
                    }
                    s
                };
                Needle::Insensitive { folded, ascii }
            }
        }
    }
}

fn push_folded(out: &mut String, c: char) {
    if c == 'ς' {
        out.push('σ');
    } else {
        out.extend(c.to_lowercase());
    }
}

fn find_in_blocks(
    src: &str,
    blocks: &[Block],
    maps: &[BlockMap],
    needle: &Needle<'_>,
    out: &mut Vec<Vec<Range<usize>>>,
) {
    let empty = BlockMap::None;
    for (b, map) in blocks.iter().zip(maps.iter().chain(std::iter::repeat(&empty))) {
        match (b, map) {
            (Block::Para(inl) | Block::Heading(_, inl), BlockMap::Inline(m)) => {
                find_in_text(src, &inl.text, m, needle, out);
            }
            (Block::Code(code) | Block::Diff(code), BlockMap::Code(m)) => {
                find_in_text(src, code, m, needle, out);
            }
            (Block::Mermaid(code), BlockMap::Code(m)) => {
                #[cfg(feature = "mermaid")]
                if crate::mermaid::supported(code) {
                    continue;
                }
                find_in_text(src, code, m, needle, out);
            }
            (Block::Table(t), BlockMap::Table { head, rows }) => {
                find_in_table(src, t, head, rows, needle, out);
            }
            (Block::List { items, .. }, BlockMap::List(item_maps)) => {
                for (item, imap) in items.iter().zip(item_maps) {
                    find_in_blocks(src, item, imap, needle, out);
                }
            }
            (Block::Quote(inner), BlockMap::Quote(inner_maps)) => {
                find_in_blocks(src, inner, inner_maps, needle, out);
            }
            _ => {}
        }
    }
}

fn find_in_table(
    src: &str,
    t: &Table,
    head_maps: &[SpanMap],
    row_maps: &[Vec<SpanMap>],
    needle: &Needle<'_>,
    out: &mut Vec<Vec<Range<usize>>>,
) {
    for (cell, map) in t.head.iter().zip(head_maps) {
        find_in_text(src, &cell.text, map, needle, out);
    }
    for (row, rmaps) in t.rows.iter().zip(row_maps) {
        for (cell, map) in row.iter().zip(rmaps) {
            find_in_text(src, &cell.text, map, needle, out);
        }
    }
}

fn find_in_text(
    src: &str,
    text: &str,
    map: &SpanMap,
    needle: &Needle<'_>,
    out: &mut Vec<Vec<Range<usize>>>,
) {
    if text.is_empty() || map.0.is_empty() {
        return;
    }
    let contiguous = map.0[0].1.start == 0
        && map.0[map.0.len() - 1].1.end == text.len()
        && map.0.windows(2).all(|w| w[0].1.end == w[1].1.start);

    let (owned_vis, owned_segs);
    let (vis, segments): (&str, &[(Range<usize>, Range<usize>)]) = if contiguous {
        (text, &map.0)
    } else {
        let mut v = String::with_capacity(text.len());
        let mut s = Vec::with_capacity(map.0.len());
        for (src_r, out_r) in &map.0 {
            if let Some(slice) = text.get(out_r.clone())
                && !slice.is_empty()
            {
                let start = v.len();
                v.push_str(slice);
                s.push((src_r.clone(), start..v.len()));
            }
        }
        owned_vis = v;
        owned_segs = s;
        (&owned_vis, &owned_segs)
    };
    if vis.is_empty() {
        return;
    }

    let mut seg_idx = 0usize;
    match needle {
        Needle::Sensitive(q) => {
            for (v0, _) in vis.match_indices(q) {
                let ranges = map_vis_range(src, vis, segments, &mut seg_idx, v0..v0 + q.len());
                if !ranges.is_empty() {
                    out.push(ranges);
                }
            }
        }
        Needle::Insensitive { folded: q, ascii: true } if vis.is_ascii() => {
            let lower = vis.to_ascii_lowercase();
            for (v0, _) in lower.match_indices(q.as_str()) {
                let ranges = map_vis_range(src, vis, segments, &mut seg_idx, v0..v0 + q.len());
                if !ranges.is_empty() {
                    out.push(ranges);
                }
            }
        }
        Needle::Insensitive { folded: q, .. } => {
            find_folded(src, vis, segments, q, &mut seg_idx, out);
        }
    }
}

fn find_folded(
    src: &str,
    vis: &str,
    segments: &[(Range<usize>, Range<usize>)],
    q: &str,
    seg_idx: &mut usize,
    out: &mut Vec<Vec<Range<usize>>>,
) {
    let mut folded = String::with_capacity(vis.len());
    let mut f_to_v = Vec::with_capacity(vis.len() + 1);
    let mut multi_char = false;
    for (v_byte, c) in vis.char_indices() {
        f_to_v.push(v_byte);
        if c == 'ς' {
            folded.push('σ');
        } else {
            let mut count = 0usize;
            for fc in c.to_lowercase() {
                folded.push(fc);
                count += 1;
            }
            if count > 1 {
                multi_char = true;
            }
        }
        f_to_v.resize(folded.len(), usize::MAX);
    }
    f_to_v.push(vis.len());

    if !multi_char {
        for (f0, _) in folded.match_indices(q) {
            let (v0, v1) = (f_to_v[f0], f_to_v[f0 + q.len()]);
            if v0 < v1 && v1 != usize::MAX {
                let ranges = map_vis_range(src, vis, segments, seg_idx, v0..v1);
                if !ranges.is_empty() {
                    out.push(ranges);
                }
            }
        }
        return;
    }

    let mut at = 0usize;
    while at + q.len() <= folded.len() {
        let Some(rel) = folded[at..].find(q) else { break };
        let f0 = at + rel;
        let f1 = f0 + q.len();
        let (v0, v1) = (f_to_v[f0], f_to_v[f1]);
        if v0 != usize::MAX && v1 != usize::MAX && v0 < v1 {
            let ranges = map_vis_range(src, vis, segments, seg_idx, v0..v1);
            if !ranges.is_empty() {
                out.push(ranges);
            }
            at = f1;
        } else {
            at = f0 + 1;
            while at < folded.len() && f_to_v[at] == usize::MAX {
                at += 1;
            }
        }
    }
}

fn map_vis_range(
    src: &str,
    vis: &str,
    segments: &[(Range<usize>, Range<usize>)],
    seg_idx: &mut usize,
    v_range: Range<usize>,
) -> Vec<Range<usize>> {
    while *seg_idx < segments.len() && segments[*seg_idx].1.end <= v_range.start {
        *seg_idx += 1;
    }
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut j = *seg_idx;
    while j < segments.len() && segments[j].1.start < v_range.end {
        let (s_seg, o_seg) = &segments[j];
        let o0 = v_range.start.max(o_seg.start);
        let o1 = v_range.end.min(o_seg.end);
        if o0 < o1 {
            let rel0 = o0 - o_seg.start;
            let rel1 = o1 - o_seg.start;
            let (mut s0, mut s1) = if s_seg.len() == o_seg.len()
                && (src.get(s_seg.clone()) == vis.get(o_seg.clone())
                    || (src.is_char_boundary(s_seg.start + rel0)
                        && src.is_char_boundary(s_seg.start + rel1)
                        && !src.get(s_seg.clone()).is_some_and(|s| s.starts_with('&'))))
            {
                (s_seg.start + rel0, s_seg.start + rel1)
            } else {
                (s_seg.start, s_seg.end)
            };
            s0 = src.floor_char_boundary(s0.clamp(s_seg.start, s_seg.end).min(src.len()));
            s1 = src.ceil_char_boundary(s1.clamp(s_seg.start, s_seg.end).min(src.len()));
            if s0 < s1 {
                if let Some(prev) = ranges.last_mut()
                    && prev.end == s0
                {
                    prev.end = s1;
                } else if ranges.last().is_none_or(|prev| prev.end < s0) {
                    ranges.push(s0..s1);
                }
            }
        }
        if o_seg.end <= v_range.end {
            *seg_idx = j + 1;
        } else {
            *seg_idx = j;
        }
        j += 1;
    }
    ranges
}

impl SpanMap {
    pub(super) fn map_ranges(&self, queries: &[Range<usize>], out_text: &str) -> Vec<Range<usize>> {
        if queries.is_empty() || self.0.is_empty() || out_text.is_empty() {
            return Vec::new();
        }
        let mut mapped: Vec<Range<usize>> = Vec::new();
        for q in queries {
            for r in self.map_range(q.clone(), out_text) {
                if let Some(prev) = mapped.last_mut()
                    && (prev.end >= r.start || is_pad_only(out_text.get(prev.end..r.start)))
                {
                    prev.end = prev.end.max(r.end);
                } else {
                    mapped.push(r);
                }
            }
        }
        mapped
    }

    fn map_range(&self, query: Range<usize>, out_text: &str) -> Vec<Range<usize>> {
        if query.start >= query.end
            || self.0.is_empty()
            || out_text.is_empty()
            || query.end <= self.0[0].0.start
            || self.0[self.0.len() - 1].0.end <= query.start
        {
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
                && (prev.end == o0 || is_pad_only(out_text.get(prev.end..o0)))
            {
                prev.end = o1;
            } else {
                mapped.push(o0..o1);
            }
        }
        mapped
    }
}

fn is_pad_only(s: Option<&str>) -> bool {
    s.is_some_and(|s| s == PAD || (!s.is_empty() && s.trim_matches('\u{202f}').is_empty()))
}

fn span_bounds(ranges: &[Range<usize>]) -> Range<usize> {
    match (ranges.first(), ranges.last()) {
        (Some(first), Some(last)) => first.start..last.end,
        _ => 0..0,
    }
}

impl HighlightItem for (Range<usize>, Highlight) {
    fn into_highlight(self) -> (Range<usize>, Highlight) { self }
}
impl HighlightItem for &(Range<usize>, Highlight) {
    fn into_highlight(self) -> (Range<usize>, Highlight) { self.clone() }
}
impl HighlightItem for (&Range<usize>, Highlight) {
    fn into_highlight(self) -> (Range<usize>, Highlight) { (self.0.clone(), self.1) }
}
impl HighlightItem for Range<usize> {
    fn into_highlight(self) -> (Range<usize>, Highlight) { (self, Highlight::new()) }
}
impl HighlightItem for &Range<usize> {
    fn into_highlight(self) -> (Range<usize>, Highlight) { (self.clone(), Highlight::new()) }
}
impl HighlightItem for Vec<Range<usize>> {
    fn into_highlight(self) -> (Range<usize>, Highlight) { (span_bounds(&self), Highlight::new()) }
    fn into_ranges(self) -> (Vec<Range<usize>>, Highlight) { (self, Highlight::new()) }
}
impl HighlightItem for &Vec<Range<usize>> {
    fn into_highlight(self) -> (Range<usize>, Highlight) { (span_bounds(self), Highlight::new()) }
    fn into_ranges(self) -> (Vec<Range<usize>>, Highlight) { (self.clone(), Highlight::new()) }
}
impl HighlightItem for &[Range<usize>] {
    fn into_highlight(self) -> (Range<usize>, Highlight) { (span_bounds(self), Highlight::new()) }
    fn into_ranges(self) -> (Vec<Range<usize>>, Highlight) { (self.to_vec(), Highlight::new()) }
}
impl HighlightItem for (Vec<Range<usize>>, Highlight) {
    fn into_highlight(self) -> (Range<usize>, Highlight) { (span_bounds(&self.0), self.1) }
    fn into_ranges(self) -> (Vec<Range<usize>>, Highlight) { self }
}
impl HighlightItem for &(Vec<Range<usize>>, Highlight) {
    fn into_highlight(self) -> (Range<usize>, Highlight) { (span_bounds(&self.0), self.1.clone()) }
    fn into_ranges(self) -> (Vec<Range<usize>>, Highlight) { self.clone() }
}
impl HighlightItem for (&Vec<Range<usize>>, Highlight) {
    fn into_highlight(self) -> (Range<usize>, Highlight) { (span_bounds(self.0), self.1) }
    fn into_ranges(self) -> (Vec<Range<usize>>, Highlight) { (self.0.clone(), self.1) }
}
impl HighlightItem for (&[Range<usize>], Highlight) {
    fn into_highlight(self) -> (Range<usize>, Highlight) { (span_bounds(self.0), self.1) }
    fn into_ranges(self) -> (Vec<Range<usize>>, Highlight) { (self.0.to_vec(), self.1) }
}

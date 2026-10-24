//! Virtual row bookkeeping. Only geometry outside the mounted window is estimated.

use std::cell::{Cell, Ref, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// What a row keeps while its key stays.
#[derive(Debug, Clone, Copy)]
struct Row {
    index: usize,
    /// Its measured height, once it has been laid out.
    height: Option<f32>,
}

#[derive(Debug, Default)]
pub(crate) struct VirtualList {
    /// Row keys in order; replace them with [`VirtualList::set_keys`].
    pub keys: Vec<String>,
    rows: HashMap<String, Row>,
    /// The commit each row in `keys` first appeared in.
    born: Vec<u64>,
    /// Each row's height, measured or estimated, in `keys` order.
    row_heights: Vec<f32>,
    /// Running totals of `row_heights` (one longer), rebuilt when one changes. Every frame reads
    /// them, so a long list doesn't hash every key to find its window.
    sums: RefCell<Vec<f32>>,
    sums_stale: Cell<bool>,
    pub range: Range<usize>,
    pub before: f32,
    pub after: f32,
    estimate: f32,
    /// Rows are built by index (`El::virtual_rows`), so a commit costs only the rows it mounts.
    /// Such a list moves its window in small steps and spreads rows new to it over frames.
    pub indexed: bool,
    /// The rows the last applied commit mounted. Every measurement of a commit compares with
    /// these, not with the pass before it.
    pub shown: HashSet<String>,
    /// The row holding the focused element, kept while that row is scrolled out.
    pub focused_row: Option<String>,
    pub scroll_path: String,
    pub viewport: f32,
    pub pad_top: f32,
    pub pad_bottom: f32,
    pub anchor: Option<(String, f32)>,
}

/// How far ahead an index-built list keeps rows mounted, in viewports either side.
const OVERSCAN: f32 = 2.0;
/// An index-built list moves its window once the viewport comes this close to its edge.
const INDEXED_EDGE: f32 = 1.75;
/// How much of an index-built list's overscan, in viewports per side, may be new in one commit.
/// Rows past it mount over the next frames, so a jump or a prepended page doesn't shape it all
/// in one frame.
const INDEXED_BUDGET: f32 = 0.5;

impl VirtualList {
    pub fn height(&self, key: &str) -> f32 {
        self.rows.get(key).and_then(|r| r.height).unwrap_or(self.estimate)
    }

    pub fn has(&self, key: &str) -> bool {
        self.rows.contains_key(key)
    }

    pub fn index_of(&self, key: &str) -> Option<usize> {
        self.rows.get(key).map(|r| r.index)
    }

    /// The commit row `i` first appeared in.
    pub fn born(&self, i: usize) -> u64 {
        self.born[i]
    }

    /// Whether the rows are exactly `keys`, in order.
    pub fn same_keys<'a>(&self, keys: impl ExactSizeIterator<Item = Option<&'a str>>) -> bool {
        keys.len() == self.keys.len() && keys.zip(&self.keys).all(|(a, b)| a == Some(b.as_str()))
    }

    /// Replaces the rows. Keys that stay keep their measured height and birth; new ones are born
    /// at `commit`. Returns the first row visible at `offset` (when given) that stays, with its
    /// top before the change, to anchor the reader to.
    pub fn set_keys(&mut self, keys: Vec<String>, commit: u64, offset: Option<f32>) -> Option<(String, f32)> {
        let mut rows = HashMap::with_capacity(keys.len());
        let mut born = Vec::with_capacity(keys.len());
        let mut heights = Vec::with_capacity(keys.len());
        for (index, key) in keys.iter().enumerate() {
            let old = self.rows.get(key);
            let height = old.and_then(|r| r.height);
            born.push(old.map_or(commit, |r| self.born[r.index]));
            heights.push(height.unwrap_or(self.estimate));
            assert!(rows.insert(key.clone(), Row { index, height }).is_none(), "virtual row keys must be unique");
        }
        let anchor = offset.and_then(|offset| self.first_visible(offset, |k| rows.contains_key(k)));
        self.rows = rows;
        self.born = born;
        self.keys = keys;
        self.row_heights = heights;
        self.sums_stale.set(true);
        anchor
    }

    pub fn set_estimate(&mut self, estimate: f32) {
        if estimate != self.estimate {
            self.estimate = estimate;
            self.row_heights = self.keys.iter().map(|k| self.height(k)).collect();
            self.sums_stale.set(true);
        }
    }

    pub fn set_height(&mut self, key: &str, height: f32) {
        let Some(row) = self.rows.get_mut(key) else { return };
        row.height = Some(height);
        let i = row.index;
        if self.row_heights[i] != height {
            self.row_heights[i] = height;
            self.sums_stale.set(true);
        }
    }

    fn sums(&self) -> Ref<'_, Vec<f32>> {
        if self.sums_stale.replace(false) || self.sums.borrow().len() != self.row_heights.len() + 1 {
            let mut sums = self.sums.borrow_mut();
            sums.clear();
            sums.push(0.0);
            let mut y = 0.0;
            for h in &self.row_heights {
                y += h;
                sums.push(y);
            }
        }
        self.sums.borrow()
    }

    pub fn prefix(&self, end: usize) -> f32 {
        self.sums()[end]
    }

    /// The first row still visible at `offset`, among those `keep` accepts, and its top.
    pub fn first_visible(&self, offset: f32, keep: impl Fn(&str) -> bool) -> Option<(String, f32)> {
        let sums = self.sums();
        let first = sums[1..].partition_point(|&s| self.pad_top + s <= offset);
        (first..self.keys.len())
            .find(|&i| keep(&self.keys[i]))
            .map(|i| (self.keys[i].clone(), self.pad_top + sums[i]))
    }

    /// The furthest the rows can scroll: their extent less the viewport.
    pub fn max_offset(&self) -> f32 {
        (self.pad_top + self.prefix(self.keys.len()) + self.pad_bottom - self.viewport).max(0.0)
    }

    /// Picks the rows to mount for `offset`, clamped to the rows' extent. With `budget` (the
    /// rows shown last commit), at most [`INDEXED_BUDGET`] of rows not among them mount per
    /// side beyond the viewport.
    pub fn select(&mut self, offset: f32, pinned: bool, budget: Option<&HashSet<String>>) {
        let total = self.prefix(self.keys.len());
        let offset = if pinned { self.max_offset() } else { offset.min(self.max_offset()) };
        let lo = offset - self.viewport * OVERSCAN;
        let hi = offset + self.viewport * (1.0 + OVERSCAN);
        let (mut start, mut end) = {
            let sums = self.sums();
            // Rows ending at or above `lo` go; rows starting below `hi` stay.
            let start = sums[1..].partition_point(|&s| self.pad_top + s <= lo);
            let end = sums[..self.keys.len()].partition_point(|&s| self.pad_top + s < hi);
            if let Some(shown) = budget.filter(|_| !pinned) {
                let mounted = |k: &str| shown.contains(k);
                // The viewport's own rows always mount; the overscan takes new rows up to a budget.
                let first = sums[1..].partition_point(|&s| self.pad_top + s <= offset).max(start);
                let last = sums[..self.keys.len()].partition_point(|&s| self.pad_top + s < offset + self.viewport).max(first);
                let budget = self.viewport * INDEXED_BUDGET;
                let mut spent = 0.0;
                let mut from = first;
                while from > start {
                    let i = from - 1;
                    if !mounted(&self.keys[i]) {
                        if spent >= budget { break; }
                        spent += self.row_heights[i];
                    }
                    from = i;
                }
                let mut spent = 0.0;
                let mut to = last.min(end);
                while to < end {
                    if !mounted(&self.keys[to]) {
                        if spent >= budget { break; }
                        spent += self.row_heights[to];
                    }
                    to += 1;
                }
                (from, to.max(from))
            } else {
                (start, end)
            }
        };
        self.anchor = if pinned { None } else { self.first_visible(offset, |_| true) };
        if pinned { start = start.min(end.saturating_sub(32)); }
        end = end.max(start);
        self.range = start..end;
        self.before = self.prefix(start);
        self.after = total - self.prefix(end);
    }

    pub fn anchor_delta(&self) -> f32 {
        let Some((key, old)) = &self.anchor else { return 0.0 };
        let Some(i) = self.index_of(key) else { return 0.0 };
        self.pad_top + self.prefix(i) - old
    }

    pub fn needs_window(&self, offset: f32, pinned: bool) -> bool {
        if self.keys.is_empty() { return false; }
        if pinned { return self.range.end != self.keys.len(); }
        let edge = self.viewport * if self.indexed { INDEXED_EDGE } else { 0.5 };
        let top = self.pad_top + self.before;
        let bottom = self.pad_top + self.prefix(self.range.end);
        (self.range.start > 0 && offset < top + edge)
            || (self.range.end < self.keys.len() && offset + self.viewport > bottom - edge)
    }
}

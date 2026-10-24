//! Virtual row bookkeeping. Only geometry outside the mounted window is estimated.

use std::cell::{Cell, Ref, RefCell};
use std::collections::HashMap;
use std::ops::Range;

#[derive(Debug, Default)]
pub(crate) struct VirtualList {
    /// Row keys in order; replace them with [`VirtualList::set_keys`].
    pub keys: Vec<String>,
    /// Measured heights by key, kept while the key stays.
    heights: HashMap<String, f32>,
    /// Each key's position in `keys`.
    index: HashMap<String, usize>,
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
    pub scroll_path: String,
    pub viewport: f32,
    pub pad_top: f32,
    pub pad_bottom: f32,
    pub anchor: Option<(String, f32)>,
    pub born: HashMap<String, u64>,
}

impl VirtualList {
    pub fn height(&self, key: &str) -> f32 {
        self.heights.get(key).copied().unwrap_or(self.estimate)
    }

    pub fn has(&self, key: &str) -> bool {
        self.index.contains_key(key)
    }

    /// Whether the rows are exactly `keys`, in order.
    pub fn same_keys<'a>(&self, keys: impl ExactSizeIterator<Item = Option<&'a str>>) -> bool {
        keys.len() == self.keys.len() && keys.zip(&self.keys).all(|(a, b)| a == Some(b.as_str()))
    }

    /// Replaces the rows, forgetting the heights of keys that left.
    pub fn set_keys(&mut self, keys: Vec<String>) {
        self.index = keys.iter().enumerate().map(|(i, k)| (k.clone(), i)).collect();
        self.heights.retain(|k, _| self.index.contains_key(k));
        self.keys = keys;
        self.refresh_rows();
    }

    pub fn set_estimate(&mut self, estimate: f32) {
        if estimate != self.estimate {
            self.estimate = estimate;
            self.refresh_rows();
        }
    }

    pub fn set_height(&mut self, key: &str, height: f32) {
        let Some(&i) = self.index.get(key) else { return };
        self.heights.insert(key.to_owned(), height);
        if self.row_heights[i] != height {
            self.row_heights[i] = height;
            self.sums_stale.set(true);
        }
    }

    fn refresh_rows(&mut self) {
        self.row_heights = self.keys.iter().map(|k| self.height(k)).collect();
        self.sums_stale.set(true);
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

    pub fn select(&mut self, offset: f32, pinned: bool) {
        let total = self.prefix(self.keys.len());
        let offset = if pinned { (total + self.pad_top + self.pad_bottom - self.viewport).max(0.0) } else { offset };
        let lo = offset - self.viewport * 2.0;
        let hi = offset + self.viewport * 3.0;
        let (mut start, end) = {
            let sums = self.sums();
            // Rows ending at or above `lo` go; rows starting below `hi` stay.
            let start = sums[1..].partition_point(|&s| self.pad_top + s <= lo);
            let end = sums[..self.keys.len()].partition_point(|&s| self.pad_top + s < hi);
            (start, end)
        };
        self.anchor = if pinned { None } else { self.first_visible(offset, |_| true) };
        if pinned { start = start.min(end.saturating_sub(32)); }
        self.range = start..end.max(start);
        self.before = self.prefix(start);
        self.after = total - self.prefix(end);
    }

    pub fn anchor_delta(&self) -> f32 {
        let Some((key, old)) = &self.anchor else { return 0.0 };
        let Some(&i) = self.index.get(key) else { return 0.0 };
        self.pad_top + self.prefix(i) - old
    }

    pub fn needs_window(&self, offset: f32, pinned: bool) -> bool {
        if self.keys.is_empty() { return false; }
        if pinned { return self.range.end != self.keys.len(); }
        let top = self.pad_top + self.before;
        let bottom = self.pad_top + self.prefix(self.range.end);
        (self.range.start > 0 && offset < top + self.viewport * 0.5)
            || (self.range.end < self.keys.len() && offset + self.viewport > bottom - self.viewport * 0.5)
    }
}

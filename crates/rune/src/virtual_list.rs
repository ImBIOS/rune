//! Virtual row bookkeeping. Only geometry outside the mounted window is estimated.

use std::collections::HashMap;
use std::ops::Range;

#[derive(Debug, Default)]
pub(crate) struct VirtualList {
    pub keys: Vec<String>,
    pub heights: HashMap<String, f32>,
    pub range: Range<usize>,
    pub before: f32,
    pub after: f32,
    pub estimate: f32,
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

    pub fn prefix(&self, end: usize) -> f32 {
        self.keys[..end].iter().map(|k| self.height(k)).sum()
    }

    pub fn select(&mut self, offset: f32, pinned: bool) {
        let total = self.prefix(self.keys.len());
        let offset = if pinned { (total + self.pad_top + self.pad_bottom - self.viewport).max(0.0) } else { offset };
        self.anchor = None;
        let mut y = self.pad_top;
        let mut start = 0;
        let mut end = 0;
        let lo = offset - self.viewport * 2.0;
        let hi = offset + self.viewport * 3.0;
        for (i, key) in self.keys.iter().enumerate() {
            let h = self.height(key);
            if y + h <= lo { start = i + 1; }
            if y < hi { end = i + 1; }
            if !pinned && self.anchor.is_none() && y + h > offset {
                self.anchor = Some((key.clone(), y));
            }
            y += h;
        }
        if pinned { start = start.min(end.saturating_sub(32)); }
        self.range = start..end.max(start);
        self.before = self.prefix(start);
        self.after = total - self.prefix(end);
    }

    pub fn anchor_delta(&self) -> f32 {
        let Some((key, old)) = &self.anchor else { return 0.0 };
        let Some(i) = self.keys.iter().position(|k| k == key) else { return 0.0 };
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

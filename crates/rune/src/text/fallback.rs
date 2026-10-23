//! System fallbacks loaded on first use: fonts for symbol blocks the eager symbol fallbacks
//! miss (math alphanumerics, enclosed alphanumerics, ...), and byte ranges of a text that should
//! try a fallback's fonts before the style's own.

use std::ops::Range;

use parley::fontique::Collection;

/// Why a byte range is routed ahead of the style's own fonts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Route {
    /// Mathematical Alphanumeric Symbols: one math font for a whole formula, so bold, italic
    /// and script letters don't mix designs when an earlier fallback covers a few of them.
    Math,
}

// Math alphanumerics and other symbol blocks. Loaded only when a text has a character in
// `needs_symbols`, and appended after the eager symbol families so existing picks don't move.
#[cfg(target_os = "macos")]
pub(super) const MATH_FONT_FILES: &[&str] = &["stixtwomath"];
#[cfg(target_os = "macos")]
pub(super) const MATH_FAMILIES: &[&str] = &["STIX Two Math"];
#[cfg(windows)]
pub(super) const MATH_FONT_FILES: &[&str] = &["cambriattc"];
#[cfg(windows)]
pub(super) const MATH_FAMILIES: &[&str] = &["Cambria Math"];
#[cfg(not(any(target_os = "macos", windows)))]
pub(super) const MATH_FONT_FILES: &[&str] = &["notosansmath", "stixtwomath", "notomusic", "symbola"];
#[cfg(not(any(target_os = "macos", windows)))]
pub(super) const MATH_FAMILIES: &[&str] = &["Noto Sans Math", "STIX Two Math", "Noto Music", "Symbola"];

/// Families of a fallback group, loaded on first use.
#[derive(Default)]
pub(super) struct Group {
    families: Option<Vec<&'static str>>,
}

impl Group {
    /// The group's installed families (empty when none are), loading its files the first time.
    pub(super) fn get(&mut self, collection: &mut Collection, files: &[&str], names: &[&'static str]) -> &[&'static str] {
        self.families.get_or_insert_with(|| {
            let mut paths = super::system_font_files(files);
            paths.sort();
            collection.load_fonts_from_paths(paths);
            names.iter().copied().filter(|name| collection.family_id(name).is_some()).collect()
        })
    }

    pub(super) fn loaded(&self) -> &[&'static str] {
        self.families.as_deref().unwrap_or(&[])
    }
}

/// Whether `ch` is in a symbol block the eager symbol fallbacks often miss. General punctuation
/// and currency (dashes, quotes, €) are left out: Latin prose is full of them.
pub(super) fn needs_symbols(ch: char) -> bool {
    matches!(ch as u32,
        0x2100..=0x2BFF | 0x2E00..=0x2E7F | 0x1D000..=0x1D7FF | 0x1F000..=0x1F0FF | 0x1F100..=0x1F1E5
        | 0x1F650..=0x1F67F | 0x1F700..=0x1F8FF | 0x1FA00..=0x1FA6F | 0x1FB00..=0x1FBFF)
}

/// Math alphanumerics, plus the Letterlike Symbols that fill that block's holes (`ℝ`, `ℎ`,
/// `ℬ`, ...), so double-struck and script runs keep one design.
pub(super) fn is_math_alphanumeric(ch: char) -> bool {
    matches!(ch as u32,
        0x1D400..=0x1D7FF | 0x2102 | 0x210A..=0x2113 | 0x2115 | 0x2119..=0x211D | 0x2124 | 0x2128 | 0x212C..=0x212D
        | 0x212F..=0x2131 | 0x2133..=0x2134 | 0x2145..=0x2149)
}

/// Appends routed byte ranges of `text`, merging neighbours on the same route.
pub(super) fn routes(text: &str, out: &mut Vec<(Range<usize>, Route)>) {
    out.clear();
    if text.is_ascii() {
        return;
    }
    for (i, ch) in text.char_indices() {
        let route = if is_math_alphanumeric(ch) { Route::Math } else { continue };
        let end = i + ch.len_utf8();
        match out.last_mut() {
            Some((range, r)) if *r == route && range.end == i => range.end = end,
            _ => out.push((i..end, route)),
        }
    }
}


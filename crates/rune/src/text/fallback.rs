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

/// A script the app's fonts usually lack, the blocks that use it and the system font files
/// (lowercase, without spaces or dots, most preferred first) that cover it. Loaded the first
/// time a text uses one of the blocks, as fallbacks for that script only.
pub(super) struct ScriptFonts {
    pub script: &'static [u8; 4],
    pub blocks: &'static [(u32, u32)],
    pub files: &'static [&'static str],
}

impl ScriptFonts {
    pub(super) fn covers(&self, ch: char) -> bool {
        self.blocks.iter().any(|&(lo, hi)| (lo..=hi).contains(&(ch as u32)))
    }
}

const HEBREW: &[(u32, u32)] = &[(0x0590, 0x05FF), (0xFB1D, 0xFB4F)];
const ARABIC: &[(u32, u32)] = &[(0x0600, 0x06FF), (0x0750, 0x077F), (0x0870, 0x08FF), (0xFB50, 0xFDFF), (0xFE70, 0xFEFF)];
const DEVANAGARI: &[(u32, u32)] = &[(0x0900, 0x097F), (0xA8E0, 0xA8FF)];
const BENGALI: &[(u32, u32)] = &[(0x0980, 0x09FF)];
const THAI: &[(u32, u32)] = &[(0x0E00, 0x0E7F)];

#[cfg(target_os = "macos")]
pub(super) const SCRIPT_FONTS: &[ScriptFonts] = &[
    ScriptFonts { script: b"Hebr", blocks: HEBREW, files: &["sfhebrewttf", "arialhb"] },
    ScriptFonts { script: b"Arab", blocks: ARABIC, files: &["sfarabicttf", "geezapro"] },
    ScriptFonts { script: b"Deva", blocks: DEVANAGARI, files: &["kohinoorttc", "itfdevanagari"] },
    ScriptFonts { script: b"Beng", blocks: BENGALI, files: &["kohinoorbangla", "banglamn"] },
    ScriptFonts { script: b"Thai", blocks: THAI, files: &["thonburi"] },
];
#[cfg(windows)]
pub(super) const SCRIPT_FONTS: &[ScriptFonts] = &[
    ScriptFonts { script: b"Hebr", blocks: HEBREW, files: &["segoeuittf", "arialttf"] },
    ScriptFonts { script: b"Arab", blocks: ARABIC, files: &["segoeuittf", "arialttf"] },
    ScriptFonts { script: b"Deva", blocks: DEVANAGARI, files: &["nirmala"] },
    ScriptFonts { script: b"Beng", blocks: BENGALI, files: &["nirmala"] },
    ScriptFonts { script: b"Thai", blocks: THAI, files: &["leelawui", "tahomattf"] },
];
#[cfg(not(any(target_os = "macos", windows)))]
pub(super) const SCRIPT_FONTS: &[ScriptFonts] = &[
    ScriptFonts { script: b"Hebr", blocks: HEBREW, files: &["notosanshebrew", "freesans"] },
    ScriptFonts { script: b"Arab", blocks: ARABIC, files: &["notosansarabic", "notonaskharabic", "freesans"] },
    ScriptFonts { script: b"Deva", blocks: DEVANAGARI, files: &["notosansdevanagari", "lohitdevanagari", "freesans"] },
    ScriptFonts { script: b"Beng", blocks: BENGALI, files: &["notosansbengali", "lohitbengali", "freesans"] },
    ScriptFonts { script: b"Thai", blocks: THAI, files: &["notosansthai", "tlwg", "freesans"] },
];

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


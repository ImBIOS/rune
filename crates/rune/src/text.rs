//! Parley layout (wrapping, rich spans, registered families) over embedded fonts, swash rasterising
//! glyphs and SVG icons into one greyscale atlas.
//!
//! Invariants: layouts are in device pixels; every layout is cached by a 64-bit hash of
//! (text, spans, style, scale, width), kept for two commits unused and then while a budget of
//! stale text lasts (`STALE_TEXT`), least recently used out first; glyph keys include
//! the run's variation coordinates, so one variable font serves every weight.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;

use parley::fontique::{Blob, Collection, CollectionOptions, FallbackKey, FamilyId, Script, SourceCache};
use parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, FontFamilyName, FontStyle, FontWeight, Layout, LayoutContext,
    LineHeight, PositionedLayoutItem, StyleProperty,
};
use swash::FontRef;
use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{Cap, Fill, Format, Join, Mask, Stroke, Transform, Vector};

use crate::color::{Color, srgb_encode};
use crate::tok::{Family, OpticalSize, TextStyle};

mod case;
#[cfg(all(test, target_os = "macos"))]
mod core_text_tests;
mod fallback;
mod measure;
mod svg_path;
mod trak;
pub use case::TextCase;
pub use measure::measure;
pub(crate) use case::Source as CaseSource;

use fallback::Route;
use trak::Trak;

/// Fonts' `trak` tables (None for an untracked font), by blob and index.
type Traks = HashMap<(u64, u32), Option<Rc<Trak>>>;

fn font_trak(traks: &mut Traks, data: &Blob<u8>, index: u32) -> Option<Rc<Trak>> {
    traks
        .entry((data.id(), index))
        .or_insert_with(|| FontRef::from_index(data.as_ref(), index as usize).and_then(Trak::of).map(Rc::new))
        .clone()
}

#[cfg(not(target_arch = "wasm32"))]
const DEFAULT_FONTS: [&[u8]; 3] = [
    include_bytes!("../assets/Inter-Regular.ttf"),
    include_bytes!("../assets/Inter-Medium.ttf"),
    include_bytes!("../assets/Inter-SemiBold.ttf"),
];

#[cfg(target_arch = "wasm32")]
const DEFAULT_FONTS: [&[u8]; 2] = [
    include_bytes!("../assets/Inter-Regular.ttf"),
    include_bytes!("../assets/IBMPlexMono-Regular.ttf"),
];

/// Scripts the app's own families usually lack: Han, kana, Hangul and Bopomofo.
const CJK_SCRIPTS: [&[u8; 4]; 5] = [b"Hani", b"Hira", b"Kana", b"Hang", b"Bopo"];

// Common-script symbols can inherit the script of adjacent letters. Put these families at
// the end of the requested family list, rather than registering only a Zyyy fallback.
// On macOS the system font comes first: its keyboard symbols (⌘⇧⏎⌫⎋⌥⌃) sit on the baseline at
// cap height and follow the requested weight through its `wght` axis. Apple Symbols draws
// them light, small and (⇧) below the baseline next to app fonts' capitals.
// The colour emoji family comes last, so symbols with a text form keep it.
#[cfg(target_os = "macos")]
const SYMBOL_FAMILIES: &[&str] = &["System Font", ".SF NS", "Apple Symbols", ".SF NS Symbols", "Menlo", "Apple Color Emoji"];
#[cfg(target_os = "macos")]
// SF italic rides along so `Family::Named("System Font").italic()` gets the real face.
const SYMBOL_FONT_FILES: &[&str] = &["sfnsttf", "sfnsitalic", "applesymbols", "sfnssymbols", "menlo", "applecoloremoji"];
#[cfg(windows)]
const SYMBOL_FAMILIES: &[&str] = &["Segoe UI Symbol", "Segoe UI Emoji"];
#[cfg(windows)]
const SYMBOL_FONT_FILES: &[&str] = &["seguisym", "seguiemj"];
#[cfg(not(any(target_os = "macos", windows)))]
const SYMBOL_FAMILIES: &[&str] = &["DejaVu Sans", "Noto Sans Symbols2", "Noto Sans Symbols", "Noto Color Emoji"];
#[cfg(not(any(target_os = "macos", windows)))]
const SYMBOL_FONT_FILES: &[&str] = &["dejavusans", "notosanssymbols", "notocoloremoji"];

// `Family::SystemMono`: the platform's monospaced UI families, most preferred first, and the
// files they're in. SF Mono's family name is `.SF NS Mono`, as SF's is `.SF NS`.
#[cfg(target_os = "macos")]
const SYSTEM_MONO_FAMILIES: &[&str] = &[".SF NS Mono", "Menlo"];
#[cfg(target_os = "macos")]
const SYSTEM_MONO_FILES: &[&str] = &["sfnsmono", "menlo"];
#[cfg(windows)]
const SYSTEM_MONO_FAMILIES: &[&str] = &["Cascadia Mono", "Consolas", "Courier New"];
#[cfg(windows)]
const SYSTEM_MONO_FILES: &[&str] = &["cascadiamono", "consola", "cour"];
// After fontconfig's own answer (`fontconfig_monospace`).
#[cfg(not(any(target_os = "macos", windows)))]
const SYSTEM_MONO_FAMILIES: &[&str] = &["DejaVu Sans Mono", "Liberation Mono", "Noto Sans Mono", "Ubuntu Mono"];
#[cfg(not(any(target_os = "macos", windows)))]
const SYSTEM_MONO_FILES: &[&str] = &["dejavusansmono", "liberationmono", "notosansmono", "ubuntumono"];

/// The family and file fontconfig resolves `monospace` to, asked once per process.
#[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
fn fontconfig_monospace() -> Option<(String, std::path::PathBuf)> {
    static ANSWER: std::sync::OnceLock<Option<(String, std::path::PathBuf)>> = std::sync::OnceLock::new();
    ANSWER.get_or_init(|| {
        let out = std::process::Command::new("fc-match").args(["--format=%{family[0]}\n%{file}", "monospace"]).output().ok()?;
        let out = String::from_utf8(out.stdout).ok()?;
        let (family, file) = out.split_once('\n')?;
        (!family.is_empty() && !file.is_empty()).then(|| (family.to_owned(), file.trim().into()))
    }).clone()
}

/// The installed `Family::SystemMono` families, most preferred first, loading their files.
fn load_system_mono(collection: &mut Collection, loaded: &mut LoadedFiles) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    #[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
    if let Some((family, file)) = fontconfig_monospace() {
        load_files(collection, loaded, [file]);
        names.push(family);
    }
    load_files(collection, loaded, system_font_files(SYSTEM_MONO_FILES).into_iter().take(12));
    for name in SYSTEM_MONO_FAMILIES {
        if !names.iter().any(|n| n == name) { names.push((*name).to_owned()); }
    }
    names.retain(|name| collection.family_id(name).is_some());
    names
}

/// Whether the CSS family list `list` names `ui-monospace`, which stands for `Family::SystemMono`'s
/// families, so the mono slot can be SF Mono, whose family name is hidden.
fn names_system_mono(list: &str) -> bool {
    FontFamilyName::parse_css_list(list).flatten().any(|f| f == FontFamilyName::Generic(parley::GenericFamily::UiMonospace))
}

/// `list` parsed, with `ui-monospace` replaced by the installed `Family::SystemMono` families.
/// When that leaves no named family (no monospace font installed, or the web), `sans` follows,
/// so text still finds a font.
fn family_list<'a>(list: &'a str, system_mono: &'a [String], sans: &'a str) -> Vec<FontFamilyName<'a>> {
    let mut system = false;
    let mut out: Vec<FontFamilyName<'a>> = FontFamilyName::parse_css_list(list)
        .flatten()
        .flat_map(|f| {
            let ui_mono = f == FontFamilyName::Generic(parley::GenericFamily::UiMonospace);
            system |= ui_mono;
            let names = if ui_mono { system_mono } else { &[] };
            names.iter().map(|name| FontFamilyName::named(name)).chain((!ui_mono).then_some(f)).collect::<Vec<_>>()
        })
        .collect();
    if system && !out.iter().any(|f| matches!(f, FontFamilyName::Named(_))) {
        out.extend(FontFamilyName::parse_css_list(sans).flatten());
    }
    out
}

/// The installed families among `names`, loading the system font files matching `files`.
fn load_families(collection: &mut Collection, files: &[&str], names: &[&'static str]) -> Vec<&'static str> {
    collection.load_fonts_from_paths(system_font_files(files));
    names.iter().copied().filter(|name| collection.family_id(name).is_some()).collect()
}

/// File-name fragments (lowercase) of the CJK fonts desktops ship, most preferred first. Only
/// these files are loaded, and only as fallbacks, so Latin text stays on the app's own fonts.
const CJK_FONT_FILES: [&str; 24] = [
    "notosanscjk", "notosanssc", "notosansjp", "notosanstc", "notosanshk", "notosanskr", "sourcehansans",
    "notoserifcjk", "sourcehanserif", "pingfang", "hiragino", "msyh", "yugoth", "meiryo", "malgun", "msjh",
    "wqy", "wenquanyi", "droidsansfallback", "sarasa", "lxgw", "ukai", "uming", "simsun",
];

/// Makes text in scripts the app's fonts don't cover render instead of drawing tofu. Families
/// the app registered beyond its sans and mono (`AppSpec::fonts`) come first, then the CJK fonts
/// installed on the system (found by file name in the platform's font directories, including
/// `$XDG_DATA_HOME/fonts` and `$XDG_DATA_DIRS/*/fonts`).
fn register_cjk_fallbacks(collection: &mut Collection, loaded: &mut LoadedFiles, registered: &[FamilyId], families: (&str, &str)) {
    let primary = [collection.family_id(families.0), collection.family_id(families.1)];
    let mut fallbacks: Vec<FamilyId> = Vec::new();
    for id in registered {
        if !primary.contains(&Some(*id)) && !fallbacks.contains(id) {
            fallbacks.push(*id);
        }
    }
    // Memory-mapped, so a 20 MB CJK collection costs a mapping, not a read.
    for id in load_files(collection, loaded, system_font_files(&CJK_FONT_FILES).into_iter().take(8)) {
        if !primary.contains(&Some(id)) && !fallbacks.contains(&id) {
            fallbacks.push(id);
        }
    }
    if fallbacks.is_empty() {
        return;
    }
    for script in CJK_SCRIPTS {
        collection.append_fallbacks(FallbackKey::new(Script::from_bytes(*script), None), fallbacks.iter().copied());
    }
}

/// The families each system font file added to the collection, so a file is loaded once.
type LoadedFiles = HashMap<std::path::PathBuf, Vec<FamilyId>>;

/// The families of `paths`, in order, loading each file the first time.
fn load_files(collection: &mut Collection, loaded: &mut LoadedFiles, paths: impl IntoIterator<Item = std::path::PathBuf>) -> Vec<FamilyId> {
    let mut families: Vec<FamilyId> = Vec::new();
    for path in paths {
        let ids = loaded.entry(path).or_insert_with_key(|path| {
            let before: Vec<FamilyId> = collection.family_ids().collect();
            collection.load_fonts_from_paths([path]);
            collection.family_ids().filter(|id| !before.contains(id)).collect()
        });
        for id in ids.iter() {
            if !families.contains(id) {
                families.push(*id);
            }
        }
    }
    families
}

/// Appends the system fonts for `fonts.script` as that script's fallbacks.
fn register_script_fallbacks(collection: &mut Collection, loaded: &mut LoadedFiles, fonts: &fallback::ScriptFonts) {
    let added = load_files(collection, loaded, system_font_files(fonts.files).into_iter().take(4));
    if !added.is_empty() {
        collection.append_fallbacks(FallbackKey::new(Script::from_bytes(*fonts.script), None), added.into_iter());
    }
}

/// Font files whose lowercase name, without spaces and dots, contains one of `fragments`,
/// ordered by the first fragment they match, then by path. The platform's font directories are
/// walked once per process.
#[cfg(target_arch = "wasm32")]
fn system_font_files(_: &[&str]) -> Vec<std::path::PathBuf> { Vec::new() }

#[cfg(not(target_arch = "wasm32"))]
fn system_font_files(fragments: &[&str]) -> Vec<std::path::PathBuf> {
    static FILES: std::sync::OnceLock<Vec<(String, std::path::PathBuf)>> = std::sync::OnceLock::new();
    let mut files: Vec<_> = FILES
        .get_or_init(walk_font_dirs)
        .iter()
        .filter_map(|(name, path)| Some((fragments.iter().position(|fragment| name.contains(fragment))?, path)))
        .collect();
    files.sort();
    files.into_iter().map(|(_, path)| path.clone()).collect()
}

#[cfg(not(target_arch = "wasm32"))]
fn walk_font_dirs() -> Vec<(String, std::path::PathBuf)> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        dirs.extend(["/System/Library/Fonts", "/Library/Fonts"].map(Into::into));
    } else if cfg!(windows) {
        let root = std::env::var_os("WINDIR").map_or_else(|| "C:\\Windows".into(), std::path::PathBuf::from);
        dirs.push(root.join("Fonts"));
    } else {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".local/share")));
        dirs.extend(data_home.map(|d| d.join("fonts")));
        dirs.extend(home.map(|h| h.join(".fonts")));
        let data_dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
        dirs.extend(data_dirs.split(':').filter(|d| !d.is_empty()).map(|d| std::path::Path::new(d).join("fonts")));
    }
    let mut found: Vec<(String, std::path::PathBuf)> = Vec::new();
    let mut stack = dirs;
    let mut visited = std::collections::HashSet::new();
    while let Some(dir) = stack.pop() {
        if visited.len() > 4096 || !visited.insert(dir.clone()) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let font = [".ttf", ".otf", ".ttc", ".otc"].iter().any(|ext| name.ends_with(ext));
            if font {
                found.push((name.replace([' ', '.'], ""), path));
            }
        }
    }
    found
}

pub const ATLAS_SIZE: u32 = 2048;
const SUBPIXEL_STEPS: f32 = 4.0;

/// Brush carried through Parley: 0 is the element's own colour, n is `spans[n - 1]`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ink(pub u16);

/// A styled byte range of a text element (markdown inline code, bold, links).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub weight: Option<f32>,
    pub mono: bool,
    pub size: Option<f32>,
    pub color: Option<Color>,
    /// Rounded background behind the span's runs (inline code), padded by `bg_pad` points.
    pub bg: Option<Color>,
    pub bg_pad: f32,
    pub underline: Option<Color>,
    /// The family's italic face, or a synthetic oblique when it has none.
    pub italic: bool,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Span {
        Span { start: start as u32, end: end as u32, ..Default::default() }
    }
}

/// A pressable byte range of a text element: a link, an @mention, a file chip. Attach with
/// [`El::link`](crate::El::link) and handle with [`El::on_link`](crate::El::on_link). Its resting
/// look is whatever [`Span`]s cover it; [`LinkPaint`]s add the hover, pressed and focus looks.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub(crate) start: u32,
    pub(crate) end: u32,
    pub(crate) id: String,
    pub(crate) role: &'static str,
    pub(crate) label: Option<String>,
    pub(crate) hover: LinkPaint,
    pub(crate) pressed: Option<LinkPaint>,
    pub(crate) focused: Option<LinkPaint>,
}

impl Link {
    /// Bytes `range` of the text, reported to `on_link` as `id`. Its role is "link".
    pub fn new(range: std::ops::Range<usize>, id: impl Into<String>) -> Link {
        Link {
            start: range.start as u32,
            end: range.end as u32,
            id: id.into(),
            role: "link",
            label: None,
            hover: LinkPaint::default(),
            pressed: None,
            focused: None,
        }
    }
    /// Accessibility role, such as "button" for a file chip.
    pub fn role(mut self, role: &'static str) -> Self {
        self.role = role;
        self
    }
    /// Accessible name; the range's text by default.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
    /// The look while the pointer is over it.
    pub fn hover(mut self, paint: LinkPaint) -> Self {
        self.hover = paint;
        self
    }
    /// The look while it is held down; the hover look by default.
    pub fn pressed(mut self, paint: LinkPaint) -> Self {
        self.pressed = Some(paint);
        self
    }
    /// The look while it has keyboard focus; the hover look by default, or an underline in
    /// its text colour when that is empty.
    pub fn focused(mut self, paint: LinkPaint) -> Self {
        self.focused = Some(paint);
        self
    }
}

/// A link's look in one state, painted over its resting span style without reshaping. Unset
/// parts keep the span's own.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LinkPaint {
    pub(crate) color: Option<Color>,
    pub(crate) bg: Option<Color>,
    pub(crate) underline: Option<Color>,
}

impl LinkPaint {
    pub fn new() -> LinkPaint {
        LinkPaint::default()
    }
    /// Glyph colour.
    pub fn color(mut self, c: Color) -> Self {
        self.color = Some(c);
        self
    }
    /// A rounded background behind each line of the range.
    pub fn bg(mut self, c: Color) -> Self {
        self.bg = Some(c);
        self
    }
    pub fn underline(mut self, c: Color) -> Self {
        self.underline = Some(c);
        self
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.color.is_none() && self.bg.is_none() && self.underline.is_none()
    }
}

/// Looked up for every glyph of every frame, so hashed with Fx rather than SipHash.
type GlyphMap<V> = HashMap<GlyphKey, V, std::hash::BuildHasherDefault<rune_motion::Fx>>;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    font: u64,
    index: u32,
    glyph: u32,
    size: u32,
    coords: u64,
    subpx: u8,
    dilation: u32,
    /// Synthetic oblique, in degrees (0 for none).
    skew: u8,
}

#[derive(Clone, Copy)]
struct AtlasGlyph {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    left: i32,
    top: i32,
}

/// One glyph to draw: device-pixel rect, atlas origin, brush and source byte (for reveals).
#[derive(Clone, Copy, Debug)]
pub struct GlyphQuad {
    pub rect: [f32; 4],
    pub uv: [f32; 2],
    pub ink: u16,
    pub byte: u32,
    /// A colour glyph (emoji) drawn from [`TextSystem::color_glyph`] instead of the alpha
    /// atlas: 0 for none, else its index plus one.
    pub color: u32,
}

/// A colour glyph's RGBA image (`GlyphQuad::color`) and its placement around the pen (device px).
#[derive(Clone, Copy)]
struct ColorGlyph {
    index: u32,
    left: i32,
    top: i32,
    w: u32,
    h: u32,
}

impl ColorGlyph {
    fn quad(&self, x: f32, y: f32, ink: u16, byte: u32) -> GlyphQuad {
        let rect = [x.round() + self.left as f32, y.round() - self.top as f32, self.w as f32, self.h as f32];
        GlyphQuad { rect, uv: [0.0, 0.0], ink, byte, color: self.index }
    }
}

/// A rect drawn under or over text: span backgrounds and underlines (device px).
#[derive(Clone, Copy, Debug)]
pub struct Deco {
    pub rect: [f32; 4],
    pub color: Color,
    pub radius: f32,
}

pub struct Atlas {
    pub data: Vec<u8>,
    /// Rows `[y0, y1)` changed since the last upload.
    pub dirty: Option<(u32, u32)>,
    shelf_x: u32,
    shelf_y: u32,
    shelf_h: u32,
    map: GlyphMap<Option<AtlasGlyph>>,
    icons: HashMap<(usize, u32), Option<AtlasGlyph>>,
}

impl Atlas {
    /// Marks every row for upload (a new texture, e.g. after the window's format changed).
    pub fn invalidate(&mut self) {
        self.dirty = Some((0, ATLAS_SIZE));
    }

    /// Rasterized glyph entries, for tests that bound the atlas.
    #[cfg(test)]
    pub(crate) fn glyph_entries(&self) -> usize { self.map.len() }

    fn new() -> Self {
        Atlas {
            data: vec![0; (ATLAS_SIZE * ATLAS_SIZE) as usize],
            dirty: None,
            shelf_x: 1,
            shelf_y: 1,
            shelf_h: 0,
            map: GlyphMap::default(),
            icons: HashMap::new(),
        }
    }

    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if self.shelf_x + w + 1 > ATLAS_SIZE {
            self.shelf_y += self.shelf_h + 1;
            self.shelf_x = 1;
            self.shelf_h = 0;
        }
        if self.shelf_y + h + 1 > ATLAS_SIZE {
            return None;
        }
        let pos = (self.shelf_x, self.shelf_y);
        self.shelf_x += w + 1;
        self.shelf_h = self.shelf_h.max(h);
        Some(pos)
    }

    fn put(&mut self, w: u32, h: u32, src: &[u8], left: i32, top: i32) -> Option<AtlasGlyph> {
        let (ax, ay) = self.alloc(w, h)?;
        for row in 0..h {
            let s = &src[(row * w) as usize..((row + 1) * w) as usize];
            let dst = ((ay + row) * ATLAS_SIZE + ax) as usize;
            self.data[dst..dst + w as usize].copy_from_slice(s);
        }
        let d = self.dirty.get_or_insert((ay, ay + h));
        *d = (d.0.min(ay), d.1.max(ay + h));
        Some(AtlasGlyph { x: ax, y: ay, w, h, left, top })
    }
}

/// Lean of a synthetic oblique, in degrees, for faces with no italic.
const SYNTHETIC_OBLIQUE: u8 = 12;

fn rasterize(scaler: &mut swash::scale::Scaler<'_>, atlas: &mut Atlas, glyph: u16, subpx: u8, dilation: f32, skew: u8) -> Option<AtlasGlyph> {
    // Outlines are y-up here, so a positive x-skew leans the tops right.
    let lean = (skew != 0).then(|| Transform::skew(swash::zeno::Angle::from_degrees(skew as f32), swash::zeno::Angle::ZERO));
    let image = Render::new(&[Source::Outline])
        .format(Format::Alpha)
        .transform(lean)
        // Swash expands toward +x/+y; offset back to preserve the outline's centre.
        .embolden(dilation)
        .offset(Vector::new(subpx as f32 / SUBPIXEL_STEPS - dilation, -dilation))
        .render(scaler, glyph)?;
    let (w, h) = (image.placement.width, image.placement.height);
    if w == 0 || h == 0 || !image.data.iter().any(|&coverage| coverage != 0) {
        return None;
    }
    atlas.put(w, h, &image.data, image.placement.left, image.placement.top)
}

/// The colour image of `glyph` (COLR layers or an embedded bitmap scaled to size), or None
/// when the font draws it as an outline.
fn rasterize_color(scaler: &mut swash::scale::Scaler<'_>, glyph: u16) -> Option<(crate::pixels::Pixels, swash::zeno::Placement)> {
    if !scaler.has_color_bitmaps() && !scaler.has_color_outlines() {
        return None;
    }
    let image = Render::new(&[Source::ColorOutline(0), Source::ColorBitmap(StrikeWith::BestFit)]).render(scaler, glyph)?;
    let (w, h) = (image.placement.width, image.placement.height);
    if image.content != Content::Color || w == 0 || h == 0 || image.data.len() != (w * h * 4) as usize {
        return None;
    }
    let pixels = crate::pixels::Pixels::new();
    pixels.set(w, h, image.data);
    Some((pixels, image.placement))
}

/// CoreText-calibrated coverage for a linear-light target. Keep colour adjustment out of
/// atlas keys: theme animation must not rasterize five copies or jump between buckets.
/// See research/23-text-quality.md for the held-out glyph and motion measurements.
pub fn glyph_coverage_exponent(color: Color, scale: f32) -> f32 {
    // Stronger bright-ink contrast at Retina density limits phase-dependent blooming.
    0.35 + (1.15 + 0.2 * (scale - 1.0).clamp(0.0, 1.0)) * srgb_encode(color.luminance())
}

// ---------------------------------------------------------------------------------------------
// Icons: a subset of SVG (path, rect, circle, ellipse, line; fill/stroke, caps, joins, evenodd).

/// An SVG icon body as found in `@opencode/ui`'s catalog: elements inside a square viewBox.
#[derive(Debug)]
pub struct Icon {
    pub name: &'static str,
    pub view: f32,
    pub body: &'static str,
}

fn attr<'a>(el: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = el;
    loop {
        let i = rest.find(name)?;
        let before = rest[..i].chars().last();
        let after = &rest[i + name.len()..];
        if before.is_some_and(|c| c.is_whitespace()) && after.starts_with("=\"") {
            let v = &after[2..];
            return Some(&v[..v.find('"')?]);
        }
        rest = &rest[i + name.len()..];
    }
}

fn num(el: &str, name: &str) -> f32 {
    attr(el, name).and_then(|v| v.trim_end_matches("px").parse().ok()).unwrap_or(0.0)
}

/// SVG path data for one icon element, in viewBox units.
fn element_path(el: &str) -> Option<String> {
    let tag = el.trim_start_matches('<').split_whitespace().next()?;
    Some(match tag {
        "path" => attr(el, "d")?.to_owned(),
        "rect" => {
            let (x, y, w, h) = (num(el, "x"), num(el, "y"), num(el, "width"), num(el, "height"));
            let r = num(el, "rx").min(w / 2.0).min(h / 2.0);
            if r <= 0.0 {
                format!("M{x} {y}h{w}v{h}h{}Z", -w)
            } else {
                format!(
                    "M{} {y}h{}a{r} {r} 0 0 1 {r} {r}v{}a{r} {r} 0 0 1 {} {r}h{}a{r} {r} 0 0 1 {} {}v{}a{r} {r} 0 0 1 {r} {}Z",
                    x + r,
                    w - 2.0 * r,
                    h - 2.0 * r,
                    -r,
                    -(w - 2.0 * r),
                    -r,
                    -r,
                    -(h - 2.0 * r),
                    -r
                )
            }
        }
        "circle" | "ellipse" => {
            let (cx, cy) = (num(el, "cx"), num(el, "cy"));
            let (rx, ry) = if tag == "circle" { (num(el, "r"), num(el, "r")) } else { (num(el, "rx"), num(el, "ry")) };
            format!(
                "M{} {cy}a{rx} {ry} 0 1 0 {} 0a{rx} {ry} 0 1 0 {} 0Z",
                cx - rx,
                2.0 * rx,
                -2.0 * rx
            )
        }
        "line" => format!("M{} {}L{} {}", num(el, "x1"), num(el, "y1"), num(el, "x2"), num(el, "y2")),
        _ => return None,
    })
}

fn rasterize_icon(icon: &Icon, px: u32, atlas: &mut Atlas) -> Option<AtlasGlyph> {
    atlas.put(px, px, &icon_coverage(icon, px), 0, 0)
}

/// `icon`'s coverage at `px` × `px` device pixels, row-major, as the glyph atlas stores it.
pub fn icon_coverage(icon: &Icon, px: u32) -> Vec<u8> {
    let s = px as f32 / icon.view;
    let mut acc = vec![0u8; (px * px) as usize];
    let mut buf = vec![0u8; (px * px) as usize];
    for raw in icon.body.split('>') {
        let el = raw.trim();
        if el.is_empty() || el.starts_with("</") {
            continue;
        }
        let Some(d) = element_path(el) else { continue };
        let d = svg_path::parse(&d);
        let stroke = attr(el, "stroke").is_some_and(|v| v != "none");
        let fill = attr(el, "fill").map_or(!stroke, |v| v != "none");
        let mut paint = |style: swash::zeno::Style<'_>| {
            buf.iter_mut().for_each(|b| *b = 0);
            Mask::new(d.as_slice())
                .style(style)
                .transform(Some(Transform::scale(s, s)))
                .size(px, px)
                .render_into(&mut buf, None);
            for (a, &b) in acc.iter_mut().zip(buf.iter()) {
                let (fa, fb) = (*a as u32, b as u32);
                *a = (fa + fb - fa * fb / 255).min(255) as u8;
            }
        };
        if fill {
            let rule = if attr(el, "fill-rule") == Some("evenodd") { Fill::EvenOdd } else { Fill::NonZero };
            paint(rule.into());
        }
        if stroke {
            let w = attr(el, "stroke-width").and_then(|v| v.parse().ok()).unwrap_or(1.0f32);
            // The transform scales the width with the geometry.
            let mut st = Stroke::new(w);
            st.cap(match attr(el, "stroke-linecap") {
                Some("round") => Cap::Round,
                Some("square") => Cap::Square,
                _ => Cap::Butt,
            });
            st.join(match attr(el, "stroke-linejoin") {
                Some("round") => Join::Round,
                Some("bevel") => Join::Bevel,
                _ => Join::Miter,
            });
            paint(st.into());
        }
    }
    acc
}

// ---------------------------------------------------------------------------------------------

/// Shaped text by hash: the value, the commit generation it was last used in, and its text's
/// length in bytes.
type Cache<T> = HashMap<u64, (Rc<T>, u32, u32)>;

/// Bytes of text, each layout counting [`STALE_ENTRY`] more, kept shaped after the commits
/// that used it, so a view shown again after a while (a list returned to from a grid someone
/// browsed) doesn't reshape. About 800 short labels or 64 KB of paragraphs, either about 3 MB.
const STALE_TEXT: usize = 64 * 1024;
const STALE_ENTRY: usize = 64;

/// Drops what `cache` hasn't used in the last two commits once it outgrows [`STALE_TEXT`],
/// least recently used first. A view's layouts all share the commit it was last shown in, so
/// that commit's are kept while the budget lasts rather than all dropped together.
fn evict<T>(cache: &mut Cache<T>, g: u32) {
    let stale = |used: u32| g.wrapping_sub(used) >= 2;
    let cost = |bytes: u32| bytes as usize + STALE_ENTRY;
    let total: usize = cache.values().filter(|e| stale(e.1)).map(|e| cost(e.2)).sum();
    if total <= STALE_TEXT {
        return;
    }
    let mut ages: Vec<(u32, usize)> = cache.values().filter(|e| stale(e.1)).map(|e| (g.wrapping_sub(e.1), cost(e.2))).collect();
    ages.sort_unstable_by_key(|a| a.0);
    let (mut younger, mut kept, mut cutoff) = (0, 0, u32::MAX);
    for (i, &(age, cost)) in ages.iter().enumerate() {
        if i > 0 && ages[i - 1].0 != age { younger = kept; }
        kept += cost;
        if kept > STALE_TEXT {
            cutoff = age;
            break;
        }
    }
    let mut room = STALE_TEXT - younger;
    cache.retain(|_, e| {
        let age = g.wrapping_sub(e.1);
        age < cutoff || age == cutoff && cost(e.2) <= room && { room -= cost(e.2); true }
    });
}

pub struct TextSystem {
    font_cx: FontContext,
    layout_cx: LayoutContext<Ink>,
    scale_cx: ScaleContext,
    layouts: Cache<Layout<Ink>>,
    runs: Cache<RunLayout>,
    generation: u32,
    pub sans: String,
    pub mono: String,
    symbol_families: Vec<&'static str>,
    /// The colour emoji family among `symbol_families`.
    emoji: Option<&'static str>,
    /// The installed math families, once a text has needed them.
    math: Option<Vec<&'static str>>,
    /// The installed `Family::SystemMono` families, once a text has needed them.
    system_mono: Option<Vec<String>>,
    /// The sans or mono slot names `ui-monospace` (see `family_list`).
    slot_system_mono: bool,
    /// Bit `i` is set once `fallback::SCRIPT_FONTS[i]` has been looked up.
    scripts: u64,
    font_files: LoadedFiles,
    pub atlas: Atlas,
    color_glyphs: GlyphMap<ColorGlyph>,
    color_pixels: Vec<crate::pixels::Pixels>,
    /// Layouts built since the last commit (shaping work counter for perf gates).
    pub shaped: u32,
    /// Reused across lines and frames when mapping reveal glyphs back to source bytes.
    cluster_scratch: Vec<(f32, u32)>,
    coords_scratch: Vec<i16>,
    traks: Traks,
    /// The `trak` table of the first font of a family slot at a weight, upright or italic.
    primary_traks: HashMap<(Family, u32, bool), Option<Rc<Trak>>>,
}

impl Default for TextSystem {
    fn default() -> Self {
        Self::new(&[], ("Inter", "Inter"))
    }
}

pub fn text_hash(text: &str, spans: &[Span], style: TextStyle, scale: f32, width: Option<f32>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    for s in spans {
        (s.start, s.end, s.mono, s.weight.map(f32::to_bits), s.size.map(f32::to_bits), s.italic).hash(&mut h);
    }
    (style.size.to_bits(), style.weight.to_bits(), style.family, style.line_height.to_bits(), style.tracking.to_bits())
        .hash(&mut h);
    (scale.to_bits(), width.map(f32::to_bits)).hash(&mut h);
    let optical = match style.optical_size {
        OpticalSize::Auto => (0, 0), OpticalSize::FontDefault => (1, 0), OpticalSize::Fixed(size) => (2, size.to_bits()),
    };
    (style.variations, style.features, optical, style.italic).hash(&mut h);
    h.finish()
}

/// A string that several text cells each show one grapheme of (see [`El::advance_in`]), hashed
/// once so cells and frames can look up its shaped layout without rehashing it.
///
/// [`El::advance_in`]: crate::El::advance_in
#[derive(Clone, Debug)]
pub struct Run {
    text: Rc<str>,
    pub(crate) hash: u64,
}

impl Run {
    pub fn new(text: impl Into<Rc<str>>) -> Run {
        let text = text.into();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        Run { hash: h.finish(), text }
    }
    pub fn as_str(&self) -> &str {
        &self.text
    }
    /// The run's cache key for one style and scale.
    pub(crate) fn key(&self, style: TextStyle, scale: f32) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (self.hash, text_hash("", &[], style, scale, None)).hash(&mut h);
        h.finish()
    }
}

/// One glyph run's face, as the rasterizer needs it.
struct Face {
    font: parley::FontData,
    size: f32,
    coords_key: u64,
    ink: u16,
    /// A face without italics leans by a fixed angle instead of fontique's 14°.
    skew: u8,
    baseline: f32,
}

impl Face {
    /// The face of `glyph_run`; its normalized coordinates go to `coords`.
    fn of(glyph_run: &parley::GlyphRun<'_, Ink>, coords: &mut Vec<i16>) -> Face {
        let run = glyph_run.run();
        coords.clear();
        coords.extend(run.normalized_coords().iter().map(|c| c.to_bits()));
        Face {
            font: run.font().clone(),
            size: run.font_size(),
            coords_key: coords.iter().fold(0u64, |h, &c| h.wrapping_mul(31).wrapping_add(c as u16 as u64)),
            ink: glyph_run.style().brush.0,
            skew: if run.synthesis().skew().is_some() { SYNTHETIC_OBLIQUE } else { 0 },
            baseline: glyph_run.baseline(),
        }
    }
}

struct RunFace {
    face: Face,
    coords: Vec<i16>,
}

struct RunGlyph {
    face: u16,
    id: u32,
    /// From the layout's left edge and its line's baseline (device px).
    x: f32,
    y: f32,
}

/// A [`Run`] shaped as one line, with each grapheme's offset and glyphs.
pub struct RunLayout {
    pub layout: Rc<Layout<Ink>>,
    /// Per grapheme: (byte, x on the 1/64 px grid element rects snap to, first glyph); then
    /// (len, end rounded up to a device pixel, glyph count).
    graphemes: Vec<(u32, f32, u32)>,
    faces: Vec<RunFace>,
    glyphs: Vec<RunGlyph>,
}

impl RunLayout {
    fn new(layout: Rc<Layout<Ink>>, text: &str) -> RunLayout {
        use unicode_segmentation::UnicodeSegmentation;
        let starts: Vec<u32> = text.grapheme_indices(true).map(|(i, _)| i as u32).collect();
        let grapheme_of = |byte: u32| starts.partition_point(|&s| s <= byte).saturating_sub(1);
        let mut xs = vec![f32::INFINITY; starts.len()];
        let mut faces = Vec::new();
        let mut glyphs: Vec<(usize, RunGlyph)> = Vec::new();
        for line in layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(glyph_run) = item else { continue };
                let mut coords = Vec::new();
                let face = Face::of(&glyph_run, &mut coords);
                faces.push(RunFace { face, coords });
                let face = faces.len() as u16 - 1;
                // Glyphs belong to the cluster they were shaped in (by count, not by x), so a
                // zero-advance mark stays with its base.
                let mut owners = glyph_run.run().visual_clusters().flat_map(|cl| {
                    std::iter::repeat_n(cl.text_range().start as u32, cl.glyphs().count())
                });
                let mut x = glyph_run.offset();
                for cl in glyph_run.run().visual_clusters() {
                    let gi = grapheme_of(cl.text_range().start as u32);
                    if let Some(g) = xs.get_mut(gi) {
                        *g = g.min(x);
                    }
                    x += cl.advance();
                }
                let mut rel = glyph_run.offset();
                for g in glyph_run.glyphs() {
                    let byte = owners.next().unwrap_or(0);
                    glyphs.push((grapheme_of(byte), RunGlyph { face, id: g.id, x: rel + g.x, y: g.y }));
                    rel += g.advance;
                }
            }
        }
        glyphs.sort_by_key(|g| g.0);
        let mut graphemes = Vec::with_capacity(starts.len() + 1);
        let mut prev = 0.0;
        for (i, &start) in starts.iter().enumerate() {
            let x = if xs[i].is_finite() { xs[i] } else { prev };
            prev = x;
            graphemes.push((start, (x * 64.0).round() / 64.0, glyphs.partition_point(|g| g.0 < i) as u32));
        }
        graphemes.push((text.len() as u32, (layout.full_width() - 1e-3).ceil(), glyphs.len() as u32));
        RunLayout { layout, graphemes, faces, glyphs: glyphs.into_iter().map(|g| g.1).collect() }
    }

    /// The index of the grapheme starting at byte `at` (never the end sentinel).
    fn grapheme(&self, at: usize) -> Option<usize> {
        let i = self.graphemes.binary_search_by_key(&(at as u32), |e| e.0).ok()?;
        (i + 1 < self.graphemes.len()).then_some(i)
    }

    /// The x (device px) of the grapheme at byte `at`.
    pub fn x(&self, at: usize) -> Option<f32> {
        self.grapheme(at).map(|i| self.graphemes[i].1)
    }

    /// Width in points of the grapheme at byte `at`: the gap to the next grapheme, or for the
    /// last to the run's end rounded up as [`TextSystem::measure`] does, so cells measured this
    /// way keep the run's kerning and sum to its measured width.
    pub fn advance(&self, at: usize, scale: f32) -> Option<f32> {
        let i = self.grapheme(at)?;
        Some(((self.graphemes[i + 1].1 - self.graphemes[i].1) / scale).max(0.0))
    }
}

fn variations(style: TextStyle, size: f32) -> Vec<parley::FontVariation> {
    let mut settings: Vec<_> = parley::FontVariation::parse_css_list(style.variations).flatten().collect();
    let opsz = parley::setting::Tag::new(b"opsz");
    let value = match style.optical_size {
        OpticalSize::Auto if !settings.iter().any(|v| v.tag == opsz) => Some(size),
        OpticalSize::Fixed(value) => { settings.retain(|v| v.tag != opsz); Some(value) }
        _ => None,
    };
    if let Some(value) = value.filter(|v| v.is_finite() && *v > 0.0) {
        settings.push(parley::FontVariation::new(opsz, value));
    }
    settings
}

impl TextSystem {
    /// `fonts` replaces the built-in Inter faces when non-empty on native. Web keeps its
    /// bundled sans and mono fallbacks, since there are no system fonts to discover.
    /// `families` names the sans and mono slots; others are selectable with `Family::Named`.
    pub fn new(fonts: &[&'static [u8]], families: (&str, &str)) -> Self {
        measure::note_fonts(fonts, families);
        let mut collection = Collection::new(CollectionOptions { shared: false, system_fonts: false });
        let list: &[&'static [u8]] = if fonts.is_empty() { &DEFAULT_FONTS } else { fonts };
        let mut registered = Vec::new();
        #[cfg(target_arch = "wasm32")]
        if !fonts.is_empty() {
            for data in DEFAULT_FONTS {
                collection.register_fonts(Blob::new(Arc::new(data)), None);
            }
        }
        for data in list {
            registered.extend(collection.register_fonts(Blob::new(Arc::new(*data)), None).into_iter().map(|(id, _)| id));
        }
        let mut font_files = LoadedFiles::new();
        register_cjk_fallbacks(&mut collection, &mut font_files, &registered, families);
        let symbol_families = load_families(&mut collection, SYMBOL_FONT_FILES, SYMBOL_FAMILIES);
        let emoji = symbol_families.iter().copied().find(|f| f.contains("Emoji"));
        #[cfg(target_arch = "wasm32")]
        let families = if fonts.is_empty() { ("Inter", "IBM Plex Mono") } else { families };
        TextSystem {
            font_cx: FontContext { collection, source_cache: SourceCache::default() },
            layout_cx: LayoutContext::new(),
            scale_cx: ScaleContext::new(),
            layouts: HashMap::new(),
            runs: HashMap::new(),
            generation: 0,
            sans: if cfg!(target_arch = "wasm32") { format!("{}, Inter", families.0) } else { families.0.to_owned() },
            mono: if cfg!(target_arch = "wasm32") { format!("{}, IBM Plex Mono", families.1) } else { families.1.to_owned() },
            symbol_families,
            emoji,
            math: None,
            system_mono: None,
            slot_system_mono: names_system_mono(families.0) || names_system_mono(families.1),
            scripts: 0,
            font_files,
            atlas: Atlas::new(),
            color_glyphs: GlyphMap::default(),
            color_pixels: Vec::new(),
            shaped: 0,
            cluster_scratch: Vec::new(),
            coords_scratch: Vec::new(),
            traks: Traks::new(),
            primary_traks: HashMap::new(),
        }
    }

    /// The `trak` table of the font text in `family` at `weight` shapes with first.
    fn primary_trak(&mut self, family: Family, weight: f32, italic: bool) -> Option<Rc<Trak>> {
        let key = (family, weight.to_bits(), italic);
        if let Some(trak) = self.primary_traks.get(&key) {
            return trak.clone();
        }
        if family == Family::SystemMono || self.slot_system_mono {
            self.system_mono();
        }
        let system = self.system_mono.as_deref().unwrap_or_default();
        let named = |list: &str| -> Vec<String> {
            family_list(list, system, &self.sans)
                .into_iter()
                .filter_map(|f| match f { FontFamilyName::Named(name) => Some(name.into_owned()), _ => None })
                .collect()
        };
        let names = match family {
            Family::Sans => named(&self.sans),
            Family::Mono => named(&self.mono),
            Family::Named(name) => std::iter::once(name.to_owned()).chain(named(&self.sans)).collect(),
            Family::SystemMono => system.iter().cloned().chain(named(&self.mono)).collect(),
        };
        let mut query = self.font_cx.collection.query(&mut self.font_cx.source_cache);
        query.set_families(names.iter().map(|name| parley::fontique::QueryFamily::Named(name)));
        let style = if italic { FontStyle::Italic } else { FontStyle::Normal };
        query.set_attributes(parley::fontique::Attributes::new(parley::fontique::FontWidth::NORMAL, style, FontWeight::new(weight)));
        let font = query.first_available_font();
        drop(query);
        let trak = font.and_then(|f| font_trak(&mut self.traks, &f.blob, f.index));
        self.primary_traks.insert(key, trak.clone());
        trak
    }

    /// The installed `Family::SystemMono` families, loading them the first time.
    fn system_mono(&mut self) -> &[String] {
        let (collection, loaded) = (&mut self.font_cx.collection, &mut self.font_files);
        self.system_mono.get_or_insert_with(|| load_system_mono(collection, loaded))
    }

    /// The RGBA image of a colour glyph quad (`GlyphQuad::color`, nonzero).
    pub fn color_glyph(&self, color: u32) -> &crate::pixels::Pixels {
        &self.color_pixels[color as usize - 1]
    }

    /// Keeps layouts used in the last two commits, and older ones, most recently used first,
    /// while they fit in [`STALE_TEXT`].
    pub fn end_commit(&mut self) {
        let g = self.generation;
        evict(&mut self.layouts, g);
        evict(&mut self.runs, g);
        self.generation = g.wrapping_add(1);
    }

    /// A layout in device pixels, wrapped at `max_width` device px when given.
    pub fn layout_rich(
        &mut self,
        text: &str,
        spans: &[Span],
        style: TextStyle,
        scale: f32,
        max_width: Option<f32>,
    ) -> Rc<Layout<Ink>> {
        self.layout_aligned(text, spans, style, scale, max_width, Alignment::Start)
    }

    /// [`layout_rich`](Self::layout_rich) with each line aligned within `max_width`, so wrapped
    /// centred or end-aligned text lines up per line rather than as one left-aligned block.
    pub fn layout_aligned(
        &mut self,
        text: &str,
        spans: &[Span],
        style: TextStyle,
        scale: f32,
        max_width: Option<f32>,
        align: Alignment,
    ) -> Rc<Layout<Ink>> {
        let mut key = text_hash(text, spans, style, scale, max_width);
        if align != Alignment::Start {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (key, align as u8).hash(&mut h);
            key = h.finish();
        }
        let g = self.generation;
        if let Some((l, used, _)) = self.layouts.get_mut(&key) {
            *used = g;
            return l.clone();
        }
        self.shaped += 1;
        let _shape_time = crate::frame_profile::Scope::new(crate::frame_profile::Phase::Text);
        let routes = fallback::routes(text);
        if !text.is_ascii() {
            let (mut scripts, mut symbols) = (0u64, false);
            for ch in text.chars() {
                for (i, fonts) in fallback::SCRIPT_FONTS.iter().enumerate() {
                    if (self.scripts | scripts) & (1 << i) == 0 && fonts.covers(ch) {
                        scripts |= 1 << i;
                    }
                }
                symbols |= self.math.is_none() && fallback::needs_symbols(ch);
            }
            for (i, fonts) in fallback::SCRIPT_FONTS.iter().enumerate() {
                if scripts & (1 << i) != 0 {
                    self.scripts |= 1 << i;
                    register_script_fallbacks(&mut self.font_cx.collection, &mut self.font_files, fonts);
                }
            }
            if symbols {
                self.math = Some(load_families(&mut self.font_cx.collection, fallback::MATH_FONT_FILES, fallback::MATH_FAMILIES));
            }
        }
        // `trak` tracking at the point size (see `trak.rs`), on top of `style.tracking`. An
        // explicit optical size `o` picks the entry instead, as Core Text tracks `o`-point text
        // scaled to the set size; spans scale the lookup with their size. Each style range gets
        // its predicted first font's spacing; runs in another font are checked after shaping.
        let trak_tag = parley::setting::Tag::new(b"trak");
        let mut features: Vec<parley::FontFeature> = parley::FontFeature::parse_css_list(style.features).flatten().collect();
        let tracked = features.iter().rev().find(|f| f.tag == trak_tag).is_none_or(|f| f.value != 0);
        let listed = features.len();
        features.retain(|f| f.tag != trak_tag);
        let stripped = features.len() != listed;
        let optical = match style.optical_size {
            OpticalSize::Fixed(o) if o.is_finite() && o > 0.0 && style.size > 0.0 => Some(o),
            _ => None,
        };
        let spacing = |trak: Option<Rc<Trak>>, size: f32| {
            let px = size * scale;
            let points = px / scale;
            // `o` itself at the style's size (a run's size comes back through device px).
            let points = optical.map_or(points, |o| {
                if (points - style.size).abs() <= style.size * 1e-6 { o } else { o * points / style.size }
            });
            style.tracking + trak.map_or(0.0, |t| t.spacing(px, points, tracked) / scale)
        };
        let base_spacing = spacing(self.primary_trak(style.family, style.weight, style.italic), style.size);
        let span_spacing: Vec<Option<f32>> = spans
            .iter()
            .map(|s| {
                if !(s.mono || s.italic || s.weight.is_some() || s.size.is_some()) {
                    return None;
                }
                let family = if s.mono { Family::Mono } else { style.family };
                let trak = self.primary_trak(family, s.weight.unwrap_or(style.weight), s.italic || style.italic);
                Some(spacing(trak, s.size.unwrap_or(style.size))).filter(|&v| v != base_spacing)
            })
            .collect();
        if style.family == Family::SystemMono || self.slot_system_mono {
            self.system_mono();
        }
        let math = self.math.as_deref().unwrap_or(&[]);
        let emoji = self.emoji.as_slice();
        let system = self.system_mono.as_deref().unwrap_or_default();
        let with_symbols = |primary| {
            family_list(primary, system, self.sans.as_str())
                .into_iter()
                .chain(self.symbol_families.iter().chain(math).map(|name| FontFamilyName::named(name)))
                .collect::<Vec<_>>()
        };
        let sans = match style.family {
            Family::Sans => with_symbols(self.sans.as_str()),
            Family::Mono => with_symbols(self.mono.as_str()),
            Family::Named(name) => {
                let mut families = with_symbols(self.sans.as_str());
                families.insert(0, FontFamilyName::named(name));
                families
            }
            Family::SystemMono => {
                let mut families = with_symbols(self.mono.as_str());
                families.splice(0..0, system.iter().map(|name| FontFamilyName::named(name)));
                families
            }
        };
        let mono = with_symbols(self.mono.as_str());
        // Routed clusters try their fallback first, then the run's own stack (mono spans included).
        // Each of the four {math, emoji} × {sans, mono} stacks is built at most once.
        let mut stacks: [Option<Vec<FontFamilyName<'_>>>; 4] = Default::default();
        let mut routed = Vec::new();
        for (range, route) in routes {
            let fonts = match route { Route::Math => math, Route::Emoji => emoji };
            if fonts.is_empty() {
                continue;
            }
            let mono_span = spans.iter().any(|s| s.mono && (s.start as usize) < range.end && range.start < s.end as usize);
            let stack = 2 * (route == Route::Emoji) as usize + mono_span as usize;
            stacks[stack].get_or_insert_with(|| {
                let base = if mono_span { &mono } else { &sans };
                fonts.iter().map(|name| FontFamilyName::named(name)).chain(base.iter().cloned()).collect()
            });
            routed.push((range, stack));
        }
        let span_range = |s: &Span| (s.start as usize).min(text.len())..(s.end as usize).min(text.len());
        // `exact` overrides the predicted spacing with each run's own, by byte range.
        let build = |lcx: &mut LayoutContext<Ink>, fcx: &mut FontContext, exact: &[(std::ops::Range<usize>, f32)]| {
            let mut b = lcx.ranged_builder(fcx, text, scale, true);
            b.push_default(FontFamily::from(sans.as_slice()));
            b.push_default(StyleProperty::FontSize(style.size));
            b.push_default(StyleProperty::FontWeight(FontWeight::new(style.weight)));
            if style.italic {
                b.push_default(StyleProperty::FontStyle(FontStyle::Italic));
            }
            // Parley turns optional ligatures off under letter spacing; Core Text's tracking
            // keeps them, so spacing that only corrects `trak` turns them back on.
            let corrected = base_spacing != 0.0 || span_spacing.iter().flatten().chain(exact.iter().map(|e| &e.1)).any(|&v| v != 0.0);
            if style.tracking == 0.0 && corrected {
                let ligatures = [b"liga", b"clig"].map(|tag| parley::FontFeature::new(parley::setting::Tag::new(tag), 1));
                let list = ligatures.iter().chain(&features).copied().collect();
                b.push_default(StyleProperty::FontFeatures(parley::FontFeatures::List(std::borrow::Cow::Owned(list))));
            } else if stripped {
                b.push_default(StyleProperty::FontFeatures(features.as_slice().into()));
            } else if !style.features.is_empty() {
                b.push_default(StyleProperty::FontFeatures(style.features.into()));
            }
            let settings = variations(style, style.size);
            if !settings.is_empty() {
                b.push_default(parley::FontVariations::List(std::borrow::Cow::Owned(settings)));
            }
            b.push_default(LineHeight::Absolute(style.line_px()));
            if base_spacing != 0.0 {
                b.push_default(StyleProperty::LetterSpacing(base_spacing));
            }
            for (i, s) in spans.iter().enumerate() {
                let r = span_range(s);
                if r.is_empty() {
                    continue;
                }
                b.push(StyleProperty::Brush(Ink(i as u16 + 1)), r.clone());
                if s.mono {
                    b.push(FontFamily::from(mono.as_slice()), r.clone());
                }
                if let Some(w) = s.weight {
                    b.push(StyleProperty::FontWeight(FontWeight::new(w)), r.clone());
                }
                if s.italic {
                    b.push(StyleProperty::FontStyle(FontStyle::Italic), r.clone());
                }
                if let Some(sz) = s.size {
                    b.push(StyleProperty::FontSize(sz), r.clone());
                    if style.optical_size == OpticalSize::Auto {
                        b.push(parley::FontVariations::List(std::borrow::Cow::Owned(variations(style, sz))), r.clone());
                    }
                }
                if let Some(v) = span_spacing[i] {
                    b.push(StyleProperty::LetterSpacing(v), r.clone());
                }
            }
            for (range, stack) in &routed {
                b.push(FontFamily::from(stacks[*stack].as_deref().unwrap_or_default()), range.clone());
            }
            for (range, v) in exact {
                b.push(StyleProperty::LetterSpacing(*v), range.clone());
            }
            let mut layout: Layout<Ink> = b.build(text);
            layout.break_all_lines(max_width);
            layout
        };
        let mut layout = build(&mut self.layout_cx, &mut self.font_cx, &[]);
        // The spacing each run got, against the spacing its own font needs.
        let given = |at: usize| {
            spans.iter().zip(&span_spacing).rev().find_map(|(s, v)| v.filter(|_| span_range(s).contains(&at))).unwrap_or(base_spacing)
        };
        let mut exact: Vec<(std::ops::Range<usize>, f32)> = Vec::new();
        let mut mismatch = false;
        for line in layout.lines() {
            for run in line.runs() {
                let font = run.font();
                let trak = font_trak(&mut self.traks, &font.data, font.index);
                let needed = spacing(trak, run.font_size() / scale);
                let range = run.text_range();
                if range.is_empty() {
                    continue;
                }
                mismatch |= (needed - given(range.start)).abs() > 1e-4;
                match exact.last_mut() {
                    Some((last, v)) if last.end == range.start && *v == needed => last.end = range.end,
                    _ => exact.push((range, needed)),
                }
            }
        }
        if mismatch {
            layout = build(&mut self.layout_cx, &mut self.font_cx, &exact);
        }
        layout.align(align, AlignmentOptions::default());
        let l = Rc::new(layout);
        self.layouts.insert(key, (l.clone(), g, text.len() as u32));
        l
    }

    /// Size in points, rounded up to whole device pixels so layout stays on the pixel grid.
    pub fn measure(&mut self, text: &str, style: TextStyle, scale: f32) -> (f32, f32) {
        let l = self.layout_rich(text, &[], style, scale, None);
        (l.full_width().ceil() / scale, layout_height(&l, style, scale) / scale)
    }

    /// `run` shaped as one line in `style`, cached as [`Self::end_commit`] keeps layouts.
    pub fn run_layout(&mut self, run: &Run, style: TextStyle, scale: f32) -> Rc<RunLayout> {
        let key = run.key(style, scale);
        let g = self.generation;
        if let Some((r, used, _)) = self.runs.get_mut(&key) {
            *used = g;
            return r.clone();
        }
        let r = Rc::new(RunLayout::new(self.layout_rich(run.as_str(), &[], style, scale, None), run.as_str()));
        self.runs.insert(key, (r.clone(), g, run.as_str().len() as u32));
        r
    }

    /// A live run owns its latest layout; don't retain every timer tick in the commit cache.
    pub(crate) fn layout_live(&mut self, text: &str, style: TextStyle, scale: f32) -> Rc<Layout<Ink>> {
        let key = text_hash(text, &[], style, scale, None);
        let cached = self.layouts.contains_key(&key);
        let layout = self.layout_rich(text, &[], style, scale, None);
        if !cached { self.layouts.remove(&key); }
        layout
    }

    /// Glyph quads for single-line `text` with its layout box's top-left at `origin` (device px).
    pub fn glyphs(&mut self, text: &str, style: TextStyle, scale: f32, origin: [f32; 2], out: &mut Vec<GlyphQuad>) {
        let l = self.layout_rich(text, &[], style, scale, None);
        self.glyphs_for_layout(&l, scale, origin, false, out);
    }

    /// Glyph quads for a cached [`Layout`] with its top-left at `origin` (device px). When
    /// `bytes` is set, each quad carries the byte offset of its cluster.
    pub fn glyphs_for_layout(
        &mut self,
        layout: &Layout<Ink>,
        scale: f32,
        origin: [f32; 2],
        bytes: bool,
        out: &mut Vec<GlyphQuad>,
    ) {
        self.glyphs_for_layout_scaled(layout, scale, origin, 1.0, bytes, out);
    }

    /// [`Self::glyphs_for_layout`] with the layout magnified `k` times about `origin`: pen
    /// positions scale and each glyph rasterizes at `k` times its size, so it stays crisp.
    pub(crate) fn glyphs_for_layout_scaled(
        &mut self,
        layout: &Layout<Ink>,
        scale: f32,
        origin: [f32; 2],
        k: f32,
        bytes: bool,
        out: &mut Vec<GlyphQuad>,
    ) {
        let mut table = std::mem::take(&mut self.cluster_scratch);
        let mut coords = std::mem::take(&mut self.coords_scratch);
        for line in layout.lines() {
            if bytes {
                table.clear();
                let mut x = line.metrics().offset;
                for run in line.runs() {
                    for cl in run.visual_clusters() {
                        table.push((x, cl.text_range().start as u32));
                        x += cl.advance();
                    }
                }
            }
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(glyph_run) = item else { continue };
                let face = Face::of(&glyph_run, &mut coords);
                let baseline = (origin[1] + face.baseline * k).round();
                let mut rel = glyph_run.offset();
                for g in glyph_run.glyphs() {
                    let byte = if bytes {
                        let i = table.partition_point(|&(x0, _)| x0 <= rel + 0.01);
                        table.get(i.saturating_sub(1)).map_or(0, |e| e.1)
                    } else {
                        0
                    };
                    let at = if k == 1.0 { [origin[0] + rel + g.x, baseline + g.y] }
                        else { [origin[0] + (rel + g.x) * k, baseline + g.y * k] };
                    self.push_glyph(&face, &coords, g.id, at, scale, k, byte, out);
                    rel += g.advance;
                }
            }
        }
        self.cluster_scratch = table;
        self.coords_scratch = coords;
    }

    /// Glyph quads for the grapheme at byte `at` of a shaped run, with the run's top-left at
    /// `origin` (device px). Only that grapheme's glyphs are visited.
    pub fn glyphs_in(&mut self, run: &RunLayout, at: usize, scale: f32, origin: [f32; 2], out: &mut Vec<GlyphQuad>) {
        let Some(i) = run.grapheme(at) else { return };
        let glyphs = run.graphemes[i].2 as usize..run.graphemes[i + 1].2 as usize;
        for g in &run.glyphs[glyphs] {
            let face = &run.faces[g.face as usize];
            let baseline = (origin[1] + face.face.baseline).round();
            self.push_glyph(&face.face, &face.coords, g.id, [origin[0] + g.x, baseline + g.y], scale, 1.0, at as u32, out);
        }
    }

    /// Rasterizes (once) and appends one glyph at device position `at`, `k` times its size.
    #[allow(clippy::too_many_arguments)]
    fn push_glyph(&mut self, face: &Face, coords: &[i16], glyph: u32, at: [f32; 2], scale: f32, k: f32, byte: u32, out: &mut Vec<GlyphQuad>) {
        let dilation = 0.1 * scale;
        let [gx, gy] = at;
        let (font, font_size, ink, skew) = (&face.font, face.size * k, face.ink, face.skew);
        let fx = gx.floor();
        let subpx = ((gx - fx) * SUBPIXEL_STEPS).round() as u8;
        let (fx, subpx) = if subpx as f32 >= SUBPIXEL_STEPS { (fx + 1.0, 0) } else { (fx, subpx) };
        let key = GlyphKey {
            font: font.data.id(),
            index: font.index,
            glyph,
            size: font_size.to_bits(),
            coords: face.coords_key,
            subpx,
            dilation: dilation.to_bits(),
            skew,
        };
        let entry = match self.atlas.map.get(&key) {
            Some(&e) => e,
            None => {
                // Colour glyphs sit on whole pixels and ignore emboldening and obliques.
                let color_key = GlyphKey { subpx: 0, dilation: 0, skew: 0, ..key };
                if let Some(c) = self.color_glyphs.get(&color_key) {
                    out.push(c.quad(gx, gy, ink, byte));
                    return;
                }
                let Some(font_ref) = FontRef::from_index(font.data.as_ref(), font.index as usize) else {
                    return;
                };
                let mut scaler = self
                    .scale_cx
                    .builder(font_ref)
                    .size(font_size)
                    .hint(false)
                    .normalized_coords(coords.iter().copied())
                    .build();
                if let Some((pixels, at)) = rasterize_color(&mut scaler, glyph as u16) {
                    self.color_pixels.push(pixels);
                    let index = self.color_pixels.len() as u32;
                    let c = ColorGlyph { index, left: at.left, top: at.top, w: at.width, h: at.height };
                    self.color_glyphs.insert(color_key, c);
                    out.push(c.quad(gx, gy, ink, byte));
                    return;
                }
                let e = rasterize(&mut scaler, &mut self.atlas, glyph as u16, subpx, dilation, skew);
                self.atlas.map.insert(key, e);
                e
            }
        };
        if let Some(a) = entry {
            out.push(GlyphQuad {
                rect: [fx + a.left as f32, gy.round() - a.top as f32, a.w as f32, a.h as f32],
                uv: [a.x as f32, a.y as f32],
                ink,
                byte,
                color: 0,
            });
        }
    }

    /// Backgrounds and underlines for `spans` (device px, layout top-left at `origin`). Touching
    /// runs with the same background merge into one rounded rect.
    pub fn decorations(&self, layout: &Layout<Ink>, spans: &[Span], scale: f32, origin: [f32; 2], out: &mut Vec<Deco>) {
        if spans.iter().all(|s| s.bg.is_none() && s.underline.is_none()) {
            return;
        }
        for line in layout.lines() {
            let m = line.metrics();
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(gr) = item else { continue };
                let ink = gr.style().brush.0;
                if ink == 0 {
                    continue;
                }
                let Some(s) = spans.get(ink as usize - 1) else { continue };
                let x0 = origin[0] + gr.offset();
                let w = gr.advance();
                if let Some(bg) = s.bg {
                    let pad = s.bg_pad * scale;
                    let fm = gr.run().font_metrics();
                    let (asc, desc) = (fm.ascent, fm.descent);
                    let base = origin[1] + m.baseline;
                    let top = (base - asc - 1.0 * scale).round();
                    let bot = (base + desc.abs() + 1.0 * scale).round();
                    let rect = [x0 - pad, top, w + 2.0 * pad, bot - top];
                    // Touching runs with the same background on one line (a span split by style, or
                    // inline code and its padding spaces in the body font) share one rect, as tall as
                    // its widest run, so a thin run in another font doesn't poke out as a sliver.
                    match out.last_mut() {
                        Some(d)
                            if d.color == bg
                                && d.rect[1] < bot
                                && top < d.rect[1] + d.rect[3]
                                && (d.rect[0] + d.rect[2] - rect[0]).abs() < 1.0 =>
                        {
                            if rect[2] > d.rect[2] {
                                (d.rect[1], d.rect[3]) = (rect[1], rect[3]);
                            }
                            d.rect[2] = rect[0] + rect[2] - d.rect[0];
                        }
                        _ => out.push(Deco { rect, color: bg, radius: 4.0 * scale }),
                    }
                }
                if let Some(uc) = s.underline {
                    let y = (origin[1] + m.baseline + 2.0 * scale).round();
                    out.push(Deco { rect: [x0, y, w, scale.max(1.0)], color: uc, radius: 0.0 });
                }
            }
        }
    }

    /// The atlas quad for `icon` at `px` device pixels square, top-left at `origin`.
    pub fn icon_quad(&mut self, icon: &'static Icon, px: u32, origin: [f32; 2]) -> Option<GlyphQuad> {
        let key = (icon as *const Icon as usize, px);
        let entry = match self.atlas.icons.get(&key) {
            Some(&e) => e,
            None => {
                let e = rasterize_icon(icon, px, &mut self.atlas);
                self.atlas.icons.insert(key, e);
                e
            }
        };
        entry.map(|a| GlyphQuad {
            rect: [origin[0].round(), origin[1].round(), a.w as f32, a.h as f32],
            uv: [a.x as f32, a.y as f32],
            ink: 0,
            byte: 0,
            color: 0,
        })
    }
}

/// Layout height in device px: whole lines at the style's line height (an empty text still
/// takes one line, like a browser).
pub fn layout_height(l: &Layout<Ink>, style: TextStyle, scale: f32) -> f32 {
    l.height().max(style.line_px() * scale).ceil()
}

/// Fraction of the final line occupied by ink advance, excluding hanging whitespace.
/// Width and the layout use device pixels. No shaping or allocation occurs here.
pub fn last_line_fill(l: &Layout<Ink>, width: f32) -> f32 {
    let Some(line) = l.lines().next_back() else { return 0.0 };
    let m = line.metrics();
    ((m.advance - m.hanging_advance) / width.max(1.0)).clamp(0.0, 1.0)
}

/// Height held back while the last line fills (research/18 §7), in points.
pub fn stream_height_lag(l: &Layout<Ink>, width: f32, scale: f32) -> f32 {
    let Some(line) = l.lines().next_back() else { return 0.0 };
    let cap = if l.len() == 1 { 0.52 } else { 0.82 };
    let lag = line.metrics().line_height / scale * cap * (1.0 - last_line_fill(l, width));
    (lag * 64.0).round() / 64.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph_ids(text: &str) -> Vec<u32> {
        let mut ts = TextSystem::default();
        let layout = ts.layout_rich(text, &[], TextStyle::new(13.0, 400.0), 2.0, None);
        let mut ids = Vec::new();
        for line in layout.lines() {
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(run) = item {
                    ids.extend(run.glyphs().map(|g| g.id));
                }
            }
        }
        ids
    }

    /// Han text shapes with real glyphs (not .notdef) whenever a CJK font is installed; Latin
    /// text keeps the app's own family.
    #[test]
    fn cjk_falls_back_to_an_installed_font() {
        if system_font_files(&CJK_FONT_FILES).is_empty() {
            eprintln!("skipping: no CJK font installed (set XDG_DATA_DIRS to a dir with fonts/)");
            return;
        }
        let han = glyph_ids("你好世界");
        assert_eq!(han.len(), 4);
        assert!(han.iter().all(|id| *id != 0), "tofu: {han:?}");
        assert_eq!(glyph_ids("Hello"), glyph_ids("Hello"));
    }

    /// `Family::SystemMono` shapes with the platform's monospaced font when one is installed, so
    /// every character advances the same; without one it is the app's mono slot.
    #[test]
    fn system_mono_is_monospaced_or_the_mono_slot() {
        let mut ts = TextSystem::default();
        let style = TextStyle::new(13.0, 400.0).family(Family::SystemMono);
        let width = |ts: &mut TextSystem, text: &str, style: TextStyle| ts.layout_rich(text, &[], style, 2.0, None).full_width();
        if ts.system_mono().is_empty() {
            eprintln!("skipping the monospace check: no system monospaced font installed");
            assert_eq!(width(&mut ts, "iiii MMMM", style), width(&mut ts, "iiii MMMM", style.mono()));
            return;
        }
        let (narrow, wide) = (width(&mut ts, "iiiiiiii", style), width(&mut ts, "MMMMMMMM", style));
        assert!((narrow - wide).abs() < 0.5, "{narrow} vs {wide}");
        assert!(width(&mut ts, "iiiiiiii", TextStyle::new(13.0, 400.0)) < narrow * 0.7, "the sans slot is proportional");
        let italic = width(&mut ts, "iiiiiiii", style.italic());
        assert!((italic - narrow).abs() < 0.5, "italic keeps the cell: {italic} vs {narrow}");
    }

    /// `ui-monospace` in the mono slot puts mono spans and `.mono()` in `Family::SystemMono`'s
    /// font, so inline code matches a code block set in it.
    #[test]
    fn ui_monospace_in_the_mono_slot_is_system_mono() {
        let fonts = |ts: &mut TextSystem, text: &str, spans: &[Span], style: TextStyle| {
            let mut ids = Vec::new();
            runs(ts, text, spans, style, |_, font| ids.push((font.data.as_ptr() as usize, font.offset)));
            ids
        };
        let code = &[Span { mono: true, ..Span::new(4, 8) }];
        let mut ts = TextSystem::new(&[], ("Inter", "ui-monospace, Inter"));
        let block = fonts(&mut ts, "code", &[], TextStyle::new(13.0, 400.0).family(Family::SystemMono));
        let spans = fonts(&mut ts, "see code", code, TextStyle::new(13.0, 400.0));
        assert_eq!(spans.last(), block.last(), "the span shapes with the code block's font");
        assert_eq!(fonts(&mut ts, "code", &[], TextStyle::new(13.0, 400.0).mono()), block);
        if !ts.system_mono().is_empty() {
            let mut plain = TextSystem::new(&[], ("Inter", "Inter"));
            assert_ne!(fonts(&mut plain, "see code", code, TextStyle::new(13.0, 400.0)).last(), block.last());
        }
    }

    /// With no monospace font to stand for `ui-monospace`, the mono slot falls back to sans
    /// rather than to no named family at all.
    #[test]
    fn ui_monospace_with_no_mono_font_falls_back_to_sans() {
        let names = |list: &str, mono: &[String]| {
            family_list(list, mono, "Inter, sans-serif")
                .into_iter()
                .map(|f| match f { FontFamilyName::Named(n) => n.into_owned(), FontFamilyName::Generic(g) => format!("{g:?}") })
                .collect::<Vec<_>>()
        };
        assert_eq!(names("ui-monospace", &[]), ["Inter", "SansSerif"]);
        assert_eq!(names("ui-monospace", &["Menlo".into()]), ["Menlo"]);
        assert_eq!(names("ui-monospace, Iosevka", &[]), ["Iosevka"], "a named family after it still wins");
        assert_eq!(names("monospace", &[]), ["Monospace"], "only ui-monospace falls back");
    }

    /// Each glyph run of `text` with the font it shaped with.
    fn runs(ts: &mut TextSystem, text: &str, spans: &[Span], style: TextStyle, mut f: impl FnMut(&parley::GlyphRun<'_, Ink>, FontRef<'_>)) {
        let layout = ts.layout_rich(text, spans, style, 2.0, None);
        for line in layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(run) = item else { continue };
                let font = run.run().font();
                f(&run, FontRef::from_index(font.data.as_ref(), font.index as usize).unwrap());
            }
        }
    }

    /// `(text range, synthetic skew, face style)` of each glyph run of `text`.
    fn run_skews(ts: &mut TextSystem, text: &str, spans: &[Span], style: TextStyle) -> Vec<(std::ops::Range<usize>, Option<f32>, String)> {
        let mut out = Vec::new();
        runs(ts, text, spans, style, |run, font| {
            out.push((run.run().text_range(), run.run().synthesis().skew(), format!("{:?}", font.attributes().style())));
        });
        out
    }

    /// A face without italics (bundled Inter) leans synthetically, in the whole style or one
    /// span; a family with an italic face (system SF, DejaVu) uses it unskewed.
    #[test]
    fn italic_uses_the_real_face_or_a_synthetic_oblique() {
        let mut ts = TextSystem::default();
        let upright = TextStyle::new(13.0, 400.0);
        assert!(run_skews(&mut ts, "Hello", &[], upright).iter().all(|r| r.1.is_none()));
        assert!(run_skews(&mut ts, "Hello", &[], upright.italic()).iter().all(|r| r.1.is_some()), "Inter has no italic");
        let spans = [Span { italic: true, ..Span::new(6, 10) }];
        for (range, skew, _) in run_skews(&mut ts, "plain emph", &spans, upright) {
            assert_eq!(skew.is_some(), range.start >= 6, "{range:?}");
        }
        // The synthetic lean widens an upright stem's ink and moves its top right.
        let mut quads = |style| {
            let l = ts.layout_rich("l", &[], style, 2.0, None);
            let mut q = Vec::new();
            ts.glyphs_for_layout(&l, 2.0, [0.0, 0.0], false, &mut q);
            q[0]
        };
        let (a, b) = (quads(upright), quads(upright.italic()));
        assert!(b.rect[2] >= a.rect[2] + 3.0, "upright {:?}, oblique {:?}", a.rect, b.rect);

        let real = if cfg!(target_os = "macos") { "System Font" } else { "DejaVu Sans" };
        if ts.font_cx.collection.family_id(real).is_none() {
            eprintln!("skipping real italic: {real} not loaded");
            return;
        }
        let style = upright.family(Family::Named(real)).italic();
        for (range, skew, face) in run_skews(&mut ts, "Hello", &[], style) {
            assert!(skew.is_none() && face != "Normal", "{range:?} of {real}: skew {skew:?}, face {face}");
        }
    }

    /// Hebrew, Arabic, Devanagari, Bengali and Thai load their system fonts on first use and
    /// shape with real glyphs; Latin text loads none of them.
    #[test]
    fn scripts_load_their_system_fonts_on_first_use() {
        let mut ts = TextSystem::default();
        ts.layout_rich("Hello, “café” — naïve…", &[], TextStyle::new(13.0, 400.0), 2.0, None);
        assert_eq!(ts.scripts, 0, "Latin text must not load script fallbacks");
        let samples = ["שלום עולם", "مرحبا بالعالم", "नमस्ते दुनिया", "ওহে বিশ্ব", "สวัสดีชาวโลก"];
        for (fonts, sample) in fallback::SCRIPT_FONTS.iter().zip(samples) {
            assert!(sample.chars().any(|ch| fonts.covers(ch)), "{sample} is not {:?}", fonts.script);
            if system_font_files(fonts.files).is_empty() {
                eprintln!("skipping {}: no font installed", String::from_utf8_lossy(fonts.script));
                continue;
            }
            let glyphs = faces(&mut ts, &format!("a {sample}"), &[], TextStyle::new(13.0, 400.0));
            assert!(glyphs.iter().all(|(_, id)| *id != 0), "tofu in {sample}: {glyphs:?}");
        }
        assert_eq!(ts.scripts.count_ones() as usize, fallback::SCRIPT_FONTS.len());
    }

    #[test]
    fn plex_keyboard_symbols_use_real_glyphs() {
        const PLEX: &[u8] = include_bytes!("../../../themes/opencode/assets/fonts/IBMPlexMono-Regular.ttf");
        let mut ts = TextSystem::new(&[PLEX], ("IBM Plex Mono", "IBM Plex Mono"));
        if ts.symbol_families.is_empty() {
            eprintln!("skipping: no symbol font installed");
            return;
        }
        let plex = FontRef::from_index(PLEX, 0).unwrap();
        assert_eq!(plex.charmap().map('⌘'), 0, "fixture must need fallback");
        // Exercise standalone Common script, surrounding Latin and a mono rich-text span.
        for text in ["⌘⇧⏎⌥⌃⎋⌫←↑→↓↵", "Copy ⌘K ⇧⏎ ⌥⌃⎋⌫←↑→↓↵"] {
            for style in [TextStyle::new(13.0, 400.0), TextStyle::new(13.0, 400.0).mono()] {
                let spans = [Span { mono: true, ..Span::new(0, text.len()) }];
                for spans in [&[][..], &spans[..]] {
                    let layout = ts.layout_rich(text, spans, style, 2.0, None);
                    let mut count = 0;
                    for line in layout.lines() {
                        for item in line.items() {
                            if let PositionedLayoutItem::GlyphRun(run) = item {
                                for glyph in run.glyphs() {
                                    assert_ne!(glyph.id, 0, "missing symbol in {text:?}");
                                    count += 1;
                                }
                            }
                        }
                    }
                    assert_eq!(count, text.chars().count());
                }
            }
        }
        let layout = ts.layout_rich("Hello", &[], TextStyle::new(13.0, 400.0), 2.0, None);
        let ids: Vec<_> = layout
            .lines()
            .flat_map(|line| line.items())
            .filter_map(|item| match item {
                PositionedLayoutItem::GlyphRun(run) => Some(run.glyphs().map(|glyph| glyph.id).collect::<Vec<_>>()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(ids, "Hello".chars().map(|ch| u32::from(plex.charmap().map(ch))).collect::<Vec<_>>());
    }

    /// `(family, glyph id)` for every glyph of `text`, in visual order.
    fn faces(ts: &mut TextSystem, text: &str, spans: &[Span], style: TextStyle) -> Vec<(String, u32)> {
        let mut out = Vec::new();
        runs(ts, text, spans, style, |run, font| {
            let family = font.localized_strings().find_by_id(swash::StringId::Family, None).map(|s| s.to_string()).unwrap_or_default();
            out.extend(run.glyphs().map(|g| (family.clone(), g.id)));
        });
        out
    }

    /// Math alphanumerics load the math fallback on first use and shape from one math font,
    /// mono spans included; Latin around them stays on the app's font.
    #[test]
    fn math_alphanumerics_use_one_math_font() {
        let mut ts = TextSystem::default();
        ts.layout_rich("Hello, “café” — naïve…", &[], TextStyle::new(13.0, 400.0), 2.0, None);
        assert!(ts.math.is_none(), "Latin text must not load the math fallback");
        if system_font_files(fallback::MATH_FONT_FILES).is_empty() {
            eprintln!("skipping: no math font installed");
            return;
        }
        let formula = "𝐀𝐁𝐂 𝑥𝑦𝑧 𝒜ℬ𝒞 𝔄𝔅 𝕒𝕓 ℝℂ 𝟎𝟏𝟐 𝛼𝛽𝛾";
        let latin = faces(&mut ts, "Let ", &[], TextStyle::new(13.0, 400.0));
        for (style, spans) in [
            (TextStyle::new(13.0, 400.0), vec![]),
            (TextStyle::new(13.0, 400.0).mono(), vec![]),
            (TextStyle::new(13.0, 400.0), vec![Span { mono: true, ..Span::new(4, 4 + formula.len()) }]),
        ] {
            let text = format!("Let {formula}");
            let glyphs = faces(&mut ts, &text, &spans, style);
            assert!(ts.math.as_ref().is_some_and(|m| !m.is_empty()));
            assert!(glyphs.iter().all(|(_, id)| *id != 0), "tofu in {text:?}: {glyphs:?}");
            if spans.is_empty() && style.family == Family::Sans {
                assert_eq!(glyphs[..4], latin[..], "Latin before a formula keeps the app font");
            }
            let math_glyphs: Vec<_> = text[4..].chars().zip(&glyphs[4..]).filter(|(ch, _)| !ch.is_whitespace()).collect();
            let family = &math_glyphs[0].1.0;
            assert!(fallback::MATH_FAMILIES.contains(&family.as_str()), "{family} is not a math font");
            assert!(math_glyphs.iter().all(|(_, g)| &g.0 == family), "mixed math designs: {math_glyphs:?}");
        }
    }

    #[test]
    fn routes_merge_math_and_emoji_presentation() {
        assert_eq!(fallback::routes("a 𝐀𝐁 b 𝟎"), vec![(2..10, Route::Math), (13..17, Route::Math)]);
        assert!(fallback::routes("plain ascii").is_empty());
        let (heart, flag) = ("❤\u{FE0F}", "🏳\u{FE0F}\u{200D}🌈");
        let flag_at = 1 + heart.len() + '❤'.len_utf8();
        assert_eq!(fallback::routes(&format!("a{heart}❤{flag}")), vec![(1..1 + heart.len(), Route::Emoji), (flag_at..flag_at + flag.len(), Route::Emoji)]);
    }

    /// `U+FE0F` asks for the colour emoji even when a text font has the base character; without
    /// it the text form stays.
    #[test]
    fn emoji_presentation_selector_draws_in_colour() {
        let mut ts = TextSystem::default();
        if ts.emoji.is_none() {
            eprintln!("skipping: no colour emoji font installed");
            return;
        }
        let colour = |ts: &mut TextSystem, text: &str| {
            let layout = ts.layout_rich(text, &[], TextStyle::new(13.0, 400.0), 2.0, None);
            let mut quads = Vec::new();
            ts.glyphs_for_layout(&layout, 2.0, [0.0, 0.0], false, &mut quads);
            quads.iter().filter(|q| q.color != 0).count()
        };
        assert_eq!(colour(&mut ts, "a❤\u{FE0F}b ☀\u{FE0F} ✔\u{FE0F}"), 3);
        assert_eq!(colour(&mut ts, "a❤b ☺\u{FE0E}"), 0);
        assert_eq!(colour(&mut ts, "🏳\u{FE0F}\u{200D}🌈 👩🏽\u{200D}💻"), 2, "ZWJ sequences stay one image each");
    }

    /// Each glyph's ink top and bottom in px above the baseline, with its run's weight axis.
    fn ink_extents(ts: &mut TextSystem, text: &str, style: TextStyle) -> Vec<(f32, f32)> {
        let layout = ts.layout_rich(text, &[], style, 2.0, None);
        let mut out = Vec::new();
        for line in layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(glyph_run) = item else { continue };
                let run = glyph_run.run();
                let font = run.font();
                let font_ref = FontRef::from_index(font.data.as_ref(), font.index as usize).unwrap();
                let mut scaler = ts
                    .scale_cx
                    .builder(font_ref)
                    .size(run.font_size())
                    .normalized_coords(run.normalized_coords().iter().map(|c| c.to_bits()))
                    .build();
                for g in glyph_run.glyphs() {
                    let b = scaler.scale_outline(g.id as u16).unwrap().bounds();
                    out.push((b.max.y, b.min.y));
                }
            }
        }
        out
    }

    /// Keyboard symbols a primary font lacks fall back to glyphs that stand on the baseline at
    /// about cap height, so `⇧` and `⏎` don't shrink and sink next to the font's capitals.
    #[test]
    fn keyboard_symbol_fallbacks_match_cap_height() {
        const PLEX: &[u8] = include_bytes!("../../../themes/opencode/assets/fonts/IBMPlexMono-Regular.ttf");
        let mut ts = TextSystem::new(&[PLEX], ("IBM Plex Mono", "IBM Plex Mono"));
        if !cfg!(target_os = "macos") || system_font_files(&["sfnsttf"]).is_empty() {
            eprintln!("skipping: no macOS system font");
            return;
        }
        for size in [13.0, 15.0] {
            let glyphs = ink_extents(&mut ts, "H⌘⇧⏎⌫⎋⌥", TextStyle::new(size, 400.0).mono());
            let (cap, _) = glyphs[0];
            for (ch, (top, bottom)) in "H⌘⇧⏎⌫⎋⌥".chars().zip(&glyphs).skip(1) {
                assert!((top / cap - 1.0).abs() < 0.12, "{ch} at {size}pt: top {top:.1} vs cap {cap:.1}");
                assert!(bottom.abs() < cap * 0.06, "{ch} at {size}pt: bottom {bottom:.1} off the baseline");
            }
        }
    }

    /// Emoji shape with the system's colour emoji font and draw as colour images about the
    /// size of the text around them, not as blank alpha glyphs.
    #[test]
    fn emoji_draw_as_colour_images_beside_text() {
        let mut ts = TextSystem::default();
        if ts.emoji.is_none() {
            eprintln!("skipping: no colour emoji font installed");
            return;
        }
        let style = TextStyle::new(13.0, 400.0);
        let layout = ts.layout_rich("Done 🎉", &[], style, 2.0, None);
        let mut quads = Vec::new();
        ts.glyphs_for_layout(&layout, 2.0, [0.0, 0.0], false, &mut quads);
        let letters: Vec<_> = quads.iter().filter(|q| q.color == 0).collect();
        let emoji: Vec<_> = quads.iter().filter(|q| q.color != 0).collect();
        assert_eq!((letters.len(), emoji.len()), (4, 1), "{quads:?}");
        let e = emoji[0];
        let d = letters[0];
        assert!(e.rect[0] > letters[3].rect[0] + letters[3].rect[2], "after the text: {e:?}");
        assert!((20.0..40.0).contains(&e.rect[3]), "about 13pt at 2x: {e:?}");
        // It sits on the line: overlapping the capital D's ink, not above or below it.
        assert!(e.rect[1] < d.rect[1] + d.rect[3] && e.rect[1] + e.rect[3] > d.rect[1], "{e:?} vs {d:?}");
        let pixels = ts.color_glyph(e.color);
        let (w, h) = pixels.size();
        assert_eq!((w as f32, h as f32), (e.rect[2], e.rect[3]));
        let coloured = pixels.rgba().chunks(4).filter(|p| p[3] > 128 && (p[0].abs_diff(p[1]) > 40 || p[1].abs_diff(p[2]) > 40)).count();
        assert!(coloured > (w * h / 10) as usize, "only {coloured} coloured pixels");
        // Drawing it again reuses the image.
        let mut again = Vec::new();
        ts.glyphs_for_layout(&layout, 2.0, [0.0, 0.0], false, &mut again);
        assert_eq!(again.iter().filter(|q| q.color != 0).map(|q| q.color).collect::<Vec<_>>(), vec![e.color]);
        assert_eq!(ts.color_pixels.len(), 1);
    }

    #[test]
    fn run_cells_draw_the_run_glyphs_and_marks_stay_with_their_base() {
        let mut ts = TextSystem::default();
        let style = crate::tok::type_::BODY;
        // No precomposed x-acute, so the accent is its own zero-advance glyph after "x".
        let run = Run::new("x\u{301}yAV");
        let r = ts.run_layout(&run, style, 2.0);
        let mut whole = Vec::new();
        ts.glyphs_for_layout(&r.layout, 2.0, [0.0, 0.0], false, &mut whole);
        let mut cells = Vec::new();
        for at in [0, 3, 4, 5] {
            let mut one = Vec::new();
            ts.glyphs_in(&r, at, 2.0, [0.0, 0.0], &mut one);
            cells.push(one);
        }
        assert_eq!(cells.iter().map(Vec::len).collect::<Vec<_>>(), [2, 1, 1, 1], "the accent draws in the x cell");
        let rects: Vec<_> = cells.concat().iter().map(|q| q.rect).collect();
        assert_eq!(rects, whole.iter().map(|q| q.rect).collect::<Vec<_>>(), "cells draw exactly the run's glyphs");
        // Widths tile the run and sum to its measured width.
        let sum: f32 = [0, 3, 4, 5].iter().map(|&at| r.advance(at, 2.0).unwrap()).sum();
        assert!((sum - ts.measure(run.as_str(), style, 2.0).0).abs() < 1e-3, "{sum}");
    }

    #[test]
    fn run_lookups_off_a_grapheme_start_find_nothing() {
        let mut ts = TextSystem::default();
        let run = Run::new("x\u{301}y");
        let r = ts.run_layout(&run, crate::tok::type_::BODY, 2.0);
        for at in [1, 2, 4, 99] {
            assert_eq!((r.x(at), r.advance(at, 2.0)), (None, None), "byte {at}");
            let mut out = Vec::new();
            ts.glyphs_in(&r, at, 2.0, [0.0, 0.0], &mut out);
            assert!(out.is_empty());
        }
        // A cell whose byte is off the run measures and draws as plain text.
        use crate::{Cx, El, Env, Runtime};
        fn view(_: &(), _: &mut Cx<()>) -> El {
            crate::el::row().child(crate::el::text("y").advance_in(&Run::new("x\u{301}y"), 2)).child(crate::el::text("y").advance_in(&Run::new("y"), 99))
        }
        let mut rt = Runtime::new((), view, (200.0, 100.0), Env::new(true, 2.0));
        rt.frame(0.0);
        assert_eq!(rt.instances.iter().filter(|i| i.params[1] == crate::render::KIND_GLYPH).count(), 2);
    }
}

#[cfg(test)]
mod quality_tests {
    use super::*;

    #[test]
    fn centred_wrapped_lines_centre_each_line_in_the_wrap_width() {
        let mut text = TextSystem::default();
        let style = TextStyle::new(13.0, 400.0);
        let content = "a much longer first line of words that wraps\nshort";
        let width = 400.0;
        let start = text.layout_rich(content, &[], style, 2.0, Some(width));
        let centred = text.layout_aligned(content, &[], style, 2.0, Some(width), Alignment::Center);
        assert!(!Rc::ptr_eq(&start, &centred), "alignment is part of the cache key");
        let offsets = |l: &Layout<Ink>| {
            l.lines().map(|line| (line.metrics().offset, line.metrics().advance - line.metrics().hanging_advance)).collect::<Vec<_>>()
        };
        assert!(offsets(&start).iter().all(|(offset, _)| *offset == 0.0));
        let lines = offsets(&centred);
        assert!(lines.len() >= 2);
        for (offset, advance) in lines {
            assert!((offset - (width - advance) / 2.0).abs() < 1.0, "offset {offset} advance {advance}");
        }
    }

    /// Glyph extents per drawn line, in device pixels, for one text element 300 pt wide at 2×.
    fn line_spans(content: &str, align: crate::Align, justify: bool) -> Vec<(f32, f32)> {
        use crate::{Cx, El, Env, Runtime};
        type S = (String, crate::Align, bool);
        fn view(s: &S, _: &mut Cx<S>) -> El {
            let t = crate::el::text(s.0.clone()).w(300.0).wrap().align(s.1);
            if s.2 { t.justify() } else { t }
        }
        let mut rt = Runtime::new((content.to_string(), align, justify), view, (300.0, 400.0), Env::new(true, 2.0));
        rt.frame(0.0);
        let mut quads: Vec<[f32; 4]> =
            rt.instances.iter().filter(|i| i.params[1] == crate::render::KIND_GLYPH).map(|i| i.rect).collect();
        quads.sort_by(|a, b| (a[1] + a[3] / 2.0).total_cmp(&(b[1] + b[3] / 2.0)));
        let mut lines: Vec<(f32, f32, f32)> = Vec::new();
        for q in quads {
            let cy = q[1] + q[3] / 2.0;
            match lines.last_mut() {
                Some(l) if cy - l.0 < 16.0 => { l.1 = l.1.min(q[0]); l.2 = l.2.max(q[0] + q[2]); }
                _ => lines.push((cy, q[0], q[0] + q[2])),
            }
        }
        lines.into_iter().map(|(_, l, r)| (l, r)).collect()
    }

    #[test]
    fn justified_text_fills_each_wrapped_line_but_the_last() {
        use crate::Align;
        let content = "the quick brown fox jumps over the lazy dog while five boxing wizards jump quickly and pack my box with five dozen liquor jugs";
        let justified = line_spans(content, Align::Start, true);
        assert!(justified.len() >= 3, "{justified:?}");
        for &(l, r) in &justified[..justified.len() - 1] {
            assert!(l < 8.0 && r > 592.0, "justified line spans {l}..{r}");
        }
        let start = line_spans(content, Align::Start, false);
        assert_eq!(start.len(), justified.len(), "justifying never rewraps");
        assert!(start[..start.len() - 1].iter().any(|&(_, r)| r < 592.0), "start lines stay ragged: {start:?}");
    }

    #[test]
    fn wrapped_right_to_left_text_aligns_and_justifies_from_the_right() {
        use crate::Align;
        let hebrew = "שלום עולם זהו טקסט ארוך שנשבר לכמה שורות כדי לבדוק יישור מימין לשמאל בתוך רוחב קבוע של האלמנט";
        let start = line_spans(hebrew, Align::Start, false);
        if start.is_empty() {
            eprintln!("skipping: no Hebrew glyphs");
            return;
        }
        assert!(start.len() >= 2, "{start:?}");
        assert!(start.iter().all(|&(_, r)| r > 592.0), "wrapped RTL start hugs the right edge: {start:?}");
        let end = line_spans(hebrew, Align::End, false);
        assert!(end.iter().all(|&(l, _)| l < 8.0), "wrapped RTL end hugs the left edge: {end:?}");
        let justified = line_spans(hebrew, Align::Start, true);
        let last = justified.len() - 1;
        assert!(justified[..last].iter().all(|&(l, r)| l < 8.0 && r > 592.0), "{justified:?}");
        assert!(justified[last].1 > 592.0, "justified RTL last line starts at the right: {justified:?}");
    }

    /// Inline code between padding spaces in the body font draws one chip at the code's height,
    /// with no separate sliver for each padding run.
    #[test]
    fn inline_code_and_its_padding_share_one_chip() {
        let mut text = TextSystem::default();
        let bg = Color::BLACK.alpha(0.08);
        let content = "say \u{202f}code\u{202f} now";
        let (a, b) = (content.find('c').unwrap(), content.find(" now").unwrap());
        let pad = |s: usize, e: usize| Span { bg: Some(bg), ..Span::new(s, e) };
        let code = Span { mono: true, size: Some(13.0 * 0.9), bg: Some(bg), ..Span::new(a, b - 3) };
        let spans = [pad(4, a), code.clone(), pad(b - 3, b)];
        let style = TextStyle::new(13.0, 400.0);
        let layout = text.layout_rich(content, &spans, style, 2.0, None);
        let mut decos = Vec::new();
        text.decorations(&layout, &spans, 2.0, [0.0, 0.0], &mut decos);
        assert_eq!(decos.len(), 1, "{:?}", decos.iter().map(|d| d.rect).collect::<Vec<_>>());
        let mut alone = Vec::new();
        let code_only = text.layout_rich(content, std::slice::from_ref(&code), style, 2.0, None);
        text.decorations(&code_only, std::slice::from_ref(&code), 2.0, [0.0, 0.0], &mut alone);
        assert_eq!((decos[0].rect[1], decos[0].rect[3]), (alone[0].rect[1], alone[0].rect[3]), "the code's height");
        assert!(decos[0].rect[2] > alone[0].rect[2], "padding widens it");
    }

    #[test]
    fn live_layouts_do_not_accumulate_between_commits() {
        let mut text = TextSystem::default();
        let style = TextStyle::new(13.0, 400.0);
        let stable = text.layout_rich("stable label", &[], style, 2.0, None);
        for tick in 0..256 { text.layout_live(&tick.to_string(), style, 2.0); }
        assert_eq!(text.layouts.len(), 1);
        assert!(Rc::ptr_eq(&stable, &text.layout_rich("stable label", &[], style, 2.0, None)));
    }

    #[test]
    fn a_view_shown_again_after_a_while_does_not_reshape() {
        let mut text = TextSystem::default();
        let style = TextStyle::new(13.0, 400.0);
        let row = text.layout_rich("Back to the list", &[], style, 2.0, None);
        for _ in 0..8 { text.end_commit(); }
        let shaped = text.shaped;
        assert!(Rc::ptr_eq(&row, &text.layout_rich("Back to the list", &[], style, 2.0, None)));
        assert_eq!(text.shaped, shaped);
    }

    #[test]
    fn stale_layouts_stay_within_their_budget_newest_kept() {
        let mut text = TextSystem::default();
        let style = TextStyle::new(13.0, 400.0);
        let labels: Vec<String> = (0..1200).map(|i| format!("label {i:04}")).collect();
        for label in &labels {
            text.layout_rich(label, &[], style, 2.0, None);
            text.end_commit();
        }
        // Two more, so even the last label is stale.
        for _ in 0..2 { text.end_commit(); }
        let cost: usize = text.layouts.values().map(|e| e.2 as usize + STALE_ENTRY).sum();
        assert!(cost <= STALE_TEXT, "{cost}");
        assert!(cost > STALE_TEXT * 9 / 10, "{cost}");
        let key = |label: &str| text_hash(label, &[], style, 2.0, None);
        assert!(text.layouts.contains_key(&key(&labels[1199])));
        assert!(!text.layouts.contains_key(&key(&labels[0])));
    }

    /// A view's labels all share the commit it was last shown in.
    #[test]
    fn a_view_over_the_budget_keeps_a_budget_of_its_layouts() {
        let mut text = TextSystem::default();
        let style = TextStyle::new(13.0, 400.0);
        let labels: Vec<String> = (0..1200).map(|i| format!("label {i:04}")).collect();
        let show = |text: &mut TextSystem| for label in &labels { text.layout_rich(label, &[], style, 2.0, None); };
        show(&mut text);
        for _ in 0..2 {
            text.end_commit();
            assert_eq!(text.layouts.len(), 1200, "the current and previous commits are never evicted");
        }
        text.end_commit();
        let cost: usize = text.layouts.values().map(|e| e.2 as usize + STALE_ENTRY).sum();
        assert!(cost <= STALE_TEXT, "{cost}");
        assert!(cost > STALE_TEXT * 9 / 10, "{cost}");
        let kept = text.layouts.len();
        let shaped = text.shaped;
        show(&mut text);
        assert_eq!((text.shaped - shaped) as usize, 1200 - kept, "shown again, only what was dropped reshapes");
        for _ in 0..2 {
            text.end_commit();
            assert_eq!(text.layouts.len(), 1200, "the current and previous commits are never evicted");
        }
    }

    #[test]
    fn optical_size_default_keeps_static_font_glyphs_byte_identical() {
        let mut a = TextSystem::default(); let mut b = TextSystem::default();
        for scale in [1.0, 2.0] {
            for size in [11.0, 13.0, 24.0] {
                let style = TextStyle::new(size, 400.0);
                let mut qa = Vec::new(); let mut qb = Vec::new();
                a.glyphs("Hello ffi 0123456789", style, scale, [0.0, 0.0], &mut qa);
                b.glyphs("Hello ffi 0123456789", style.optical_sizing(OpticalSize::FontDefault), scale, [0.0, 0.0], &mut qb);
                assert_eq!(qa.iter().map(|q| (q.rect, q.uv)).collect::<Vec<_>>(), qb.iter().map(|q| (q.rect, q.uv)).collect::<Vec<_>>());
                assert_eq!(a.atlas.data, b.atlas.data);
            }
        }
    }

    #[test]
    fn moving_glyphs_reuse_four_phases_and_land_without_atlas_work() {
        let mut text = TextSystem::default();
        let layout = text.layout_rich("lHEx ", &[], TextStyle::new(13.0, 400.0), 2.0, None);
        let mut quads = Vec::new();
        // Negative origins exercise phase carry as well as both sides of zero.
        for step in 0..256 {
            quads.clear();
            text.glyphs_for_layout(&layout, 2.0, [-2.0 + step as f32 / 64.0, 12.37], false, &mut quads);
            assert_eq!(quads.len(), 4, "dilation must not turn whitespace into empty draw instances");
            assert!(quads.iter().all(|g| g.rect[0].fract() == 0.0 && g.rect[1].fract() == 0.0));
        }
        let entries = text.atlas.map.len();
        assert!(entries <= 5 * 4, "four glyphs plus cached whitespace have at most four x phases");
        let mut track = rune_motion::Animated::new(-2.0f32).with_class(rune_motion::PropClass::Spatial);
        track.animate_to(8.0, rune_motion::Spring::new(0.3, 0.0), 0.0);
        let end = track.settle_time();
        assert!(end.is_finite());
        text.atlas.dirty = None;
        quads.clear();
        text.glyphs_for_layout(&layout, 2.0, [8.0, 12.37], false, &mut quads);
        let landed: Vec<_> = quads.iter().map(|q| (q.rect, q.uv)).collect();
        for frame in 0..120 {
            let x = track.sample(end + frame as f64 / 120.0);
            assert_eq!(x, 8.0, "the analytic tail lands on its target exactly");
            quads.clear();
            text.glyphs_for_layout(&layout, 2.0, [x, 12.37], false, &mut quads);
            assert!(quads.iter().map(|q| (q.rect, q.uv)).eq(landed.iter().copied()));
        }
        assert_eq!(text.atlas.map.len(), entries);
        assert!(text.atlas.dirty.is_none());
    }

    #[test]
    fn nominal_size_dilation_is_part_of_the_cache_key() {
        let mut text = TextSystem::default();
        let small = text.layout_rich("H", &[], TextStyle::new(12.0, 400.0), 2.0, None);
        let large = text.layout_rich("H", &[], TextStyle::new(24.0, 400.0), 1.0, None);
        let mut a = Vec::new(); let mut b = Vec::new();
        text.glyphs_for_layout(&small, 2.0, [0.0, 0.0], false, &mut a);
        text.glyphs_for_layout(&large, 1.0, [0.0, 0.0], false, &mut b);
        assert_ne!(a[0].uv, b[0].uv, "same pixel size, different nominal-size dilation");
    }
}

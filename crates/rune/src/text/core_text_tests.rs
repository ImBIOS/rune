//! Rune's advances against Core Text's for the macOS system font. Core Text applies the font's
//! `trak` table at the point size, so text matches AppKit's width at every display scale.

use std::ffi::c_void;

use super::*;

type Ref = *const c_void;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    fn CFNumberCreate(alloc: Ref, kind: isize, value: *const c_void) -> Ref;
    fn CFDictionaryCreate(alloc: Ref, keys: *const Ref, values: *const Ref, count: isize, kc: *const c_void, vc: *const c_void) -> Ref;
    fn CFStringCreateWithBytes(alloc: Ref, bytes: *const u8, len: isize, encoding: u32, external: u8) -> Ref;
    fn CFAttributedStringCreate(alloc: Ref, string: Ref, attributes: Ref) -> Ref;
    fn CFRelease(value: Ref);
}

#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    static kCTFontAttributeName: Ref;
    static kCTFontVariationAttribute: Ref;
    fn CTFontCreateUIFontForLanguage(kind: u32, size: f64, language: Ref) -> Ref;
    fn CTFontDescriptorCreateWithAttributes(attributes: Ref) -> Ref;
    fn CTFontCreateCopyWithAttributes(font: Ref, size: f64, matrix: Ref, descriptor: Ref) -> Ref;
    fn CTLineCreateWithAttributedString(string: Ref) -> Ref;
    fn CTLineGetTypographicBounds(line: Ref, ascent: *mut f64, descent: *mut f64, leading: *mut f64) -> f64;
}

unsafe fn dictionary(keys: &[Ref], values: &[Ref]) -> Ref {
    unsafe {
        CFDictionaryCreate(std::ptr::null(), keys.as_ptr(), values.as_ptr(), keys.len() as isize,
            &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks)
    }
}

/// The system UI font at `size` points with its `wght` axis at `weight` and `opsz` at `size`,
/// the instance Rune shapes for `Family::Named("System Font")`.
fn system_font(size: f32, weight: f32) -> Ref {
    unsafe {
        let base = CTFontCreateUIFontForLanguage(2, size as f64, std::ptr::null());
        let tag = |t: &[u8; 4]| { let v = i32::from_be_bytes(*t); CFNumberCreate(std::ptr::null(), 3, (&v as *const i32).cast()) };
        let number = |v: f64| CFNumberCreate(std::ptr::null(), 6, (&v as *const f64).cast());
        let axes = dictionary(&[tag(b"wght"), tag(b"opsz")], &[number(weight as f64), number(size.clamp(17.0, 96.0) as f64)]);
        let descriptor = CTFontDescriptorCreateWithAttributes(dictionary(&[kCTFontVariationAttribute], &[axes]));
        CTFontCreateCopyWithAttributes(base, size as f64, std::ptr::null(), descriptor)
    }
}

/// Core Text's typographic width of `text` in `font`, in points.
fn core_text_width(text: &str, font: Ref) -> f64 {
    unsafe {
        let string = CFStringCreateWithBytes(std::ptr::null(), text.as_ptr(), text.len() as isize, 0x0800_0100, 0);
        let attributed = CFAttributedStringCreate(std::ptr::null(), string, dictionary(&[kCTFontAttributeName], &[font]));
        let line = CTLineCreateWithAttributedString(attributed);
        let width = CTLineGetTypographicBounds(line, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
        CFRelease(line);
        CFRelease(attributed);
        CFRelease(string);
        width
    }
}

const STRINGS: [&str; 7] = [
    "Send dev-site captures to OpenCode",
    "Copy \u{2318}K",
    "General",
    "Appearance",
    "Hello, World",
    "The quick brown fox jumps over the lazy dog",
    "Launch at login 0123456789",
];
/// Fractional sizes check the rounding of interpolated tracking (16.3 and 33 pt fall between
/// the system font's listed sizes).
const SIZES: [f32; 17] = [8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 16.3, 17.0, 20.0, 22.0, 24.0, 28.0, 33.0, 34.0];

/// Rune's unrounded width of `text` in points.
fn rune_width(ts: &mut TextSystem, text: &str, style: TextStyle, scale: f32) -> f64 {
    (ts.layout_rich(text, &[], style, scale, None).full_width() / scale) as f64
}

/// Within 0.05 pt per glyph of Core Text at 8-34 pt, weights 400 and 600, scales 1-3. Prints
/// the comparison table with `--nocapture`.
#[test]
fn system_font_advances_match_core_text_at_every_scale() {
    let mut ts = TextSystem::default();
    let family = Family::Named("System Font");
    if ts.font_cx.collection.family_id("System Font").is_none() {
        eprintln!("skipping: no macOS system font");
        return;
    }
    let example = STRINGS[0];
    println!("{:>5} {:>4} | {:>8} {:>8} {:>8} {:>8} | per glyph, worst string: {:>6} {:>6} {:>6}", "pt", "wght", "CT", "1x", "2x", "3x", "1x", "2x", "3x");
    let mut worst = 0.0f64;
    for weight in [400.0, 600.0] {
        for size in SIZES {
            let font = system_font(size, weight);
            let style = TextStyle::new(size, weight).family(family);
            let mut per_glyph = [0.0f64; 3];
            let mut row = [0.0f64; 4];
            for text in STRINGS {
                let ct = core_text_width(text, font);
                let glyphs = text.chars().count() as f64;
                for (i, scale) in [1.0, 2.0, 3.0].into_iter().enumerate() {
                    let rune = rune_width(&mut ts, text, style, scale);
                    let error = (rune - ct) / glyphs;
                    if error.abs() > per_glyph[i].abs() { per_glyph[i] = error; }
                    if text == example { row[0] = ct; row[i + 1] = rune; }
                }
            }
            unsafe { CFRelease(font) };
            println!("{size:>5} {weight:>4} | {:>8.2} {:>8.2} {:>8.2} {:>8.2} | {:>+27.3} {:>+6.3} {:>+6.3}",
                row[0], row[1], row[2], row[3], per_glyph[0], per_glyph[1], per_glyph[2]);
            worst = per_glyph.iter().fold(worst, |w, e| w.max(e.abs()));
        }
    }
    assert!(worst < 0.05, "worst per-glyph error {worst:.3} pt");
}

fn widths(ts: &mut TextSystem, text: &str, spans: &[Span], style: TextStyle) -> [f32; 3] {
    [1.0, 2.0, 3.0].map(|scale| ts.layout_rich(text, spans, style, scale, None).full_width() / scale)
}

fn assert_scale_free(widths: [f32; 3], what: &str) {
    assert!(widths.iter().all(|w| (w - widths[0]).abs() < 0.02), "{what}: {widths:?}");
}

/// `'trak' 0` turns the table's tracking off at every scale, and `TextStyle::tracking` adds to
/// it, as kerning adds to tracking in AppKit.
#[test]
fn trak_opts_out_per_style_and_tracking_adds_to_it() {
    let mut ts = TextSystem::default();
    if ts.font_cx.collection.family_id("System Font").is_none() {
        eprintln!("skipping: no macOS system font");
        return;
    }
    let text = STRINGS[0];
    let n = text.chars().count() as f32;
    let style = TextStyle::new(14.0, 400.0).family(Family::Named("System Font"));
    let tracked = widths(&mut ts, text, &[], style);
    let untracked = widths(&mut ts, text, &[], style.features("'tnum' 1, 'trak' 0"));
    let extra = widths(&mut ts, text, &[], style.tracking(0.5));
    for w in [tracked, untracked, extra] {
        assert_scale_free(w, "width");
    }
    // The system font tracks 14 pt text by -22/2048 em.
    let trak = -22.0 / 2048.0 * 14.0;
    assert!((tracked[0] - untracked[0] - trak * n).abs() < 0.05, "{tracked:?} vs {untracked:?}");
    assert!((extra[0] - tracked[0] - 0.5 * n).abs() < 0.05, "{extra:?} vs {tracked:?}");
}

/// Spans in another size or weight, and runs that fall back to the system font from an app font
/// without `trak`, get their own font's tracking.
#[test]
fn spans_and_fallback_runs_track_at_their_own_point_size() {
    const PLEX: &[u8] = include_bytes!("../../../../themes/opencode/assets/fonts/IBMPlexMono-Regular.ttf");
    let mut ts = TextSystem::new(&[PLEX], ("IBM Plex Mono", "IBM Plex Mono"));
    if ts.font_cx.collection.family_id("System Font").is_none() {
        eprintln!("skipping: no macOS system font");
        return;
    }
    let system = TextStyle::new(13.0, 400.0).family(Family::Named("System Font"));
    let text = "Send dev-site captures to OpenCode";
    let spans = [
        Span { size: Some(20.0), ..Span::new(0, 4) },
        Span { weight: Some(700.0), ..Span::new(5, 13) },
        Span { mono: true, ..Span::new(14, 22) },
    ];
    assert_scale_free(widths(&mut ts, text, &spans, system), "spans");
    assert_scale_free(widths(&mut ts, "Copy \u{2318}\u{21e7}K \u{2325}\u{2303}", &[], TextStyle::new(13.0, 400.0)), "fallback");
}

/// Carets sit on the drawn glyphs' pens when tracking is corrected.
#[test]
fn carets_follow_corrected_glyph_positions() {
    let mut ts = TextSystem::default();
    if ts.font_cx.collection.family_id("System Font").is_none() {
        eprintln!("skipping: no macOS system font");
        return;
    }
    let text = "Send dev-site captures";
    let layout = ts.layout_rich(text, &[], TextStyle::new(14.0, 400.0).family(Family::Named("System Font")), 2.0, None);
    let mut pens = Vec::new();
    for line in layout.lines() {
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(run) = item else { continue };
            let mut x = run.offset();
            for g in run.glyphs() {
                pens.push(x + g.x);
                x += g.advance;
            }
        }
    }
    assert_eq!(pens.len(), text.len());
    for (i, pen) in pens.iter().enumerate() {
        let caret = parley::editing::Cursor::from_byte_index(&layout, i, parley::layout::Affinity::Downstream).geometry(&layout, 1.0);
        assert!((caret.x0 as f32 - pen).abs() < 1e-3, "byte {i}: caret {} vs pen {pen}", caret.x0);
    }
}

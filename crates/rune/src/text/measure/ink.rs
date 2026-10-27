//! Where text's ink sits inside its box. Text is placed by its line box and advance, so a symbol
//! like ↵, ⇥ or ⌘ can draw well off the middle of a keycap that centres it. [`ink`] rasterizes
//! the text as the renderer would, with the same fonts and fallbacks, and reports the ink's box
//! and its centre of mass, so a view can centre what is actually drawn.

use std::cell::RefCell;
use std::collections::HashMap;

use parley::PositionedLayoutItem;
use swash::FontRef;
use swash::scale::image::Content;
use swash::scale::{Render, Source, StrikeWith};
use swash::zeno::{Format, Vector};

use super::super::Face;
use super::with_text;
use crate::tok::TextStyle;

/// Rasterizing at this many times the size puts every measurement within 1/8 pt.
const OVERSAMPLE: f32 = 8.0;

/// Where a text's ink sits, in points from the top-left of its box (the size
/// [`measure`](super::measure) gives, which is the box a `text` element lays out).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextInk {
    /// The ink's bounds as `[x, y, w, h]`.
    pub bounds: [f32; 4],
    /// The ink's centre of mass, weighting each pixel by its coverage.
    pub mass: [f32; 2],
}

impl TextInk {
    /// The middle of the ink's bounds.
    pub fn centre(&self) -> [f32; 2] {
        let [x, y, w, h] = self.bounds;
        [x + w / 2.0, y + h / 2.0]
    }

    /// The point that looks like the middle: `pull` of the way from the bounds' centre to the
    /// centre of mass. A glyph heavy on one side (an arrowhead, ⇥'s bar) reads as centred when
    /// its bounds shift a little toward the light side; about 0.3 suits keyboard symbols.
    pub fn optical(&self, pull: f32) -> [f32; 2] {
        let [cx, cy] = self.centre();
        [cx + (self.mass[0] - cx) * pull, cy + (self.mass[1] - cy) * pull]
    }
}

thread_local! {
    static CACHE: RefCell<HashMap<(String, String), Option<TextInk>>> = RefCell::new(HashMap::new());
}

/// Where `text` set in `style` puts its ink, or None when it draws nothing (spaces, empty).
/// Cached per text and style for the thread; the first call for each costs one shaping pass and
/// a raster of each glyph at 8× size.
pub fn ink(text: &str, style: TextStyle) -> Option<TextInk> {
    let key = (text.to_owned(), format!("{style:?}"));
    if let Some(hit) = CACHE.with_borrow(|c| c.get(&key).copied()) {
        return hit;
    }
    let found = with_text(|t| {
        let layout = t.layout_rich(text, &[], style, OVERSAMPLE, None);
        let mut acc = Acc::default();
        let mut coords = Vec::new();
        for line in layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(run) = item else { continue };
                let face = Face::of(&run, &mut coords);
                let Some(font) = FontRef::from_index(face.font.data.as_ref(), face.font.index as usize) else { continue };
                let mut scaler = t.scale_cx.builder(font).size(face.size).hint(false).normalized_coords(coords.iter().copied()).build();
                let mut x = run.offset();
                for g in run.glyphs() {
                    let (pen_x, pen_y) = (x + g.x, face.baseline + g.y);
                    let image = Render::new(&[Source::ColorOutline(0), Source::ColorBitmap(StrikeWith::BestFit), Source::Outline])
                        .format(Format::Alpha)
                        .offset(Vector::new(pen_x.fract(), 0.0))
                        .render(&mut scaler, g.id as u16);
                    if let Some(image) = image {
                        let stride = if image.content == Content::Color { 4 } else { 1 };
                        let at = image.placement;
                        let (left, top) = (pen_x.floor() + at.left as f32, pen_y.round() - at.top as f32);
                        for (i, px) in image.data.chunks(stride).enumerate() {
                            let a = px[stride - 1] as f32 / 255.0;
                            if a > 0.0 {
                                let (col, row) = ((i as u32 % at.width) as f32, (i as u32 / at.width) as f32);
                                acc.add(left + col, top + row, a);
                            }
                        }
                    }
                    x += g.advance;
                }
            }
        }
        acc.done()
    });
    CACHE.with_borrow_mut(|c| c.insert(key, found));
    found
}

/// Covered pixels, in oversampled device pixels.
#[derive(Default)]
struct Acc {
    min: [f32; 2],
    max: [f32; 2],
    sum: [f32; 2],
    weight: f32,
}

impl Acc {
    fn add(&mut self, x: f32, y: f32, a: f32) {
        if self.weight == 0.0 {
            (self.min, self.max) = ([x, y], [x + 1.0, y + 1.0]);
        }
        self.min = [self.min[0].min(x), self.min[1].min(y)];
        self.max = [self.max[0].max(x + 1.0), self.max[1].max(y + 1.0)];
        self.sum = [self.sum[0] + (x + 0.5) * a, self.sum[1] + (y + 0.5) * a];
        self.weight += a;
    }

    fn done(&self) -> Option<TextInk> {
        (self.weight > 0.0).then(|| {
            let k = OVERSAMPLE;
            TextInk {
                bounds: [self.min[0] / k, self.min[1] / k, (self.max[0] - self.min[0]) / k, (self.max[1] - self.min[1]) / k],
                mass: [self.sum[0] / self.weight / k, self.sum[1] / self.weight / k],
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ink_sits_inside_the_box_and_spaces_have_none() {
        let style = TextStyle::new(12.0, 400.0).lh(16.0);
        let (w, h) = super::super::measure("H", &[], style, None);
        let h_ink = ink("H", style).expect("H draws");
        let [x, y, iw, ih] = h_ink.bounds;
        assert!(x >= 0.0 && y >= 0.0 && x + iw <= w && y + ih <= h, "{h_ink:?} in {w}×{h}");
        assert!(ih > 7.0 && ih < 10.0, "an H is about cap height: {ih}");
        let [cx, _] = h_ink.centre();
        assert!((h_ink.mass[0] - cx).abs() < 0.2, "H is symmetric: {h_ink:?}");
        assert_eq!(ink(" ", style), None);
    }

    #[test]
    fn a_wider_word_has_wider_ink() {
        let style = TextStyle::new(12.0, 400.0);
        assert!(ink("HH", style).unwrap().bounds[2] > ink("H", style).unwrap().bounds[2] * 1.8);
    }
}

//! Measuring text from a view, for views that place things themselves (table columns, diagram
//! nodes). A view has no handle on the runtime's text system, so each thread keeps one of its own
//! built from the fonts the most recent [`TextSystem`] was given: the app's `AppSpec::fonts` in
//! an app, the built-in Inter in a test runtime. Same shaper, same fonts, same numbers.

use std::cell::RefCell;
use std::sync::Mutex;

use super::{Span, TextSystem};

mod ink;
pub use ink::{TextInk, ink};
use crate::tok::TextStyle;

struct Fonts {
    generation: u64,
    fonts: Vec<&'static [u8]>,
    families: (String, String),
}

static FONTS: Mutex<Option<Fonts>> = Mutex::new(None);

/// Layouts kept between evictions (each eviction keeps those used since the one before).
const EVICT_EVERY: u32 = 512;

struct Measurer {
    generation: u64,
    text: TextSystem,
    calls: u32,
}

thread_local! {
    static MEASURER: RefCell<Option<Measurer>> = const { RefCell::new(None) };
}

/// Called by every [`TextSystem::new`]; the next measurement uses these fonts.
pub(super) fn note_fonts(fonts: &[&'static [u8]], families: (&str, &str)) {
    let Ok(mut slot) = FONTS.lock() else { return };
    let same = slot.as_ref().is_some_and(|f| f.fonts == fonts && f.families.0 == families.0 && f.families.1 == families.1);
    if !same {
        let generation = slot.as_ref().map_or(1, |f| f.generation + 1);
        *slot = Some(Fonts { generation, fonts: fonts.to_vec(), families: (families.0.to_owned(), families.1.to_owned()) });
    }
}

/// The size in points of `text` (with `spans`, as [`crate::rich`] takes them) set in `style`,
/// wrapped at `max_width` points when given, as the runtime will lay it out. Widths round up
/// to whole points. `max_width: Some(0.0)` gives the widest unbreakable run (CSS min-content).
///
/// The first call on a thread loads the fonts (a few milliseconds); later calls cost a hash
/// lookup when the same text was measured recently, or one shaping pass when not.
pub fn measure(text: &str, spans: &[Span], style: TextStyle, max_width: Option<f32>) -> (f32, f32) {
    with_text(|t| {
        let l = t.layout_rich(text, spans, style, 1.0, max_width);
        (l.full_width().ceil(), super::layout_height(&l, style, 1.0))
    })
}

/// The advance in points of `text` set on one line in `style`, unrounded: how much wider a row
/// of [`El::advance_in`](crate::El::advance_in) cells gets for it, to within a device pixel.
pub fn advance(text: &str, style: TextStyle) -> f32 {
    with_text(|t| t.layout_rich(text, &[], style, 1.0, None).full_width())
}

/// Runs `f` on this thread's text system, built from the fonts the app was given.
pub(super) fn with_text<R>(f: impl FnOnce(&mut TextSystem) -> R) -> R {
    let generation = || FONTS.lock().ok().and_then(|f| f.as_ref().map(|f| f.generation)).unwrap_or(0);
    MEASURER.with_borrow_mut(|m| {
        let current = generation();
        if m.as_ref().is_none_or(|m| m.generation != current) {
            let wanted = FONTS.lock().ok().and_then(|f| f.as_ref().map(|f| (f.fonts.clone(), f.families.clone())));
            let text = match &wanted {
                Some((fonts, (sans, mono))) => TextSystem::new(fonts, (sans, mono)),
                None => TextSystem::default(),
            };
            // Building a text system notes its fonts, so read the generation it settled on.
            *m = Some(Measurer { generation: generation(), text, calls: 0 });
        }
        let m = m.as_mut().expect("set above");
        m.calls += 1;
        if m.calls % EVICT_EVERY == 0 {
            m.text.end_commit();
        }
        f(&mut m.text)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_text_and_wraps() {
        let style = TextStyle::new(14.0, 400.0);
        let (w, h) = measure("Hello world", &[], style, None);
        assert!(w > 50.0 && h >= 17.0, "{w} {h}");
        let (min, tall) = measure("Hello world", &[], style, Some(0.0));
        assert!(min < w && tall > h, "min-content breaks at every opportunity: {min} {tall}");
        let mut bold = Span::new(0, 5);
        bold.weight = Some(700.0);
        assert!(measure("Hello world", &[bold], style, None).0 > w);
    }
}

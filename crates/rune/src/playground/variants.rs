//! Variants: a story's state made with one of its named tweaks, picked by 1 to 9.

use super::*;

impl<S: 'static> Live<S> {
    /// The open story's variants' names, in key order.
    pub(super) fn variants(&self) -> Vec<&'static str> {
        if self.scenes.get(self.selected).is_some() {
            return self.variants.iter().filter(|(s, _, _)| *s == self.selected).map(|(_, name, _)| *name).collect();
        }
        match self.page() {
            Some((_, Page { kind: Kind::Own(own), .. })) => own.variants(),
            _ => Vec::new(),
        }
    }

    /// The open story's variant, by index, of those it has.
    pub(super) fn variant_at(&self) -> usize {
        self.variant.get(self.selected).copied().unwrap_or(0)
    }

    /// The open scene's variant's fn, if it has variants.
    pub(super) fn scene_variant(&self) -> Option<fn(&mut S)> {
        self.variants.iter().filter(|(s, _, _)| *s == self.selected).nth(self.variant_at()).map(|(_, _, apply)| *apply)
    }

    /// What a digit key does: the open story's variant it names, if it has that many.
    pub(super) fn variant_key(&self, c: char) -> Option<Request> {
        let n = c.to_digit(10).filter(|d| (1..=9).contains(d))? as usize;
        (n <= self.variants().len()).then_some(Request::Variant(n - 1))
    }
}

//! Keybind (`@opencode/ui` data-display/keybind): small keycaps, upper-cased.

use rune::{El, TextStyle, row, text};

use crate::Kit;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum KeybindVariant {
    #[default]
    Neutral,
    Ghost,
}

pub struct Keybind {
    keys: Vec<String>,
    variant: KeybindVariant,
}

impl Keybind {
    pub fn new<S: AsRef<str>>(keys: &[S]) -> Keybind {
        Keybind { keys: keys.iter().map(|k| k.as_ref().to_owned()).collect(), variant: KeybindVariant::Neutral }
    }
    pub fn variant(mut self, v: KeybindVariant) -> Self {
        self.variant = v;
        self
    }

    pub fn build(self, k: &Kit) -> El {
        let c = &k.c;
        let (fill, ink) = match self.variant {
            KeybindVariant::Neutral => (c.layer_03, c.text_muted),
            KeybindVariant::Ghost => (rune::Color::TRANSPARENT, c.text_faint),
        };
        let font = TextStyle { line_height: 11.0, ..k.ty.small_medium };
        let label = self.keys.join(" ");
        row().role("group").label(label).gap(2.0).center_y().children(self.keys.into_iter().enumerate().map(
            |(i, key)| {
                row()
                    .key(format!("key{i}"))
                    .min_w(14.0)
                    .h(14.0)
                    .px(4.0)
                    .center()
                    .radius(2.0)
                    .fill(fill)
                    .child(text(key.to_uppercase()).font(font).color(ink))
            },
        ))
    }
}

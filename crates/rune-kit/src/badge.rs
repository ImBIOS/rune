//! Badge / tag (`@opencode/ui` data-display/badge) and Divider (`layout/divider`).

use rune::{El, TextStyle, el, row, text};

use crate::Kit;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BadgeVariant {
    #[default]
    Neutral,
    Accent,
    Subtle,
}

pub struct Badge {
    label: String,
    variant: BadgeVariant,
    standard: bool,
    high_contrast: bool,
}

impl Badge {
    pub fn new(label: impl Into<String>) -> Badge {
        Badge { label: label.into(), variant: BadgeVariant::Neutral, standard: false, high_contrast: false }
    }
    pub fn variant(mut self, v: BadgeVariant) -> Self {
        self.variant = v;
        self
    }
    /// The older, larger tag (`appearance="standard"`).
    pub fn standard(mut self) -> Self {
        self.standard = true;
        self
    }
    /// A stronger border (`data-high-contrast`).
    pub fn high_contrast(mut self) -> Self {
        self.high_contrast = true;
        self
    }

    pub fn build(self, k: &Kit) -> El {
        let c = &k.c;
        let mut e = row().role("status").label(self.label.clone()).center().gap(4.0);
        if self.standard {
            e = e.h(18.0).px(6.0).radius(k.radius.xs);
            let (fill, ink, border) = match self.variant {
                BadgeVariant::Neutral => (c.surface_raised_base, c.std_text, Some(c.border_weak)),
                BadgeVariant::Accent => (c.bg_accent, c.text_contrast, None),
                BadgeVariant::Subtle => (c.layer_03, c.text_muted, None),
            };
            e = e.fill(fill).child(text(self.label).font(k.ty.text_small_medium).color(ink));
            if let Some(b) = border {
                e = e.border(0.5, b);
            }
            return e;
        }
        let font = TextStyle { line_height: 11.0, ..k.ty.small_medium };
        let (fill, ink, border) = match self.variant {
            BadgeVariant::Neutral => {
                (c.layer_02, c.text_muted, Some(if self.high_contrast { c.border_strong } else { c.border_base }))
            }
            BadgeVariant::Accent => (c.bg_accent, c.text_contrast, None),
            BadgeVariant::Subtle => (c.layer_03, c.text_muted, None),
        };
        e = e.h(16.0).px(4.0).radius(2.0).fill(fill).child(text(self.label).font(font).color(ink));
        if let Some(b) = border {
            e = e.border(0.5, b);
        }
        e
    }
}

/// A 1 point rule (`Divider`), horizontal unless `vertical`.
pub struct Divider {
    vertical: bool,
}

impl Divider {
    pub fn horizontal() -> Divider {
        Divider { vertical: false }
    }
    pub fn vertical() -> Divider {
        Divider { vertical: true }
    }
    pub fn build(self, k: &Kit) -> El {
        let e = el().role("separator").fill(k.c.border_base);
        if self.vertical { e.w(1.0).h_full() } else { e.h(1.0).w_full() }
    }
}

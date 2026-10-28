//! A label whose changed characters roll (iOS numeric/text content transition), for proportional
//! and monospace fonts alike.
//!
//! Each grapheme cluster is its own keyed text node in a soft-clipped row, as wide as its advance
//! in the whole label shaped once (kerning included), so at rest the row sets exactly like the
//! same string as one text. When the text changes,
//! the new clusters are matched to the old ones (common prefix and suffix, then an LCS of the
//! middle for short strings): matched clusters keep their key and slide to their new x on the
//! layout spring; the rest leave through one edge and enter from the other with a small per-glyph
//! stagger. A label given its [`RollingText::value`] picks the edge from the value's direction, as
//! SwiftUI's `.numericText(value:)` does: up as it rises, down as it falls. The row's width springs to the new width (`width_leads`). Everything runs on the
//! render clock; reduced motion drops the travel and leaves a crossfade.
//!
//! Keys must survive between builds, so each label remembers its previous clusters by its
//! [`RollingText::new`] key (per thread, per key). Rebuilding with the same text is a no-op.

use std::cell::RefCell;
use std::collections::HashMap;

use rune::text::Run;
use rune::{Color, Edge, El, Spring, SpringPair, TextStyle, Transition, row, text};
use unicode_segmentation::UnicodeSegmentation;

use crate::Kit;

/// Extra spring duration per changed glyph, left to right.
const STAGGER: f32 = 0.03;
/// The stagger stops growing after this many glyphs, so long replacements still land together.
const STAGGER_CAP: usize = 6;
/// LCS only runs while `old × new` middle clusters fit this budget; longer middles just roll.
const LCS_BUDGET: usize = 48 * 48;

#[derive(Default)]
struct Memory {
    clusters: Vec<(String, u64)>,
    next: u64,
    value: Option<f64>,
    width: Option<f32>,
}

thread_local! {
    static MEMORY: RefCell<HashMap<String, Memory>> = RefCell::new(HashMap::new());
}

/// A label that rolls only the characters that changed.
///
/// ```ignore
/// RollingText::new("copy-label", if copied { format!("Copied, clears in 0:{left:02}") } else { "Copy password".into() })
///     .font(k.ty.ui_medium)
///     .build(&k)
/// ```
pub struct RollingText {
    key: String,
    text: String,
    font: Option<TextStyle>,
    color: Option<Color>,
    spring: Spring,
    from: Edge,
    value: Option<f64>,
    trailing: bool,
}

impl RollingText {
    /// The default roll and slide spring: quick, with a hint of overshoot.
    pub const SPRING: Spring = Spring::new(0.36, 0.12);

    /// `key` names this label (it keys the row and remembers the previous text); `text` is what
    /// it shows now.
    pub fn new(key: impl Into<String>, text: impl Into<String>) -> RollingText {
        RollingText {
            key: key.into(),
            text: text.into(),
            font: None,
            color: None,
            spring: Self::SPRING,
            from: Edge::Bottom,
            value: None,
            trailing: false,
        }
    }
    pub fn font(mut self, font: TextStyle) -> Self {
        self.font = Some(font);
        self
    }
    pub fn color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }
    /// The roll and slide spring (default [`RollingText::SPRING`]).
    pub fn spring(mut self, spring: Spring) -> Self {
        self.spring = spring;
        self
    }
    /// The edge new glyphs enter from (old ones leave through the opposite edge). Default
    /// `Edge::Bottom`; `Edge::Top` reads as counting down.
    pub fn from(mut self, edge: Edge) -> Self {
        self.from = edge;
        self
    }
    /// The number the label shows (SwiftUI's `.numericText(value:)`): when it rises, new glyphs
    /// enter from [`RollingText::from`] (digits move up); when it falls, from the opposite edge.
    /// Text that changes while the value holds (a new unit or format) rolls from `from`, as a
    /// label without a value does; so does a value that isn't finite.
    pub fn value(mut self, value: f64) -> Self {
        self.value = Some(value);
        self
    }
    /// The label sits against its trailing edge (right-aligned), so it widens to the left. New
    /// glyphs then roll in where they come to rest, rather than riding in from the right with
    /// the label's slide: "9s" → "10s" rolls "10" in beside the "s", not over it.
    pub fn trailing(mut self) -> Self {
        self.trailing = true;
        self
    }

    pub fn build(self, k: &Kit) -> El {
        let font = self.font.unwrap_or(k.ty.ui);
        let color = self.color.unwrap_or(k.c.text_base);
        let width = rune::text::measure(&self.text, &[], font, None).0;
        let (glyphs, falling, was) = keys(&self.key, &self.text, self.value, width);
        let from = if falling { self.from.opposite() } else { self.from };
        let edge = (font.line_px() * 0.12).max(1.0);
        // The fades sit in padding that negative margins take back out of layout, so the resting
        // glyphs (descenders included) are never faded but rolling glyphs and a growing or
        // shrinking edge stay soft. Glyphs slide on the row's own width-leading X spring, so in a
        // label whose trailing edge holds still (right-aligned) the glyphs that stay hold still.
        let side = (font.size * 0.3).max(2.0);
        let mut e = row()
            .key(self.key.clone())
            .role("text")
            .label(self.text.clone())
            .h(font.line_px() + 2.0 * edge)
            .px(side)
            .py(edge)
            .ml(-side)
            .mr(-side)
            .mt(-edge)
            .mb(-edge)
            .motion(self.spring)
            .width_leads()
            .clip_fade(side, edge, side, edge);
        // Every cell measures as its slice of the whole label shaped once, so the row keeps the
        // run's kerned, fractional advances and the width of the same string set as one text.
        let run = Run::new(self.text.as_str());
        let mut rank = 0;
        for (g, (at, _)) in glyphs.into_iter().zip(self.text.grapheme_indices(true)) {
            let spring = if g.entered {
                let s = Spring::new(self.spring.duration + STAGGER * rank.min(STAGGER_CAP) as f32, self.spring.bounce);
                rank += 1;
                s
            } else {
                self.spring
            };
            let tr = if g.entered {
                let push = Transition::push(from);
                match was.map(|w| width - w).filter(|d| self.trailing && d.abs() > 0.01) {
                    // The label's left edge slides by its change in width, on its X spring; a new
                    // glyph starts that far the other way, on the same spring, so on screen it
                    // only rolls.
                    Some(grow) => push
                        .combined(Transition::offset(-grow, 0.0))
                        .springs(SpringPair::new(Spring::new(self.spring.duration * rune_motion::WIDTH_LEAD, self.spring.bounce), push.springs.exit)),
                    None => push,
                }
            } else {
                Transition::asymmetric(Transition::identity(), Transition::push(from))
            };
            e = e.child(
                text(g.cluster)
                    .key(format!("{}.{}", self.key, g.id))
                    .font(font)
                    .advance_in(&run, at)
                    .color(color)
                    .motion(spring)
                    .width_leads()
                    .transition(tr),
            );
        }
        e
    }
}

struct Glyph {
    cluster: String,
    id: u64,
    /// New in this build (it rolls in); otherwise it kept its identity.
    entered: bool,
}

/// Assigns each cluster of `text` a stable id: clusters matched to the previous text under `key`
/// keep theirs, the rest get fresh ones. Also whether `value` fell below the last finite value
/// one, and the label's last `width`.
fn keys(key: &str, text: &str, value: Option<f64>, width: f32) -> (Vec<Glyph>, bool, Option<f32>) {
    MEMORY.with(|m| {
        let mut m = m.borrow_mut();
        let mem = m.entry(key.to_owned()).or_default();
        let new: Vec<&str> = text.graphemes(true).collect();
        let old: Vec<&str> = mem.clusters.iter().map(|(c, _)| c.as_str()).collect();
        let matched = diff(&old, &new);
        let fresh = mem.clusters.is_empty() && mem.next == 0;
        let mut glyphs = Vec::with_capacity(new.len());
        for (c, from) in new.iter().zip(matched) {
            let (id, entered) = match from {
                Some(i) => (mem.clusters[i].1, false),
                None => {
                    mem.next += 1;
                    (mem.next, !fresh)
                }
            };
            glyphs.push(Glyph { cluster: (*c).to_owned(), id, entered });
        }
        mem.clusters = glyphs.iter().map(|g| (g.cluster.clone(), g.id)).collect();
        let falling = matches!((mem.value, value), (Some(old), Some(new)) if new.is_finite() && new < old);
        if let Some(v) = value.filter(|v| v.is_finite()) {
            mem.value = Some(v);
        }
        let was = mem.width.replace(width);
        (glyphs, falling, was)
    })
}

/// For each cluster of `new`, the index of the `old` cluster it continues, if any. Matches keep
/// order: the common prefix and suffix, then an LCS of the middle when it is short. Middle matches
/// survive in runs of two or more, or alone when they hold their place (the ":" of "1:00" →
/// "0:59"); a lone letter that would slide among rolling neighbours rolls too, so "Copy password"
/// → "Copied, clears in" doesn't leave a stray "r" or "s" gliding across the label.
pub(crate) fn diff(old: &[&str], new: &[&str]) -> Vec<Option<usize>> {
    let mut out = vec![None; new.len()];
    let n = old.len().min(new.len());
    let mut pre = 0;
    while pre < n && old[pre] == new[pre] {
        out[pre] = Some(pre);
        pre += 1;
    }
    let mut suf = 0;
    while suf < n - pre && old[old.len() - 1 - suf] == new[new.len() - 1 - suf] {
        out[new.len() - 1 - suf] = Some(old.len() - 1 - suf);
        suf += 1;
    }
    let (a, b) = (&old[pre..old.len() - suf], &new[pre..new.len() - suf]);
    if a.is_empty() || b.is_empty() || a.len() * b.len() > LCS_BUDGET {
        return out;
    }
    // How far a match slides, in glyphs, measured from whichever end it sits closer to.
    let shift = |i: usize, j: usize| i.abs_diff(j).min((a.len() - i).abs_diff(b.len() - j));
    // A weighted LCS (suffix table): each match scores more the less it slides, so among
    // equally long subsequences the one that stays in place wins ("1:00" → "0:59" keeps ":").
    let weight = |i: usize, j: usize| 8 - shift(i, j).min(7) as u32;
    let w = b.len() + 1;
    let mut t = vec![0u32; (a.len() + 1) * w];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            let skip = t[(i + 1) * w + j].max(t[i * w + j + 1]);
            t[i * w + j] = if a[i] == b[j] { skip.max(t[(i + 1) * w + j + 1] + weight(i, j)) } else { skip };
        }
    }
    let mut pairs = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] && t[i * w + j] == t[(i + 1) * w + j + 1] + weight(i, j) {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if t[(i + 1) * w + j] >= t[i * w + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    for (k, &(i, j)) in pairs.iter().enumerate() {
        let run = (k > 0 && pairs[k - 1] == (i - 1, j - 1)) || pairs.get(k + 1) == Some(&(i + 1, j + 1));
        if run || shift(i, j) == 0 {
            out[pre + j] = Some(pre + i);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(old: &str, new: &str) -> String {
        let (o, n): (Vec<&str>, Vec<&str>) = (old.graphemes(true).collect(), new.graphemes(true).collect());
        diff(&o, &n).iter().zip(&n).map(|(m, c)| if m.is_some() { *c } else { "_" }).collect()
    }

    #[test]
    fn a_countdown_rolls_only_the_changed_digits() {
        assert_eq!(kept("0:30", "0:29"), "0:__");
        assert_eq!(kept("0:21", "0:20"), "0:2_");
        assert_eq!(kept("1:00", "0:59"), "_:__");
        assert_eq!(kept("9", "10"), "__");
    }

    #[test]
    fn prefixes_suffixes_and_runs_keep_identity_but_lone_far_matches_roll() {
        assert_eq!(kept("Copy password", "Copied, clears in 0:30"), "Cop___________________");
        assert_eq!(kept("Copied, clears in 0:30", "Copy password"), "Cop__________");
        assert_eq!(kept("12 files", "3 files"), "_ files");
        assert_eq!(kept("Save draft", "Saved draft"), "Save_ draft");
        // The middle "ab" run moves as a pair.
        assert_eq!(kept("xxabyy", "xqqqabzyy"), "x___ab_yy");
    }

    #[test]
    fn graphemes_are_one_glyph() {
        assert_eq!(kept("e\u{301}1", "e\u{301}2"), "e\u{301}_");
        assert_eq!(kept("👍🏽 1", "👍🏽 2"), "👍🏽 _");
    }

    #[test]
    fn rebuilding_the_same_text_keeps_every_key() {
        let a: Vec<u64> = keys("same", "0:30", None, 0.0).0.iter().map(|g| g.id).collect();
        let b: Vec<u64> = keys("same", "0:30", None, 0.0).0.iter().map(|g| g.id).collect();
        assert_eq!(a, b);
        let c = keys("same", "0:29", None, 0.0).0;
        assert_eq!((c[0].id, c[1].id), (a[0], a[1]));
        assert!(c[2].entered && c[3].entered && !c[0].entered);
        assert!(c[2].id > a[3] && c[3].id > a[3], "new glyphs never reuse an exiting key");
    }

    #[test]
    fn a_value_rolls_the_way_it_moves() {
        let falls = |text: &str, v: Option<f64>| keys("dir", text, v, 0.0).1;
        assert!(!falls("9s", Some(9.0)), "the first build rolls nowhere, as counting up");
        assert!(!falls("10s", Some(10.0)), "rising");
        assert!(falls("8s", Some(8.0)), "falling");
        assert!(!falls("8 s", Some(8.0)), "new text with the same value rolls by the text rule");
        assert!(!falls("8s", None), "so does a build without a value");
        assert!(!falls("-", Some(f64::NAN)) && !falls("∞", Some(f64::NEG_INFINITY)), "no direction for NaN or infinity");
        assert!(falls("7s", Some(7.0)), "and they don't replace the last value");
        assert!(!falls("12s", Some(12.0)), "rising again");
    }
}

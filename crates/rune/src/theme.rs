//! Themes as data. A theme file names a palette and, per appearance, semantic tokens that point
//! at palette entries (or literal colours) plus plain numbers. Apps resolve the tokens they use
//! once into their own typed struct; the framework bakes in no brand.
//!
//! ```json
//! { "name": "Example",
//!   "blend": { "mode": "srgb", "over": "bg" },
//!   "palette": { "grey-50": "#ffffff", "alpha-dark-10": "#0000001a" },
//!   "light": { "bg": "grey-50", "border": "alpha-dark-10" },
//!   "dark":  { "bg": "#161616",  "border": "#ffffff1a" },
//!   "numbers": { "radius-panel": 10 } }
//! ```
//!
//! `blend.mode = "srgb"`: web themes are authored for sRGB-space blending while Rune blends in
//! linear light. Each translucent token is re-solved so that, composited over the `over` token's
//! colour, it gives the pixel a browser would; over other surfaces it stays close.

use std::collections::HashMap;

use serde_json::Value;

pub use crate::color::{ Color, hex, srgb_decode, srgb_encode };

#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    pub dark: bool,
    blend_bg: Option<Color>,
    colors: HashMap<String, Color>,
    numbers: HashMap<String, f32>,
}

/// Compensate a translucent colour authored for sRGB blending, for a known opaque backdrop.
/// Input/output RGB remain linear and unchanged; only alpha is re-solved. Neutral foreground
/// and background pairs match sRGB source-over exactly (apart from output quantization).
/// Chromatic pairs use a luminance-weighted least-squares fit: one alpha cannot generally
/// match three channels. `bg.a` must be 1; HDR, translucent/unknown backdrops, overlapping
/// paints and group fades are not equivalent. Recompute from the *current authored alpha*
/// during animation: multiplying the solved alpha afterwards is not the same operation.
/// This is a CPU colour helper, with no extra GPU resources or passes.
pub fn srgb_equivalent(c: Color, bg: Color) -> Color {
    if c.a >= 0.999 || c.a <= 0.0 {
        return c;
    }
    let ch = |cv: f32, bv: f32| {
        let t = c.a * srgb_encode(cv) + (1.0 - c.a) * srgb_encode(bv);
        (srgb_decode(t), cv, bv)
    };
    let parts = [ch(c.r, bg.r), ch(c.g, bg.g), ch(c.b, bg.b)];
    let w = [0.2126, 0.7152, 0.0722];
    let (mut num, mut den) = (0.0, 0.0);
    for (i, (t, cv, bv)) in parts.into_iter().enumerate() {
        let d = cv - bv;
        num += w[i] * d * (t - bv);
        den += w[i] * d * d;
    }
    let a = if den > 1e-9 { (num / den).clamp(0.0, 1.0) } else { c.a };
    Color { a, ..c }
}

impl Theme {
    pub fn from_json(src: &str, dark: bool) -> Result<Theme, String> {
        let v: Value = serde_json::from_str(src).map_err(|e| format!("theme: {e}"))?;
        let palette = v.get("palette").and_then(Value::as_object);
        let mode = if dark { "dark" } else { "light" };
        let tokens = v.get(mode).and_then(Value::as_object).ok_or(format!("theme: no \"{mode}\" section"))?;
        let resolve = |val: &str| -> Result<Color, String> {
            if let Some(c) = hex(val) {
                return Ok(c);
            }
            let p = palette.and_then(|p| p.get(val)).and_then(Value::as_str).ok_or(format!("theme: unknown colour {val:?}"))?;
            hex(p).ok_or(format!("theme: bad hex {p:?} for {val:?}"))
        };
        let mut colors = HashMap::new();
        for (k, val) in tokens {
            let Some(s) = val.as_str() else { continue };
            colors.insert(k.clone(), resolve(s).map_err(|e| format!("{e} (token {k})"))?);
        }
        if let Some(p) = palette {
            for (k, val) in p {
                if let (false, Some(c)) = (colors.contains_key(k), val.as_str().and_then(hex)) {
                    colors.insert(k.clone(), c);
                }
            }
        }
        let blend = v.get("blend");
        let mut blend_bg = None;
        if blend.and_then(|b| b.get("mode")).and_then(Value::as_str) == Some("srgb") {
            let over = blend.and_then(|b| b.get("over")).and_then(Value::as_str).unwrap_or("");
            if let Some(&bg) = colors.get(over) {
                blend_bg = Some(bg);
                for c in colors.values_mut() {
                    *c = srgb_equivalent(*c, bg);
                }
            }
        }
        let mut numbers = HashMap::new();
        if let Some(n) = v.get("numbers").and_then(Value::as_object) {
            for (k, val) in n {
                if let Some(f) = val.as_f64() {
                    numbers.insert(k.clone(), f as f32);
                }
            }
        }
        let name = v.get("name").and_then(Value::as_str).unwrap_or("theme").to_owned();
        Ok(Theme { name, dark, blend_bg, colors, numbers })
    }

    pub fn try_color(&self, name: &str) -> Option<Color> {
        self.colors.get(name).copied()
    }

    /// A colour token. Panics naming the closest tokens when it is missing.
    #[track_caller]
    pub fn color(&self, name: &str) -> Color {
        self.try_color(name).unwrap_or_else(|| {
            let mut near: Vec<&String> =
                self.colors.keys().filter(|k| k.contains(name.split('-').next_back().unwrap_or(name))).collect();
            near.sort();
            near.truncate(6);
            panic!("theme {:?} has no colour {name:?}; similar: {near:?}", self.name)
        })
    }

    /// `c` at opacity `a`, re-solved for linear-light blending when the theme blends in sRGB.
    pub fn alpha(&self, c: Color, a: f32) -> Color {
        let ca = c.alpha(a);
        self.blend_bg.map_or(ca, |bg| srgb_equivalent(ca, bg))
    }

    pub fn num(&self, name: &str) -> f32 {
        *self.numbers.get(name).unwrap_or_else(|| panic!("theme {:?} has no number {name:?}", self.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_overlay_matches_browser_pixel() {
        let bg = hex("#161616").unwrap();
        let c = srgb_equivalent(hex("#ffffff0f").unwrap(), bg);
        // Browser: a·255 + (1 − a)·22 in sRGB bytes, a = 15/255.
        let a = 15.0 / 255.0;
        let want = srgb_decode((a * 255.0 + (1.0 - a) * 22.0) / 255.0);
        let got = c.a * c.r + (1.0 - c.a) * bg.r;
        assert!((got - want).abs() < 1e-4, "{got} vs {want}");
    }
}

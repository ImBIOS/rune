//! Colour in linear sRGB with straight alpha. Authored in OKLCH, animated in Oklab.

pub use rune_motion::{srgb_decode, srgb_encode};
use rune_motion::{linear_to_oklab, oklab_to_linear};

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

pub fn oklch(l: f32, c: f32, h: f32) -> Color {
    oklcha(l, c, h, 1.0)
}

pub fn oklcha(l: f32, c: f32, h: f32, a: f32) -> Color {
    let (s, co) = h.to_radians().sin_cos();
    Color::from_oklab([l, c * co, c * s, a])
}

impl Color {
    pub const TRANSPARENT: Color = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
    pub const WHITE: Color = Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };
    pub const BLACK: Color = Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };

    pub fn alpha(self, a: f32) -> Color {
        Color { a, ..self }
    }

    /// `t` of the way from `self` to `other`, straight in linear light.
    pub fn mix(self, other: Color, t: f32) -> Color {
        let m = |a: f32, b: f32| a + (b - a) * t;
        Color { r: m(self.r, other.r), g: m(self.g, other.g), b: m(self.b, other.b), a: m(self.a, other.a) }
    }

    pub fn to_oklab(self) -> [f32; 4] {
        let [l, a, b] = linear_to_oklab(self.r, self.g, self.b);
        [l, a, b, self.a]
    }

    pub fn from_oklab([ll, aa, bb, alpha]: [f32; 4]) -> Color {
        let [r, g, b] = oklab_to_linear([ll, aa, bb]).map(|c| c.clamp(0.0, 1.0));
        Color { r, g, b, a: alpha.clamp(0.0, 1.0) }
    }

    /// Premultiplied linear RGBA.
    pub fn premul(self, opacity: f32) -> [f32; 4] {
        let a = self.a * opacity;
        [self.r * a, self.g * a, self.b * a, a]
    }

    /// Premultiplied sRGB-encoded RGBA, for gamma-space (`srgb_blend`) targets.
    pub fn premul_srgb(self, opacity: f32) -> [f32; 4] {
        let a = self.a * opacity;
        [srgb_encode(self.r) * a, srgb_encode(self.g) * a, srgb_encode(self.b) * a, a]
    }

    pub fn luminance(self) -> f32 {
        0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b
    }

    /// `#rrggbbaa` in sRGB, for trees and traces.
    pub fn hex(self) -> String {
        let enc = |c: f32| (srgb_encode(c) * 255.0).round() as u8;
        format!("#{:02x}{:02x}{:02x}{:02x}", enc(self.r), enc(self.g), enc(self.b), (self.a * 255.0).round() as u8)
    }
}

/// `#rgb`, `#rrggbb` or `#rrggbbaa` (sRGB) to linear [`Color`].
pub fn hex(s: &str) -> Option<Color> {
    let h = s.strip_prefix('#')?;
    let p = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok().map(|v| v as f32 / 255.0);
    let (r, g, b, a) = match h.len() {
        3 => {
            let q = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).ok().map(|v| (v * 17) as f32 / 255.0);
            (q(0)?, q(1)?, q(2)?, 1.0)
        }
        6 => (p(0)?, p(2)?, p(4)?, 1.0),
        8 => (p(0)?, p(2)?, p(4)?, p(6)?),
        _ => return None,
    };
    Some(Color { r: srgb_decode(r), g: srgb_decode(g), b: srgb_decode(b), a })
}

/// Source-over of premultiplied `top` onto premultiplied `bottom`.
pub fn over(top: [f32; 4], bottom: [f32; 4]) -> [f32; 4] {
    let k = 1.0 - top[3];
    [top[0] + bottom[0] * k, top[1] + bottom[1] * k, top[2] + bottom[2] * k, top[3] + bottom[3] * k]
}

impl rune_motion::Animatable for Color {
    const LANES: usize = 4;
    fn write_lanes(&self, out: &mut [f32]) {
        let [l, a, b, al] = self.to_oklab();
        out[..4].copy_from_slice(&[l * al, a * al, b * al, al]);
    }
    fn read_lanes(x: &[f32]) -> Self {
        let al = x[3].clamp(0.0, 1.0);
        if al <= 1e-6 {
            return Color::TRANSPARENT;
        }
        Color::from_oklab([x[0] / al, x[1] / al, x[2] / al, al])
    }
}

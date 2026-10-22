//! Colour in linear sRGB with straight alpha. Authored in OKLCH, animated in Oklab.

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

    pub fn to_oklab(self) -> [f32; 4] {
        let Color { r, g, b, a } = self;
        let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
        let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
        let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
        [
            0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
            1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
            0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
            a,
        ]
    }

    pub fn from_oklab([ll, aa, bb, alpha]: [f32; 4]) -> Color {
        let l = ll + 0.396_337_78 * aa + 0.215_803_76 * bb;
        let m = ll - 0.105_561_346 * aa - 0.063_854_17 * bb;
        let s = ll - 0.089_484_18 * aa - 1.291_485_5 * bb;
        let (l, m, s) = (l * l * l, m * m * m, s * s * s);
        Color {
            r: (4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s).clamp(0.0, 1.0),
            g: (-1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s).clamp(0.0, 1.0),
            b: (-0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s).clamp(0.0, 1.0),
            a: alpha.clamp(0.0, 1.0),
        }
    }

    /// Premultiplied linear RGBA.
    pub fn premul(self, opacity: f32) -> [f32; 4] {
        let a = self.a * opacity;
        [self.r * a, self.g * a, self.b * a, a]
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

pub fn srgb_encode(c: f32) -> f32 {
    if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

pub fn srgb_decode(c: f32) -> f32 {
    if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
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

//! The fill catalog: each fill style's id, display name, params (with ranges and units) and named
//! presets, plus generic get/set by param id and Rust emission. A tool can build its style list,
//! inspector and code output from this instead of keeping its own schema.
//!
//! Param ids are the Rust field names, so `FillParams::to_rust` can write struct literals.

use crate::*;

/// How a number reads in an inspector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    None,
    /// Points.
    Pt,
    /// Radians (show as degrees).
    Radians,
    /// 0..1 of something (show as a percentage).
    Fraction,
    /// Per second (speeds, rates).
    PerSecond,
}

/// One option of a choice param.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Choice {
    pub id: &'static str,
    pub name: &'static str,
    /// The Rust path of the variant, for emitted code.
    pub rust: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamKind {
    /// A float; `step` is a sensible slider step.
    Number { min: f32, max: f32, step: f32, unit: Unit },
    /// A whole number.
    Count { min: u32, max: u32 },
    Toggle,
    Choice(&'static [Choice]),
    Color,
    /// An ordered colour list (`Stops`).
    Colors { min: usize, max: usize },
    /// A point in the shape's rect, 0..1 on each axis.
    Point,
    /// Up to `max` points in the shape's rect (one per colour of a `Colors` param).
    Points { max: usize },
}

/// A tunable param. Its default is whatever the style's `Default` (or a preset) holds: read it
/// with `FillParams::get`.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct Param {
    /// The Rust field name.
    pub id: &'static str,
    pub name: &'static str,
    pub kind: ParamKind,
}

/// A param's value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    Number(f32),
    Count(u32),
    Toggle(bool),
    /// Index into the param's `Choice` list.
    Choice(usize),
    Color(Rgb),
    Colors(Stops),
    Point([f32; 2]),
    Points([[f32; 2]; MAX_STOPS]),
}

/// A named starting point for a style.
#[derive(Clone, Copy)]
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    /// The Rust expression that builds it, for emitted code.
    pub rust: &'static str,
    pub(crate) make: fn() -> FillStyle,
}

impl Preset {
    pub fn style(&self) -> FillStyle {
        (self.make)()
    }
    pub fn fill(&self) -> FillParams {
        FillParams::new(self.style())
    }
}

impl std::fmt::Debug for Preset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Preset").field("id", &self.id).finish()
    }
}

/// A fill style as a catalog entry.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct StyleInfo {
    /// Stable id, snake_case (`"liquid_metal"`).
    pub id: &'static str,
    pub name: &'static str,
    pub summary: &'static str,
    /// The params type, for emitted code.
    pub rust: &'static str,
    /// The style's own params, in inspector order. `FillParams::COMMON` applies to every style.
    pub params: &'static [Param],
    /// Named presets; the first is the style's `Default`.
    pub presets: &'static [Preset],
    /// Whether the style may read the backdrop (always for glass; for some inputs otherwise).
    /// `Fx::needs_backdrop` gives the answer for a given set of params.
    pub reads_backdrop: bool,
}

impl StyleInfo {
    pub fn preset(&self, id: &str) -> Option<&'static Preset> {
        self.presets.iter().find(|p| p.id == id)
    }
    /// The style with its default params.
    pub fn default_fill(&self) -> FillParams {
        self.presets[0].fill()
    }
}

/// Every fill style, in catalog order.
pub fn fill_styles() -> &'static [StyleInfo] {
    FILL_STYLES
}

pub fn fill_style(id: &str) -> Option<&'static StyleInfo> {
    FILL_STYLES.iter().find(|s| s.id == id)
}

// ---------------------------------------------------------------- values

pub(crate) trait ParamValue: Sized {
    fn to_value(&self) -> Value;
    fn from_value(v: Value) -> Option<Self>;
    fn rust(&self) -> String;
}

fn float(v: f32) -> String {
    format!("{v:?}")
}

impl ParamValue for f32 {
    fn to_value(&self) -> Value {
        Value::Number(*self)
    }
    fn from_value(v: Value) -> Option<Self> {
        match v {
            Value::Number(x) => Some(x),
            Value::Count(n) => Some(n as f32),
            _ => None,
        }
    }
    fn rust(&self) -> String {
        float(*self)
    }
}

impl ParamValue for u32 {
    fn to_value(&self) -> Value {
        Value::Count(*self)
    }
    fn from_value(v: Value) -> Option<Self> {
        match v {
            Value::Count(n) => Some(n),
            Value::Number(x) if x.is_finite() => Some(x.round().max(0.0) as u32),
            _ => None,
        }
    }
    fn rust(&self) -> String {
        self.to_string()
    }
}

impl ParamValue for bool {
    fn to_value(&self) -> Value {
        Value::Toggle(*self)
    }
    fn from_value(v: Value) -> Option<Self> {
        match v {
            Value::Toggle(b) => Some(b),
            _ => None,
        }
    }
    fn rust(&self) -> String {
        self.to_string()
    }
}

impl Rgb {
    /// To sRGB hex (0xRRGGBB), rounding.
    pub fn hex(self) -> u32 {
        let e = |c: f32| (rune_motion::srgb_encode(c.clamp(0.0, 1.0)) * 255.0).round() as u32;
        (e(self.0[0]) << 16) | (e(self.0[1]) << 8) | e(self.0[2])
    }
}

impl ParamValue for Rgb {
    fn to_value(&self) -> Value {
        Value::Color(*self)
    }
    fn from_value(v: Value) -> Option<Self> {
        match v {
            Value::Color(c) => Some(c),
            _ => None,
        }
    }
    fn rust(&self) -> String {
        format!("rgb(0x{:06x})", self.hex())
    }
}

impl ParamValue for Stops {
    fn to_value(&self) -> Value {
        Value::Colors(*self)
    }
    fn from_value(v: Value) -> Option<Self> {
        match v {
            Value::Colors(s) => Some(s),
            _ => None,
        }
    }
    fn rust(&self) -> String {
        let hex: Vec<String> = self.as_slice().iter().map(|c| format!("0x{:06x}", c.hex())).collect();
        format!("Stops::hex(&[{}])", hex.join(", "))
    }
}

impl ParamValue for [f32; 2] {
    fn to_value(&self) -> Value {
        Value::Point(*self)
    }
    fn from_value(v: Value) -> Option<Self> {
        match v {
            Value::Point(p) => Some(p),
            _ => None,
        }
    }
    fn rust(&self) -> String {
        format!("[{}, {}]", float(self[0]), float(self[1]))
    }
}

impl ParamValue for [[f32; 2]; MAX_STOPS] {
    fn to_value(&self) -> Value {
        Value::Points(*self)
    }
    fn from_value(v: Value) -> Option<Self> {
        match v {
            Value::Points(p) => Some(p),
            _ => None,
        }
    }
    fn rust(&self) -> String {
        let v: Vec<String> = self.iter().map(|p| p.rust()).collect();
        format!("[{}]", v.join(", "))
    }
}

/// Choice params: an enum with a fixed list of options.
macro_rules! choices {
    ($ty:ident { $($v:ident = $id:literal $name:literal),* $(,)? }) => {
        impl $ty {
            pub const CHOICES: &'static [Choice] = &[$(Choice { id: $id, name: $name, rust: concat!(stringify!($ty), "::", stringify!($v)) }),*];
            const ALL: &'static [$ty] = &[$($ty::$v),*];
        }
        impl ParamValue for $ty {
            fn to_value(&self) -> Value {
                Value::Choice($ty::ALL.iter().position(|x| x == self).unwrap_or(0))
            }
            fn from_value(v: Value) -> Option<Self> {
                match v {
                    Value::Choice(i) => $ty::ALL.get(i).copied(),
                    _ => None,
                }
            }
            fn rust(&self) -> String {
                $ty::CHOICES[$ty::ALL.iter().position(|x| x == self).unwrap_or(0)].rust.to_string()
            }
        }
    };
}
pub(crate) use choices;

/// A params struct's table: `PARAMS` plus get/set/emit by field name.
macro_rules! params {
    ($ty:ident { $($field:ident : $name:literal => $kind:expr),* $(,)? }) => {
        impl $ty {
            pub const PARAMS: &'static [Param] = &[$(Param { id: stringify!($field), name: $name, kind: $kind }),*];
            pub(crate) fn get_param(&self, id: &str) -> Option<Value> {
                match id {
                    $(stringify!($field) => Some(ParamValue::to_value(&self.$field)),)*
                    _ => None,
                }
            }
            pub(crate) fn set_param(&mut self, id: &str, v: Value) -> bool {
                match id {
                    $(stringify!($field) => match ParamValue::from_value(v) {
                        Some(x) => { self.$field = x; true }
                        None => false,
                    },)*
                    _ => false,
                }
            }
            pub(crate) fn rust_field(&self, id: &str) -> Option<String> {
                match id {
                    $(stringify!($field) => Some(ParamValue::rust(&self.$field)),)*
                    _ => None,
                }
            }
        }
    };
}
pub(crate) use params;

pub(crate) const fn num(min: f32, max: f32, step: f32, unit: Unit) -> ParamKind {
    ParamKind::Number { min, max, step, unit }
}

// ---------------------------------------------------------------- FillParams by id

impl FillParams {
    /// Params every fill style has.
    pub const COMMON: &'static [Param] = &[
        Param { id: "border", name: "Border", kind: num(0.0, 32.0, 0.5, Unit::Pt) },
        Param { id: "intensity", name: "Intensity", kind: num(0.0, 1.0, 0.01, Unit::Fraction) },
    ];

    /// This style's catalog entry.
    pub fn info(&self) -> &'static StyleInfo {
        &FILL_STYLES[self.style.index()]
    }

    /// A param's value, common or the style's own, by id.
    pub fn get(&self, id: &str) -> Option<Value> {
        match id {
            "border" => Some(Value::Number(self.border)),
            "intensity" => Some(Value::Number(self.intensity)),
            _ => self.style.get_param(id),
        }
    }

    /// Set a param by id. False when the id is unknown or the value has the wrong kind.
    pub fn set(&mut self, id: &str, v: Value) -> bool {
        match (id, f32::from_value(v)) {
            ("border", Some(x)) => self.border = x,
            ("intensity", Some(x)) => self.intensity = x,
            ("border" | "intensity", None) => return false,
            _ => return self.style.set_param(id, v),
        }
        true
    }

    /// Rust that rebuilds these params: `Fx::fill(...)`, written against the closest preset.
    pub fn to_rust(&self) -> String {
        let info = self.info();
        let mut best: Option<(usize, &Preset, Vec<String>)> = None;
        for p in info.presets {
            let base = p.style();
            let diff: Vec<String> = info
                .params
                .iter()
                .filter(|q| base.get_param(q.id) != self.style.get_param(q.id))
                .filter_map(|q| self.style.rust_field(q.id).map(|v| format!("{}: {v}", q.id)))
                .collect();
            if best.as_ref().is_none_or(|b| diff.len() < b.0) {
                best = Some((diff.len(), p, diff));
            }
        }
        let (_, preset, diff) = best.expect("every style has a preset");
        let mut s = if diff.is_empty() { preset.rust.to_string() } else { format!("{} {{ {}, ..{} }}", info.rust, diff.join(", "), preset.rust) };
        if self.border != 0.0 {
            s += &format!(".border({})", float(self.border));
        }
        if self.intensity != 1.0 {
            s += &format!(".intensity({})", float(self.intensity));
        }
        format!("Fx::fill({s})")
    }
}

// ---------------------------------------------------------------- the table

macro_rules! presets {
    ($style:ident($ty:ident) { $($id:literal $name:literal => $f:ident),* $(,)? }) => {
        &[$(Preset { id: $id, name: $name, rust: concat!(stringify!($ty), "::", stringify!($f), "()"), make: || FillStyle::$style($ty::$f()) }),*]
    };
}

pub(crate) static FILL_STYLES: &[StyleInfo] = &[
    StyleInfo {
        id: "liquid_metal",
        name: "Liquid metal",
        summary: "Polished metal reflecting a studio, flowing like liquid. Works as a fill or a tube-like border.",
        rust: "LiquidMetalParams",
        params: LiquidMetalParams::PARAMS,
        presets: presets!(LiquidMetal(LiquidMetalParams) { "chrome" "Chrome" => chrome, "gold" "Gold" => gold, "rose" "Rose gold" => rose, "gunmetal" "Gunmetal" => gunmetal }),
        reads_backdrop: false,
    },
    StyleInfo {
        id: "mesh_gradient",
        name: "Mesh gradient",
        summary: "Up to six colours pinned to slowly drifting points, blended in OKLab.",
        rust: "MeshGradientParams",
        params: MeshGradientParams::PARAMS,
        presets: presets!(MeshGradient(MeshGradientParams) { "dusk" "Dusk" => dusk, "lagoon" "Lagoon" => lagoon, "orchid" "Orchid" => orchid, "graphite" "Graphite" => graphite }),
        reads_backdrop: false,
    },
    StyleInfo {
        id: "dither",
        name: "Dither",
        summary: "Bayer or blue-noise dithering of a ramp, noise, the backdrop or an image into a palette.",
        rust: "DitherParams",
        params: DitherParams::PARAMS,
        presets: presets!(Dither(DitherParams) { "bayer" "Bayer" => bayer, "handheld" "Handheld" => handheld, "grain" "Blue noise" => grain, "backdrop" "Backdrop" => backdrop }),
        reads_backdrop: true,
    },
    StyleInfo {
        id: "grain_gradient",
        name: "Grain gradient",
        summary: "A linear, radial or conic gradient scattered into film grain.",
        rust: "GrainGradientParams",
        params: GrainGradientParams::PARAMS,
        presets: presets!(GrainGradient(GrainGradientParams) { "ember" "Ember" => ember, "halo" "Halo" => halo, "sweep" "Sweep" => sweep }),
        reads_backdrop: false,
    },
    StyleInfo {
        id: "fluted_glass",
        name: "Fluted glass",
        summary: "Reeded glass over the backdrop: each rib shows a squeezed, flipped strip of what is behind.",
        rust: "FlutedGlassParams",
        params: FlutedGlassParams::PARAMS,
        presets: presets!(FlutedGlass(FlutedGlassParams) { "reeded" "Reeded" => reeded, "fine" "Fine" => fine, "frosted" "Frosted" => frosted, "wide" "Wide" => wide }),
        reads_backdrop: true,
    },
    StyleInfo {
        id: "metaballs",
        name: "Metaballs",
        summary: "Glossy blobs that drift and merge.",
        rust: "MetaballParams",
        params: MetaballParams::PARAMS,
        presets: presets!(Metaballs(MetaballParams) { "lava" "Lava" => lava, "mint" "Mint" => mint }),
        reads_backdrop: false,
    },
    StyleInfo {
        id: "swirl",
        name: "Swirl",
        summary: "Colours stirred into a slow swirl.",
        rust: "SwirlParams",
        params: SwirlParams::PARAMS,
        presets: presets!(Swirl(SwirlParams) { "ink" "Ink" => ink, "marble" "Marble" => marble }),
        reads_backdrop: false,
    },
    StyleInfo {
        id: "voronoi",
        name: "Voronoi",
        summary: "Cells around drifting seed points, with seams.",
        rust: "VoronoiParams",
        params: VoronoiParams::PARAMS,
        presets: presets!(Voronoi(VoronoiParams) { "cells" "Cells" => cells, "stained" "Stained glass" => stained }),
        reads_backdrop: false,
    },
    StyleInfo {
        id: "waves",
        name: "Waves",
        summary: "Layered bands of colour with rolling edges.",
        rust: "WaveParams",
        params: WaveParams::PARAMS,
        presets: presets!(Waves(WaveParams) { "sea" "Sea" => sea, "dunes" "Dunes" => dunes }),
        reads_backdrop: false,
    },
    StyleInfo {
        id: "halftone",
        name: "Halftone",
        summary: "Print dots on a rotated screen, sized by a ramp, noise, the backdrop or an image.",
        rust: "HalftoneParams",
        params: HalftoneParams::PARAMS,
        presets: presets!(Halftone(HalftoneParams) { "print" "Print" => print, "backdrop" "Backdrop" => backdrop }),
        reads_backdrop: true,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_are_in_variant_order_with_unique_ids() {
        for (k, s) in fill_styles().iter().enumerate() {
            assert_eq!(s.default_fill().style.index(), k, "{}", s.id);
            assert_eq!(s.default_fill().info().id, s.id);
            assert_eq!(fill_style(s.id).map(|x| x.id), Some(s.id));
            assert!(!s.presets.is_empty() && !s.params.is_empty());
            let mut ids: Vec<_> = s.params.iter().map(|p| p.id).chain(FillParams::COMMON.iter().map(|p| p.id)).collect();
            ids.sort();
            ids.dedup();
            assert_eq!(ids.len(), s.params.len() + FillParams::COMMON.len(), "{}: unique param ids", s.id);
            let mut ps: Vec<_> = s.presets.iter().map(|p| p.id).collect();
            ps.sort();
            ps.dedup();
            assert_eq!(ps.len(), s.presets.len());
        }
        let mut ids: Vec<_> = fill_styles().iter().map(|s| s.id).collect();
        ids.dedup();
        assert_eq!(ids.len(), fill_styles().len());
        assert!(fill_style("nope").is_none());
    }

    #[test]
    fn first_preset_is_the_default() {
        let d = [
            FillStyle::from(LiquidMetalParams::default()),
            MeshGradientParams::default().into(),
            DitherParams::default().into(),
            GrainGradientParams::default().into(),
            FlutedGlassParams::default().into(),
            MetaballParams::default().into(),
            SwirlParams::default().into(),
            VoronoiParams::default().into(),
            WaveParams::default().into(),
            HalftoneParams::default().into(),
        ];
        assert_eq!(d.len(), fill_styles().len());
        for (s, d) in fill_styles().iter().zip(d) {
            assert_eq!(s.presets[0].style(), d, "{}", s.id);
        }
    }

    #[test]
    fn every_param_gets_sets_and_sits_in_range() {
        for s in fill_styles() {
            for pr in s.presets {
                let f = pr.fill();
                for p in s.params.iter().chain(FillParams::COMMON) {
                    let v = f.get(p.id).unwrap_or_else(|| panic!("{}.{} reads", s.id, p.id));
                    match (p.kind, v) {
                        (ParamKind::Number { min, max, .. }, Value::Number(x)) => assert!((min..=max).contains(&x), "{}/{}.{} = {x} outside {min}..{max}", s.id, pr.id, p.id),
                        (ParamKind::Count { min, max }, Value::Count(n)) => assert!((min..=max).contains(&n), "{}.{}", s.id, p.id),
                        (ParamKind::Choice(c), Value::Choice(k)) => assert!(k < c.len()),
                        (ParamKind::Colors { min, max }, Value::Colors(c)) => assert!((min..=max).contains(&c.len()), "{}/{}.{}", s.id, pr.id, p.id),
                        (ParamKind::Toggle, Value::Toggle(_)) | (ParamKind::Color, Value::Color(_)) | (ParamKind::Point, Value::Point(_)) | (ParamKind::Points { .. }, Value::Points(_)) => {}
                        (k, v) => panic!("{}.{}: {k:?} holds {v:?}", s.id, p.id),
                    }
                    let mut g = f;
                    assert!(g.set(p.id, v), "{}.{} writes", s.id, p.id);
                    assert_eq!(g, f);
                    assert!(!g.set(p.id, Value::Point([9.0, 9.0])) || matches!(p.kind, ParamKind::Point), "{}.{} rejects the wrong kind", s.id, p.id);
                }
                assert!(f.get("nope").is_none());
            }
        }
    }

    #[test]
    fn changing_a_param_changes_the_fill() {
        let mut f = Fx::fill(LiquidMetalParams::gold());
        let Fx::Fill(p) = &mut f else { unreachable!() };
        assert!(p.set("flow", Value::Number(0.9)));
        assert!(p.set("tint", Value::Color(rgb(0x808080))));
        assert!(p.set("border", Value::Number(3.0)));
        let FillStyle::LiquidMetal(m) = p.style else { unreachable!() };
        assert_eq!(m.flow, 0.9);
        assert_eq!(p.border, 3.0);
        assert!(!p.set("pattern", Value::Choice(0)), "not a liquid metal param");
        let mut d = FillParams::from(DitherParams::bayer());
        assert!(d.set("pattern", Value::Choice(3)));
        let FillStyle::Dither(dp) = d.style else { unreachable!() };
        assert_eq!(dp.pattern, DitherPattern::BlueNoise);
    }

    #[test]
    fn rust_is_written_against_the_closest_preset() {
        assert_eq!(FillParams::from(LiquidMetalParams::gold()).to_rust(), "Fx::fill(LiquidMetalParams::gold())");
        let f = FillParams::from(LiquidMetalParams { flow: 0.8, ..LiquidMetalParams::rose() }).border(4.0);
        assert_eq!(f.to_rust(), "Fx::fill(LiquidMetalParams { flow: 0.8, ..LiquidMetalParams::rose() }.border(4.0))");
        let d = FillParams::from(DitherParams { pattern: DitherPattern::Bayer8, colors: Stops::hex(&[0x000000, 0xffffff]), ..DitherParams::bayer() });
        assert_eq!(d.to_rust(), "Fx::fill(DitherParams { pattern: DitherPattern::Bayer8, colors: Stops::hex(&[0x000000, 0xffffff]), ..DitherParams::bayer() })");
        for s in fill_styles() {
            for p in s.presets {
                assert_eq!(p.fill().to_rust(), format!("Fx::fill({})", p.rust));
            }
        }
    }

    #[test]
    fn colours_round_trip_through_hex() {
        for h in [0x000000, 0xffffff, 0xf3c56b, 0x0b3d5c, 0x808080] {
            assert_eq!(rgb(h).hex(), h);
        }
    }
}

//! The runtime's own animated properties beside `rune_motion::prop`: their ids, trace names and settle time.
use super::*;

pub(super) const P_FILL: Prop<Color> = Prop::custom(64, PropClass::Color);
pub(super) const P_BORDER: Prop<Color> = Prop::custom(65, PropClass::Color);
pub(super) const P_TEXT: Prop<Color> = Prop::custom(66, PropClass::Color);
pub(super) const P_HOVER_C: Prop<Color> = Prop::custom(67, PropClass::Color);
pub(super) const P_PRESS_C: Prop<Color> = Prop::custom(68, PropClass::Color);
pub(super) const P_SHADOWS: [Prop<Color>; 4] = [
    Prop::custom(69, PropClass::Color),
    Prop::custom(70, PropClass::Color),
    Prop::custom(71, PropClass::Color),
    Prop::custom(72, PropClass::Color),
];
pub(super) const P_HOVER: Prop<f32> = Prop::custom(73, PropClass::Opacity);
pub(super) const P_PRESS: Prop<f32> = Prop::custom(74, PropClass::Opacity);
pub(super) const P_GRADIENT: [Prop<Color>; 2] = [Prop::custom(75, PropClass::Color), Prop::custom(76, PropClass::Color)];
pub(super) const P_OUTLINE: Prop<Color> = Prop::custom(77, PropClass::Color);
pub(super) const P_BORDER_BOTTOM: Prop<Color> = Prop::custom(78, PropClass::Color);
/// `El::press_scale`'s presented factor, multiplied into the element's own scale.
pub(super) const P_PRESS_SCALE: Prop<f32> = Prop::custom(79, PropClass::Scale);
pub(super) const P_RADIAL: [Prop<Color>; 8] = [
    Prop::custom(80, PropClass::Color),
    Prop::custom(81, PropClass::Color),
    Prop::custom(82, PropClass::Color),
    Prop::custom(83, PropClass::Color),
    Prop::custom(84, PropClass::Color),
    Prop::custom(85, PropClass::Color),
    Prop::custom(86, PropClass::Color),
    Prop::custom(87, PropClass::Color),
];
/// A property's name in traces.
pub(super) fn prop_name(id: u16) -> &'static str {
    const BUILTIN: [&str; 12] = ["opacity", "frame", "offset", "scale", "rotation", "blur", "radius", "fill", "mask", "scroll", "presence", "travel"];
    match id {
        0..12 => BUILTIN[id as usize],
        64 => "fill-color",
        65 => "border-color",
        66 => "text-color",
        67 => "hover-color",
        68 => "press-color",
        69..=72 => "shadow-color",
        73 => "hover",
        74 => "press",
        75 | 76 => "gradient-color",
        77 => "outline-color",
        78 => "border-bottom-color",
        79 => "press-scale",
        80..=87 => "radial-color",
        88 => "icon-turn",
        89 => "scale",
        90..=93 => "shadow",
        _ => "custom",
    }
}

/// `El::icon_rotation`'s presented turn, in radians.
pub(super) const P_ICON_TURN: Prop<f32> = Prop::custom(88, PropClass::Rotation);
/// `El::scale`'s presented factor. `prop::SCALE` belongs to `live_scale`; they multiply.
pub(super) const P_SCALE: Prop<f32> = Prop::custom(89, PropClass::Scale);
/// The first four shadows' geometry: x, y, blur and spread in a rect's lanes.
pub(super) const P_SHADOW_GEO: [Prop<rune_motion::Rect>; 4] = [
    Prop::custom(90, PropClass::Shape),
    Prop::custom(91, PropClass::Shape),
    Prop::custom(92, PropClass::Shape),
    Prop::custom(93, PropClass::Shape),
];
/// Whether a shadow colour or geometry spring, or an icon's turn, is moving node `id`.
pub(super) fn paint_pose_springing(store: &rune_motion::AnimationStore, id: NodeId) -> bool {
    store.is_animating(id, P_ICON_TURN)
        || P_SHADOWS.iter().any(|&p| store.is_animating(id, p))
        || P_SHADOW_GEO.iter().any(|&p| store.is_animating(id, p))
}
/// Seconds a changed `El::scale` holds still before its text rasterizes at that size.
pub(super) const STATIC_SCALE_SETTLE: f64 = 0.1;

use rune::{Align, El, Family, OpticalSize, TextStyle, text};

// Exactly TextStyle's public fields. Deliberately exhaustive: alignment is an element builder,
// not a style field. Consumers build styles with `TextStyle::new` and `..` (checked for `italic`).
const LEGACY_STYLE: TextStyle = TextStyle {
    size: 13.0,
    weight: 400.0,
    family: Family::Sans,
    line_height: 0.0,
    tracking: 0.0,
    variations: "",
    features: "",
    optical_size: OpticalSize::Auto,
    italic: false,
};

#[test]
fn legacy_style_literal_and_align_builders_compile_beside_justify() {
    let _: El = text("legacy").font(LEGACY_STYLE).align(Align::Center);
    let _: El = text("wrapped").font(LEGACY_STYLE).wrap().align(Align::End).justify();
    let _: El = text("input").single_line().align(Align::Start);
}

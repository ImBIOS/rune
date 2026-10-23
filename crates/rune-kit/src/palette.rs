//! A compact starting palette for apps that don't carry a full semantic token set.

use rune::{Color, Shadow};

use crate::{Kit, Palette, StateColors};

/// Three opaque theme colours from which [`Kit::from_palette`] derives its semantic tokens.
/// Typography, radii, motion and individual colours remain editable on the resulting kit.
#[derive(Clone, Copy, Debug)]
pub struct PaletteSeed {
    pub bg: Color,
    pub fg: Color,
    pub accent: Color,
}

impl Kit {
    /// Derives surfaces, text, borders, states and elevation from three theme colours.
    /// Appearance is dark when `fg` is lighter than `bg`. Colours mix in linear light;
    /// translucent hover/press tokens use Rune's native blending (`blend_over: None`).
    ///
    /// ```
    /// use rune::{Color, color::hex};
    /// use rune_kit::{Kit, PaletteSeed};
    /// let kit = Kit::from_palette(PaletteSeed {
    ///     bg: Color::BLACK,
    ///     fg: Color::WHITE,
    ///     accent: hex("#e53935").unwrap(),
    /// });
    /// ```
    pub fn from_palette(seed: PaletteSeed) -> Kit {
        let PaletteSeed { bg, fg, accent } = seed;
        let dark = fg.luminance() > bg.luminance();
        let mut kit = Kit::neutral(dark);
        let layer = |amount| bg.mix(fg, amount);
        let muted = layer(0.60);
        let faint = layer(0.40);
        let border = layer(0.16);
        let raised = layer(0.035);
        let hover = fg.alpha(0.06);
        let pressed = fg.alpha(0.12);
        let state =
            |role: StateColors| StateColors { bg: bg.mix(role.fg, 0.10), border: bg.mix(role.fg, 0.30), ..role };
        kit.c = Palette {
            page: bg,
            bg_base: bg,
            bg_deep: bg,
            layer_01: raised,
            layer_02: layer(0.06),
            layer_03: layer(0.10),
            layer_04: layer(0.16),
            bg_inverse: fg,
            bg_contrast: fg,
            bg_button_neutral: raised,
            bg_icon_button_contrast: fg,
            bg_accent: accent,
            submit_highlight: Color::WHITE.alpha(0.20),
            submit_hover: bg.alpha(0.12),
            submit_fg: bg,
            submit_icon: bg,
            text_base: fg,
            text_muted: muted,
            text_faint: faint,
            text_inverse: bg,
            text_contrast: bg,
            text_accent: accent,
            icon_base: fg,
            icon_muted: muted,
            icon_faint: faint,
            icon_inverse: bg,
            icon_contrast: bg,
            icon_accent: accent,
            border_muted: layer(0.10),
            border_base: border,
            border_strong: layer(0.26),
            border_focus: accent,
            hover,
            pressed,
            contrast_hover: bg.alpha(0.10),
            contrast_pressed: bg.alpha(0.20),
            scrim: Color::BLACK.alpha(0.30),
            depth_top: Color::WHITE,
            depth_bottom: Color::WHITE.alpha(0.0),
            white: Color::WHITE,
            black: Color::BLACK,
            knob: fg,
            knob_on: fg,
            success: state(kit.c.success),
            warning: state(kit.c.warning),
            danger: state(kit.c.danger),
            info: state(kit.c.info),
            surface_base: layer(0.06),
            surface_weak: raised,
            surface_hover: hover,
            surface_raised: raised,
            surface_raised_base: raised,
            surface_raised_hover: layer(0.10),
            surface_float: fg,
            surface_disabled: layer(0.06),
            border_weak: layer(0.10),
            border_hover: layer(0.26),
            border_active: accent,
            border_disabled: layer(0.10),
            text_strong: fg,
            text_weak: muted,
            text_invert_strong: bg,
            icon_weaker: faint,
            std_border: border,
            std_icon: muted,
            std_icon_invert: bg,
            std_icon_strong: fg,
            std_text: fg,
        };
        // Keep neutral elevation geometry, but use this palette's rings and black shadows.
        let recolor = |shadows: &mut Vec<Shadow>| {
            for shadow in shadows {
                shadow.color =
                    if shadow.blur == 0.0 { border } else { Color::BLACK.alpha(if dark { 0.30 } else { 0.12 }) };
            }
        };
        let e = &mut kit.elevation;
        for shadows in [
            &mut e.raised,
            &mut e.floating,
            &mut e.overlay,
            &mut e.button_neutral,
            &mut e.button_contrast,
            &mut e.elements,
            &mut e.switch_off,
            &mut e.switch_on,
            &mut e.md,
            &mut e.knob,
        ] {
            recolor(shadows);
        }
        kit
    }
}

//! OpenCode's v2 tokens, resolved once per appearance from `assets/theme.json`
//! (generated from `@opencode/ui` by `tools/theme.py`).

use std::sync::OnceLock;

use rune::{Color, Theme};

#[derive(Clone, Copy, Debug)]
pub struct Pal {
    pub dark: bool,
    pub bg_base: Color,
    pub bg_deep: Color,
    pub layer1: Color,
    pub layer2: Color,
    pub layer3: Color,
    pub contrast: Color,
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
    pub accent: Color,
    pub icon: Color,
    pub icon_muted: Color,
    pub icon_faint: Color,
    pub border: Color,
    pub border_muted: Color,
    pub hover: Color,
    pub pressed: Color,
    pub ring: Color,
    pub shadow: Color,
    pub success: Color,
    pub danger: Color,
    pub search_bg: Color,
    pub inline_code_bg: Color,
    pub selection: Color,
    pub avatar_fg: Color,
    pub avatars: [(Color, Color); 9],
}

const THEME: &str = include_str!("../assets/theme.json");

fn build(dark: bool) -> Pal {
    let t = Theme::from_json(THEME, dark).unwrap_or_else(|e| panic!("{e}"));
    let c = |n: &str| t.color(n);
    let avatar = |n: &str| (c(&format!("avatar-bg-{n}")), c(&format!("avatar-border-{n}")));
    let text = c("text-text-base");
    let layer2 = c("background-bg-layer-02");
    Pal {
        dark,
        bg_base: c("background-bg-base"),
        bg_deep: c("background-bg-deep"),
        layer1: c("background-bg-layer-01"),
        layer2,
        layer3: c("background-bg-layer-03"),
        contrast: c("background-bg-contrast"),
        text,
        muted: c("text-text-muted"),
        faint: c("text-text-faint"),
        accent: c("text-text-accent"),
        icon: c("icon-icon-base"),
        icon_muted: c("icon-icon-muted"),
        icon_faint: c("icon-icon-faint"),
        border: c("border-border-base"),
        border_muted: c("border-border-muted"),
        hover: c("overlay-simple-overlay-hover"),
        pressed: c("overlay-simple-overlay-pressed"),
        // --v2-elevation-raised: a 0.5px ring over two drop shadows.
        ring: if dark { c("alpha-light-16") } else { c("alpha-dark-8") },
        shadow: if dark { c("alpha-dark-30") } else { c("alpha-dark-8") },
        success: c("state-fg-success"),
        danger: c("state-fg-danger"),
        search_bg: t.alpha(layer2, 0.6),
        // color-mix(in oklch, text-base 8%, transparent)
        inline_code_bg: t.alpha(text, 0.08),
        selection: t.alpha(c("text-text-accent"), 0.3),
        avatar_fg: c("avatar-fg"),
        avatars: [
            avatar("orange"),
            avatar("yellow"),
            avatar("cyan"),
            avatar("green"),
            avatar("red"),
            avatar("pink"),
            avatar("blue"),
            avatar("purple"),
            avatar("gray"),
        ],
    }
}

pub fn pal(dark: bool) -> &'static Pal {
    static DARK: OnceLock<Pal> = OnceLock::new();
    static LIGHT: OnceLock<Pal> = OnceLock::new();
    if dark { DARK.get_or_init(|| build(true)) } else { LIGHT.get_or_init(|| build(false)) }
}

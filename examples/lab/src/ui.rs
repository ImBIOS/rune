//! Chrome in OpenCode's dark design language: a scene list, the stage, and a knobs panel with a
//! frame-time readout.

use std::sync::OnceLock;

use rune::tok::motion;
use rune::{Align, Color, Cx, Drag, El, Phase, Spring, TextStyle, Theme, col, el, row, spacer, text};

use crate::knobs::{ALL, Kind, KnobSpec, Scene, specs};
use crate::stage;
use crate::Lab;

const SIDEBAR_W: f32 = 212.0;
const PANEL_W: f32 = 272.0;
// Sidebar geometry. Every text line (eyebrows, rows, the footer hint) starts on one edge,
// TEXT_X, which is also the knobs panel's inset. Row pills (hover and the travelling selection)
// sit PILL_INSET in from both sidebar edges and bleed out PILL_PAD around their text.
const PILL_INSET: f32 = 10.0;
const PILL_PAD: f32 = 10.0;
const TEXT_X: f32 = PILL_INSET + PILL_PAD;
const PILL_W: f32 = SIDEBAR_W - 2.0 * PILL_INSET;
const ROW_H: f32 = 28.0;
const ROW_PITCH: f32 = 30.0;
const PILL_R: f32 = 8.0;
const GROUP_H: f32 = 28.0;
const GROUP_GAP: f32 = 16.0;
const SLIDER_W: f32 = PANEL_W - 40.0;

pub struct Pal {
    pub base: Color,
    pub deep: Color,
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
    pub accent: Color,
    pub border: Color,
    pub hover: Color,
    pub pressed: Color,
    pub selected: Color,
    pub track: Color,
    pub knob: Color,
    pub shadow: Color,
}

pub fn pal() -> &'static Pal {
    static P: OnceLock<Pal> = OnceLock::new();
    P.get_or_init(|| {
        let t = Theme::from_json(include_str!("../assets/theme.json"), true).unwrap_or_else(|e| panic!("{e}"));
        let c = |n: &str| t.color(n);
        Pal {
            base: c("background-bg-base"),
            deep: c("background-bg-deep"),
            text: c("text-text-base"),
            muted: c("text-text-muted"),
            faint: c("text-text-faint"),
            accent: c("text-text-accent"),
            border: c("border-border-base"),
            hover: c("overlay-simple-overlay-hover"),
            pressed: c("overlay-simple-overlay-pressed"),
            selected: c("background-bg-layer-02"),
            track: c("background-bg-layer-03"),
            knob: c("text-text-base"),
            shadow: c("alpha-dark-30"),
        }
    })
}

const BODY: TextStyle = TextStyle::new(13.0, 400.0);
const LABEL: TextStyle = TextStyle::new(12.5, 500.0);
const EYEBROW: TextStyle = TextStyle::new(11.0, 600.0).tracking(0.4);
const TITLE: TextStyle = TextStyle::new(17.0, 600.0);
const VALUE: TextStyle = TextStyle::new(11.5, 500.0).mono();

pub fn view(s: &Lab, cx: &mut Cx<Lab>) -> El {
    cx.reduced_motion(s.reduced);
    let p = pal();
    let hair = 1.0 / cx.env().scale;
    row()
        .key("window")
        .fill(p.deep)
        .w_full()
        .h_full()
        .child(sidebar(s, hair))
        .child(stage_view(s))
        .child(panel(s, hair))
}

// ---------------------------------------------------------------- sidebar

fn sidebar(s: &Lab, hair: f32) -> El {
    let p = pal();
    let mut list = el().key("scenes").w_full();
    let mut y = 0.0;
    let mut sel_y = 0.0;
    let mut items: Vec<El> = Vec::new();
    for (g, group) in [("Light", &ALL[..5]), ("Effects", &ALL[5..])].iter().enumerate() {
        if g > 0 {
            y += GROUP_GAP;
        }
        items.push(
            row()
                .key(format!("group{g}"))
                .absolute(0.0, y)
                .size(SIDEBAR_W, GROUP_H)
                .pl(TEXT_X)
                .center_y()
                .child(text(group.0.to_uppercase()).key("label").font(EYEBROW).color(p.faint)),
        );
        y += GROUP_H;
        for sc in group.1.iter().copied() {
            let selected = sc == s.scene;
            if selected {
                sel_y = y;
            }
            items.push(
                row()
                    .key(sc.id())
                    .role("tab")
                    .label(sc.title())
                    .absolute(PILL_INSET, y)
                    .size(PILL_W, ROW_H)
                    .px(PILL_PAD)
                    .center_y()
                    .radius(PILL_R)
                    .hover(if selected { Color::TRANSPARENT } else { p.hover })
                    .press(p.pressed)
                    .on_press(move |s: &mut Lab| s.select(sc))
                    .child(text(sc.title()).key("label").font(BODY).color(if selected { p.text } else { p.muted })),
            );
            y += ROW_PITCH;
        }
    }
    list = list
        .h(y)
        .child(
            el().key("selection")
                .absolute(PILL_INSET, sel_y)
                .size(PILL_W, ROW_H)
                .radius(PILL_R)
                .fill(p.selected)
                .lead(motion::LEAD, motion::DEFAULT),
        )
        .children(items);
    row()
        .key("sidebar-wrap")
        .h_full()
        .child(
            col()
                .key("sidebar")
                .w(SIDEBAR_W)
                .h_full()
                .fill(p.base)
                .child(row().key("brand").h(52.0).w_full().pl(86.0).center_y().child(text("Rune Lab").key("title").font(TextStyle::new(13.0, 600.0)).color(p.text)))
                .child(list)
                .child(spacer())
                .child(
                    text("↑ ↓ to switch scenes")
                        .key("keys-hint")
                        .font(TextStyle::new(11.5, 400.0))
                        .color(p.faint)
                        .ml(TEXT_X)
                        .mb(16.0),
                ),
        )
        .child(el().key("sidebar-edge").w(hair).h_full().fill(p.border))
}

// ---------------------------------------------------------------- stage

fn stage_view(s: &Lab) -> El {
    el().key("stage")
        .role("canvas")
        .grow(1.0)
        .h_full()
        .center()
        .fill(stage::backdrop(s.scene))
        .motion(motion::DEFAULT)
        .clip_soft(0.0)
        .mark(stage::STAGE_MARK)
        .on_drag(stage::on_stage)
        .child(stage::canvas(s, s.scene))
}

// ---------------------------------------------------------------- panel

fn panel(s: &Lab, hair: f32) -> El {
    let p = pal();
    let sc = s.scene;
    let mut knobs = col().key(format!("knobs-{}", sc.id())).gap(18.0).w_full();
    for (i, k) in specs(sc).iter().enumerate() {
        knobs = knobs.child(knob(s, sc, i, k));
    }
    row()
        .key("panel-wrap")
        .h_full()
        .child(el().key("panel-edge").w(hair).h_full().fill(p.border))
        .child(
            col()
                .key("panel")
                .w(PANEL_W)
                .h_full()
                .fill(p.base)
                .px(20.0)
                .pt(48.0)
                .pb(20.0)
                .child(text(sc.title()).key("scene-title").font(TITLE).color(p.text))
                .child(text(sc.hint()).key("scene-hint").w(PANEL_W - 40.0).wrap().font(TextStyle::new(12.5, 400.0).lh(18.0)).color(p.muted).mt(8.0))
                .child(el().key("sep").h(hair).w_full().fill(p.border).mt(20.0).mb(20.0))
                .child(knobs)
                .child(spacer())
                .child(el().key("sep2").h(hair).w_full().fill(p.border).mb(14.0))
                .child(toggle_row("reduced", "Reduce motion", s.reduced, |s: &mut Lab| s.reduced = !s.reduced))
                .child(readout(s)),
        )
}

fn readout(s: &Lab) -> El {
    let p = pal();
    let f = |v: Option<f32>| v.map_or("—".to_owned(), |v| format!("{v:.2} ms"));
    let line = |key: &str, name: &str, value: String| {
        row()
            .key(key.to_owned())
            .h(20.0)
            .w_full()
            .center_y()
            .justify_between()
            .child(text(name).key("name").font(TextStyle::new(12.0, 400.0)).color(p.faint))
            .child(text(value).key("value").font(VALUE).color(p.muted).align(Align::End))
    };
    let state = if s.perf.live { "Animating" } else { "At rest · no frames" };
    col()
        .key("readout")
        .mt(12.0)
        .gap(2.0)
        .child(line("gpu", "GPU frame", f(s.perf.gpu_ms)))
        .child(line("cpu", "CPU frame", f(Some(s.perf.cpu_ms).filter(|v| *v > 0.0))))
        .child(line("state", "Display link", state.to_owned()))
}

fn knob(s: &Lab, sc: Scene, i: usize, k: &KnobSpec) -> El {
    let v = s.vals[sc.idx()][i];
    match k.kind {
        Kind::Slider(dec) => slider(sc, i, k, v, dec),
        Kind::Toggle => {
            let on = v > 0.5;
            toggle_row(k.id, k.label, on, move |s: &mut Lab| {
                let cur = &mut s.vals[sc.idx()][i];
                *cur = if *cur > 0.5 { 0.0 } else { 1.0 };
            })
        }
        Kind::Swatch(palette) => swatches(sc, i, k, v as usize, palette),
    }
}

fn slider(sc: Scene, i: usize, k: &KnobSpec, v: f32, dec: u8) -> El {
    let p = pal();
    let (w, h, kd, bar) = (SLIDER_W, 18.0, 14.0, 4.0);
    let travel = w - kd;
    let (min, max) = (k.min, k.max);
    let u = ((v - min) / (max - min)).clamp(0.0, 1.0);
    let to_value = move |x: f32| min + ((x - kd / 2.0) / travel).clamp(0.0, 1.0) * (max - min);
    let value = format!("{:.*}", dec as usize, v);
    col()
        .key(k.id)
        .gap(6.0)
        .child(
            row()
                .key("head")
                .w_full()
                .justify_between()
                .child(text(k.label).key("label").font(LABEL).color(p.text))
                .child(text(value).key("value").font(VALUE).color(p.muted).align(Align::End)),
        )
        .child(
            el().key("slider")
                .role("slider")
                .label(k.label)
                .size(w, h)
                .motion(motion::SNAPPY)
                .on_drag(move |s: &mut Lab, d: &mut Drag, tx| {
                    let cur = s.vals[sc.idx()][i];
                    let knob_x = kd / 2.0 + ((cur - min) / (max - min)).clamp(0.0, 1.0) * travel;
                    match d.phase {
                        Phase::Down => {
                            d.data = cur;
                            if (d.x - knob_x).abs() > kd / 2.0 + 3.0 {
                                s.vals[sc.idx()][i] = to_value(d.x);
                            }
                        }
                        Phase::Move => {
                            let knob = kd / 2.0 + ((d.data - min) / (max - min)).clamp(0.0, 1.0) * travel;
                            let grab = if (d.start_x - knob).abs() <= kd / 2.0 + 3.0 { d.start_x - knob } else { 0.0 };
                            s.vals[sc.idx()][i] = to_value(d.x - grab);
                            tx.instant();
                        }
                        Phase::Up => { if d.cancelled() { s.vals[sc.idx()][i] = d.data; } }
                    }
                })
                .child(el().key("track").size(w, bar).absolute(0.0, (h - bar) / 2.0).radius(bar / 2.0).fill(p.track))
                .child(el().key("fill").size(kd / 2.0 + u * travel, bar).absolute(0.0, (h - bar) / 2.0).radius(bar / 2.0).fill(p.accent))
                .child(
                    el().key("knob")
                        .size(kd, kd)
                        .absolute(u * travel, (h - kd) / 2.0)
                        .radius(kd / 2.0)
                        .fill(p.knob)
                        .shadow(1.0, 3.0, p.shadow),
                ),
        )
}

fn toggle_row(key: &'static str, label: &str, on: bool, flip: impl Fn(&mut Lab) + 'static) -> El {
    let p = pal();
    let (w, h, k) = (32.0, 18.0, 14.0);
    let inset = (h - k) / 2.0;
    row()
        .key(key)
        .role("toggle")
        .label(label)
        .h(26.0)
        .w_full()
        .center_y()
        .justify_between()
        .on_press(flip)
        .child(text(label.to_owned()).key("label").font(LABEL).color(p.text))
        .child(
            el().key("switch")
                .size(w, h)
                .radius(h / 2.0)
                .fill(if on { p.accent } else { p.track })
                .motion(motion::SNAPPY)
                .child(el().key("knob").size(k, k).absolute(if on { w - k - inset } else { inset }, inset).radius(k / 2.0).fill(p.knob).shadow(1.0, 2.0, p.shadow)),
        )
}

fn swatches(sc: Scene, i: usize, k: &KnobSpec, cur: usize, palette: &'static [u32]) -> El {
    let p = pal();
    let (d, gap, ring_w) = (16.0, 10.0, 1.5);
    let ring = d + 2.0 * (ring_w + 2.0);
    let mut r = row().key("swatches").gap(gap).motion(motion::SNAPPY).child(
        el().key("ring")
            .size(ring, ring)
            .absolute(cur as f32 * (d + gap) - (ring - d) / 2.0, -(ring - d) / 2.0)
            .radius(ring / 2.0)
            .border(ring_w, p.text)
            .lead(Spring::new(0.22, 0.0), Spring::new(0.28, 0.0)),
    );
    for (j, hex) in palette.iter().enumerate() {
        let c = rune::color::hex(&format!("#{hex:06x}")).unwrap_or(Color::WHITE);
        r = r.child(
            el().key(format!("c{j}"))
                .role("swatch")
                .size(d, d)
                .radius(d / 2.0)
                .fill(c)
                .on_press(move |s: &mut Lab| s.vals[sc.idx()][i] = j as f32),
        );
    }
    col().key(k.id).gap(10.0).child(text(k.label).key("label").font(LABEL).color(p.text)).child(r)
}

//! The stage: one canvas of Rune elements per scene, laid out in canvas points, plus the
//! stage's pointer handling. Marks tell the compositor what each element is: which canvas it
//! belongs to, its lighting material, and the effect node it anchors.

use rune::color::{Color, oklch, oklcha};
use rune::{Drag, El, Phase, Tx, TextStyle, el, text};
use rune_fx::Rect;
use rune_motion::{Spring, Vec2};

use crate::knobs::Scene;
use crate::{Grab, Lab, Shot, ShotKind};

pub const CW: f32 = 780.0;
pub const CH: f32 = 600.0;

// ---------------------------------------------------------------- marks

/// The stage's own background (not in any canvas).
pub const STAGE_MARK: u32 = 0xFE << 24;

pub fn mk(scene: Scene, material: u8, node: u8) -> u32 {
    ((scene.idx() as u32 + 1) << 24) | ((node as u32) << 8) | material as u32
}

pub fn mark_scene(m: u32) -> Option<Scene> {
    let i = (m >> 24) as usize;
    (1..=crate::knobs::ALL.len()).contains(&i).then(|| crate::knobs::ALL[i - 1])
}

pub fn mark_material(m: u32) -> u8 {
    (m & 0xFF) as u8
}

pub fn mark_node(m: u32) -> u8 {
    ((m >> 8) & 0xFF) as u8
}

/// Lighting materials (how a marked rect stands on the UI plane).
pub mod mat {
    pub const CARD: u8 = 1;
    pub const CHIP: u8 = 2;
    pub const BUTTON: u8 = 3;
    pub const KNOB: u8 = 4;
    pub const KEY: u8 = 5;
    pub const BODY: u8 = 6;
    pub const SCREEN: u8 = 7;
    pub const EMIT: u8 = 8;
    pub const TILE: u8 = 9;
    pub const SWITCH: u8 = 10;
    pub const ON_CARD: u8 = 11;
    pub const BAR: u8 = 12;
    pub const PILL: u8 = 13;
    pub const DIAL: u8 = 14;
    pub const COMPOSER: u8 = 15;
    pub const FLAT: u8 = 16;
}

/// Effect anchors.
pub mod node {
    pub const LAMP: u8 = 1;
    pub const GLOW_BUTTON: u8 = 2;
    pub const NEON: u8 = 3;
    pub const AURA: u8 = 4;
    pub const GLOW_TEXT: u8 = 5;
    pub const HOT_DOT: u8 = 6;
    pub const RADAR: u8 = 7;
    pub const AVATAR: u8 = 8;
    pub const LIVE: u8 = 9;
    pub const SEND: u8 = 10;
    pub const INBOX: u8 = 11;
    pub const TERMINALS: u8 = 12;
    pub const OVERLOAD: u8 = 14;
    pub const CHARGED: u8 = 15;
    pub const FLAMES: u8 = 16;
    pub const PLASMA: u8 = 17;
    pub const DEPLOY: u8 = 18;
    pub const CARD: u8 = 19;
    pub const CRT: u8 = 20;
    pub const CHROMA: u8 = 21;
    pub const SKELETON: u8 = 22;
    pub const THINKING: u8 = 23;
    pub const AURORA: u8 = 24;
    pub const PRO: u8 = 25;
    pub const REC: u8 = 26;
    pub const PROGRESS: u8 = 27;
    pub const STATUS: u8 = 28;
    pub const PAGE: u8 = 29;
    /// Keys are `KEY0 + index`.
    pub const KEY0: u8 = 100;
}

// ---------------------------------------------------------------- layout (canvas points)

pub const LAMP_REST: [f32; 2] = [690.0, 196.0];

pub const GLOW_BTN: Rect = Rect::new(70.0, 110.0, 180.0, 52.0);
pub const NEON: Rect = Rect::new(320.0, 84.0, 190.0, 116.0);
pub const HOT_DOT: [f32; 3] = [592.0, 132.0, 8.0];
pub const AURA: Rect = Rect::new(70.0, 300.0, 360.0, 56.0);
pub const GLOW_TEXT: Rect = Rect::new(496.0, 270.0, 220.0, 100.0);

pub const RADAR: [f32; 3] = [110.0, 140.0, 20.0];
pub const AVATAR: [f32; 3] = [290.0, 140.0, 26.0];
pub const LIVE: Rect = Rect::new(430.0, 124.0, 88.0, 32.0);
pub const SEND: [f32; 3] = [100.0, 390.0, 20.0];
pub const INBOX: Rect = Rect::new(530.0, 332.0, 190.0, 116.0);
pub const PING_DURATION: f32 = 0.62;

pub const ARC_A: [f32; 2] = [70.0, 120.0];
pub const ARC_B: [f32; 2] = [710.0, 120.0];
pub const OVERLOAD: Rect = Rect::new(70.0, 230.0, 290.0, 160.0);
pub const CHARGED: Rect = Rect::new(450.0, 280.0, 240.0, 60.0);

pub const FLAMES: Rect = Rect::new(80.0, 320.0, 270.0, 150.0);
pub const PLASMA: Rect = Rect::new(430.0, 150.0, 270.0, 66.0);
pub const DEPLOY: Rect = Rect::new(475.0, 360.0, 180.0, 52.0);

pub const CARD_SIZE: [f32; 2] = [156.0, 92.0];
pub const CARD_REST: [f32; 2] = [312.0, 254.0];

pub const LENS_SIZE: [f32; 2] = [220.0, 72.0];
pub const LENS_REST: [f32; 2] = [230.0, 204.0];
pub const CRT: Rect = Rect::new(40.0, 330.0, 300.0, 140.0);
pub const CHROMA: Rect = Rect::new(372.0, 356.0, 360.0, 76.0);

pub const SKELETON: Rect = Rect::new(40.0, 70.0, 330.0, 216.0);
pub const THINKING: Rect = Rect::new(34.0, 330.0, 150.0, 44.0);
pub const AURORA: Rect = Rect::new(420.0, 70.0, 310.0, 186.0);
pub const PRO: Rect = Rect::new(524.0, 336.0, 104.0, 40.0);

pub const REC: [f32; 3] = [390.0, 318.0, 30.0];
pub const PROGRESS: Rect = Rect::new(20.0, 430.0, 740.0, 10.0);

pub const KEY_COLS: usize = 10;
pub const KEY_COUNT: usize = 20;

pub fn key_rect(i: usize) -> Rect {
    let (r, c) = (i / KEY_COLS, i % KEY_COLS);
    Rect::new(40.0 + c as f32 * 71.0, 300.0 + r as f32 * 106.0, 62.0, 88.0)
}

fn accent_key(i: usize) -> bool {
    matches!(i, 2 | 7 | 18)
}

pub fn lens_rect(c: [f32; 2]) -> Rect {
    Rect::new(c[0] - LENS_SIZE[0] * 0.5, c[1] - LENS_SIZE[1] * 0.5, LENS_SIZE[0], LENS_SIZE[1])
}

fn inside(r: Rect, p: [f32; 2], pad: f32) -> bool {
    p[0] >= r.x - pad && p[0] <= r.x + r.w + pad && p[1] >= r.y - pad && p[1] <= r.y + r.h + pad
}

fn near(c: [f32; 2], p: [f32; 2], r: f32) -> bool {
    (c[0] - p[0]).hypot(c[1] - p[1]) <= r
}

/// Background colour behind each scene's canvas.
pub fn backdrop(scene: Scene) -> Color {
    match scene {
        Scene::Lamp => oklch(0.17, 0.006, 265.0),
        Scene::Flashlight => oklch(0.16, 0.005, 265.0),
        Scene::Gi => oklch(0.15, 0.006, 265.0),
        Scene::Keys => oklch(0.18, 0.004, 265.0),
        Scene::GodRays => oklch(0.11, 0.014, 265.0),
        _ => oklch(0.145, 0.006, 265.0),
    }
}

// ---------------------------------------------------------------- input

/// Every stage gesture: lamp, keys, emitters, ripples, the card, the lens.
pub fn on_stage(s: &mut Lab, d: &mut Drag, tx: &mut Tx) {
    let p = [d.x - (d.w - CW) * 0.5, d.y - (d.h - CH) * 0.5];
    let now = s.now;
    let t = now as f32;
    match d.phase {
        Phase::Down => {
            s.grab = None;
            match s.scene {
                Scene::Lamp if near(s.lamp, p, 30.0) => s.grab = Some(Grab::Lamp([p[0] - s.lamp[0], p[1] - s.lamp[1]])),
                Scene::Keys => {
                    if let Some(i) = (0..KEY_COUNT).find(|&i| inside(key_rect(i), p, 0.0)) {
                        s.keys[i].animate_to(1.0, Spring::new(0.1, 0.0), now);
                        s.grab = Some(Grab::Key(i));
                    }
                }
                Scene::Gi => {
                    if near([REC[0], REC[1]], p, REC[2] + 20.0) {
                        s.recording = !s.recording;
                    } else if inside(PROGRESS, p, 10.0) {
                        s.uploading = !s.uploading;
                    }
                }
                Scene::Glow if inside(NEON, p, 0.0) => s.neon_on = !s.neon_on,
                Scene::Pulses => {
                    let shot = if near([SEND[0], SEND[1]], p, SEND[2] + 4.0) {
                        Shot { kind: ShotKind::Ping, at: [SEND[0], SEND[1]], t0: t }
                    } else if inside(INBOX, p, 0.0) {
                        Shot { kind: ShotKind::RippleCard, at: p, t0: t }
                    } else {
                        Shot { kind: ShotKind::Ripple, at: p, t0: t }
                    };
                    s.shots.prune(t, 3.0);
                    s.shots.trigger(t, shot);
                }
                Scene::Motion => {
                    let c = s.card.sample(now);
                    if inside(Rect::new(c.x, c.y, CARD_SIZE[0], CARD_SIZE[1]), p, 6.0) {
                        s.grab = Some(Grab::Card([p[0] - c.x, p[1] - c.y]));
                        // Catching it mid-glide holds it where it is.
                        s.card.set(c);
                        s.card_vt.reset();
                        s.card_vt.add(now, c);
                        tx.instant();
                    }
                }
                Scene::Electric if !inside(CHARGED, p, 0.0) => {
                    s.shots.prune(t, 3.0);
                    s.shots.trigger(t, Shot { kind: ShotKind::Zap, at: p, t0: t });
                }
                Scene::Extras if inside(PRO, p, 8.0) => {
                    s.shots.prune(t, 3.0);
                    s.shots.trigger(t, Shot { kind: ShotKind::Sparkles, at: p, t0: t });
                }
                Scene::Lens => {
                    if inside(lens_rect(s.lens), p, 4.0) {
                        s.grab = Some(Grab::Lens([p[0] - s.lens[0], p[1] - s.lens[1]]));
                    } else {
                        s.shots.prune(t, 3.0);
                        s.shots.trigger(t, Shot { kind: ShotKind::Shockwave, at: p, t0: t });
                    }
                }
                _ => {}
            }
        }
        Phase::Move => match s.grab {
            Some(Grab::Lamp(o)) => {
                s.lamp = [(p[0] - o[0]).clamp(0.0, CW), (p[1] - o[1]).clamp(0.0, CH)];
                tx.instant();
            }
            Some(Grab::Card(o)) => {
                let c = Vec2 { x: p[0] - o[0], y: p[1] - o[1] };
                s.card.set(c);
                s.card_vt.add(now, c);
                tx.instant();
            }
            Some(Grab::Lens(o)) => {
                s.lens = [(p[0] - o[0]).clamp(0.0, CW), (p[1] - o[1]).clamp(0.0, CH)];
                tx.instant();
            }
            _ => {}
        },
        Phase::Up => {
            if d.cancelled() {
                let start = [d.start_x - (d.w - CW) * 0.5, d.start_y - (d.h - CH) * 0.5];
                match s.grab {
                    Some(Grab::Lamp(o)) => s.lamp = [start[0] - o[0], start[1] - o[1]],
                    Some(Grab::Lens(o)) => s.lens = [start[0] - o[0], start[1] - o[1]],
                    Some(Grab::Card(o)) => { s.card.set(Vec2::new(start[0] - o[0], start[1] - o[1])); s.card_vt.reset(); }
                    Some(Grab::Key(i)) => s.keys[i].animate_to(0.0, Spring::new(0.25, 0.0), now),
                    _ => {}
                }
                s.grab = None;
                return;
            }
            match s.grab {
                Some(Grab::Key(i)) => s.keys[i].animate_to(0.0, Spring::new(0.25, 0.0), now),
                Some(Grab::Card(_)) => {
                    // Fling: project where the throw would coast to, keep it on the canvas, and
                    // glide there on a zero-bounce spring that starts at the release velocity.
                    let v = s.card_vt.velocity();
                    let c = s.card.sample(now);
                    let rate = rune_motion::deceleration::NORMAL;
                    let tx_ = rune_motion::project(c.x, v.x, rate).clamp(0.0, CW - CARD_SIZE[0]);
                    let ty_ = rune_motion::project(c.y, v.y, rate).clamp(0.0, CH - CARD_SIZE[1]);
                    let glide = s.k(Scene::Motion, "glide");
                    s.card.release(v, Vec2 { x: tx_, y: ty_ }, Spring::new(glide, 0.0), now);
                    tx.instant();
                }
                _ => {}
            }
            s.grab = None;
        }
    }
}

// ---------------------------------------------------------------- view helpers

struct P {
    sc: Scene,
    txt: Color,
    txt2: Color,
    txt3: Color,
    hair: Color,
    card: Color,
}

fn bx(x: f32, y: f32, w: f32, h: f32) -> El {
    el().absolute(x, y).size(w, h)
}

fn label(x: f32, y: f32, s: &str, size: f32, weight: f32, c: Color) -> El {
    text(s).absolute(x, y).font(TextStyle::new(size, weight)).color(c)
}

fn mono(x: f32, y: f32, s: &str, size: f32, c: Color) -> El {
    text(s).absolute(x, y).font(TextStyle::new(size, 500.0).mono()).color(c)
}

fn r(rc: Rect) -> El {
    bx(rc.x, rc.y, rc.w, rc.h)
}

fn circle(c: [f32; 3]) -> El {
    bx(c[0] - c[2], c[1] - c[2], c[2] * 2.0, c[2] * 2.0).radius(c[2])
}

fn hx(h: u32) -> Color {
    rune::color::hex(&format!("#{h:06x}")).unwrap_or(Color::BLACK)
}

/// Placeholder text lines (skeleton bars).
fn lines(p: &P, x: f32, y: f32, w: f32, n: usize) -> Vec<El> {
    (0..n)
        .map(|i| {
            let ww = w * [1.0, 0.82, 0.6, 0.9][i % 4];
            bx(x, y + i as f32 * 14.0, ww, 6.0).radius(3.0).fill(oklch(0.3, 0.008, 265.0)).mark(mk(p.sc, 0, 0))
        })
        .collect()
}

fn fx_card(p: &P, rc: Rect, radius: f32, node: u8) -> El {
    r(rc).radius(radius)
        .fill(p.card)
        .border(0.5, p.hair)
        .shadow(8.0, 28.0, oklcha(0.0, 0.0, 0.0, 0.5))
        .mark(mk(p.sc, 0, node))
}

fn caption(p: &P, x: f32, y: f32, s: &str) -> El {
    label(x, y, s, 11.0, 500.0, p.txt3)
}

// ---------------------------------------------------------------- canvases

pub fn canvas(s: &Lab, scene: Scene) -> El {
    let p = P {
        sc: scene,
        txt: oklch(0.94, 0.003, 265.0),
        txt2: oklch(0.68, 0.01, 265.0),
        txt3: oklch(0.52, 0.01, 265.0),
        hair: oklcha(1.0, 0.0, 0.0, 0.07),
        card: oklch(0.225, 0.007, 265.0),
    };
    let kids = match scene {
        Scene::Lamp => lamp(s, &p),
        Scene::Flashlight => flashlight(&p),
        Scene::Gi => gi(s, &p),
        Scene::Keys => keys(&p),
        Scene::GodRays => godrays(&p),
        Scene::Glow => glow(s, &p),
        Scene::Pulses => pulses(&p),
        Scene::Electric => electric(&p),
        Scene::Fire => fire(&p),
        Scene::Motion => motion(s, &p),
        Scene::Lens => lens(&p),
        Scene::Extras => extras(&p),
    };
    el().key(format!("canvas-{}", scene.id())).size(CW, CH).mark(mk(scene, 0, 0)).children(kids)
}

fn lamp(s: &Lab, p: &P) -> Vec<El> {
    let user = oklch(0.3, 0.045, 258.0);
    let user_hair = oklcha(1.0, 0.0, 0.0, 0.08);
    let card = |rc: Rect, c: Color, h: Color| r(rc).radius(16.0).fill(c).border(1.0, h).mark(mk(p.sc, mat::CARD, 0));
    let body = |x: f32, y: f32, ls: &[&str]| -> Vec<El> {
        ls.iter().enumerate().map(|(i, l)| label(x, y + i as f32 * 21.0, l, 14.0, 400.0, p.txt)).collect()
    };
    let mut v = vec![
        card(Rect::new(400.0, 20.0, 360.0, 64.0), user, user_hair),
        card(Rect::new(20.0, 106.0, 500.0, 150.0), p.card, p.hair),
        card(Rect::new(500.0, 280.0, 260.0, 44.0), user, user_hair),
        card(Rect::new(20.0, 348.0, 540.0, 160.0), p.card, p.hair),
    ];
    v.extend(body(418.0, 32.0, &["what if lighting effects. like you could", "put a light and it would raytrace…"]));
    v.extend(body(
        40.0,
        122.0,
        &[
            "Every Rune element is an analytic rounded rect, so the",
            "frame is already an exact distance field. A light can",
            "sphere-trace soft shadows straight through it.",
        ],
    ));
    for (i, chip) in ["Read shader.wgsl", "Edit lib.rs", "Run bench"].iter().enumerate() {
        let x = 40.0 + [0.0, 136.0, 236.0][i];
        let w = [124.0, 88.0, 84.0][i];
        v.push(bx(x, 212.0, w, 26.0).radius(13.0).fill(oklch(0.27, 0.008, 265.0)).border(1.0, p.hair).mark(mk(p.sc, mat::CHIP, 0)));
        v.push(label(x + 11.0, 217.0, chip, 12.0, 500.0, p.txt2));
    }
    v.extend(body(520.0, 292.0, &["MAKE THIS SOOO GOOOD"]));
    v.extend(body(40.0, 364.0, &["Lights are springs like everything else."]));
    v.push(bx(36.0, 396.0, 508.0, 96.0).radius(10.0).fill(oklch(0.195, 0.008, 265.0)).border(1.0, p.hair).mark(mk(p.sc, mat::ON_CARD, 0)));
    let code = oklch(0.8, 0.06, 200.0);
    for (i, l) in ["let lamp = Light::point(x, y, 40.0)", "    .color(WARM).intensity(2.6).radius(11.0);", "lights.render(&scene, &[lamp], target);"].iter().enumerate() {
        v.push(mono(54.0, 412.0 + i as f32 * 22.0, l, 12.5, code));
    }
    v.push(r(Rect::new(20.0, 532.0, 740.0, 56.0)).radius(18.0).fill(p.card).border(1.0, oklcha(1.0, 0.0, 0.0, 0.09)).mark(mk(p.sc, mat::COMPOSER, 0)));
    v.push(label(42.0, 550.0, "Ask anything…", 14.0, 400.0, p.txt3));
    v.push(circle([728.0, 560.0, 16.0]).fill(oklch(0.72, 0.15, 255.0)).mark(mk(p.sc, mat::BUTTON, 0)));
    v.push(bx(723.0, 559.0, 10.0, 2.0).radius(1.0).fill(oklch(0.2, 0.02, 255.0)).mark(mk(p.sc, 0, 0)));
    // The lamp's handle: a ring around the bulb, which the light draws itself.
    let held = matches!(s.grab, Some(Grab::Lamp(_)));
    v.push(
        circle([s.lamp[0], s.lamp[1], 15.0])
            .key("lamp")
            .border(1.0, oklcha(1.0, 0.0, 0.0, if held { 0.5 } else { 0.22 }))
            .mark(mk(p.sc, 0, node::LAMP))
            .motion(Spring::new(0.2, 0.0)),
    );
    v
}

fn flashlight(p: &P) -> Vec<El> {
    let accent = oklch(0.7, 0.15, 255.0);
    let mut v = vec![
        label(20.0, 6.0, "Settings", 24.0, 600.0, p.txt),
        label(20.0, 42.0, "Appearance, sound and shortcuts", 13.0, 400.0, p.txt2),
    ];
    let rows: [(&str, &str, u8); 9] = [
        ("Appearance", "Dark", 2),
        ("Accent", "", 3),
        ("Reduce motion", "", 1),
        ("Sound effects", "", 0),
        ("Volume", "", 4),
        ("Push to talk", "⌥ Space", 2),
        ("Auto-update", "", 1),
        ("Crash reports", "", 0),
        ("Launch at login", "", 1),
    ];
    for g in 0..3 {
        let x = 20.0 + g as f32 * 253.0;
        let y = 84.0;
        v.push(bx(x, y, 233.0, 156.0).radius(14.0).fill(p.card).border(1.0, p.hair).mark(mk(p.sc, mat::CARD, 0)));
        for ri in 0..3 {
            let (name, value, kind) = rows[g * 3 + ri];
            let ry = y + ri as f32 * 52.0;
            if ri > 0 {
                v.push(bx(x + 14.0, ry, 205.0, 1.0).fill(p.hair).mark(mk(p.sc, 0, 0)));
            }
            v.push(label(x + 16.0, ry + 18.0, name, 13.0, 500.0, p.txt));
            let rx = x + 233.0 - 16.0;
            match kind {
                0 | 1 => {
                    let on = kind == 1;
                    v.push(bx(rx - 40.0, ry + 15.0, 40.0, 22.0).radius(11.0).fill(if on { accent } else { oklch(0.33, 0.006, 265.0) }).mark(mk(p.sc, mat::ON_CARD, 0)));
                    let kx = if on { rx - 20.0 } else { rx - 38.0 };
                    v.push(bx(kx, ry + 17.0, 18.0, 18.0).radius(9.0).fill(oklch(0.96, 0.0, 0.0)).mark(mk(p.sc, mat::SWITCH, 0)));
                }
                2 => {
                    let w = 14.0 + value.chars().count() as f32 * 7.4;
                    v.push(bx(rx - w, ry + 13.0, w, 26.0).radius(8.0).fill(oklch(0.3, 0.006, 265.0)).border(1.0, p.hair).mark(mk(p.sc, mat::PILL, 0)));
                    v.push(label(rx - w + 8.0, ry + 18.0, value, 12.0, 500.0, p.txt));
                }
                3 => {
                    for (k, h) in [255.0, 300.0, 350.0, 40.0, 150.0].iter().enumerate() {
                        let cx = rx - 16.0 - k as f32 * 24.0;
                        v.push(bx(cx, ry + 18.0, 16.0, 16.0).radius(8.0).fill(oklch(0.68, 0.15, *h)).mark(mk(p.sc, mat::SWITCH, 0)));
                    }
                }
                _ => {
                    v.push(bx(rx - 110.0, ry + 25.0, 110.0, 4.0).radius(2.0).fill(oklch(0.33, 0.006, 265.0)).mark(mk(p.sc, mat::ON_CARD, 0)));
                    v.push(bx(rx - 110.0, ry + 25.0, 70.0, 4.0).radius(2.0).fill(accent).mark(mk(p.sc, mat::ON_CARD, 0)));
                    v.push(bx(rx - 49.0, ry + 18.0, 18.0, 18.0).radius(9.0).fill(oklch(0.96, 0.0, 0.0)).mark(mk(p.sc, mat::SWITCH, 0)));
                }
            }
        }
    }
    let by = 266.0;
    v.push(bx(20.0, by, 739.0, 132.0).radius(14.0).fill(p.card).border(1.0, p.hair).mark(mk(p.sc, mat::CARD, 0)));
    v.push(label(40.0, by + 18.0, "Keyboard shortcuts", 15.0, 600.0, p.txt));
    v.push(label(40.0, by + 42.0, "Press a key combination to record a new shortcut.", 13.0, 400.0, p.txt2));
    for (i, k) in ["⌘", "⇧", "K"].iter().enumerate() {
        let x = 40.0 + i as f32 * 52.0;
        v.push(bx(x, by + 72.0, 44.0, 44.0).radius(10.0).fill(oklch(0.3, 0.006, 265.0)).border(1.0, p.hair).mark(mk(p.sc, mat::PILL, 0)));
        v.push(label(x + 15.0, by + 84.0, k, 15.0, 600.0, p.txt));
    }
    v.push(bx(589.0, by + 78.0, 150.0, 36.0).radius(10.0).fill(accent).mark(mk(p.sc, mat::PILL, 0)));
    v.push(label(611.0, by + 87.0, "Record shortcut", 13.0, 600.0, oklch(0.18, 0.03, 255.0)));
    v.push(bx(459.0, by + 78.0, 116.0, 36.0).radius(10.0).fill(oklch(0.3, 0.006, 265.0)).border(1.0, p.hair).mark(mk(p.sc, mat::PILL, 0)));
    v.push(label(498.0, by + 87.0, "Reset", 13.0, 600.0, p.txt));
    let names = ["Microphone", "Speakers", "Camera", "Battery"];
    let subs = ["MacBook Pro", "Studio Display", "FaceTime HD", "92 %"];
    for i in 0..4 {
        let x = 20.0 + i as f32 * 189.5;
        v.push(bx(x, 424.0, 172.0, 116.0).radius(14.0).fill(p.card).border(1.0, p.hair).mark(mk(p.sc, mat::CARD, 0)));
        v.push(label(x + 16.0, 440.0, names[i], 13.0, 600.0, p.txt));
        v.push(label(x + 16.0, 462.0, subs[i], 12.0, 400.0, p.txt2));
        v.push(bx(x + 16.0, 502.0, 140.0, 16.0).radius(8.0).fill(oklch(0.27, 0.006, 265.0)).mark(mk(p.sc, mat::ON_CARD, 0)));
        let f = [0.7, 0.45, 0.0, 0.92][i];
        if f > 0.0 {
            v.push(bx(x + 16.0, 502.0, 140.0 * f, 16.0).radius(8.0).fill(accent).mark(mk(p.sc, mat::PILL, 0)));
        }
    }
    v
}

fn gi(s: &Lab, p: &P) -> Vec<El> {
    let mut v = vec![
        label(20.0, 4.0, "Voice memo", 24.0, 600.0, p.txt),
        label(20.0, 40.0, "Recording · Studio Display microphone", 13.0, 400.0, p.txt2),
    ];
    let card = |rc: Rect, radius: f32| r(rc).radius(radius).fill(p.card).border(1.0, p.hair).mark(mk(p.sc, mat::CARD, 0));
    for (i, (t, sub)) in [("Transcript", "Live, 2 speakers"), ("Markers", "3 added"), ("Noise", "Voice isolation on")].iter().enumerate() {
        let y = 80.0 + i as f32 * 110.0;
        v.push(card(Rect::new(20.0, y, 200.0, 96.0), 16.0));
        v.push(label(40.0, y + 18.0, t, 14.0, 600.0, p.txt));
        v.push(label(40.0, y + 42.0, sub, 13.0, 400.0, p.txt2));
    }
    v.push(card(Rect::new(240.0, 80.0, 300.0, 316.0), 20.0));
    v.push(label(262.0, 98.0, "00:42.18", 30.0, 600.0, p.txt));
    v.push(label(262.0, 140.0, if s.recording { "Recording" } else { "Paused" }, 13.0, 500.0, oklch(0.75, 0.12, 25.0)));
    for k in 0..30 {
        let h = 8.0 + 40.0 * (0.5 + 0.5 * ((k as f32 * 0.7).sin() * (k as f32 * 0.23 + 1.0).cos())).abs();
        v.push(bx(262.0 + k as f32 * 8.6, 206.0 - h * 0.5, 4.4, h).radius(2.2).fill(oklch(0.5, 0.01, 265.0)).mark(mk(p.sc, mat::BAR, 0)));
    }
    v.push(circle([REC[0], REC[1], 50.0]).fill(oklch(0.27, 0.008, 265.0)).border(1.0, p.hair).mark(mk(p.sc, mat::DIAL, 0)));
    v.push(circle(REC).fill(oklch(0.66, 0.2, 25.0)).mark(mk(p.sc, mat::EMIT, node::REC)));
    for (i, (t, sub)) in [("Agent finished", "Demos rendered"), ("Build passed", "ci · 1m 12s"), ("Review", "No blockers")].iter().enumerate() {
        let y = 80.0 + i as f32 * 110.0;
        v.push(card(Rect::new(560.0, y, 200.0, 96.0), 16.0));
        v.push(label(598.0, y + 18.0, t, 14.0, 600.0, p.txt));
        v.push(label(598.0, y + 42.0, sub, 13.0, 400.0, p.txt2));
        let c = if i == 0 { oklch(0.78, 0.17, 150.0) } else { oklch(0.45, 0.01, 265.0) };
        v.push(bx(578.0, y + 22.0, 10.0, 10.0).radius(5.0).fill(c).mark(mk(p.sc, mat::EMIT, if i == 0 { node::STATUS } else { 0 })));
    }
    v.push(r(PROGRESS).radius(5.0).fill(oklch(0.24, 0.006, 265.0)).mark(mk(p.sc, mat::FLAT, 0)));
    v.push(r(Rect::new(PROGRESS.x, PROGRESS.y, PROGRESS.w * 0.62, PROGRESS.h)).radius(5.0).fill(oklch(0.78, 0.13, 210.0)).mark(mk(p.sc, mat::EMIT, node::PROGRESS)));
    v.push(label(20.0, 452.0, "Uploading to OpenCode · 3 of 5 files", 12.0, 500.0, p.txt2));
    for i in 0..5 {
        let x = 20.0 + i as f32 * 151.0;
        v.push(card(Rect::new(x, 492.0, 136.0, 90.0), 16.0));
        v.push(label(x + 18.0, 510.0, ["Pause", "Marker", "Trim", "Share", "Export"][i], 13.0, 600.0, p.txt));
    }
    v
}

fn keys(p: &P) -> Vec<El> {
    let key = oklch(0.29, 0.004, 265.0);
    let mut v = vec![
        r(Rect::new(0.0, 0.0, CW, CH)).radius(32.0).fill(oklch(0.255, 0.004, 265.0)).border(1.0, oklcha(1.0, 0.0, 0.0, 0.05)).mark(mk(p.sc, mat::BODY, 0)),
        label(40.0, 30.0, "OP-1 Field · Control surface", 15.0, 600.0, p.txt),
        label(40.0, 54.0, "Bevels from the SDF catch the light", 12.0, 400.0, p.txt2),
    ];
    let hues = [255.0, 150.0, 60.0, 25.0];
    for (i, h) in hues.iter().enumerate() {
        let cx = 90.0 + i as f32 * 116.0;
        v.push(circle([cx, 170.0, 42.0]).fill(key).mark(mk(p.sc, mat::KNOB, 0)));
        v.push(circle([cx, 140.0, 5.0]).fill(oklch(0.7, 0.14, *h)).mark(mk(p.sc, mat::FLAT, 0)));
    }
    v.push(bx(530.0, 100.0, 210.0, 140.0).radius(18.0).fill(oklch(0.14, 0.01, 265.0)).mark(mk(p.sc, mat::SCREEN, 0)));
    v.push(label(554.0, 124.0, "128 BPM", 26.0, 600.0, oklch(0.85, 0.12, 150.0)));
    v.push(label(554.0, 166.0, "Pattern A · bar 12", 13.0, 400.0, oklch(0.6, 0.06, 150.0)));
    let names = ["C", "D", "E", "F", "G", "A", "B", "C", "D", "E"];
    for i in 0..KEY_COUNT {
        let rc = key_rect(i);
        let accent = accent_key(i);
        v.push(r(rc).radius(14.0).fill(if accent { oklch(0.62, 0.17, 35.0) } else { key }).mark(mk(p.sc, mat::KEY, node::KEY0 + i as u8)));
        v.push(label(rc.x + 12.0, rc.y + 12.0, names[i % KEY_COLS], 12.0, 600.0, if accent { oklch(0.2, 0.03, 35.0) } else { p.txt2 }));
    }
    v
}

fn godrays(p: &P) -> Vec<El> {
    let names = [
        "Inbox", "Calendar", "Notes", "Music", "Photos", "Maps", "Files", "Terminal", "Mail", "Weather", "Clock", "Books", "Home", "Wallet",
        "Health", "Code", "Notes", "Rune",
    ];
    let mut v = vec![
        label(28.0, 70.0, "Good evening", 30.0, 600.0, p.txt),
        label(28.0, 114.0, "18 apps · light from behind the grid", 14.0, 400.0, p.txt2),
    ];
    for i in 0..18 {
        let (rw, c) = (i / 6, i % 6);
        let (x, y) = (28.0 + c as f32 * 124.0, 190.0 + rw as f32 * 124.0);
        v.push(bx(x, y, 104.0, 104.0).radius(22.0).fill(oklch(0.21, 0.008, 265.0)).border(1.0, p.hair).mark(mk(p.sc, mat::TILE, 0)));
        v.push(bx(x + 16.0, y + 16.0, 36.0, 36.0).radius(11.0).fill(oklch(0.62, 0.12, (i as f32 * 47.0) % 360.0)).mark(mk(p.sc, mat::FLAT, 0)));
        v.push(label(x + 16.0, y + 70.0, names[i], 13.0, 600.0, p.txt));
    }
    v
}

pub const GODRAY_REST: [f32; 2] = [374.0, 304.0];

fn glow(s: &Lab, p: &P) -> Vec<El> {
    let btn = GLOW_BTN;
    let mut v = vec![
        r(btn).radius(btn.h * 0.5).fill(hx(0x7a5cf6)).border(0.5, oklcha(1.0, 0.0, 0.0, 0.16)).shadow(4.0, 10.0, oklcha(0.0, 0.0, 0.0, 0.4)).mark(mk(p.sc, 0, node::GLOW_BUTTON)),
        label(btn.x + 52.0, btn.y + 16.0, "Generate", 15.0, 600.0, Color::WHITE),
        caption(p, btn.x, btn.y + 80.0, "Soft · breathing"),
        fx_card(p, NEON, 16.0, node::NEON),
        label(NEON.x + 18.0, NEON.y + 20.0, "Neon", 15.0, 600.0, hx(0xe6f9ff)),
    ];
    v.extend(lines(p, NEON.x + 18.0, NEON.y + 50.0, 120.0, 3));
    v.push(caption(p, NEON.x, NEON.y + NEON.h + 24.0, if s.neon_on { "Neon · click to switch off" } else { "Neon · click to switch on" }));
    v.push(circle(HOT_DOT).fill(hx(0xfff3dc)).mark(mk(p.sc, 0, node::HOT_DOT)));
    v.push(label(HOT_DOT[0] + 18.0, HOT_DOT[1] - 9.0, "Recording", 13.0, 500.0, oklch(0.88, 0.005, 265.0)));
    v.push(caption(p, HOT_DOT[0] - 10.0, NEON.y + NEON.h + 24.0, "Hot core"));
    v.push(r(AURA).radius(AURA.h * 0.5).fill(oklch(0.2, 0.008, 265.0)).border(0.5, oklcha(1.0, 0.0, 0.0, 0.08)).shadow(6.0, 14.0, oklcha(0.0, 0.0, 0.0, 0.5)).mark(mk(p.sc, 0, node::AURA)));
    v.push(label(AURA.x + 26.0, AURA.y + 18.0, "Ask anything…", 15.0, 400.0, oklch(0.62, 0.01, 265.0)));
    v.push(caption(p, AURA.x, AURA.y + AURA.h + 24.0, "Aura · flowing spectrum"));
    v.push(text("Rune").absolute(GLOW_TEXT.x, GLOW_TEXT.y).font(TextStyle::new(76.0, 600.0)).color(hx(0xfff4e6)).mark(mk(p.sc, 0, node::GLOW_TEXT)));
    v.push(caption(p, GLOW_TEXT.x, AURA.y + AURA.h + 24.0, "Mask · text bloom"));
    v
}

fn pulses(p: &P) -> Vec<El> {
    let mut v = vec![
        circle(RADAR).fill(hx(0xf04e5e)).border(0.5, oklcha(1.0, 0.0, 0.0, 0.2)).mark(mk(p.sc, 0, node::RADAR)),
        bx(RADAR[0] - 6.0, RADAR[1] - 6.0, 12.0, 12.0).radius(3.0).fill(oklcha(1.0, 0.0, 0.0, 0.95)).mark(mk(p.sc, 0, 0)),
        caption(p, RADAR[0] - 18.0, RADAR[1] + 70.0, "Radar"),
        circle(AVATAR).fill(oklch(0.33, 0.012, 265.0)).border(1.0, hx(0x34d399)).mark(mk(p.sc, 0, node::AVATAR)),
        label(AVATAR[0] - 10.0, AVATAR[1] - 9.0, "KL", 14.0, 600.0, oklch(0.95, 0.003, 265.0)),
        caption(p, AVATAR[0] - 40.0, AVATAR[1] + 70.0, "Breathing halo"),
        r(LIVE).radius(LIVE.h * 0.5).fill(hx(0xe8315a)).border(0.5, oklcha(1.0, 0.0, 0.0, 0.2)).mark(mk(p.sc, 0, node::LIVE)),
        label(LIVE.x + 28.0, LIVE.y + 8.0, "LIVE", 12.0, 600.0, Color::WHITE),
        caption(p, LIVE.x + 10.0, AVATAR[1] + 70.0, "Heartbeat"),
        fx_card(p, INBOX, 16.0, node::INBOX),
        label(INBOX.x + 18.0, INBOX.y + 20.0, "Inbox", 15.0, 600.0, p.txt),
    ];
    v.extend(lines(p, INBOX.x + 18.0, INBOX.y + 50.0, 120.0, 3));
    v.push(circle(SEND).key("send").fill(hx(0x3d7ff5)).border(0.5, oklcha(1.0, 0.0, 0.0, 0.16)).shadow(4.0, 10.0, oklcha(0.0, 0.0, 0.0, 0.4)).mark(mk(p.sc, 0, node::SEND)));
    v.push(label(SEND[0] - 6.0, SEND[1] - 10.0, "→", 16.0, 600.0, Color::WHITE));
    v.push(caption(p, SEND[0] - 30.0, SEND[1] + 46.0, "Send · ping → ripple"));
    v.push(caption(p, 250.0, 560.0, "Click anywhere for a ripple"));
    v
}

fn electric(p: &P) -> Vec<El> {
    let mut v = Vec::new();
    for t in [ARC_A, ARC_B] {
        v.push(circle([t[0], t[1], 9.0]).fill(oklch(0.72, 0.01, 265.0)).border(0.5, oklcha(1.0, 0.0, 0.0, 0.3)).mark(mk(p.sc, 0, node::TERMINALS)));
    }
    v.push(caption(p, ARC_A[0], ARC_A[1] + 40.0, "Arc · click to zap"));
    v.push(fx_card(p, OVERLOAD, 18.0, node::OVERLOAD));
    v.push(label(OVERLOAD.x + 22.0, OVERLOAD.y + 22.0, "Overload", 16.0, 600.0, p.txt));
    v.extend(lines(p, OVERLOAD.x + 22.0, OVERLOAD.y + 56.0, 200.0, 4));
    v.push(caption(p, OVERLOAD.x, OVERLOAD.y + OVERLOAD.h + 26.0, "Border current"));
    v.push(r(CHARGED).key("charged").radius(CHARGED.h * 0.5).fill(hx(0x1b2740)).border(0.5, oklcha(1.0, 0.0, 0.0, 0.14)).shadow(4.0, 10.0, oklcha(0.0, 0.0, 0.0, 0.4)).mark(mk(p.sc, 0, node::CHARGED)));
    v.push(label(CHARGED.x + 86.0, CHARGED.y + 19.0, "Charged", 16.0, 600.0, hx(0xdbe8ff)));
    v.push(caption(p, CHARGED.x, CHARGED.y + CHARGED.h + 26.0, "Hover to charge"));
    v
}

fn fire(p: &P) -> Vec<El> {
    let mut v = vec![
        fx_card(p, FLAMES, 18.0, node::FLAMES),
        label(FLAMES.x + 22.0, FLAMES.y + 24.0, "Hot take", 16.0, 600.0, hx(0xfff1e6)),
    ];
    v.extend(lines(p, FLAMES.x + 22.0, FLAMES.y + 58.0, 190.0, 4));
    v.push(caption(p, FLAMES.x, FLAMES.y + FLAMES.h + 26.0, "Flames + embers"));
    v.push(r(PLASMA).radius(PLASMA.h * 0.5).fill(hx(0x15122a)).border(0.5, hx(0xb9a8ff).alpha(0.25)).mark(mk(p.sc, 0, node::PLASMA)));
    v.push(label(PLASMA.x + 106.0, PLASMA.y + 22.0, "Plasma", 16.0, 600.0, hx(0xe9e4ff)));
    v.push(caption(p, PLASMA.x, PLASMA.y + PLASMA.h + 28.0, "Plasma border"));
    v.push(r(DEPLOY).radius(DEPLOY.h * 0.5).fill(hx(0xf2702a)).border(0.5, oklcha(1.0, 0.0, 0.0, 0.16)).shadow(4.0, 10.0, oklcha(0.0, 0.0, 0.0, 0.4)).mark(mk(p.sc, 0, node::DEPLOY)));
    v.push(label(DEPLOY.x + 62.0, DEPLOY.y + 16.0, "Deploy", 15.0, 600.0, Color::WHITE));
    v.push(caption(p, DEPLOY.x, DEPLOY.y + DEPLOY.h + 28.0, "Heat + haze"));
    v
}

fn motion(s: &Lab, p: &P) -> Vec<El> {
    let mut v = Vec::new();
    // A faint dot grid so travel reads.
    for gy in 0..11 {
        for gx in 0..14 {
            v.push(bx(30.0 + gx as f32 * 55.0, 26.0 + gy as f32 * 55.0, 2.0, 2.0).radius(1.0).fill(oklcha(1.0, 0.0, 0.0, 0.07)).mark(mk(p.sc, 0, 0)));
        }
    }
    // The card sits at its target; the compositor draws it along its track (with blur in flight).
    let c = s.card.target();
    let m = mk(p.sc, 0, node::CARD);
    let (w, h) = (CARD_SIZE[0], CARD_SIZE[1]);
    v.push(
        bx(c.x, c.y, w, h)
            .key("card")
            .radius(14.0)
            .fill(oklch(0.25, 0.008, 265.0))
            .border(0.5, oklcha(1.0, 0.0, 0.0, 0.1))
            .shadow(4.0, 8.0, oklcha(0.0, 0.0, 0.0, 0.35))
            .mark(m)
            .child(bx(12.0, 12.0, 28.0, 28.0).radius(14.0).fill(hx(0x8462fa)))
            .child(label(50.0, 13.0, "Flight 204", 13.0, 600.0, oklch(0.96, 0.003, 265.0)))
            .child(label(50.0, 31.0, "Departing 9:41", 11.0, 400.0, oklch(0.7, 0.01, 265.0)))
            .child(bx(12.0, 66.0, w - 24.0, 6.0).radius(3.0).fill(oklch(0.33, 0.008, 265.0)))
            .child(bx(12.0, 66.0, (w - 24.0) * 0.62, 6.0).radius(3.0).fill(hx(0x22c48a))),
    );
    v.push(caption(p, 300.0, 560.0, "Drag and fling the card"));
    v
}

fn lens(p: &P) -> Vec<El> {
    let tiles = [(0x7c4ff0, "Violet"), (0xf07a2c, "Ember"), (0x22b5a4, "Lagoon"), (0xe0559c, "Orchid")];
    let mut v = Vec::new();
    for (i, (c, n)) in tiles.iter().enumerate() {
        let x = 40.0 + i as f32 * 180.0;
        v.push(bx(x, 60.0, 160.0, 80.0).radius(14.0).fill(hx(*c)).border(0.5, oklcha(1.0, 0.0, 0.0, 0.15)).mark(mk(p.sc, 0, node::PAGE)));
        v.push(label(x + 14.0, 110.0, n, 14.0, 600.0, oklcha(1.0, 0.0, 0.0, 0.95)));
    }
    for (i, l) in [
        "Glass bends what is behind it: the rim squeezes the page,",
        "the middle stays sharp, and colour splits only at the edge.",
        "Rune copies the backdrop once per lens and lights the rim.",
    ]
    .iter()
    .enumerate()
    {
        v.push(label(40.0, 176.0 + i as f32 * 24.0, l, 15.0, 400.0, oklch(0.78, 0.01, 265.0)));
    }
    v.push(r(CRT).radius(14.0).fill(hx(0x0c1910)).mark(mk(p.sc, 0, node::CRT)));
    v.push(mono(CRT.x + 18.0, CRT.y + 22.0, "> rune build --release", 13.0, hx(0x4ade80)));
    v.push(mono(CRT.x + 18.0, CRT.y + 44.0, "  compiling rune-fx", 13.0, hx(0x86efac).alpha(0.8)));
    v.push(mono(CRT.x + 18.0, CRT.y + 66.0, "  finished in 1.84 s", 13.0, hx(0x86efac).alpha(0.8)));
    v.push(bx(CRT.x + 18.0, CRT.y + 96.0, 132.0, 14.0).radius(3.0).fill(hx(0x22c55e)).mark(mk(p.sc, 0, node::CRT)));
    v.push(text("Chromatic").absolute(CHROMA.x + 8.0, CHROMA.y + 6.0).font(TextStyle::new(52.0, 600.0)).color(oklch(0.96, 0.003, 265.0)).mark(mk(p.sc, 0, node::CHROMA)));
    v.push(caption(p, 270.0, 560.0, "Drag the glass · click for a shockwave"));
    v
}

fn extras(p: &P) -> Vec<El> {
    let mut v = vec![fx_card(p, SKELETON, 16.0, 0)];
    let bar = oklch(0.27, 0.006, 265.0);
    let sk = mk(p.sc, 0, node::SKELETON);
    for i in 0..3 {
        let y = SKELETON.y + 24.0 + i as f32 * 62.0;
        v.push(bx(SKELETON.x + 22.0, y, 36.0, 36.0).radius(18.0).fill(bar).mark(sk));
        v.push(bx(SKELETON.x + 72.0, y + 5.0, 190.0 - i as f32 * 28.0, 11.0).radius(5.5).fill(bar).mark(sk));
        v.push(bx(SKELETON.x + 72.0, y + 23.0, 120.0, 9.0).radius(4.5).fill(bar).mark(sk));
    }
    v.push(caption(p, SKELETON.x, SKELETON.y + SKELETON.h + 24.0, "Skeleton shimmer"));
    v.push(text("Thinking…").absolute(THINKING.x + 6.0, THINKING.y + 8.0).font(TextStyle::new(22.0, 500.0)).color(oklch(0.62, 0.01, 265.0)).mark(mk(p.sc, 0, node::THINKING)));
    v.push(caption(p, THINKING.x + 6.0, THINKING.y + THINKING.h + 16.0, "Text sheen"));
    v.push(fx_card(p, AURORA, 18.0, node::AURORA));
    v.push(label(AURORA.x + 22.0, AURORA.y + AURORA.h - 40.0, "Aurora", 19.0, 600.0, oklch(0.97, 0.003, 265.0)));
    v.push(caption(p, AURORA.x, AURORA.y + AURORA.h + 24.0, "Aurora + static grain"));
    v.push(r(PRO).radius(PRO.h * 0.5).fill(hx(0xe9c46a)).border(0.5, oklcha(1.0, 0.0, 0.0, 0.4)).mark(mk(p.sc, 0, node::PRO)));
    v.push(label(PRO.x + 36.0, PRO.y + 11.0, "PRO", 14.0, 600.0, hx(0x3b2a06)));
    v.push(caption(p, PRO.x, PRO.y + PRO.h + 26.0, "Sparkles · click for a burst"));
    v
}

//! An effect drawn by a Rune compositor at an effect slot: inside a faded, rounded layer, in
//! either blend mode, through one renderer serving the frame target and the layer's target.

use std::sync::{Arc, Mutex};

use rune::color::{hex, srgb_decode, srgb_encode};
use rune::compose::{Compositor, Frame};
use rune::render::{EffectPhase, EffectSite, KIND_EFFECT};
use rune::{App, AppSpec, Cx, El, effect, el};
use rune_fx::*;

struct Dusk {
    fx: Option<FxRenderer>,
    formats: Arc<Mutex<Vec<wgpu::TextureFormat>>>,
    effect: fn(u32) -> Fx,
}

fn dusk() -> Fx {
    Fx::fill(MeshGradientParams::dusk())
}

fn glow(occlusion: f32) -> Fx {
    Fx::glow(GlowParams { radius: 12.0, occlusion, ..Default::default() })
}

impl Compositor<()> for Dusk {
    fn render(&mut self, f: &mut Frame<'_, ()>) {
        let (device, queue, size) = (f.device, f.queue, f.size);
        let marks = &f.rt.marks;
        let slots: Vec<(u32, rune::render::Instance, u32)> = f.rt.instances.iter().enumerate()
            .filter(|(_, i)| i.params[1] == KIND_EFFECT)
            .map(|(n, i)| (n as u32, *i, marks.get(n).copied().unwrap_or(1)))
            .collect();
        let layers: Vec<FxLayer> = slots.iter()
            .map(|(_, i, m)| {
                let k = i.params[3];
                let [x, y, w, h] = i.rect.map(|v| v / k);
                let r = i.radii.iter().fold(0.0f32, |a, v| a.max(v.abs())) / k;
                (self.effect)(*m).on(Shape::rounded(x, y, w, h, r)).scale(k).clip(i.clip, i.fade).at(1.0)
            }).collect();
        let fx = self.fx.get_or_insert_with(|| FxRenderer::new(device, f.format, FxOptions::default()));
        fx.set_srgb_blend(f.srgb_blend());
        fx.add_format(device, wgpu::TextureFormat::Rgba16Float);
        fx.prepare(device, queue, &FxFrame { viewport: [size.0, size.1], scale: 1.0, layers: &layers, backdrop: None, sources: &[] });
        let clear = f.rt.clear_color();
        f.ui.prepare(device, queue, &mut f.rt.text.atlas, &f.rt.instances, size);
        let formats = &self.formats;
        f.ui.draw_with_backdrop(f.encoder, f.target, clear, &mut |phase, s: &EffectSite<'_>| match phase {
            EffectPhase::Backdrop(enc) => {
                if let (Some(bd), Some(k)) = (s.backdrop, slots.iter().position(|(at, _, _)| *at == s.index)) {
                    fx.blur_backdrop(device, queue, enc, bd, k);
                }
            }
            EffectPhase::Draw(pass) => {
                let mut seen = formats.lock().unwrap();
                if !seen.contains(&s.format) { seen.push(s.format); }
                if let Some(k) = slots.iter().position(|(at, _, _)| *at == s.index) {
                    assert_eq!(s.scale, slots[k].1.params[3]);
                    fx.draw_layer_in(pass, s.format, s.origin, k);
                }
            }
        });
    }
}

fn snap(name: &'static str, view: fn(&(), &mut Cx<()>) -> El, srgb: bool, formats: Arc<Mutex<Vec<wgpu::TextureFormat>>>) -> (u32, Vec<u8>) {
    snap_with(name, view, srgb, formats, 1, dusk)
}

fn snap_at(name: &'static str, view: fn(&(), &mut Cx<()>) -> El, srgb: bool, formats: Arc<Mutex<Vec<wgpu::TextureFormat>>>, scale: u32) -> (u32, Vec<u8>) {
    snap_with(name, view, srgb, formats, scale, dusk)
}

fn snap_with(name: &'static str, view: fn(&(), &mut Cx<()>) -> El, srgb: bool, formats: Arc<Mutex<Vec<wgpu::TextureFormat>>>, scale: u32, effect: fn() -> Fx) -> (u32, Vec<u8>) {
    let dir = std::env::temp_dir().join(format!("rune-fx-layer-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("snap.png");
    thread_local! { static EFFECT: std::cell::Cell<fn() -> Fx> = const { std::cell::Cell::new(dusk) }; }
    EFFECT.set(effect);
    App::new(AppSpec::new(name, (96.0, 96.0), || (), view))
        .compositor(Dusk { fx: None, formats, effect: |_| EFFECT.get()() })
        .srgb_blend(srgb)
        .args(["--snap".to_string(), path.to_str().unwrap().to_string(), "--scale".to_string(), scale.to_string()])
        .run();
    let mut reader = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap())).read_info().unwrap();
    let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    buf.truncate(info.buffer_size());
    std::fs::remove_dir_all(&dir).unwrap();
    (info.width, buf)
}

fn px(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 3] {
    let at = ((y * width + x) * 4) as usize;
    [rgba[at], rgba[at + 1], rgba[at + 2]]
}

fn background() -> El {
    el().size(96.0, 96.0).fill(hex("#202020").unwrap())
}

fn plain(_: &(), _: &mut Cx<()>) -> El {
    background().child(effect(1).absolute(16.0, 16.0).size(64.0, 64.0))
}

fn faded_rounded(_: &(), _: &mut Cx<()>) -> El {
    background().child(
        el().size(64.0, 64.0).absolute(16.0, 16.0).opacity(0.5).clip_rounded(24.0)
            .child(effect(1).absolute(0.0, 0.0).size(64.0, 64.0))
            .child(el().absolute(48.0, 48.0).size(8.0, 8.0).fill(hex("#0000ff").unwrap())),
    )
}

#[test]
fn fill_in_a_faded_rounded_layer_matches_the_plain_fill_faded() {
    for srgb in [false, true] {
        let (w, plain) = snap("plain", plain, srgb, Arc::default());
        let formats = Arc::default();
        let (_, layered) = snap("layer", faded_rounded, srgb, Arc::clone(&formats));
        let bg = 32.0 / 255.0;
        for (x, y) in [(40, 40), (60, 30)] {
            let fill = px(&plain, w, x, y);
            let want: [u8; 3] = std::array::from_fn(|c| {
                let f = fill[c] as f32 / 255.0;
                let v = if srgb { 0.5 * f + 0.5 * bg } else { srgb_encode(0.5 * srgb_decode(f) + 0.5 * srgb_decode(bg)) };
                (v * 255.0).round() as u8
            });
            let got = px(&layered, w, x, y);
            assert!(got.iter().zip(want).all(|(a, b)| a.abs_diff(b) <= 4), "srgb {srgb} at ({x}, {y}): got {got:?}, want {want:?}");
        }
        let corner = px(&layered, w, 17, 17);
        assert!(corner.iter().all(|c| c.abs_diff(32) <= 2), "corner is clipped away: {corner:?}");
        assert_eq!(formats.lock().unwrap().as_slice(), [wgpu::TextureFormat::Rgba16Float]);
    }
}

#[test]
fn fills_match_across_blend_modes_when_opaque() {
    let (_, linear) = snap("lin", plain, false, Arc::default());
    let (_, gamma) = snap("gam", plain, true, Arc::default());
    let worst = linear.iter().zip(&gamma).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    assert!(worst <= 3, "modes differ by {worst}");
}

fn glow_on_black(_: &(), _: &mut Cx<()>) -> El {
    el().size(96.0, 96.0).fill(hex("#000000").unwrap()).child(effect(1).absolute(32.0, 32.0).size(32.0, 32.0))
}

/// A glow's light is premultiplied colour past its occlusion alpha. In a gamma-blended frame a
/// faint occlusion must barely change it, not encode the light as a straight colour many times
/// full scale and all but black it out.
#[test]
fn faint_occlusion_keeps_the_glow_lit_in_gamma_blend() {
    let (w, clear) = snap_with("glow-clear", glow_on_black, true, Arc::default(), 1, || glow(0.0));
    let (_, faint) = snap_with("glow-faint", glow_on_black, true, Arc::default(), 1, || glow(0.002));
    let mut lit = 0;
    for (x, y) in [(28, 48), (24, 48), (48, 27), (48, 22), (68, 48), (28, 28)] {
        let (a, b) = (px(&clear, w, x, y), px(&faint, w, x, y));
        lit += a.iter().any(|c| *c > 24) as u32;
        assert!(a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 6), "({x}, {y}): clear {a:?}, faint {b:?}");
    }
    assert!(lit >= 3, "the glow should light the samples");
}

fn grain_on_grey(_: &(), _: &mut Cx<()>) -> El {
    el().size(96.0, 96.0).fill(hex("#808080").unwrap()).child(effect(1).absolute(0.0, 0.0).size(96.0, 96.0))
}

/// Grain is texture: over mid-grey it must lighten and darken by the same amount, and leave the
/// mean where it was, in either blend mode.
#[test]
fn grain_is_symmetric_in_both_blend_modes() {
    for srgb in [false, true] {
        let (w, img) = snap_with("grain", grain_on_grey, srgb, Arc::default(), 1, || {
            Fx::grain(GrainParams { amount: 0.06, ..Default::default() })
        });
        let (_, base) = snap_with("grain-base", grain_on_grey, srgb, Arc::default(), 1, || {
            Fx::grain(GrainParams { amount: 0.0, ..Default::default() })
        });
        // Blending happens in linear light when `srgb` is off, so compare in the space the
        // frame blends in; the grain moves each pixel toward white or black by the same fraction.
        let space = |v: u8| if srgb { v as f64 / 255.0 } else { srgb_decode(v as f32 / 255.0) as f64 };
        let (mut up, mut down, mut raw, mut n) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for y in 0..96 {
            for x in 0..w.min(96) {
                let (v, b) = (px(&img, w, x, y)[0], px(&base, w, x, y)[0]);
                let (vs, bs) = (space(v), space(b));
                if vs > bs { up += (vs - bs) / (1.0 - bs) } else { down += (bs - vs) / bs }
                raw += v as f64 - b as f64;
                n += 1.0;
            }
        }
        let (up, down, raw) = (up / n, down / n, raw / n);
        eprintln!("srgb {srgb}: mean fraction toward white {up:.4}, toward black {down:.4}, mean shift {raw:.3}/255");
        assert!(up > 0.005 && down > 0.005, "srgb {srgb}: grain should show, up {up} down {down}");
        assert!((up - down).abs() <= 0.1 * (up + down) / 2.0, "srgb {srgb}: toward white {up}, toward black {down}");
        if srgb {
            assert!(raw.abs() <= 1.0, "srgb {srgb}: mean shifts by {raw:.2}/255");
        }
    }
}

fn zoomed_slot(_: &(), _: &mut Cx<()>) -> El {
    background().child(
        el().absolute(32.0, 32.0).size(32.0, 32.0).live_scale(&rune::Live::new(2.0))
            .child(effect(1).size(32.0, 32.0).radius(8.0)),
    )
}

fn unzoomed_double(_: &(), _: &mut Cx<()>) -> El {
    background().child(effect(1).absolute(16.0, 16.0).size(64.0, 64.0).radius(16.0))
}

#[test]
fn slot_scale_scales_effect_lengths_and_radii_without_bleed() {
    let ring6 = || Fx::fill(FillParams::from(MeshGradientParams::dusk()).border(6.0));
    let ring12 = || Fx::fill(FillParams::from(MeshGradientParams::dusk()).border(12.0));
    let (w, zoomed) = snap_with("zoomed-ring", zoomed_slot, false, Arc::default(), 1, ring6);
    let (_, direct) = snap_with("direct-ring", unzoomed_double, false, Arc::default(), 1, ring12);
    // The 6pt border at 2x live_scale is 12px wide (x = 16..28); x = 22 is inside the ring,
    // x = 48 is the hollow centre.
    assert!(px(&zoomed, w, 22, 48).iter().any(|&c| c > 45), "ring is lit at 12px width");
    assert!(px(&zoomed, w, 48, 48).iter().all(|&c| c.abs_diff(32) <= 2), "centre is hollow");
    let worst = zoomed.iter().zip(&direct).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    assert!(worst <= 3, "zoomed slot and 2x direct ring differ by {worst}");
}

fn rounded_scroll(_: &(), _: &mut Cx<()>) -> El {
    background().child(
        el().scroll(rune::ScrollSpec { stick_bottom: false }).size(64.0, 64.0).absolute(16.0, 16.0)
            .clip_rounded(24.0).clip_fade(0.0, 0.0, 0.0, 16.0)
            .child(el().size(64.0, 96.0).child(effect(1).size(64.0, 64.0))),
    )
}

#[test]
fn fill_under_a_rounded_scroll_container_clips_without_a_layer() {
    for srgb in [false, true] {
        let formats = Arc::default();
        let (w, plain_img) = snap("scroll-plain", plain, srgb, Arc::default());
        let (_, clipped) = snap("scroll-clipped", rounded_scroll, srgb, Arc::clone(&formats));
        let seen = formats.lock().unwrap().clone();
        assert!(!seen.contains(&wgpu::TextureFormat::Rgba16Float), "must not allocate an isolated layer: {seen:?}");
        // Rounded top-left corner at (17, 17) is outside the 24px radius and cut to the #202020 background.
        let corner = px(&clipped, w, 17, 17);
        assert!(corner.iter().all(|c| c.abs_diff(32) <= 2), "srgb {srgb} corner is clipped away: {corner:?}");
        // Straight top edge (48, 17) and centre (48, 48) match the unclipped fill.
        for (x, y) in [(48, 17), (48, 48)] {
            let got = px(&clipped, w, x, y);
            let want = px(&plain_img, w, x, y);
            assert!(got.iter().zip(want).all(|(a, b)| a.abs_diff(b) <= 2), "srgb {srgb} at ({x}, {y}): got {got:?}, want {want:?}");
        }
        // Soft bottom edge (y = 64..80, 16px fade): at y = 72 coverage is ~0.5, between background and fill.
        let mid_fade = px(&clipped, w, 48, 72);
        let full = px(&plain_img, w, 48, 72);
        assert!(mid_fade.iter().zip(full).any(|(m, f)| *m < f.saturating_sub(15)), "srgb {srgb} soft edge fades toward background: {mid_fade:?} vs {full:?}");
        assert!(mid_fade.iter().any(|&c| c > 40), "srgb {srgb} soft edge is not hard-cut at y = 72: {mid_fade:?}");
    }
}

fn snap_by_mark(name: &'static str, view: fn(&(), &mut Cx<()>) -> El, srgb: bool, formats: Arc<Mutex<Vec<wgpu::TextureFormat>>>, effect: fn(u32) -> Fx) -> (u32, Vec<u8>) {
    let dir = std::env::temp_dir().join(format!("rune-fx-layer-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("snap.png");
    App::new(AppSpec::new(name, (96.0, 96.0), || (), view))
        .compositor(Dusk { fx: None, formats, effect })
        .srgb_blend(srgb)
        .args(["--snap".to_string(), path.to_str().unwrap().to_string(), "--scale".to_string(), "1".to_string()])
        .run();
    let mut reader = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap())).read_info().unwrap();
    let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    buf.truncate(info.buffer_size());
    std::fs::remove_dir_all(&dir).unwrap();
    (info.width, buf)
}

fn single_glass_scene(_: &(), _: &mut Cx<()>) -> El {
    background()
        .child(el().absolute(0.0, 0.0).size(48.0, 96.0).fill(hex("#00e5ff").unwrap()))
        .child(effect(1).effect_backdrop(true).absolute(16.0, 16.0).size(64.0, 64.0))
}

fn stacked_glass_scene(_: &(), _: &mut Cx<()>) -> El {
    background()
        .child(el().absolute(0.0, 0.0).size(48.0, 96.0).fill(hex("#00e5ff").unwrap()))
        .child(effect(1).effect_backdrop(true).absolute(16.0, 16.0).size(64.0, 64.0))
        .child(el().absolute(40.0, 32.0).size(16.0, 32.0).fill(hex("#ff2040").unwrap()))
        .child(effect(2).effect_backdrop(true).absolute(24.0, 24.0).size(56.0, 48.0))
        .child(el().absolute(68.0, 60.0).size(10.0, 10.0).fill(hex("#00ff00").unwrap()))
}

fn frosted_glass_fx(mark: u32) -> Fx {
    match mark {
        1 => Fx::lens(LensParams { frost: 0.85, depth: 0.0, magnification: 1.0, shadow: 0.0, rim: 0.0, ..Default::default() }),
        _ => Fx::fill(FlutedGlassParams { frost: 0.85, refraction: 0.0, dispersion: 0.0, highlight: 0.0, ..FlutedGlassParams::frosted() }),
    }
}

#[test]
fn backdrop_slots_blur_background_and_stack_glass_over_glass_in_painter_order() {
    let (w, one) = snap_by_mark("one-glass", single_glass_scene, false, Arc::default(), frosted_glass_fx);
    // At (54, 48), just to the right of the x=48 cyan stripe, frosted glass 1 blurs cyan into the dark half.
    let p_one = px(&one, w, 54, 48);
    assert!(p_one[1] > 60 && p_one[2] > 70, "glass 1 blurs cyan across the x=48 edge: {p_one:?}");

    let (_, stacked) = snap_by_mark("stacked-glass", stacked_glass_scene, false, Arc::default(), frosted_glass_fx);
    // At (60, 48), to the right of the red bar (x=40..56) drawn between glass 1 and glass 2,
    // glass 2 blurs both glass 1's cyan and the intermediate red bar, raising the red channel over `one`.
    let p_two = px(&stacked, w, 60, 48);
    let p_one_60 = px(&one, w, 60, 48);
    assert!(p_two[0] > p_one_60[0] + 15, "glass 2 sees the intermediate red bar and glass 1: {p_two:?} vs {p_one_60:?}");
    // The green badge at (72, 64) is drawn after glass 2, so it stays sharp green on top.
    let badge = px(&stacked, w, 72, 64);
    assert!(badge[0] < 10 && badge[1] > 245 && badge[2] < 10, "later content stays sharp on top of glass 2: {badge:?}");
}

fn layered_glass_scene(_: &(), _: &mut Cx<()>) -> El {
    background().child(
        el().absolute(16.0, 16.0).size(64.0, 64.0).opacity(0.5)
            .child(el().absolute(0.0, 0.0).size(32.0, 64.0).fill(hex("#00e5ff").unwrap()))
            .child(effect(1).effect_backdrop(true).absolute(0.0, 0.0).size(64.0, 64.0)),
    )
}

#[test]
fn backdrop_slot_in_an_isolated_layer_reads_and_blurs_that_layers_target() {
    let formats = Arc::default();
    let (w, img) = snap_by_mark("layer-glass", layered_glass_scene, false, Arc::clone(&formats), frosted_glass_fx);
    let seen = formats.lock().unwrap().clone();
    assert!(seen.contains(&wgpu::TextureFormat::Rgba16Float), "isolated layer uses Rgba16Float: {seen:?}");
    // Inside the isolated layer, the cyan stripe covers x = 16..48. At (52, 48) (right half of the
    // layer, where nothing was drawn before the slot), the frosted lens blurs the layer's cyan stripe
    // into the right half, and the layer composites at 0.5 opacity over #202020.
    let blurred_right = px(&img, w, 52, 48);
    assert!(blurred_right[1] > 45 && blurred_right[2] > 50, "layer backdrop slot blurs the layer's stripe: {blurred_right:?}");
    // Inside the cyan stripe at (24, 48), the 0.5-opacity layer is dimmer than full #00e5ff.
    let faded_left = px(&img, w, 24, 48);
    assert!(faded_left[2] > 100 && faded_left[2] < 220, "isolated layer composites at 0.5 opacity: {faded_left:?}");
}

/// Writes `demo/fx/in-layer.png`: a fill in a faded, rounded, clipped card, linear blend on the
/// left and gamma blend on the right. `cargo test -p rune-fx --test in_layer -- --ignored`.
#[test]
#[ignore]
fn write_demo_still() {
    let (w, linear) = snap_at("demo-lin", faded_rounded, false, Arc::default(), 3);
    let (_, gamma) = snap_at("demo-gam", faded_rounded, true, Arc::default(), 3);
    let h = linear.len() as u32 / 4 / w;
    let mut out = Vec::with_capacity(linear.len() * 2);
    for y in 0..h as usize {
        let row = y * w as usize * 4..(y + 1) * w as usize * 4;
        out.extend_from_slice(&linear[row.clone()]);
        out.extend_from_slice(&gamma[row]);
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../demo/fx/in-layer.png");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(path).unwrap()), w * 2, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&out).unwrap();
}

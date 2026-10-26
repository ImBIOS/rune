//! A runtime the app draws itself (`Runtime::with_text`, a headless snapshot) can blend in sRGB
//! with `Runtime::set_srgb_blend`, hover and press tints included, as an `App::srgb_blend`
//! window does.
use rune::color::over;
use rune::render::{Gpu, OFFSCREEN_FORMAT, Offscreen, Renderer};
use rune::text::TextSystem;
use rune::{Color, Cx, El, Env, Runtime, el};

const RED: Color = Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };

fn view(_: &(), _: &mut Cx<()>) -> El {
    el().size(40.0, 40.0).fill(Color::BLACK)
        .child(el().size(20.0, 20.0).fill(RED).hover(Color::WHITE.alpha(0.5)).on_press(|_: &mut ()| {}))
}

fn hovered(srgb: bool) -> Runtime<()> {
    let text = TextSystem::new(&[], ("Inter", "Inter"));
    let mut rt = Runtime::with_text((), view, (40.0, 40.0), Env::new(true, 1.0), text);
    rt.set_srgb_blend(srgb);
    rt.frame(0.0);
    rt.pointer_move(10.0, 10.0);
    rt.frame(1.0 / 60.0);
    rt
}

/// The child's fill, with the hover tint composited over it.
fn tinted(rt: &Runtime<()>) -> [f32; 4] {
    rt.instances.iter().find(|i| i.rect[2] == 20.0 && i.rect[3] == 20.0).expect("the child's box").fill
}

fn pixel(gpu: &Gpu, rt: &mut Runtime<()>) -> [u8; 4] {
    let format = if rt.srgb_blend() { OFFSCREEN_FORMAT.remove_srgb_suffix() } else { OFFSCREEN_FORMAT };
    let mut renderer = Renderer::new(&gpu.device, format);
    renderer.set_srgb_blend(rt.srgb_blend());
    let target = Offscreen::with_format(&gpu.device, 40, 40, format);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (40, 40));
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    renderer.draw(&mut encoder, &target.view, rt.clear_color());
    target.copy(&mut encoder);
    renderer.submit(&gpu.queue, encoder);
    let px = target.read(&gpu.device);
    let at = (10 * 40 + 10) * 4;
    [px[at], px[at + 1], px[at + 2], px[at + 3]]
}

#[test]
fn hover_tints_follow_the_runtime_blend_mode() {
    let tint = Color::WHITE.alpha(0.5);
    let srgb = hovered(true);
    assert!(srgb.srgb_blend());
    assert_eq!(tinted(&srgb), over(tint.premul_srgb(1.0), RED.premul_srgb(1.0)), "composited in sRGB encoding");
    let linear = hovered(false);
    assert_eq!(tinted(&linear), over(tint.premul(1.0), RED.premul(1.0)), "composited linearly");
}

#[test]
fn a_self_drawn_runtime_renders_gamma_blended_pixels() {
    let gpu = Gpu::headless();
    let near = |got: [u8; 4], want: [u8; 4]| assert!(got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 1), "{got:?} != {want:?}");
    // Half white over red: 128 in sRGB space, as CSS draws it; linear light reads 188.
    near(pixel(&gpu, &mut hovered(true)), [255, 128, 128, 255]);
    near(pixel(&gpu, &mut hovered(false)), [255, 188, 188, 255]);
}

#[test]
fn switching_blend_mode_redraws() {
    let mut rt = hovered(false);
    rt.frame(1.0);
    let commits = rt.stats.commits;
    rt.set_srgb_blend(true);
    rt.frame(1.0 / 60.0);
    assert!(rt.stats.commits > commits, "a change asks for a frame");
    assert_eq!(tinted(&rt), over(Color::WHITE.alpha(0.5).premul_srgb(1.0), RED.premul_srgb(1.0)));
    let commits = rt.stats.commits;
    rt.set_srgb_blend(true);
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.stats.commits, commits, "the same mode is no change");
}

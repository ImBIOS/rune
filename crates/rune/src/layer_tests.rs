use super::*;
use crate::{Color, Cx, El, Env, Live, Runtime, el};

pub(super) fn rect(bounds: [f32; 4], color: [f32; 4]) -> Instance {
    Instance { rect: bounds, fill: color, clip: NO_CLIP, params: [0.0, KIND_RECT, 0.0, 1.0], ..Instance::zeroed() }
}

fn group(bounds: [f32; 4], opacity: f32, color: [f32; 4]) -> Vec<Instance> {
    vec![Instance::begin_layer(bounds, NO_CLIP, LayerSpec { opacity, ..Default::default() }),
        rect(bounds, color), rect([bounds[0] + bounds[2] * 0.25, bounds[1], bounds[2] * 0.5, bounds[3]], color),
        Instance::end_layer()]
}

pub(super) fn pixels(gpu: &Gpu, renderer: &Renderer, target: &Offscreen) -> Vec<u8> {
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    renderer.draw(&mut enc, &target.view, [0.0; 4]);
    target.copy(&mut enc);
    renderer.submit(&gpu.queue, enc);
    target.read(&gpu.device)
}

pub(super) fn rgba(linear: [f32; 4]) -> [u8; 4] {
    std::array::from_fn(|i| {
        let v = linear[i];
        let v = if i == 3 { v } else if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (v.clamp(0.0, 1.0) * 255.0).round() as u8
    })
}

pub(super) fn near(got: &[u8], want: [u8; 4]) {
    assert!(got.iter().zip(want).all(|(&a,b)| a.abs_diff(b) <= 1), "{got:?} != {want:?}");
}

#[test]
fn isolated_layers_match_scalar_overlap_crossfade_and_nesting() {
    let gpu = Gpu::headless();
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let target = Offscreen::new(&gpu.device, 32, 16);
    let mut atlas = crate::text::TextSystem::default().atlas;
    let bounds = [4.0, 4.0, 24.0, 8.0];
    let mut instances = group(bounds, 0.5, [1.0; 4]);
    renderer.prepare(&gpu.device, &gpu.queue, &mut atlas, &instances, (32, 16));
    let out = pixels(&gpu, &renderer, &target);
    for x in [6, 16, 26] { near(&out[(8 * 32 + x) * 4..(8 * 32 + x + 1) * 4], rgba([0.5; 4])); }
    assert_eq!(renderer.layer_stats().targets, 1);
    assert_eq!(renderer.layer_stats().bytes, 64 * 64 * 8, "minimum bucket");
    let allocations = renderer.layer_stats().allocations;

    // Two opaque states, each with internal overlap. Source-over crossfade is B(1-t) over
    // A(t): alpha = 1-t+t², NOT 1. Internal child overlap must never alter that oracle.
    for t in [0.25, 0.5, 0.75] {
        instances = group(bounds, t, [1.0, 0.0, 0.0, 1.0]);
        instances.extend(group(bounds, 1.0 - t, [0.0, 0.0, 1.0, 1.0]));
        renderer.prepare(&gpu.device, &gpu.queue, &mut atlas, &instances, (32, 16));
        let out = pixels(&gpu, &renderer, &target);
        let want = rgba([t * t, 0.0, 1.0 - t, 1.0 - t + t * t]);
        for x in [6, 16, 26] { near(&out[(8 * 32 + x) * 4..(8 * 32 + x + 1) * 4], want); }
    }
    assert_eq!(renderer.layer_stats().allocations, allocations + 1, "reuse targets while opacity changes");

    instances = vec![Instance::begin_layer(bounds, NO_CLIP, LayerSpec { opacity: 0.5, ..Default::default() })];
    instances.extend(group(bounds, 0.5, [1.0; 4]));
    instances.push(Instance::end_layer());
    renderer.prepare(&gpu.device, &gpu.queue, &mut atlas, &instances, (32, 16));
    let out = pixels(&gpu, &renderer, &target);
    near(&out[(8 * 32 + 16) * 4..(8 * 32 + 17) * 4], rgba([0.25; 4]));

    instances = group(bounds, 0.5, [1.0; 4]);
    for i in &mut instances[..3] { i.clip = [8.0, 5.0, 24.0, 11.0]; }
    renderer.prepare(&gpu.device, &gpu.queue, &mut atlas, &instances, (32, 16));
    assert_eq!(renderer.layer_stats().bytes, 64 * 64 * 8, "clip uses the minimum bucket");
    assert_eq!(renderer.layers.plan[0].bounds, [8.0, 5.0, 16.0, 6.0]);
    let out = pixels(&gpu, &renderer, &target);
    near(&out[(8 * 32 + 6) * 4..(8 * 32 + 7) * 4], [0; 4]);
    near(&out[(8 * 32 + 16) * 4..(8 * 32 + 17) * 4], rgba([0.5; 4]));

    for alpha in [1.0, 0.0] {
        instances = group(bounds, alpha, [1.0; 4]);
        renderer.prepare(&gpu.device, &gpu.queue, &mut atlas, &instances, (32, 16));
        let out = pixels(&gpu, &renderer, &target);
        near(&out[(8 * 32 + 16) * 4..(8 * 32 + 17) * 4], rgba([alpha; 4]));
        assert_eq!(renderer.layer_stats().targets, 0, "release on both endpoints");
        assert_eq!(renderer.layer_stats().extra_passes, 0);
    }
    // Fully opaque streams allocate no planner storage and issue only the ordinary pass.
    let mut ordinary = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    ordinary.prepare(&gpu.device, &gpu.queue, &mut atlas, &[rect(bounds, [1.0; 4])], (32, 16));
    assert_eq!(ordinary.layer_stats(), LayerStats::default());
    assert_eq!(ordinary.layers.plan.capacity(), 0);
    assert_eq!(ordinary.layers.targets.capacity(), 0);
    assert_eq!(ordinary.layers.stack.capacity(), 0);
    assert_eq!(ordinary.layers.pipelines.capacity(), 0);
}

#[test]
fn leaf_dimming_keeps_container_opacity_isolated() {
    fn view(alpha: &Live<f32>, _: &mut Cx<Live<f32>>) -> El {
        el().size(32.0, 16.0).live_opacity(alpha)
            .child(el().size(24.0, 8.0).absolute(4.0, 4.0).fill(Color::WHITE))
            .child(el().size(12.0, 8.0).absolute(10.0, 4.0).fill(Color::WHITE))
            .dim(0.5)
    }
    let gpu = Gpu::headless();
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let target = Offscreen::new(&gpu.device, 32, 16);
    let mut rt = Runtime::new(Live::new(0.5), view, (32.0, 16.0), Env { scale: 1.0, system_dark: true });
    for (alpha, targets) in [(0.5, 1), (1.0, 0)] {
        rt.state.set(alpha); rt.frame(0.0);
        renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (32, 16));
        assert_eq!(renderer.layer_stats().targets, targets);
        let out = pixels(&gpu, &renderer, &target);
        near(&out[(8*32+6)*4..(8*32+7)*4], rgba([0.5 * alpha; 4]));
        near(&out[(8*32+16)*4..(8*32+17)*4], rgba([0.75 * alpha; 4]));
    }
}

#[test]
fn dissolve_keeps_opaque_coverage_and_isolates_each_state() {
    type State = (Live<f32>, Live<f32>);
    fn view(s: &State, _: &mut Cx<State>) -> El {
        let state = |alpha: &Live<f32>, color: Color| {
            el().size(24.0, 8.0).absolute(4.0, 4.0).live_opacity(alpha)
                .child(el().size(24.0, 8.0).fill(color))
                .child(el().size(12.0, 8.0).absolute(6.0, 0.0).fill(color))
        };
        el().size(32.0, 16.0).dissolve()
            .child(state(&s.0, Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }))
            .child(state(&s.1, Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 }))
    }
    let gpu = Gpu::headless();
    // Linear destination makes the exact colour-average assertion independent of sRGB encoding.
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut renderer = Renderer::new(&gpu.device, format);
    let target = Offscreen::with_format(&gpu.device, 32, 16, format);
    let mut rt = Runtime::new((Live::new(0.5), Live::new(0.5)), view, (32.0, 16.0), Env { scale: 1.0, system_dark: true });
    for t in [0.5, 0.0, 0.25, 0.75, 1.0] {
        rt.state.0.set(1.0 - t); rt.state.1.set(t); rt.frame(0.0);
        renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (32, 16));
        if t == 0.0 || t == 1.0 {
            assert!(rt.instances.iter().all(|i| i.params[1] != KIND_LAYER), "no dissolve scope at rest");
            assert_eq!(renderer.layer_stats().extra_passes, 0);
        } else {
            assert!(renderer.layers.targets[0].is_some(), "mid-fade dissolve must isolate");
        }
        let out = pixels(&gpu, &renderer, &target);
        let want = [((1.0-t)*255.0).round() as u8, 0, (t*255.0).round() as u8, 255];
        for x in [6, 16, 26] {
            near(&out[(8*32+x)*4..(8*32+x+1)*4], want);
            assert_eq!(out[(8*32+x)*4+3], 255, "opaque coverage at t={t}");
        }
        near(&out[..4], [0; 4]);
    }
    // The first/last isolated frame must not introduce a rasterization jump. These deltas
    // are below an output byte but above zero, so they exercise both sides of scope selection.
    for (rest, moving) in [(0.0, 0.00001), (1.0, 0.99999)] {
        let mut frames = Vec::new();
        for t in [rest, moving] {
            rt.state.0.set(1.0 - t); rt.state.1.set(t); rt.frame(0.0);
            renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (32, 16));
            assert_eq!(renderer.layer_stats().targets > 0, t == moving);
            frames.push(pixels(&gpu, &renderer, &target));
        }
        assert_eq!(frames[0], frames[1], "scope boundary at {rest}");
    }
    // A sole multi-draw state at opacity 1 still composites internally with source-over.
    // Using translucent primitives distinguishes correct isolation from additive saturation.
    let bounds = [4.0, 4.0, 24.0, 8.0];
    let mut all = vec![Instance::begin_layer(bounds, NO_CLIP, LayerSpec { blend: Blend::PlusLighter, ..Default::default() })];
    all.extend(group(bounds, 1.0, [0.5, 0.0, 0.0, 0.5]));
    all.push(Instance::end_layer());
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &all, (32, 16));
    let out = pixels(&gpu, &renderer, &target);
    near(&out[(8*32+6)*4..(8*32+7)*4], [128,0,0,128]);
    near(&out[(8*32+16)*4..(8*32+17)*4], [191,0,0,191]);
    // Saturate colour as well as alpha, before either the dissolve or enclosing group's fade.
    let nested = [Instance::begin_layer(bounds, NO_CLIP, LayerSpec { opacity: 0.5, ..Default::default() }),
        Instance::begin_layer(bounds, NO_CLIP, LayerSpec { opacity: 0.5, blend: Blend::PlusLighter, ..Default::default() }),
        rect(bounds, [1.0; 4]), rect(bounds, [1.0; 4]), Instance::end_layer(), Instance::end_layer()];
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &nested, (32, 16));
    let out = pixels(&gpu, &renderer, &target);
    near(&out[(8*32+16)*4..(8*32+17)*4], [64; 4]);
}

#[test]
fn dissolve_samples_sequence_opacity_before_selecting_scopes() {
    fn view(_: &(), _: &mut Cx<()>) -> El {
        const OUT: &[crate::Beat] = &[crate::Beat::new(0.0),
            crate::Beat::new(1.0).opacity(0.0).easing(crate::Easing::LINEAR)];
        const IN: &[crate::Beat] = &[crate::Beat::new(0.0).opacity(0.0),
            crate::Beat::new(1.0).easing(crate::Easing::LINEAR)];
        el().size(32.0, 16.0).dissolve()
            .child(el().size(32.0, 16.0).absolute(0.0, 0.0).fill(Color::WHITE).sequence(crate::Sequence::new(OUT)))
            .child(el().size(32.0, 16.0).absolute(0.0, 0.0).fill(Color::WHITE).sequence(crate::Sequence::new(IN)))
    }
    let gpu = Gpu::headless();
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let target = Offscreen::new(&gpu.device, 32, 16);
    let mut rt = Runtime::new((), view, (32.0, 16.0), Env::new(true, 1.0));
    for (dt, active) in [(0.0, false), (0.5, true), (0.5, false)] {
        rt.frame(dt);
        assert_eq!(rt.instances.iter().any(|i| i.params[1] == KIND_LAYER), active);
        renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (32, 16));
        let out = pixels(&gpu, &renderer, &target);
        assert!(out.chunks_exact(4).all(|p| p[3] == 255), "complementary sequences retain opaque coverage");
    }
}

#[test]
fn dissolve_adds_direct_primitives_and_native_surfaces_then_clamps_before_opacity() {
    use crate::native_surface::{ByteFrame, ColorInfo, Format, NativeSurface, SurfaceDraw, SurfaceFit};
    let gpu = Gpu::headless();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut renderer = Renderer::new(&gpu.device, format);
    let target = Offscreen::with_format(&gpu.device, 32, 16, format);
    let mut atlas = crate::text::TextSystem::default().atlas;
    let bounds = [4.0, 4.0, 24.0, 8.0];
    let content = [rect([0.0, 0.0, 32.0, 16.0], [0.0, 1.0, 0.0, 1.0]),
        Instance::begin_layer(bounds, NO_CLIP, LayerSpec { opacity: 0.5, blend: Blend::PlusLighter, ..Default::default() }),
        rect(bounds, [1.0, 0.0, 0.0, 1.0]), Instance::end_layer()];
    renderer.prepare(&gpu.device, &gpu.queue, &mut atlas, &content, (32, 16));
    let surface = NativeSurface::new();
    surface.push_bytes(ByteFrame { width: 1, height: 1, format: Format::Bgra, color: ColorInfo::default(),
        planes: [vec![255, 0, 0, 255], vec![]] }, 1).unwrap();
    renderer.prepare_surfaces(&gpu.device, &gpu.queue, &[SurfaceDraw { surface, fit: SurfaceFit::Fill,
        before_instance: 3, rect: bounds, radii: [0.0; 4], clip: NO_CLIP, fade: [0; 4], opacity: 1.0 }], (32, 16));
    let out = pixels(&gpu, &renderer, &target);
    // Red + blue saturates alpha to 1, then .5 opacity composites over green with source-over.
    near(&out[(8*32+16)*4..(8*32+17)*4], [128,128,128,255]);
    near(&out[..4], [0,255,0,255]);
}

#[test]
fn scale_and_fade_reuse_bucket_for_sixty_growing_frames() {
    type State = (Live<f32>, Live<f32>);
    fn view(s: &State, _: &mut Cx<State>) -> El {
        el().size(256.0, 256.0).child(el().size(180.0, 180.0)
            .live_scale(&s.0).live_opacity(&s.1)
            .child(el().size(180.0, 180.0).fill(Color::WHITE))
            .child(el().size(90.0, 90.0).fill(Color::WHITE)))
    }
    let gpu = Gpu::headless();
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let target = Offscreen::new(&gpu.device, 256, 256);
    let mut rt = Runtime::new((Live::new(0.9), Live::new(0.1)), view, (256.0, 256.0), Env { scale: 1.0, system_dark: true });
    rt.frame(0.0);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (256, 256));
    let allocations = renderer.layer_stats().allocations;
    let first_bounds = renderer.layers.plan[0].bounds;
    assert_eq!(allocations, 1);
    for frame in 1..=60 {
        let t = frame as f32 / 60.0;
        rt.state.0.set(0.9 + 0.1 * t);
        rt.state.1.set(0.1 + 0.8 * t);
        rt.frame(1.0 / 60.0);
        renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (256, 256));
        let out = pixels(&gpu, &renderer, &target);
        near(&out[(90 * 256 + 90) * 4..(90 * 256 + 91) * 4], rgba([0.1 + 0.8 * t; 4]));
        assert_eq!(renderer.layer_stats().allocations, allocations, "frame {frame}");
    }
    assert!(renderer.layers.plan[0].bounds[2] > first_bounds[2]);
    // A genuine shrink drops an oversized allocation, then the tiny minimum bucket stays put.
    for size in [20.0, 19.0, 20.0] {
        let content = group([4.0, 4.0, size, size], 0.5, [1.0; 4]);
        renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &content, (256, 256));
        assert_eq!(renderer.layer_stats().bytes, 64 * 64 * 8);
        assert_eq!(renderer.layer_stats().allocations, allocations + 1);
    }
}

#[test]
fn live_group_opacity_changes_scopes_without_view_or_layout_work() {
    fn view(alpha: &Live<f32>, _: &mut Cx<Live<f32>>) -> El {
        el().size(32.0, 16.0).live_opacity(alpha)
            .child(el().size(24.0, 12.0).fill(Color::WHITE))
            .child(el().size(24.0, 12.0).pin(Some(4.0), Some(0.0), None, None).fill(Color::WHITE))
    }
    let alpha = Live::new(1.0);
    let mut rt = Runtime::new(alpha.clone(), view, (32.0, 16.0), Env { scale: 1.0, system_dark: true });
    rt.frame(0.0);
    let commits = rt.stats.commits;
    let original = bytemuck::cast_slice::<Instance, u8>(&rt.instances).to_vec();
    alpha.set(0.5);
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.stats.commits, commits);
    assert_eq!(rt.instances.first().unwrap().params[1], KIND_LAYER);
    assert_eq!(rt.instances.last().unwrap().params[1], KIND_LAYER_END);
    assert_eq!(rt.instances[1].fill[3], 1.0);
    alpha.set(0.0);
    rt.frame(1.0 / 60.0);
    assert!(rt.instances.is_empty());
    alpha.set(0.0009);
    rt.frame(1.0 / 60.0);
    assert!(rt.instances.is_empty(), "hide sub-threshold tails");
    alpha.set(1.0);
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.stats.commits, commits);
    assert_eq!(bytemuck::cast_slice::<Instance, u8>(&rt.instances), original);
}

#[test]
fn isolated_exit_descendants_stay_inside_their_fading_parent() {
    fn view(show: &bool, _: &mut Cx<bool>) -> El {
        let mut fading = el().key("fading").size(24.0, 16.0).opacity(0.5)
            .child(el().key("kept").size(16.0, 16.0).fill(Color::WHITE));
        if *show { fading = fading.child(el().key("leaving").size(16.0, 16.0).fill(Color::WHITE)); }
        el().size(32.0, 16.0).child(fading)
            .child(el().key("sibling").size(8.0, 16.0).pin(Some(24.0), Some(0.0), None, None).fill(Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 }))
    }
    let mut rt = Runtime::new(true, view, (32.0, 16.0), Env { scale: 1.0, system_dark: true });
    rt.frame(0.0);
    rt.state = false; rt.invalidate(); rt.frame(0.0);
    let start = rt.instances.iter().position(|i| i.params[1] == KIND_LAYER).unwrap();
    let end = rt.instances.iter().position(|i| i.params[1] == KIND_LAYER_END).unwrap();
    assert_eq!(end - start, 3, "both retained descendants share the scope");
    assert_eq!(rt.instances[end + 1].fill, [0.0, 0.0, 1.0, 1.0], "sibling paints outside the group");
}

#[test]
fn single_leaf_groups_keep_the_direct_path_and_springs_stay_live() {
    fn leaf(alpha: &Live<f32>, _: &mut Cx<Live<f32>>) -> El {
        el().size(32.0, 16.0).live_opacity(alpha).child(el().size(16.0, 16.0).fill(Color::WHITE))
    }
    fn group_view(alpha: &Live<f32>, _: &mut Cx<Live<f32>>) -> El {
        el().size(32.0, 16.0).live_opacity(alpha.smooth(crate::Spring::DEFAULT))
            .child(el().size(16.0, 16.0).fill(Color::WHITE))
            .child(el().size(16.0, 16.0).pin(Some(8.0), Some(0.0), None, None).fill(Color::WHITE))
    }
    let alpha = Live::new(0.5);
    let env = Env { scale: 1.0, system_dark: true };
    let mut rt = Runtime::new(alpha.clone(), leaf, (32.0, 16.0), env);
    rt.frame(0.0);
    assert_eq!(rt.instances.len(), 1);
    assert_eq!(rt.instances[0].fill[3], 0.5);
    alpha.set(1.0);
    let mut rt = Runtime::new(alpha.clone(), group_view, (32.0, 16.0), env);
    rt.frame(0.0);
    let commits = rt.stats.commits;
    alpha.set(0.5);
    rt.frame(1.0 / 60.0);
    let a = rt.instances[0].fill[3];
    rt.frame(1.0 / 60.0);
    let b = rt.instances[0].fill[3];
    assert_eq!(rt.instances[0].params[1], KIND_LAYER);
    assert!(b < a && b > 0.5, "spring samples change the group each presentation: {a} -> {b}");
    assert_eq!(rt.stats.commits, commits);
}

#[test]
fn clipped_single_draw_fades_do_not_allocate_targets() {
    use crate::native_surface::{ByteFrame, ColorInfo, Format, NativeSurface, SurfaceFit};
    type State = (Option<NativeSurface>, u8);
    fn view(s: &State, _: &mut Cx<State>) -> El {
        let child = el().size(64.0, 48.0).absolute(-8.0, -8.0);
        let child = if let Some(surface) = &s.0 { child.native_surface(surface, SurfaceFit::Fill) }
            else { child.fill(Color::WHITE) };
        let clip = el().size(48.0, 32.0).absolute(8.0, 8.0);
        let clip = match s.1 {
            0 => clip.clip_soft(0.0),
            1 => clip.clip_fade(4.0, 4.0, 4.0, 4.0),
            _ => clip.scroll(crate::ScrollSpec { stick_bottom: false }),
        };
        el().child(clip.child(el().size(48.0, 32.0).opacity(0.5).child(child)))
    }
    let gpu = Gpu::headless();
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let target = Offscreen::new(&gpu.device, 64, 48);
    let surface = NativeSurface::new();
    surface.push_bytes(ByteFrame { width: 1, height: 1, format: Format::Bgra, color: ColorInfo::default(),
        planes: [vec![255; 4], vec![]] }, 1).unwrap();
    for mode in 0..3 {
        let mut frames = Vec::new();
        for video in [false, true] {
            let mut rt = Runtime::new((video.then(|| surface.clone()), mode), view, (64.0, 48.0), Env::new(true, 1.0));
            rt.frame(0.0);
            assert!(rt.instances.iter().all(|i| i.params[1] != KIND_LAYER));
            renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (64, 48));
            renderer.prepare_surfaces(&gpu.device, &gpu.queue, &rt.surfaces, (64, 48));
            assert_eq!(renderer.layer_stats().targets, 0, "mode={mode} video={video}");
            let out = pixels(&gpu, &renderer, &target);
            assert_eq!(out[(24 * 64 + 32) * 4 + 3], 128);
            assert_eq!(out[(24 * 64 + 4) * 4 + 3], 0);
            if mode == 1 { assert!(out[(24 * 64 + 8) * 4 + 3] < 32); }
            frames.push(out);
        }
        assert_eq!(frames[0], frames[1], "primitive and video inherit identical clips/fades");
    }
}

#[test]
fn opacity_motion_samples_group_alpha_until_the_terminal_frame() {
    fn view(show: &bool, _: &mut Cx<bool>) -> El {
        el().size(32.0, 16.0).opacity(if *show { 1.0 } else { 0.0 })
            .opacity_motion(rune_motion::Curve::ease_in_out(0.2))
            .child(el().size(16.0, 16.0).fill(Color::WHITE))
            .child(el().size(16.0, 16.0).pin(Some(8.0), Some(0.0), None, None).fill(Color::WHITE))
    }
    let mut rt = Runtime::new(true, view, (32.0, 16.0), Env { scale: 1.0, system_dark: true });
    rt.frame(0.0);
    rt.state = false; rt.invalidate(); rt.frame(0.0);
    let commits = rt.stats.commits;
    rt.frame(0.05);
    let first = rt.instances[0].fill[3];
    assert_eq!(rt.instances[0].params[1], KIND_LAYER);
    rt.frame(0.05);
    assert!(rt.instances[0].fill[3] < first);
    assert_eq!(rt.instances[1].fill[3], 1.0);
    for _ in 0..24 { rt.frame(1.0 / 60.0); }
    assert!(rt.instances.is_empty());
    assert_eq!(rt.stats.commits, commits);
}

#[test]
fn isolated_layers_preserve_image_video_spring_and_list_order() {
    use crate::native_surface::{ByteFrame, ColorInfo, Format, NativeSurface, SurfaceDraw, SurfaceFit};
    use crate::gpu_springs::{Node, Track};
    use crate::pixels::Pixels;
    let gpu = Gpu::headless();
    let bounds = [4.0, 4.0, 24.0, 8.0];
    let mut content = group(bounds, 0.5, [1.0, 0.0, 0.0, 1.0]);
    // A glyph, a vector and a page-1 image inside the nonzero-origin layer. Absolute spring
    // indices differ from local draw indices, so a shifted subrange would use the wrong node.
    let mut atlas = crate::text::TextSystem::default().atlas;
    atlas.data[0] = 255; atlas.dirty = Some((0, 1));
    content.insert(3, Instance { rect: [9.0, 6.0, 1.0, 1.0], fill: [1.0; 4], clip: NO_CLIP,
        params: [0.0, KIND_GLYPH, 0.0, 1.0], ..Instance::zeroed() });
    let px = Pixels::new(); px.set(1, 1, vec![0, 255, 0, 255]);
    let mut images = crate::pixels::ImageAtlas::default();
    let full_page = Pixels::new(); full_page.set(1024, 1024, vec![255; 1024 * 1024 * 4]);
    images.place(&full_page, [1024.0; 2]).unwrap();
    let image = images.place(&px, [2.0, 2.0]).unwrap();
    assert_eq!(image.page, 1);
    content.insert(4, Instance { rect: [15.0, 6.0, 2.0, 2.0], fill: [0.0, 0.0, 0.0, 1.0],
        params: [image.page as f32, KIND_IMAGE, 0.0, 1.0], uv: image.rect.map(|v| v as f32), clip: NO_CLIP, ..Instance::zeroed() });
    let prefix = [rect([0.0, 0.0, 2.0, 2.0], [1.0; 4])];
    let zero = Track::constant(0.0);
    let nodes = [Node { x: zero, y: zero, opacity: Track::constant(1.0) },
        Node { x: Track::constant(1.0), y: zero, opacity: Track::constant(1.0) }];
    let mut ids = vec![0; prefix.len() + content.len()];
    ids[4] = 1; // glyph at absolute index 4, translated from x=9 to x=10
    let mut renderer = Renderer::with_springs(&gpu.device, OFFSCREEN_FORMAT, &nodes, &ids);
    let ranges = renderer.prepare_lists(&gpu.device, &gpu.queue, &mut atlas, &[&prefix, &content], (32, 16));
    renderer.upload_images(&gpu.device, &gpu.queue, &mut images);
    let surface = NativeSurface::new();
    surface.push_bytes(ByteFrame { width: 2, height: 2, format: Format::Bgra, color: ColorInfo::default(),
        planes: [vec![255, 0, 0, 255].repeat(4), vec![]] }, 1).unwrap();
    renderer.prepare_surfaces(&gpu.device, &gpu.queue, &[SurfaceDraw { surface, fit: SurfaceFit::Fill,
        before_instance: 4, rect: [22.0, 6.0, 2.0, 2.0], radii: [0.0; 4], clip: NO_CLIP, fade: [0; 4], opacity: 1.0 }], (32, 16));
    let format = wgpu::TextureFormat::Rgba8Unorm;
    renderer.add_format(&gpu.device, format);
    let target = Offscreen::with_format(&gpu.device, 32, 16, format);
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    renderer.encode_layers(&mut enc);
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None, color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.view, depth_slice: None, resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
            })], depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
        });
        for range in ranges { renderer.draw_range(&mut pass, format, range); }
    }
    target.copy(&mut enc); renderer.submit(&gpu.queue, enc);
    let out = target.read(&gpu.device);
    for (x, y, want) in [(0,0,[255;4]), (9,6,[128,0,0,128]), (10,6,[128;4]), (15,6,[0,128,0,128]), (22,6,[0,0,128,128])] {
        near(&out[(y*32+x)*4..(y*32+x+1)*4], want);
    }
}

#[test]
fn cache_matches_uncached_at_rest_and_skips_rerasterize_on_ancestor_live_offset() {
    type State = (bool, Live<[f32; 2]>);
    fn crisp_view(s: &State, _: &mut Cx<State>) -> El {
        let mut card = crate::col()
            .size(40.0, 28.0)
            .padding(4.0)
            .gap(2.0)
            .fill(Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 })
            .child(el().size(24.0, 8.0).fill(Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }))
            .child(el().size(16.0, 8.0).fill(Color::WHITE));
        if s.0 {
            card = card.cache();
        }
        el().size(64.0, 48.0).padding(6.0).live_offset(&s.1).child(card)
    }
    fn view(s: &State, _: &mut Cx<State>) -> El {
        let mut card = crate::col()
            .size(40.0, 28.0)
            .padding(4.0)
            .gap(2.0)
            .fill(Color { r: 0.2, g: 0.35, b: 0.6, a: 1.0 })
            .radius(6.0)
            .border(1.0, Color::WHITE)
            .child(crate::text("Cached").color(Color::WHITE))
            .child(el().size(24.0, 8.0).fill(Color { r: 0.9, g: 0.3, b: 0.2, a: 1.0 }).radius(3.0));
        if s.0 {
            card = card.cache();
        }
        el().size(64.0, 48.0)
            .padding(6.0)
            .live_offset(&s.1)
            .child(card)
    }
    let gpu = Gpu::headless();
    let target = Offscreen::new(&gpu.device, 64, 48);

    // 1. Crisp card is byte-for-byte identical between cached and uncached at rest and under pan.
    let mut rt_u = Runtime::new((false, Live::new([0.0, 0.0])), crisp_view, (64.0, 48.0), Env { scale: 1.0, system_dark: true });
    let mut ren_u = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    rt_u.frame(0.0);
    ren_u.prepare(&gpu.device, &gpu.queue, &mut rt_u.text.atlas, &rt_u.instances, (64, 48));
    let mut rt_c = Runtime::new((true, Live::new([0.0, 0.0])), crisp_view, (64.0, 48.0), Env { scale: 1.0, system_dark: true });
    let mut ren_c = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    rt_c.frame(0.0);
    ren_c.prepare(&gpu.device, &gpu.queue, &mut rt_c.text.atlas, &rt_c.instances, (64, 48));
    assert_eq!(pixels(&gpu, &ren_c, &target), pixels(&gpu, &ren_u, &target), "cached vs uncached must be pixel-for-pixel identical at rest");

    // 2. Rich card with text and antialiased rounded corners under ancestor live_offset pan.
    let off_uncached = Live::new([0.0, 0.0]);
    let mut rt_uncached = Runtime::new((false, off_uncached.clone()), view, (64.0, 48.0), Env { scale: 1.0, system_dark: true });
    let mut ren_uncached = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    rt_uncached.frame(0.0);
    ren_uncached.prepare(&gpu.device, &gpu.queue, &mut rt_uncached.text.atlas, &rt_uncached.instances, (64, 48));
    let px_uncached = pixels(&gpu, &ren_uncached, &target);

    let off_cached = Live::new([0.0, 0.0]);
    let mut rt_cached = Runtime::new((true, off_cached.clone()), view, (64.0, 48.0), Env { scale: 1.0, system_dark: true });
    let mut ren_cached = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    rt_cached.frame(0.0);
    ren_cached.prepare(&gpu.device, &gpu.queue, &mut rt_cached.text.atlas, &rt_cached.instances, (64, 48));
    assert_eq!(ren_cached.layer_stats().targets, 1);
    assert_eq!(ren_cached.layer_stats().extra_passes, 1, "initial frame rasterizes the cache");
    let px_cached = pixels(&gpu, &ren_cached, &target);
    let max_diff = px_cached.iter().zip(&px_uncached).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
    assert!(max_diff <= 1, "cached vs uncached at rest max_diff={max_diff}");
    assert_eq!(rt_cached.elements().len(), rt_uncached.elements().len(), "elements() unchanged");

    // Panning via an ancestor's live_offset must elide child instances and not re-rasterize.
    for delta in [[4.0, 2.0], [10.0, 6.0], [3.0, 8.0], [0.0, 0.0]] {
        off_uncached.set(delta);
        rt_uncached.frame(1.0 / 60.0);
        ren_uncached.prepare(&gpu.device, &gpu.queue, &mut rt_uncached.text.atlas, &rt_uncached.instances, (64, 48));
        let want = pixels(&gpu, &ren_uncached, &target);

        off_cached.set(delta);
        rt_cached.frame(1.0 / 60.0);
        assert_eq!(rt_cached.instances.len(), 2, "cached subtree elides to [KIND_LAYER, KIND_LAYER_END]");
        ren_cached.prepare(&gpu.device, &gpu.queue, &mut rt_cached.text.atlas, &rt_cached.instances, (64, 48));
        assert_eq!(ren_cached.layer_stats().extra_passes, 0, "ancestor live_offset must not re-rasterize");
        assert_eq!(ren_cached.layer_stats().allocations, 1);
        let got = pixels(&gpu, &ren_cached, &target);
        let max_diff = got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
        assert!(max_diff <= 1, "panned cached output must match uncached at {delta:?} (max_diff={max_diff})");
        if delta == [0.0, 0.0] {
            assert_eq!(got, px_cached, "returning to origin is bit-for-bit identical");
        }
    }
}

#[test]
fn cache_invalidates_on_state_and_hover_changes() {
    fn view(active: &bool, _: &mut Cx<bool>) -> El {
        let fill = if *active {
            Color { r: 0.1, g: 0.8, b: 0.2, a: 1.0 }
        } else {
            Color { r: 0.8, g: 0.1, b: 0.2, a: 1.0 }
        };
        el().size(32.0, 32.0).child(
            el().size(24.0, 24.0)
                .cache()
                .child(
                    el().key("btn")
                        .size(24.0, 24.0)
                        .fill(fill)
                        .hover(Color { r: 0.1, g: 0.2, b: 0.9, a: 1.0 })
                        .on_press(|active: &mut bool| *active = !*active),
                ),
        )
    }
    let gpu = Gpu::headless();
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let target = Offscreen::new(&gpu.device, 32, 32);
    let mut rt = Runtime::new(false, view, (32.0, 32.0), Env { scale: 1.0, system_dark: true });

    // Frame 1: initial rasterize (red).
    rt.frame(0.0);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (32, 32));
    assert_eq!(renderer.layer_stats().extra_passes, 1);
    let out = pixels(&gpu, &renderer, &target);
    near(&out[(12 * 32 + 12) * 4..(12 * 32 + 13) * 4], rgba([0.8, 0.1, 0.2, 1.0]));

    // Frame 2: steady at rest (cache hit).
    rt.frame(1.0 / 60.0);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (32, 32));
    assert_eq!(renderer.layer_stats().extra_passes, 0);

    // Frame 3: state change inside cached subtree invalidates and re-renders once settled (green).
    rt.state = true;
    rt.invalidate();
    rt.frame(1.0);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (32, 32));
    assert_eq!(renderer.layer_stats().extra_passes, 1, "state change must invalidate cache");
    let out = pixels(&gpu, &renderer, &target);
    near(&out[(12 * 32 + 12) * 4..(12 * 32 + 13) * 4], rgba([0.1, 0.8, 0.2, 1.0]));

    // Frame 4: hover inside cached subtree invalidates immediately and re-renders (blue hover).
    rt.pointer_move(12.0, 12.0);
    rt.frame(1.0 / 60.0);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (32, 32));
    assert_eq!(renderer.layer_stats().extra_passes, 1, "hover change must invalidate cache");
    let out = pixels(&gpu, &renderer, &target);
    near(&out[(12 * 32 + 12) * 4..(12 * 32 + 13) * 4], rgba([0.1, 0.2, 0.9, 1.0]));

    // Frame 5: settled while hovered hits the cache again.
    rt.frame(1.0 / 60.0);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (32, 32));
    assert_eq!(renderer.layer_stats().extra_passes, 0, "settled hover must hit cache");
}

#[test]
fn cache_draws_live_while_subtree_animates_and_recaches_when_settled() {
    fn view(moved: &bool, _: &mut Cx<bool>) -> El {
        let x = if *moved { 16.0 } else { 0.0 };
        el().size(48.0, 24.0).child(
            el().size(40.0, 20.0)
                .cache()
                .child(
                    el().key("chip")
                        .size(16.0, 16.0)
                        .absolute(x, 0.0)
                        .motion(crate::Spring::DEFAULT)
                        .fill(Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }),
                ),
        )
    }
    let gpu = Gpu::headless();
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let target = Offscreen::new(&gpu.device, 48, 24);
    let mut rt = Runtime::new(false, view, (48.0, 24.0), Env { scale: 1.0, system_dark: true });

    // Initial rest: cached.
    rt.frame(0.0);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (48, 24));
    assert_eq!(renderer.layer_stats().targets, 1);
    assert_eq!(renderer.layer_stats().extra_passes, 1);
    rt.frame(1.0 / 60.0);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (48, 24));
    assert_eq!(renderer.layer_stats().extra_passes, 0);

    // Start animation inside cached subtree: must bypass cache and draw live without re-rasterizing offscreen targets.
    rt.state = true;
    rt.invalidate();
    rt.frame(0.0);
    for _ in 0..3 {
        rt.frame(0.05);
        assert!(rt.instances.iter().all(|i| i.params[1] != KIND_LAYER), "must draw live while animating");
        renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (48, 24));
        assert_eq!(renderer.layer_stats().extra_passes, 0, "no offscreen pass thrash while animating");
    }

    // Advance past animation duration so it settles: re-caches once at the new position, then hits cache.
    rt.frame(2.0);
    assert!(rt.instances.iter().any(|i| i.params[1] == KIND_LAYER && i.params[3] == 1.0), "re-cached once settled");
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (48, 24));
    assert_eq!(renderer.layer_stats().extra_passes, 1, "rasterize once on settle");
    let out = pixels(&gpu, &renderer, &target);
    near(&out[(8 * 48 + 24) * 4..(8 * 48 + 25) * 4], rgba([1.0, 0.0, 0.0, 1.0]));

    rt.frame(1.0 / 60.0);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (48, 24));
    assert_eq!(renderer.layer_stats().extra_passes, 0, "subsequent frame hits settled cache");
}

#[test]
fn cache_rerasterizes_at_new_resolution_after_ancestor_scale_settles() {
    fn view(scale: &Live<f32>, _: &mut Cx<Live<f32>>) -> El {
        el().size(200.0, 200.0)
            .padding(60.0)
            .live_scale(scale)
            .child(
                el().size(48.0, 48.0)
                    .cache()
                    .fill(Color { r: 0.2, g: 0.5, b: 0.9, a: 1.0 })
                    .child(el().size(24.0, 24.0).fill(Color::WHITE)),
            )
    }
    let gpu = Gpu::headless();
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let target = Offscreen::new(&gpu.device, 200, 200);
    let scale = Live::new(1.0);
    let mut rt = Runtime::new(scale.clone(), view, (200.0, 200.0), Env { scale: 1.0, system_dark: true });

    // Frame 1: rasterize at 1.0x (48x48 -> 64x64 bucket).
    rt.frame(0.0);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (200, 200));
    assert_eq!(renderer.layer_stats().extra_passes, 1);
    assert_eq!(renderer.layer_stats().bytes, 64 * 64 * 8);
    let _ = pixels(&gpu, &renderer, &target);

    // Frame 2: during scale gesture to 2.0x (dt = 0.016 < STATIC_SCALE_SETTLE), composites scaled without re-rasterizing.
    scale.set(2.0);
    rt.frame(0.016);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (200, 200));
    assert_eq!(renderer.layer_stats().extra_passes, 0, "composite existing texture during scale gesture");
    assert_eq!(renderer.layer_stats().bytes, 64 * 64 * 8, "still 1x texture before settle");

    // Frame 3: after scale settles (dt = 0.15 >= STATIC_SCALE_SETTLE), re-rasterizes at 2.0x (96x96 -> 128x128 bucket).
    rt.frame(0.15);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (200, 200));
    assert_eq!(renderer.layer_stats().extra_passes, 1, "re-rasterize at new resolution once settled");
    assert_eq!(renderer.layer_stats().bytes, 128 * 128 * 8, "upgraded to 2x texture bucket");
    let out = pixels(&gpu, &renderer, &target);
    assert_eq!(out[(100 * 200 + 100) * 4 + 3], 255);

    // Frame 4: hits the new 2x cache.
    rt.frame(0.016);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (200, 200));
    assert_eq!(renderer.layer_stats().extra_passes, 0);
}

#[test]
fn cache_lru_eviction_under_small_budget_evicts_oldest_and_recaches_cleanly() {
    fn scroll_view(offset: &Live<[f32; 2]>, _: &mut Cx<Live<[f32; 2]>>) -> El {
        let card = |key: &str, y: f32, c: Color| {
            el().key(key)
                .size(24.0, 24.0)
                .absolute(4.0, y)
                .cache()
                .fill(c)
                .child(el().size(10.0, 10.0).fill(Color::WHITE))
        };
        el().size(32.0, 32.0).child(
            el().size(32.0, 300.0)
                .live_offset(offset)
                .child(card("a", 0.0, Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }))
                .child(card("b", 100.0, Color { r: 0.0, g: 1.0, b: 0.0, a: 1.0 }))
                .child(card("c", 200.0, Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 })),
        )
    }
    let gpu = Gpu::headless();
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let one_bucket = 64 * 64 * 8;
    // Budget fits at most 2 cached 64x64 textures.
    renderer.set_cache_budget(one_bucket * 2);
    assert_eq!(renderer.cache_budget(), one_bucket * 2);
    let target = Offscreen::new(&gpu.device, 32, 32);
    let offset = Live::new([0.0, 0.0]);
    let mut rt = Runtime::new(offset.clone(), scroll_view, (32.0, 32.0), Env { scale: 1.0, system_dark: true });

    // Visit card A (y=0): caches A.
    rt.frame(0.0);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (32, 32));
    assert_eq!(renderer.layer_stats().extra_passes, 1);
    assert!(renderer.layer_stats().bytes <= one_bucket * 2);
    let out = pixels(&gpu, &renderer, &target);
    near(&out[(16 * 32 + 16) * 4..(16 * 32 + 17) * 4], rgba([1.0, 0.0, 0.0, 1.0]));

    // Visit card B (y=-100): caches B, retains A (2 targets <= budget).
    offset.set([0.0, -100.0]);
    rt.frame(0.016);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (32, 32));
    assert_eq!(renderer.layer_stats().extra_passes, 1);
    assert_eq!(renderer.layer_stats().targets, 2);
    assert!(renderer.layer_stats().bytes <= one_bucket * 2);
    let out = pixels(&gpu, &renderer, &target);
    near(&out[(16 * 32 + 16) * 4..(16 * 32 + 17) * 4], rgba([0.0, 1.0, 0.0, 1.0]));

    // Return to card A (y=0): A is still resident in budget, so extra_passes == 0!
    offset.set([0.0, 0.0]);
    rt.frame(0.016);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (32, 32));
    assert_eq!(renderer.layer_stats().extra_passes, 0, "A was retained in budget");

    // Now visit card C (y=-200): A was used most recently, so oldest (B) is evicted!
    offset.set([0.0, -200.0]);
    rt.frame(0.016);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (32, 32));
    assert_eq!(renderer.layer_stats().extra_passes, 1, "C rasterizes and evicts LRU B");
    assert_eq!(renderer.layer_stats().targets, 2);
    assert!(renderer.layer_stats().bytes <= one_bucket * 2);
    let out = pixels(&gpu, &renderer, &target);
    near(&out[(16 * 32 + 16) * 4..(16 * 32 + 17) * 4], rgba([0.0, 0.0, 1.0, 1.0]));

    // Card A (y=0) is still resident (extra_passes == 0), while card B (y=-100) was evicted and re-caches cleanly!
    offset.set([0.0, 0.0]);
    rt.frame(0.016);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (32, 32));
    assert_eq!(renderer.layer_stats().extra_passes, 0, "MRU card A stayed resident");

    offset.set([0.0, -100.0]);
    rt.frame(0.016);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (32, 32));
    assert_eq!(renderer.layer_stats().extra_passes, 1, "evicted card B re-caches cleanly");
    let out = pixels(&gpu, &renderer, &target);
    near(&out[(16 * 32 + 16) * 4..(16 * 32 + 17) * 4], rgba([0.0, 1.0, 0.0, 1.0]));
}

/// Drives a cached and an uncached runtime through the same steps (`frames` frames each, 1/60 s
/// apart); the cached one must draw the same pixels after every frame. Returns the cached
/// renderer's stats per frame.
fn same_as_uncached<T: Clone + 'static>(state: T, view: fn(&(bool, T), &mut Cx<(bool, T)>) -> El, size: (u32, u32),
    steps: &[(&str, usize, &dyn Fn(&mut Runtime<(bool, T)>))]) -> Vec<(String, LayerStats)> {
    let gpu = Gpu::headless();
    let target = Offscreen::new(&gpu.device, size.0, size.1);
    let env = Env { scale: 1.0, system_dark: true };
    let viewport = (size.0 as f32, size.1 as f32);
    let mut runs: Vec<_> = [false, true].map(|cached| {
        (Runtime::new((cached, state.clone()), view, viewport, env), Renderer::new(&gpu.device, OFFSCREEN_FORMAT))
    }).into();
    let mut stats = Vec::new();
    let initial: &dyn Fn(&mut Runtime<(bool, T)>) = &|_| {};
    for (name, frames, step) in [("rest", 2, initial)].into_iter().chain(steps.iter().copied()) {
        let mut out = Vec::new();
        for (rt, _) in &mut runs { step(rt); }
        for f in 0..frames {
            for (rt, renderer) in &mut runs {
                rt.frame(1.0 / 60.0);
                renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, size);
                out.push(pixels(&gpu, renderer, &target));
            }
            let (want, got) = (&out[out.len() - 2], &out[out.len() - 1]);
            let max_diff = got.iter().zip(want).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
            assert!(max_diff <= 1, "{name} frame {f}: cached differs from uncached (max_diff={max_diff})");
            stats.push((format!("{name}/{f}"), runs[1].1.layer_stats()));
        }
    }
    assert!(runs[1].0.instances.iter().any(|i| i.params[1] == KIND_LAYER && i.params[3] == 1.0), "the subtree stays cached");
    stats
}

#[test]
fn cache_follows_press_paint_that_lands_without_a_view_run() {
    fn view(s: &(bool, ()), _: &mut Cx<(bool, ())>) -> El {
        let mut card = el().size(24.0, 24.0).child(
            el().size(24.0, 24.0)
                .fill(Color { r: 0.8, g: 0.1, b: 0.2, a: 1.0 })
                .press(Color { r: 0.1, g: 0.2, b: 0.9, a: 1.0 })
                .on_press(|_: &mut (bool, ())| {}),
        );
        if s.0 { card = card.cache(); }
        el().size(32.0, 32.0).child(card)
    }
    let stats = same_as_uncached((), view, (32, 32), &[
        ("hover", 2, &|rt| rt.pointer_move(12.0, 12.0)),
        ("down", 2, &|rt| rt.pointer_down(12.0, 12.0)),
        ("up", 40, &|rt| rt.pointer_up(12.0, 12.0)),
    ]);
    assert_eq!(stats.last().unwrap().1.extra_passes, 0, "the released card hits its cache again");
}

#[test]
fn cache_follows_link_hover_moving_between_links_of_one_text() {
    fn view(s: &(bool, ()), _: &mut Cx<(bool, ())>) -> El {
        let green = crate::LinkPaint::new().bg(Color { r: 0.0, g: 1.0, b: 0.0, a: 1.0 });
        let label = crate::text("MMMM MMMM")
            .font(crate::TextStyle::new(16.0, 400.0))
            .color(Color::WHITE)
            .link(crate::Link::new(0..4, "a").hover(green))
            .link(crate::Link::new(5..9, "b").hover(green))
            .on_link(|_: &mut (bool, ()), _, _| {});
        let mut card = el().size(150.0, 24.0).child(label);
        if s.0 { card = card.cache(); }
        el().size(160.0, 32.0).child(card)
    }
    same_as_uncached((), view, (160, 32), &[
        ("first link", 2, &|rt| rt.pointer_move(16.0, 10.0)),
        ("second link", 2, &|rt| rt.pointer_move(90.0, 10.0)),
    ]);
}

#[test]
fn cache_follows_an_outside_hover_group_fading_its_descendants() {
    fn view(s: &(bool, ()), _: &mut Cx<(bool, ())>) -> El {
        let mut card = el().size(24.0, 24.0).fill(Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 }).child(
            el().size(16.0, 16.0).fill(Color::WHITE).group_opacity(0.2, 1.0),
        );
        if s.0 { card = card.cache(); }
        el().size(64.0, 32.0).child(crate::row().size(64.0, 32.0).hover_group().child(card))
    }
    same_as_uncached((), view, (64, 32), &[
        ("enter", 3, &|rt| rt.pointer_move(48.0, 16.0)),
        ("leave", 12, &|rt| rt.pointer_move(200.0, 200.0)),
    ]);
}

#[test]
fn cache_redraws_a_same_length_text_change() {
    fn view(s: &(bool, bool), _: &mut Cx<(bool, bool)>) -> El {
        let mut card = el().size(60.0, 24.0).child(crate::text(if s.1 { "BBBB" } else { "AAAA" }).color(Color::WHITE));
        if s.0 { card = card.cache(); }
        el().size(64.0, 32.0).child(card)
    }
    let stats = same_as_uncached(false, view, (64, 32), &[
        ("swap", 40, &|rt| { rt.state.1 = true; rt.invalidate(); }),
    ]);
    let passes: Vec<_> = stats.iter().map(|(_, s)| s.extra_passes).collect();
    assert_eq!(passes[..2], [1, 0]);
    assert_eq!(passes[2..].iter().sum::<usize>(), 1, "the new text rasterizes once: {passes:?}");
    assert_eq!(passes.last(), Some(&0));
}

#[test]
fn cache_invalidates_the_cached_roots_blur() {
    fn view(s: &(bool, bool), _: &mut Cx<(bool, bool)>) -> El {
        let mut card = el().size(24.0, 24.0).absolute(16.0, 16.0)
            .blur(if s.1 { 3.0 } else { 0.0 }).blur_motion(crate::Anim::INSTANT)
            .child(el().size(16.0, 16.0).fill(Color::WHITE));
        if s.0 { card = card.cache(); }
        el().size(64.0, 64.0).child(card)
    }
    same_as_uncached(false, view, (64, 64), &[
        ("blur", 3, &|rt| { rt.state.1 = true; rt.invalidate(); }),
    ]);
}

#[test]
fn clicks_land_inside_a_cached_card_after_a_pan() {
    type S = (u32, Live<[f32; 2]>);
    fn view(s: &S, _: &mut Cx<S>) -> El {
        el().size(96.0, 64.0).child(
            el().size(96.0, 64.0).live_offset(&s.1).child(
                el().size(32.0, 32.0).cache().fill(Color::WHITE).child(
                    el().key("button").size(16.0, 16.0).fill(Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 })
                        .on_press(|s: &mut S| s.0 += 1),
                ),
            ),
        )
    }
    let gpu = Gpu::headless();
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let pan = Live::new([0.0, 0.0]);
    let mut rt = Runtime::new((0, pan.clone()), view, (96.0, 64.0), Env { scale: 1.0, system_dark: true });
    for _ in 0..2 {
        rt.frame(1.0 / 60.0);
        renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (96, 64));
    }
    pan.set([50.0, 30.0]);
    rt.frame(1.0 / 60.0);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (96, 64));
    assert_eq!(rt.instances.len(), 2, "the panned card is elided to its cached quad");
    for (x, y) in [(8.0, 8.0), (58.0, 38.0)] {
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(1.0 / 60.0);
        renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (96, 64));
    }
    assert_eq!(rt.state.0, 1, "only the click on the button's panned position presses it");
    let b = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("button")).expect("button listed");
    assert_eq!([b.rect[0], b.rect[1]], [50.0, 30.0], "elements() reports the panned rect");
}

#[test]
fn cache_updates_same_size_image_pixels_after_the_first_raster() {
    fn view(image: &crate::Pixels, _: &mut Cx<crate::Pixels>) -> El {
        el().size(32.0, 32.0).child(el().size(24.0, 24.0).cache().pixels(image))
    }
    let gpu = Gpu::headless();
    let target = Offscreen::new(&gpu.device, 32, 32);
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let image = crate::Pixels::new();
    let mut rt = Runtime::new(image.clone(), view, (32.0, 32.0), Env::new(true, 1.0));
    for color in [[255, 0, 0, 255], [0, 255, 0, 255]] {
        image.set(2, 2, color.repeat(4));
        rt.frame(1.0 / 60.0);
        renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (32, 32));
        renderer.upload_images(&gpu.device, &gpu.queue, &mut rt.images);
        let out = pixels(&gpu, &renderer, &target);
        assert_eq!(&out[(12 * 32 + 12) * 4..][..4], &color, "same-size image publication must change the output");
    }
}

#[test]
fn cache_does_not_hide_a_radius_change_during_scale_settling() {
    type S = (bool, Live<f32>);
    fn view(s: &S, _: &mut Cx<S>) -> El {
        el().size(100.0, 100.0).live_scale(&s.1).child(
            el().size(32.0, 32.0).cache().child(
                el().size(32.0, 32.0).motion(crate::tok::motion::INSTANT).fill(Color::WHITE).radius(if s.0 { 16.0 } else { 0.0 }),
            ),
        )
    }
    let gpu = Gpu::headless();
    let target = Offscreen::new(&gpu.device, 100, 100);
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let scale = Live::new(1.0);
    let mut rt = Runtime::new((false, scale.clone()), view, (100.0, 100.0), Env::new(true, 1.0));
    rt.frame(0.0);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (100, 100));
    let _ = pixels(&gpu, &renderer, &target);
    scale.set(1.01);
    rt.state.0 = true;
    rt.invalidate();
    rt.frame(1.0 / 60.0);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (100, 100));
    let out = pixels(&gpu, &renderer, &target);
    assert!(out[3] < 32, "the newly rounded corner must not show the stale square (alpha={})", out[3]);
}

#[test]
fn slow_pinch_composites_until_the_scale_holds_still() {
    fn view(scale: &Live<f32>, _: &mut Cx<Live<f32>>) -> El {
        el().size(200.0, 200.0).padding(60.0).live_scale(scale).child(
            el().size(48.0, 48.0).cache().fill(Color { r: 0.2, g: 0.5, b: 0.9, a: 1.0 })
                .child(el().size(24.0, 24.0).fill(Color::WHITE)),
        )
    }
    let gpu = Gpu::headless();
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let scale = Live::new(1.0);
    let mut rt = Runtime::new(scale.clone(), view, (200.0, 200.0), Env { scale: 1.0, system_dark: true });
    rt.frame(0.0);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (200, 200));
    assert_eq!(renderer.layer_stats().extra_passes, 1);
    for step in 1..=60 {
        scale.set(1.0 + step as f32 * 0.01);
        rt.frame(1.0 / 60.0);
        renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (200, 200));
        assert_eq!(renderer.layer_stats().extra_passes, 0, "pinch step {step} composites the old texture");
    }
    rt.frame(0.15);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (200, 200));
    assert_eq!(renderer.layer_stats().extra_passes, 1, "one raster once the pinch rests");
}

#[test]
fn over_budget_cache_falls_back_to_a_viewport_sized_target() {
    fn view(_: &(), _: &mut Cx<()>) -> El {
        el().size(32.0, 32.0).child(
            el().size(300.0, 300.0).cache().clip_rounded(8.0).fill(Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 })
                .child(el().size(20.0, 20.0).fill(Color::WHITE)),
        )
    }
    let gpu = Gpu::headless();
    let target = Offscreen::new(&gpu.device, 32, 32);
    let mut want = None;
    // In budget it caches a 320 px bucket; the fallback draws the same group through the 32 px it shows.
    for budget in [DEFAULT_CACHE_BUDGET, 64 * 64 * 8] {
        let mut rt = Runtime::new((), view, (32.0, 32.0), Env { scale: 1.0, system_dark: true });
        let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
        renderer.set_cache_budget(budget);
        let mut out = Vec::new();
        for _ in 0..3 {
            rt.frame(1.0 / 60.0);
            renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (32, 32));
            out = pixels(&gpu, &renderer, &target);
        }
        match &want {
            None => want = Some(out),
            Some(w) => {
                assert!(renderer.layer_stats().bytes <= 64 * 64 * 8, "over-budget fallback stays viewport-sized: {:?}", renderer.layer_stats());
                assert!(out.iter().zip(w).all(|(a, b)| a.abs_diff(*b) <= 1), "fallback draws like the cache");
            }
        }
    }
}

#[test]
fn scale_commits_draw_live_without_repeated_cache_passes() {
    fn view(scale: &f32, _: &mut Cx<f32>) -> El {
        el().size(200.0, 200.0).scale(*scale).child(
            el().size(48.0, 48.0).cache().fill(Color::WHITE),
        )
    }
    let gpu = Gpu::headless();
    let target = Offscreen::new(&gpu.device, 200, 200);
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let mut rt = Runtime::new(1.0, view, (200.0, 200.0), Env::new(true, 1.0));
    rt.frame(0.0);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (200, 200));
    let _ = pixels(&gpu, &renderer, &target);
    for step in 1..=30 {
        rt.state = 1.0 + step as f32 * 0.01;
        rt.invalidate();
        rt.frame(1.0 / 60.0);
        renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (200, 200));
        assert_eq!(renderer.layer_stats().extra_passes, 0, "scale commit {step} must not rasterize another cache");
        let _ = pixels(&gpu, &renderer, &target);
    }
    rt.frame(0.15);
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (200, 200));
    assert_eq!(renderer.layer_stats().extra_passes, 1, "cache once the committed scale settles");
}

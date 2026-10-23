use super::*;
use super::tests::{rect, pixels, rgba, near};
use crate::{Color, Cx, El, Env, Live, Runtime, el};

fn filtered(bounds: [f32; 4], sigma: f32) -> Vec<Instance> {
    vec![Instance::begin_layer(bounds, NO_CLIP, LayerSpec { blur: sigma, ..Default::default() }),
        rect(bounds, [1.0; 4]), Instance::end_layer()]
}

// Independent discrete Gaussian oracle (no paired taps, no renderer bounds/pooling logic).
fn coverage(x: i32, start: i32, end: i32, sigma: f32) -> f32 {
    let radius = (3.0 * sigma).ceil() as i32;
    let weight = |d: i32| (-0.5 * (d as f32 / sigma).powi(2)).exp();
    let total: f32 = (-radius..=radius).map(weight).sum();
    (-radius..=radius).filter(|d| (start..end).contains(&(x + d))).map(weight).sum::<f32>() / total
}

#[test]
fn gaussian_pixels_support_offscreen_saturation_and_pooling() {
    let gpu = Gpu::headless();
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let target = Offscreen::new(&gpu.device, 64, 48);
    let mut atlas = crate::text::TextSystem::default().atlas;
    // Negative origin tests retained source outside the destination viewport. The duplicate
    // additive shape must saturate BEFORE blur (clamping after convolution doubles the halo).
    for (left, top, sigma, additive) in [(12, 10, 2.0, false), (-6, -4, 3.0, false), (12, 10, 2.0, true)] {
        let bounds = [left as f32, top as f32, 20.0, 16.0];
        let mut content = filtered(bounds, sigma);
        if additive { content[0].params[0] = 1.0; content.insert(2, rect(bounds, [1.0; 4])); }
        renderer.prepare(&gpu.device, &gpu.queue, &mut atlas, &content, (64, 48));
        assert_eq!(renderer.layer_stats().extra_passes, 3);
        let out = pixels(&gpu, &renderer, &target);
        for y in 0..48 { for x in 0..64 {
            let c = coverage(x, left, left + 20, sigma) * coverage(y, top, top + 16, sigma);
            let at = ((y * 64 + x) * 4) as usize;
            near(&out[at..at + 4], rgba([c; 4]));
        } }
    }
    let allocations = renderer.layer_stats().allocations;
    for sigma in [2.1, 2.4, 2.0] {
        renderer.prepare(&gpu.device, &gpu.queue, &mut atlas, &filtered([12.0, 10.0, 20.0, 16.0], sigma), (64, 48));
        assert_eq!(renderer.layer_stats().allocations, allocations);
    }
    for sigma in [0.0, -1.0, f32::NAN] {
        renderer.prepare(&gpu.device, &gpu.queue, &mut atlas, &filtered([12.0, 10.0, 20.0, 16.0], sigma), (64, 48));
        assert_eq!(renderer.layer_stats().targets, 0);
        assert_eq!(renderer.layer_stats().extra_passes, 0);
    }
}

#[test]
fn blur_live_springs_curves_and_zero_bypass() {
    fn view(value: &Live<f32>, _: &mut Cx<Live<f32>>) -> El {
        el().size(20.0, 16.0).fill(Color::WHITE).live_blur(value)
    }
    let mut rt = Runtime::new(Live::new(0.0), view, (64.0, 48.0), Env { scale: 2.0, system_dark: true });
    rt.frame(0.0);
    assert_eq!(rt.instances.len(), 1);
    let commits = rt.stats.commits;
    rt.state.set(2.0); rt.frame(0.0);
    assert_eq!(rt.instances[0].params[1], KIND_LAYER);
    assert_eq!(rt.instances[0].params[2], 4.0);
    assert_eq!(rt.instances[1].params[2], 0.0, "do not also blur each primitive");
    rt.state.set(0.0); rt.frame(0.0);
    assert_eq!(rt.instances.len(), 1);
    assert_eq!(rt.stats.commits, commits);

    fn smooth(value: &Live<f32>, _: &mut Cx<Live<f32>>) -> El {
        el().size(20.0, 16.0).fill(Color::WHITE).live_blur(value.smooth(crate::Spring::new(0.3, 0.0)))
    }
    let mut rt = Runtime::new(Live::new(0.0), smooth, (64.0, 48.0), Env { scale: 1.0, system_dark: true });
    rt.frame(0.0); rt.state.set(4.0); rt.frame(0.05);
    let first = rt.instances[0].params[2];
    assert!(first > 0.0 && first < 4.0);
    rt.frame(0.05);
    assert!(rt.instances[0].params[2] > first);

    fn curve(value: &f32, _: &mut Cx<f32>) -> El {
        el().size(20.0, 16.0).fill(Color::WHITE).blur(*value)
            .blur_motion(rune_motion::Curve::ease_in_out(0.2))
    }
    let mut rt = Runtime::new(4.0, curve, (64.0, 48.0), Env { scale: 1.0, system_dark: true });
    rt.frame(0.0); rt.state = 0.0; rt.invalidate(); rt.frame(0.0); rt.frame(0.05);
    assert!(rt.instances[0].params[2] > 0.0 && rt.instances[0].params[2] < 4.0);
    for _ in 0..30 { rt.frame(1.0 / 60.0); }
    assert_eq!(rt.instances.len(), 1);
}

#[test]
fn runtime_keeps_offscreen_blur_sources_and_presence_transitions() {
    fn offscreen(_: &(), _: &mut Cx<()>) -> El {
        el().child(el().size(6.0, 12.0).absolute(-8.0, 12.0).fill(Color::WHITE).blur(4.0))
    }
    let gpu = Gpu::headless();
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let target = Offscreen::new(&gpu.device, 64, 48);
    let mut rt = Runtime::new((), offscreen, (64.0, 48.0), Env { scale: 1.0, system_dark: true });
    rt.frame(0.0);
    assert!(rt.instances.iter().any(|i| i.params[1] == KIND_RECT && i.rect[0] < 0.0));
    renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (64, 48));
    let out = pixels(&gpu, &renderer, &target);
    assert!(out[(18 * 64) * 4 + 3] > 0, "offscreen paint contributes to the visible halo");

    fn presence(show: &bool, _: &mut Cx<bool>) -> El {
        el().children(show.then(|| el().key("state").size(20.0, 16.0).fill(Color::WHITE)
            .transition(rune_motion::Transition::blur(4.0))))
    }
    let mut rt = Runtime::new(false, presence, (64.0, 48.0), Env { scale: 1.0, system_dark: true });
    rt.frame(0.0); rt.state = true; rt.invalidate(); rt.frame(0.0); rt.frame(0.05);
    assert!(rt.instances.iter().any(|i| i.params[1] == KIND_LAYER && i.params[2] > 0.0));
    assert!(rt.instances.iter().filter(|i| i.params[1] == KIND_RECT).all(|i| i.params[2] == 0.0));
    for _ in 0..120 { rt.frame(1.0 / 120.0); }
    assert!(rt.instances.iter().all(|i| i.params[1] != KIND_LAYER));
}

#[test]
fn later_live_blur_binding_replaces_the_earlier_source() {
    type State = (Live<f32>, Live<f32>);
    fn view(s: &State, _: &mut Cx<State>) -> El {
        el().size(20.0, 16.0).fill(Color::WHITE).live_blur(&s.0).live_blur(&s.1)
    }
    let mut rt = Runtime::new((Live::new(1.0), Live::new(2.0)), view, (64.0, 48.0), Env { scale: 1.0, system_dark: true });
    rt.frame(0.0);
    rt.state.0.set(4.0); rt.frame(0.0);
    assert_eq!(rt.instances[0].params[2], 2.0);
}

#[test]
fn nested_blur_and_capsule_mask_apply_to_completed_output() {
    let gpu = Gpu::headless();
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let target = Offscreen::new(&gpu.device, 64, 48);
    let mut atlas = crate::text::TextSystem::default().atlas;
    let clip = [8.0, 8.0, 56.0, 40.0];
    // The source covers the capsule plus filter support; mask AFTER filtering must preserve
    // unit coverage in the middle of its straight edges and transparent corner exteriors.
    let mut outer = Instance::begin_layer([0.0, 0.0, 64.0, 48.0], clip,
        LayerSpec { blur: 2.0, clip_radius: 16.0, opacity: 0.5, ..Default::default() });
    let mut all = vec![outer];
    all.extend(filtered([0.0, 0.0, 64.0, 48.0], 2.0));
    all.push(Instance::end_layer());
    renderer.prepare(&gpu.device, &gpu.queue, &mut atlas, &all, (64, 48));
    assert_eq!(renderer.layer_stats().extra_passes, 6);
    let out = pixels(&gpu, &renderer, &target);
    for (x, y, c) in [(8, 8, 0.0), (55, 39, 0.0), (32, 24, 0.5), (32, 9, 0.5), (4, 24, 0.0)] {
        let at = (y * 64 + x) * 4;
        near(&out[at..at + 4], rgba([c; 4]));
    }
    // An intersecting rectangular ancestor must not recenter the child's rounded rectangle.
    outer = Instance::begin_layer([0.0, 0.0, 64.0, 48.0], [24.0, 0.0, 64.0, 48.0],
        LayerSpec { opacity: 0.5, ..Default::default() });
    all = vec![outer, Instance::begin_layer([0.0, 0.0, 64.0, 48.0], clip,
        LayerSpec { clip_radius: 16.0, ..Default::default() }), rect([0.0, 0.0, 64.0, 48.0], [1.0; 4]),
        Instance::end_layer(), Instance::end_layer()];
    renderer.prepare(&gpu.device, &gpu.queue, &mut atlas, &all, (64, 48));
    let out = pixels(&gpu, &renderer, &target);
    near(&out[(8 * 64 + 24) * 4..(8 * 64 + 25) * 4], rgba([0.5; 4]));
    near(&out[(24 * 64 + 20) * 4..(24 * 64 + 21) * 4], [0; 4]);
}

#[test]
fn filtered_video_and_image_page_match_primitive_source() {
    use crate::native_surface::{ByteFrame, ColorInfo, Format, NativeSurface, SurfaceDraw, SurfaceFit};
    use crate::pixels::{Pixels, ImageAtlas};
    let gpu = Gpu::headless();
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let target = Offscreen::new(&gpu.device, 64, 48);
    let mut atlas = crate::text::TextSystem::default().atlas;
    let bounds = [12.0, 10.0, 20.0, 16.0];
    let mut content = filtered(bounds, 2.0);
    renderer.prepare(&gpu.device, &gpu.queue, &mut atlas, &content, (64, 48));
    let reference = pixels(&gpu, &renderer, &target);
    let mut images = ImageAtlas::default();
    let full = Pixels::new(); full.set(1024, 1024, vec![255; 1024 * 1024 * 4]);
    images.place(&full, [1024.0; 2]).unwrap();
    let white = Pixels::new(); white.set(1, 1, vec![255; 4]);
    let image = images.place(&white, [20.0, 16.0]).unwrap();
    assert_eq!(image.page, 1);
    content[1] = Instance { rect: bounds, fill: [0.0, 0.0, 0.0, 1.0], clip: NO_CLIP,
        uv: image.rect.map(|v| v as f32), params: [image.page as f32, KIND_IMAGE, 0.0, 1.0], ..Instance::zeroed() };
    renderer.prepare(&gpu.device, &gpu.queue, &mut atlas, &content, (64, 48));
    renderer.upload_images(&gpu.device, &gpu.queue, &mut images);
    assert_eq!(pixels(&gpu, &renderer, &target), reference);
    content.remove(1);
    renderer.prepare(&gpu.device, &gpu.queue, &mut atlas, &content, (64, 48));
    let surface = NativeSurface::new();
    surface.push_bytes(ByteFrame { width: 1, height: 1, format: Format::Bgra, color: ColorInfo::default(),
        planes: [vec![255; 4], vec![]] }, 1).unwrap();
    renderer.prepare_surfaces(&gpu.device, &gpu.queue, &[SurfaceDraw { surface: surface.clone(), fit: SurfaceFit::Fill,
        before_instance: 1, rect: bounds, radii: [0.0; 4], clip: NO_CLIP, fade: [0.0; 4], opacity: 1.0 }], (64, 48));
    assert_eq!(pixels(&gpu, &renderer, &target), reference);
    surface.push_bytes(ByteFrame { width: 1, height: 1, format: Format::Bgra, color: ColorInfo::default(),
        planes: [vec![0, 0, 0, 255], vec![]] }, 2).unwrap();
    renderer.prepare_surfaces(&gpu.device, &gpu.queue, &[SurfaceDraw { surface, fit: SurfaceFit::Fill,
        before_instance: 1, rect: bounds, radii: [0.0; 4], clip: NO_CLIP, fade: [0.0; 4], opacity: 1.0 }], (64, 48));
    let black = pixels(&gpu, &renderer, &target);
    assert_eq!(black[(18 * 64 + 22) * 4], 0, "new video frame is never cached");
    assert_eq!(black[(18 * 64 + 22) * 4 + 3], reference[(18 * 64 + 22) * 4 + 3]);
}

pub(super) fn gpu_time(gpu: &Gpu, mut draw: impl FnMut(&mut wgpu::CommandEncoder, wgpu::RenderPassTimestampWrites<'_>)) -> (f64, f64) {
    const N: u32 = 68;
    let queries = gpu.device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: N * 2 });
    let resolve = gpu.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: u64::from(N) * 16,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
    let read = gpu.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: u64::from(N) * 16,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
    for i in 0..N {
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        draw(&mut enc, wgpu::RenderPassTimestampWrites { query_set: &queries,
            beginning_of_pass_write_index: Some(i * 2), end_of_pass_write_index: Some(i * 2 + 1) });
        gpu.queue.submit([enc.finish()]);
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    }
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    enc.resolve_query_set(&queries, 0..N * 2, &resolve, 0);
    enc.copy_buffer_to_buffer(&resolve, 0, &read, 0, u64::from(N) * 16);
    gpu.queue.submit([enc.finish()]);
    read.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
    gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let bytes = read.slice(..).get_mapped_range().unwrap();
    let mut ms: Vec<f64> = bytes.chunks_exact(16).skip(8).map(|b| {
        let a = u64::from_le_bytes(b[..8].try_into().unwrap());
        let z = u64::from_le_bytes(b[8..].try_into().unwrap());
        assert!(z >= a && z != 0);
        (z - a) as f64 * f64::from(gpu.queue.get_timestamp_period()) / 1e6
    }).collect();
    ms.sort_by(f64::total_cmp);
    (ms[30], ms[56])
}

#[test]
#[ignore = "manual 400x40pt Scrawl pill and oversized 400x120pt stress fixture at 2x"]
fn pill_blur_gpu_cost() {
    let gpu = Gpu::headless();
    eprintln!("adapter: {:?}", gpu.adapter.get_info());
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let target = Offscreen::new(&gpu.device, 1024, 512);
    let mut atlas = crate::text::TextSystem::default().atlas;
    for height in [80.0, 240.0] {
      let mut baseline = 0.0;
      for sigma in [0.0, 3.0, 8.0, 16.0] {
        let mut all = filtered([112.0, 136.0, 800.0, height], sigma);
        all[0].fill[3] = 0.5; // already-isolated crossfade, measure incremental filter cost
        all[1].radii = [height * 0.5; 4];
        renderer.prepare(&gpu.device, &gpu.queue, &mut atlas, &all, (1024, 512));
        let (median, p95) = gpu_time(&gpu, |enc, writes| renderer.draw_timed(enc, &target.view, [0.0; 4], Some(writes)));
        if sigma == 0.0 { baseline = median; }
        eprintln!("pill height={height}px sigma={sigma}px: median={median:.6}ms p95={p95:.6}ms added={:.6}ms {:?}", median - baseline, renderer.layer_stats());
      }
    }
}

#[test]
fn reduced_gaussian_accuracy_and_resolution_boundary_continuity() {
    let gpu = Gpu::headless();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut renderer = Renderer::new(&gpu.device, format);
    let target = Offscreen::with_format(&gpu.device, 128, 96, format);
    let mut atlas = crate::text::TextSystem::default().atlas;
    let mut previous: Option<Vec<u8>> = None;
    for sigma in [2.999, 3.001, 4.0, 8.0, 16.0, 64.0] {
        renderer.prepare(&gpu.device, &gpu.queue, &mut atlas, &filtered([24.0, 24.0, 80.0, 48.0], sigma), (128, 96));
        let out = pixels(&gpu, &renderer, &target);
        let mut worst = 0;
        for y in 0..96 { for x in 0..128 {
            let c = coverage(x, 24, 104, sigma) * coverage(y, 24, 72, sigma);
            worst = worst.max(out[((y * 128 + x) * 4 + 3) as usize].abs_diff((c * 255.0).round() as u8));
        } }
        eprintln!("sigma={sigma} max alpha error versus full Gaussian={worst}/255");
        assert!(worst <= 3, "sigma={sigma} error={worst}");
        if sigma == 3.001 {
            let before = previous.as_ref().unwrap();
            assert!(out.iter().zip(before).all(|(a,b)| a.abs_diff(*b) <= 1), "resolution boundary must be continuous");
        }
        previous = Some(out);
    }
}

#[test]
#[ignore = "one headless review still; set RUNE_FILTER_STILL to the PNG destination"]
fn layer_filter_review_still() {
    let gpu = Gpu::headless();
    let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let target = Offscreen::new(&gpu.device, 880, 352);
    let mut atlas = crate::text::TextSystem::default().atlas;
    let mut all = vec![rect([0.0, 0.0, 880.0, 352.0], [0.012, 0.012, 0.016, 1.0])];
    for (row, sigma) in [0.0, 3.0, 8.0].into_iter().enumerate() {
        for (column, masked) in [false, true].into_iter().enumerate() {
            let (x, y) = (24.0 + column as f32 * 440.0, 24.0 + row as f32 * 112.0);
            let bounds = [x, y, 392.0, 80.0];
            let clip = if masked { [x, y, x + 392.0, y + 80.0] } else { NO_CLIP };
            all.push(Instance::begin_layer(bounds, clip, LayerSpec {
                blur: sigma, clip_radius: if masked { 40.0 } else { 0.0 }, ..Default::default() }));
            all.push(Instance { radii: [40.0; 4], ..rect(bounds, [0.04, 0.08, 0.16, 1.0]) });
            for offset in [-12.0, 20.0, 52.0, 340.0, 372.0] {
                all.push(Instance { radii: [12.0; 4], ..rect([x + offset, y + 28.0, 24.0, 24.0], [0.45, 0.18, 0.06, 1.0]) });
            }
            for (dy, width) in [(24.0, 188.0), (42.0, 144.0)] {
                all.push(Instance { radii: [5.0; 4], ..rect([x + 96.0, y + dy, width, 10.0], [0.8, 0.8, 0.8, 1.0]) });
            }
            all.push(Instance::end_layer());
        }
    }
    renderer.prepare(&gpu.device, &gpu.queue, &mut atlas, &all, (880, 352));
    let output = pixels(&gpu, &renderer, &target);
    write_png(&std::path::PathBuf::from(std::env::var_os("RUNE_FILTER_STILL").unwrap()), 880, 352, &output).unwrap();
}

//! Experimental backdrop-sampling compositor versus the public alpha helper. Kept test-only:
//! rendering each backdrop to a separate texture is a lower bound on a general compositor's
//! snapshot/split-pass cost, and does not imply that Blend::Srgb is a supported API.
use super::*;
use super::tests::{rect, pixels};
use super::filter_tests::gpu_time;
use crate::Color;

const COMPOSITE: &str = r#"
@group(0) @binding(0) var foreground: texture_2d<f32>;
@group(0) @binding(1) var background: texture_2d<f32>;
@vertex fn vs(@builtin(vertex_index) v: u32) -> @builtin(position) vec4<f32> {
    return vec4<f32>(f32((v << 1u) & 2u) * 2.0 - 1.0, f32(v & 2u) * 2.0 - 1.0, 0.0, 1.0);
}
fn encode(v: vec3<f32>) -> vec3<f32> {
    return select(1.055 * pow(max(v, vec3(0.0)), vec3(1.0 / 2.4)) - 0.055, v * 12.92, v <= vec3(0.0031308));
}
fn decode(v: vec3<f32>) -> vec3<f32> {
    return select(pow((v + 0.055) / 1.055, vec3(2.4)), v / 12.92, v <= vec3(0.04045));
}
@fragment fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let f = textureLoad(foreground, vec2<i32>(p.xy), 0);
    let b = textureLoad(background, vec2<i32>(p.xy), 0);
    let alpha = f.a + b.a * (1.0 - f.a);
    let rgb = encode(f.rgb / max(f.a, 0.000001)) * f.a
        + encode(b.rgb / max(b.a, 0.000001)) * b.a * (1.0 - f.a);
    return vec4<f32>(decode(rgb / max(alpha, 0.000001)) * alpha, alpha);
}
"#;

#[test]
#[ignore = "manual AppKit fidelity and GPU timing; RUNE_SRGB_REFERENCE is script JSON"]
fn appkit_srgb_fidelity_and_cost() {
    let cases: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(std::env::var("RUNE_SRGB_REFERENCE").unwrap()).unwrap()).unwrap();
    let cases = cases.as_array().unwrap();
    let gpu = Gpu::headless();
    let size = (800, 80); // 400x40pt Scrawl-height pill at 2x
    let target = Offscreen::new(&gpu.device, size.0, size.1);
    let mut helper = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
    let mut foreground = Renderer::new(&gpu.device, FORMAT);
    let mut background = Renderer::new(&gpu.device, FORMAT);
    let mut atlas = crate::text::TextSystem::default().atlas;
    let textures: [wgpu::Texture; 2] = std::array::from_fn(|_| gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sRGB experiment input"), size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[],
    }));
    let views = textures.map(|t| t.create_view(&Default::default()));
    let shader = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("experimental sRGB composite"),
        source: wgpu::ShaderSource::Wgsl(COMPOSITE.into()) });
    let pipeline = gpu.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("experimental sRGB composite"), layout: None,
        vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
        primitive: Default::default(), depth_stencil: None, multisample: Default::default(),
        fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs"), compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState { format: OFFSCREEN_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })] }),
        multiview_mask: None, cache: None,
    });
    let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&views[0]) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&views[1]) },
        ],
    });
    let mut fg = Vec::new(); let mut bg = Vec::new(); let mut compensated = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let color = |key: &str| {
            let c = case[key].as_array().unwrap();
            Color { r: crate::color::srgb_decode(c[0].as_f64().unwrap() as f32),
                g: crate::color::srgb_decode(c[1].as_f64().unwrap() as f32),
                b: crate::color::srgb_decode(c[2].as_f64().unwrap() as f32), a: c[3].as_f64().unwrap() as f32 }
        };
        let (f, b) = (color("fg"), color("bg"));
        let bounds = [index as f32 * 100.0, 0.0, 100.0, 80.0];
        fg.push(rect(bounds, f.premul(1.0))); bg.push(rect(bounds, b.premul(1.0)));
        compensated.extend([rect(bounds, b.premul(1.0)), rect(bounds, crate::theme::srgb_equivalent(f, b).premul(1.0))]);
    }
    helper.prepare(&gpu.device, &gpu.queue, &mut atlas, &compensated, size);
    foreground.prepare(&gpu.device, &gpu.queue, &mut atlas, &fg, size);
    background.prepare(&gpu.device, &gpu.queue, &mut atlas, &bg, size);
    let helper_pixels = pixels(&gpu, &helper, &target);
    let composite = |enc: &mut wgpu::CommandEncoder, writes: Option<wgpu::RenderPassTimestampWrites<'_>>| {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("experimental sRGB composite"), timestamp_writes: writes,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &target.view, depth_slice: None, resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store } })],
            ..Default::default()
        });
        pass.set_pipeline(&pipeline); pass.set_bind_group(0, &bind, &[]); pass.draw(0..3, 0..1);
    };
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    foreground.draw(&mut enc, &views[0], [0.0; 4]); background.draw(&mut enc, &views[1], [0.0; 4]);
    composite(&mut enc, None); target.copy(&mut enc); gpu.queue.submit([enc.finish()]);
    let experimental_pixels = target.read(&gpu.device);
    for (index, case) in cases.iter().enumerate() {
        let at = (40 * 800 + index * 100 + 50) * 4;
        let appkit: Vec<u8> = case["rgba"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u8).collect();
        let a = &helper_pixels[at..at + 4]; let b = &experimental_pixels[at..at + 4];
        let error = |pixel: &[u8]| pixel.iter().zip(&appkit).map(|(a,b)| a.abs_diff(*b)).max().unwrap();
        eprintln!("{} AppKit={appkit:?} helper={a:?} error={} composite={b:?} error={}", case["name"], error(a), error(b));
        if index < 5 { assert!(error(a) <= 1); }
        assert!(error(b) <= 1);
    }
    let helper_time = gpu_time(&gpu, |enc, writes| helper.draw_timed(enc, &target.view, [0.0; 4], Some(writes)));
    let composite_time = gpu_time(&gpu, |enc, writes| {
        background.draw_timed(enc, &views[1], [0.0; 4], Some(wgpu::RenderPassTimestampWrites {
            query_set: writes.query_set, beginning_of_pass_write_index: writes.beginning_of_pass_write_index, end_of_pass_write_index: None }));
        foreground.draw(enc, &views[0], [0.0; 4]);
        composite(enc, Some(wgpu::RenderPassTimestampWrites { beginning_of_pass_write_index: None, ..writes }));
    });
    eprintln!("sRGB 400x40pt@2x helper median/p95={helper_time:?}ms; experimental composite={composite_time:?}ms");
}

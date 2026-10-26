use rune::render::Gpu;
use rune_fx::*;

#[test]
fn external_struct_update_and_emitted_rust_compile() {
    let _literal = FxLayer {
        fx: Fx::distort(DistortParams::chroma_split([0.0; 2])),
        shape: Shape::rounded(0.0, 0.0, 16.0, 16.0, 0.0),
        place: Place::Over, time: 0.0, seed: 1, reduced_motion: false,
        source: None, opacity: 1.0,
    };
    let card = Shape::rounded(10.0, 10.0, 120.0, 80.0, 12.0);
    let layers = [
        Fx::fill(LiquidMetalParams { flow: 0.9, scale: 80.0, ..LiquidMetalParams::rose() }.border(4.0).intensity(0.8)),
        Fx::fill(MeshGradientParams { colors: Stops::hex(&[0x1b1f3b, 0xff6f91, 0xffc75f, 0x845ec2]), drift: 0.15, ..MeshGradientParams::dusk() }),
        Fx::fill(DitherParams { pattern: DitherPattern::BlueNoise, pixel: 2.0, ..DitherParams::backdrop() }),
        Fx::fill(GrainGradientParams { kind: GradientKind::Conic, mono: false, amount: 0.3, ..GrainGradientParams::sweep() }),
        Fx::fill(FlutedGlassParams { rib: 10.0, refraction: 7.0, frost: 0.4, ..FlutedGlassParams::reeded() }),
        Fx::fill(MetaballParams { count: 4, gloss: 0.7, ..MetaballParams::mint() }),
        Fx::fill(SwirlParams { twist: -0.8, warp: 1.2, ..SwirlParams::marble() }),
        Fx::fill(VoronoiParams { scale: 24.0, edge: 2.0, ..VoronoiParams::stained() }),
        Fx::fill(WaveParams { amplitude: 0.06, softness: 0.01, ..WaveParams::dunes() }),
        Fx::fill(HalftoneParams { input: FillInput::Linear, cell: 5.0, ..HalftoneParams::print() }),
    ];
    for fx in layers {
        let Fx::Fill(p) = fx else { unreachable!() };
        assert!(!p.to_rust().is_empty());
        let layer = fx.on(card).at(1.0);
        assert_eq!(layer.place, if p.needs_backdrop() { Place::Over } else { Place::Under });
    }
}

#[test]
fn linear_backdrop_views_are_not_double_decoded_in_srgb_frames() {
    use rune::render::Offscreen;
    let Gpu { device, queue, .. } = Gpu::headless();
    let mut fx = FxRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm, FxOptions::default());
    fx.set_srgb_blend(true);
    let layers = [Fx::distort(DistortParams::chroma_split([1e-6, 0.0]))
        .on(Shape::rounded(0.0, 0.0, 32.0, 32.0, 0.0))];
    for format in [wgpu::TextureFormat::Rgba8UnormSrgb, wgpu::TextureFormat::Rgba16Float] {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: 32, height: 32, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
            format, usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        // Direct frame input retains the old linear-samples contract; a hook can also
        // override the pass-copy convention for its own linear or hardware-decoded view.
        for per_slot in [false, true] {
            let target = Offscreen::with_format(&device, 32, 32, wgpu::TextureFormat::Rgba8Unorm);
            let frame = FxFrame { viewport: [32, 32], scale: 1.0, layers: &layers,
                backdrop: (!per_slot).then_some(&view), sources: &[] };
            let slots = if per_slot { vec![FxSlot::default().backdrop_srgb(false)] } else { vec![] };
            assert_eq!(fx.prepare_with_slots(&device, &queue, &frame, &slots).drawn, 1);
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view, depth_slice: None, resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.25, g: 0.125, b: 0.0625, a: 1.0 }), store: wgpu::StoreOp::Store },
                    })], ..Default::default()
                });
            }
            fx.blur_backdrop(&device, &queue, &mut encoder, &view, 0);
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &target.view, depth_slice: None, resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                    })], ..Default::default()
                });
                fx.draw_layer(&mut pass, 0);
            }
            target.copy(&mut encoder);
            queue.submit([encoder.finish()]);
            let pixels = target.read(&device);
            let at = (16 * 32 + 16) * 4;
            let expected = [0.25, 0.125, 0.0625].map(|c| (rune::color::srgb_encode(c) * 255.0).round() as u8);
            for (got, want) in pixels[at..at + 3].iter().zip(expected) {
                assert!(got.abs_diff(want) <= 2, "{format:?} per_slot={per_slot}: {got} != {want}");
            }
        }
    }
}

#[test]
fn gpu_fills_are_finite_at_extremes_degenerate_params_and_large_time() {
    let Gpu { device, queue, .. } = Gpu::headless();
    let fmt = wgpu::TextureFormat::Rgba16Float;
    let mut fx = FxRenderer::new(&device, fmt, FxOptions { dither: true });
    let (w, h) = (32u32, 32u32);
    let row_bytes = w * 8; // 256 bytes: aligned to COPY_BYTES_PER_ROW_ALIGNMENT
    let mk_tex = |usage: wgpu::TextureUsages| {
        let t = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: fmt,
            usage,
            view_formats: &[],
        });
        let v = t.create_view(&Default::default());
        (t, v)
    };
    let (target, target_view) = mk_tex(wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC);
    let (_bd, bd_view) = mk_tex(wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING);
    let (_src, src_view) = mk_tex(wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (row_bytes * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let shape = Shape::rounded(2.0, 2.0, 28.0, 28.0, 6.0);
    let mut draw = |layer: FxLayer| -> Vec<u16> {
        let layers = [layer];
        let sources = [&src_view];
        let stats = fx.prepare(&device, &queue, &FxFrame {
            viewport: [w, h],
            scale: 1.0,
            layers: &layers,
            backdrop: Some(&bd_view),
            sources: &sources,
        });
        assert_eq!(stats.drawn, 1);
        let mut enc = device.create_command_encoder(&Default::default());
        fx.blur_backdrop(&device, &queue, &mut enc, &bd_view, 0);
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            fx.draw_layer(&mut pass, 0);
        }
        enc.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row_bytes), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        queue.submit([enc.finish()]);
        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |r| r.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let words: Vec<u16> = bytemuck::cast_slice(&slice.get_mapped_range().unwrap()).to_vec();
        readback.unmap();
        for &word in &words {
            assert_ne!(word & 0x7c00, 0x7c00, "non-finite f16 word {word:#06x}");
        }
        words
    };

    // Every style at catalog min, catalog max, and t = 1e5 s.
    for style in fill_styles() {
        let mut min_fill = style.default_fill();
        let mut max_fill = style.default_fill();
        for p in style.params {
            match p.kind {
                ParamKind::Number { min, max, .. } => {
                    min_fill.set(p.id, Value::Number(min));
                    max_fill.set(p.id, Value::Number(max));
                }
                ParamKind::Count { min, max } => {
                    min_fill.set(p.id, Value::Count(min));
                    max_fill.set(p.id, Value::Count(max));
                }
                ParamKind::Choice(c) => {
                    min_fill.set(p.id, Value::Choice(0));
                    max_fill.set(p.id, Value::Choice(c.len() - 1));
                }
                ParamKind::Colors { min, max } => {
                    let palette = [0x112233, 0x445566, 0x778899, 0xaabbcc, 0xddeeff, 0xff8800];
                    min_fill.set(p.id, Value::Colors(Stops::hex(&palette[..min])));
                    max_fill.set(p.id, Value::Colors(Stops::hex(&palette[..max])));
                }
                _ => {}
            }
        }
        draw(Fx::fill(min_fill).on(shape).source(0, Rect::new(2.0, 2.0, 28.0, 28.0)).at(0.5));
        draw(Fx::fill(max_fill).on(shape).source(0, Rect::new(2.0, 2.0, 28.0, 28.0)).at(0.5));

        let preset = style.default_fill();
        let f0 = draw(Fx::fill(preset).on(shape).at(1e5));
        let f1 = draw(Fx::fill(preset).on(shape).at(1e5 + 0.25));
        if Fx::fill(preset).on(shape).is_animating() {
            assert_ne!(f0, f1, "{} still animates at t = 1e5", style.id);
        }
    }

    // Degenerate cases: conic centre on an exact pixel centre, zero-radius metaballs, extreme mesh sharpness, zero-size source rect.
    let pixel_center = Shape::rounded(0.5, 0.5, 20.0, 20.0, 0.0);
    draw(Fx::fill(GrainGradientParams { kind: GradientKind::Conic, center: [0.5, 0.5], ..GrainGradientParams::sweep() }).on(pixel_center).at(1.0));
    draw(Fx::fill(MetaballParams { size: 0.0, ..MetaballParams::lava() }).on(shape).at(1.0));
    draw(Fx::fill(MeshGradientParams { sharpness: 50.0, ..MeshGradientParams::dusk() }).on(shape).at(1.0));
    draw(Fx::fill(DitherParams { input: FillInput::Image, ..DitherParams::bayer() }).on(shape).source(0, Rect::new(0.0, 0.0, 0.0, 0.0)).at(1.0));
}

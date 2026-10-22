//! FxRenderer: one pipeline per effect family (built ahead of time), one storage buffer of
//! instances, one of motion poses. `prepare` uploads and culls; `draw_layer` / `draw_all` encode
//! into the caller's render pass (target must not be the backdrop texture).

use crate::*;

const COMMON: &str = include_str!("shaders/common.wgsl");
const SOURCES: [(&str, &str); KINDS] = [
    ("glow", include_str!("shaders/glow.wgsl")),
    ("pulse", include_str!("shaders/pulse.wgsl")),
    ("electric", include_str!("shaders/electric.wgsl")),
    ("fire", include_str!("shaders/fire.wgsl")),
    ("distort", include_str!("shaders/distort.wgsl")),
    ("motion", include_str!("shaders/motion.wgsl")),
    ("extras", include_str!("shaders/extras.wgsl")),
];

/// Everything one frame of effects needs.
pub struct FxFrame<'a> {
    pub viewport: [u32; 2],
    /// Device px per pt.
    pub scale: f32,
    pub layers: &'a [FxLayer],
    /// The target's content beneath the layers (for distortion). Must not be the render target.
    pub backdrop: Option<&'a wgpu::TextureView>,
    /// Source textures layers refer to by `Source::slot` (mask pyramids, node layers).
    pub sources: &'a [&'a wgpu::TextureView],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FxStats {
    pub drawn: u32,
    pub culled_idle: u32,
    pub culled_offscreen: u32,
    /// Covered device pixels across drawn quads.
    pub pixels: u64,
    pub needs_backdrop: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FxOptions {
    /// Add TPDF dither in the effect shaders (set for 8-bit targets; leave off for 16f targets
    /// that dither once at the final resolve).
    pub dither: bool,
}

struct Prepared {
    kind: usize,
    instance: u32,
    bind: usize,
}

pub struct FxRenderer {
    pipelines: Vec<wgpu::RenderPipeline>,
    bgl0: wgpu::BindGroupLayout,
    bgl1: wgpu::BindGroupLayout,
    bg0: Option<wgpu::BindGroup>,
    globals: wgpu::Buffer,
    insts: wgpu::Buffer,
    inst_cap: usize,
    xforms: wgpu::Buffer,
    xform_cap: usize,
    sampler: wgpu::Sampler,
    dummy: wgpu::TextureView,
    binds: Vec<wgpu::BindGroup>,
    prepared: Vec<Option<Prepared>>,
    scratch: Vec<InstRaw>,
    scratch_xf: Vec<[f32; 4]>,
    mip_pipeline: wgpu::RenderPipeline,
    mip_bgl: wgpu::BindGroupLayout,
    opts: FxOptions,
}

const INST_SIZE: usize = std::mem::size_of::<InstRaw>();

impl FxRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, opts: FxOptions) -> FxRenderer {
        let entry = |binding, visibility, ty| wgpu::BindGroupLayoutEntry { binding, visibility, ty, count: None };
        let vf = wgpu::ShaderStages::VERTEX_FRAGMENT;
        let fr = wgpu::ShaderStages::FRAGMENT;
        let storage = wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let bgl0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fx0"),
            entries: &[
                entry(
                    0,
                    vf,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
                entry(1, vf, storage),
                entry(2, fr, storage),
            ],
        });
        let tex = wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let bgl1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fx1"),
            entries: &[
                entry(0, fr, tex),
                entry(1, fr, tex),
                entry(2, fr, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)),
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fx"),
            bind_group_layouts: &[Some(&bgl0), Some(&bgl1)],
            immediate_size: 0,
        });
        let pipelines = SOURCES
            .iter()
            .map(|(name, src)| {
                let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(name),
                    source: wgpu::ShaderSource::Wgsl(format!("{COMMON}\n{src}").into()),
                });
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(name),
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: &module,
                        entry_point: Some("vs"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleStrip,
                        ..Default::default()
                    },
                    depth_stencil: None,
                    multisample: Default::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &module,
                        entry_point: Some("fs"),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format,
                            blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            })
            .collect();
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("fx linear"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let dummy = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("fx dummy"),
                size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());

        // Mask pyramid downsampler (Rgba16Float levels).
        let mip_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fx mip"),
            entries: &[entry(0, fr, tex), entry(1, fr, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering))],
        });
        let mip_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fx mip"),
            bind_group_layouts: &[Some(&mip_bgl)],
            immediate_size: 0,
        });
        let mip_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fx mip"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/mip.wgsl").into()),
        });
        let mip_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fx mip"),
            layout: Some(&mip_layout),
            vertex: wgpu::VertexState {
                module: &mip_module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &mip_module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: MaskPyramid::FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let inst_cap = 64;
        let xform_cap = 4096;
        FxRenderer {
            pipelines,
            bgl0,
            bgl1,
            bg0: None,
            globals,
            insts: Self::buffer(device, "fx insts", inst_cap * INST_SIZE),
            inst_cap,
            xforms: Self::buffer(device, "fx xforms", xform_cap * 16),
            xform_cap,
            sampler,
            dummy,
            binds: Vec::new(),
            prepared: Vec::new(),
            scratch: Vec::new(),
            scratch_xf: Vec::new(),
            mip_pipeline,
            mip_bgl,
            opts,
        }
    }

    fn buffer(device: &wgpu::Device, label: &str, size: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: size.max(256) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Cull, pack and upload. Layer `i` of `frame.layers` can then be drawn with `draw_layer(pass, i)`.
    pub fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, frame: &FxFrame) -> FxStats {
        let mut stats = FxStats::default();
        self.scratch.clear();
        self.scratch_xf.clear();
        self.prepared.clear();
        self.binds.clear();
        let vw = frame.viewport[0] as f32;
        let vh = frame.viewport[1] as f32;
        let s = frame.scale;
        // Bind groups keyed by (needs backdrop, source slot) so equal layers share one.
        let mut keys: Vec<(bool, Option<u32>)> = Vec::new();
        for layer in frame.layers {
            let Some((kind, mut inst, b)) = layer.pack() else {
                stats.culled_idle += 1;
                self.prepared.push(None);
                continue;
            };
            let x0 = (b.x * s).floor().max(0.0);
            let y0 = (b.y * s).floor().max(0.0);
            let x1 = ((b.x + b.w) * s).ceil().min(vw);
            let y1 = ((b.y + b.h) * s).ceil().min(vh);
            if x1 <= x0 || y1 <= y0 {
                stats.culled_offscreen += 1;
                self.prepared.push(None);
                continue;
            }
            layer.fill(&mut inst, &mut self.scratch_xf);
            inst.bounds = [x0, y0, x1, y1];
            inst.shape[1] = s;
            let needs_bd = layer.fx.needs_backdrop();
            stats.needs_backdrop |= needs_bd;
            let key = (needs_bd && frame.backdrop.is_some(), layer.source.map(|s| s.slot));
            let bind = match keys.iter().position(|k| *k == key) {
                Some(p) => p,
                None => {
                    let bd = if key.0 { frame.backdrop.unwrap() } else { &self.dummy };
                    let src = key.1.and_then(|slot| frame.sources.get(slot as usize).copied()).unwrap_or(&self.dummy);
                    self.binds.push(device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("fx1"),
                        layout: &self.bgl1,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(bd) },
                            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(src) },
                            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                        ],
                    }));
                    keys.push(key);
                    keys.len() - 1
                }
            };
            stats.drawn += 1;
            stats.pixels += ((x1 - x0) * (y1 - y0)) as u64;
            self.prepared.push(Some(Prepared { kind, instance: self.scratch.len() as u32, bind }));
            self.scratch.push(inst);
        }
        let mut regrow = self.bg0.is_none();
        if self.scratch.len() > self.inst_cap {
            self.inst_cap = self.scratch.len().next_power_of_two();
            self.insts = Self::buffer(device, "fx insts", self.inst_cap * INST_SIZE);
            regrow = true;
        }
        if self.scratch_xf.len() > self.xform_cap {
            self.xform_cap = self.scratch_xf.len().next_power_of_two();
            self.xforms = Self::buffer(device, "fx xforms", self.xform_cap * 16);
            regrow = true;
        }
        if regrow {
            self.bg0 = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("fx0"),
                layout: &self.bgl0,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.globals.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: self.insts.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: self.xforms.as_entire_binding() },
                ],
            }));
        }
        if !self.scratch.is_empty() {
            queue.write_buffer(&self.insts, 0, bytemuck::cast_slice(&self.scratch));
        }
        if !self.scratch_xf.is_empty() {
            queue.write_buffer(&self.xforms, 0, bytemuck::cast_slice(&self.scratch_xf));
        }
        let g = [vw, vh, if self.opts.dither { 1.0 } else { 0.0 }, 0.0];
        queue.write_buffer(&self.globals, 0, bytemuck::cast_slice(&g));
        stats
    }

    /// Draw one prepared layer (no-op when it was culled).
    pub fn draw_layer(&self, pass: &mut wgpu::RenderPass<'_>, layer: usize) {
        let Some(Some(p)) = self.prepared.get(layer) else { return };
        pass.set_pipeline(&self.pipelines[p.kind]);
        pass.set_bind_group(0, self.bg0.as_ref().unwrap(), &[]);
        pass.set_bind_group(1, &self.binds[p.bind], &[]);
        pass.draw(0..4, p.instance..p.instance + 1);
    }

    /// Draw every prepared layer with `place` in order.
    pub fn draw_all(&self, pass: &mut wgpu::RenderPass<'_>, layers: &[FxLayer], place: Option<Place>) {
        for (i, l) in layers.iter().enumerate() {
            if place.is_none_or(|p| p == l.place) {
                self.draw_layer(pass, i);
            }
        }
    }

    /// Fill mips 1.. of a mask pyramid from its level 0 (render the mask into `pyramid.level(0)` first).
    pub fn build_mips(&self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, pyramid: &MaskPyramid) {
        let binds = pyramid.binds.get_or_init(|| {
            (1..pyramid.mips)
                .map(|lvl| {
                    device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("fx mip"),
                        layout: &self.mip_bgl,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(&pyramid.levels[(lvl - 1) as usize]),
                            },
                            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                        ],
                    })
                })
                .collect()
        });
        for (lvl, bg) in (1..pyramid.mips).zip(binds) {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fx mip"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &pyramid.levels[lvl as usize],
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.mip_pipeline);
            pass.set_bind_group(0, bg, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

/// A mask (text, icon) and its blur pyramid, for `GlowStyle::Mask` and mask shimmer.
pub struct MaskPyramid {
    pub texture: wgpu::Texture,
    /// All mips: bind this as the layer source.
    pub view: wgpu::TextureView,
    /// One view per mip level (level 0 is the render target for the mask itself).
    pub levels: Vec<wgpu::TextureView>,
    pub mips: u32,
    pub size: [u32; 2],
    binds: std::sync::OnceLock<Vec<wgpu::BindGroup>>,
}

impl MaskPyramid {
    pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> MaskPyramid {
        let mips = (32 - width.max(height).leading_zeros()).clamp(1, 7);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("fx mask pyramid"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: mips,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: Self::FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let levels = (0..mips)
            .map(|m| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    base_mip_level: m,
                    mip_level_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        MaskPyramid { texture, view, levels, mips, size: [width, height], binds: std::sync::OnceLock::new() }
    }
}

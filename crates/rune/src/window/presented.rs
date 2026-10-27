use crate::runtime::Runtime;

/// What an idle window's last frame drew. A timer that wakes the window builds a frame before
/// taking a drawable; when it would draw these same pixels, nothing is acquired or presented.
pub(super) struct Presented {
    instances: Vec<crate::render::Instance>,
    clear: [f32; 4],
    size: (u32, u32),
    format: wgpu::TextureFormat,
    srgb: bool,
    scale: f32,
    transparent: bool,
}

impl Presented {
    pub(super) fn record<S: 'static>(this: &mut Option<Self>, rt: &Runtime<S>, config: &wgpu::SurfaceConfiguration) {
        let (clear, size, srgb, scale, transparent) = (rt.clear_color(), (config.width, config.height), rt.srgb_blend(), rt.env.scale, rt.transparent);
        match this {
            Some(p) => {
                p.instances.clone_from(&rt.instances);
                (p.clear, p.size, p.format, p.srgb, p.scale, p.transparent) = (clear, size, config.format, srgb, scale, transparent);
            }
            None => *this = Some(Self { instances: rt.instances.clone(), clear, size, format: config.format, srgb, scale, transparent }),
        }
    }

    /// Whether `rt`'s built frame draws what this one did: the same instances into the same
    /// surface, with no glyph or image upload waiting, and nothing drawn outside the instances
    /// (`SurfaceDraw` content, native materials).
    pub(super) fn same<S: 'static>(&self, rt: &Runtime<S>, config: &wgpu::SurfaceConfiguration) -> bool {
        let atlas = &rt.text.atlas;
        self.size == (config.width, config.height) && self.format == config.format && self.srgb == rt.srgb_blend()
            && self.scale == rt.env.scale && self.clear == rt.clear_color() && self.transparent == rt.transparent
            && rt.surfaces.is_empty() && rt.materials.is_empty()
            && atlas.dirty.is_none() && atlas.pages.iter().all(|p| p.dirty.is_none())
            && rt.images.pages.iter().all(|p| p.dirty.is_none())
            && bytemuck::cast_slice::<_, u8>(&self.instances) == bytemuck::cast_slice::<_, u8>(&rt.instances)
    }
}

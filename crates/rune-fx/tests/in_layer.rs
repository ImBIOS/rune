//! An effect drawn by a Rune compositor at an effect slot: inside a faded, rounded layer, in
//! either blend mode, through one renderer serving the frame target and the layer's target.

use std::sync::{Arc, Mutex};

use rune::color::{hex, srgb_decode, srgb_encode};
use rune::compose::{Compositor, Frame};
use rune::render::{EffectSite, KIND_EFFECT};
use rune::{App, AppSpec, Cx, El, effect, el};
use rune_fx::*;

struct Dusk {
    fx: Option<FxRenderer>,
    formats: Arc<Mutex<Vec<wgpu::TextureFormat>>>,
}

impl Compositor<()> for Dusk {
    fn render(&mut self, f: &mut Frame<'_, ()>) {
        let (device, queue, size) = (f.device, f.queue, f.size);
        let slots: Vec<(u32, [f32; 4])> = f.rt.instances.iter().enumerate()
            .filter(|(_, i)| i.params[1] == KIND_EFFECT).map(|(n, i)| (n as u32, i.rect)).collect();
        let layers: Vec<FxLayer> = slots.iter()
            .map(|(_, r)| Fx::fill(MeshGradientParams::dusk()).on(Shape::rounded(r[0], r[1], r[2], r[3], 0.0)).at(1.0)).collect();
        let fx = self.fx.get_or_insert_with(|| FxRenderer::new(device, f.format, FxOptions::default()));
        fx.set_srgb_blend(f.srgb_blend());
        fx.add_format(device, wgpu::TextureFormat::Rgba16Float);
        fx.prepare(device, queue, &FxFrame { viewport: [size.0, size.1], scale: 1.0, layers: &layers, backdrop: None, sources: &[] });
        let clear = f.rt.clear_color();
        let count = f.rt.instances.len() as u32;
        f.ui.prepare(device, queue, &mut f.rt.text.atlas, &f.rt.instances, size);
        let formats = &self.formats;
        let mut hook = |pass: &mut wgpu::RenderPass<'_>, s: &EffectSite<'_>| {
            let mut seen = formats.lock().unwrap();
            if !seen.contains(&s.format) { seen.push(s.format); }
            if let Some(k) = slots.iter().position(|(at, _)| *at == s.index) {
                fx.draw_layer_in(pass, s.format, s.origin, k);
            }
        };
        f.ui.encode_layers_with(f.encoder, &mut hook);
        let mut pass = f.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("frame"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: f.target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r: clear[0] as f64, g: clear[1] as f64, b: clear[2] as f64, a: clear[3] as f64 }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        f.ui.draw_range_with(&mut pass, f.format, 0..count, &mut hook);
    }
}

fn snap(name: &'static str, view: fn(&(), &mut Cx<()>) -> El, srgb: bool, formats: Arc<Mutex<Vec<wgpu::TextureFormat>>>) -> (u32, Vec<u8>) {
    let dir = std::env::temp_dir().join(format!("rune-fx-layer-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("snap.png");
    App::new(AppSpec::new(name, (96.0, 96.0), || (), view))
        .compositor(Dusk { fx: None, formats })
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

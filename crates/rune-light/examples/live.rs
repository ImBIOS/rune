//! Interactive rune-light: the flashlight and chat demos in a window.
//!
//!     live                      window (1 flashlight, 2 chat, scroll radius, click god rays, D direct follow, Esc quit)
//!     live --snap out.png [--scene chat] [--at X,Y]   one frame headless at 2560×1600

#[allow(dead_code)]
#[path = "light.rs"]
mod light;

use std::sync::Arc;
use std::time::Instant;

use light::{Frame, Spring, Track, chat_scene, flashlight_scene, set_scale};
use rune::render::Renderer;
use rune::text::TextSystem;
use rune_light::{LightRenderer, Scene};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

/// The scenes are laid out on a 1280×800 pt canvas.
const PT_W: f32 = 1280.0;
const PT_H: f32 = 800.0;
const FOLLOW: Spring = Spring::new(0.12, 0.0);
const LAMP: Spring = Spring::new(0.2, 0.0);

// ---------------------------------------------------------------- GPU

struct Gfx {
    device: wgpu::Device,
    queue: wgpu::Queue,
    rune: Renderer,
    ts: TextSystem,
    light: LightRenderer,
    base: wgpu::Texture,
    text: wgpu::Texture,
    size: (u32, u32),
    format: wgpu::TextureFormat,
}

fn layer(device: &wgpu::Device, label: &str, (w, h): (u32, u32)) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

impl Gfx {
    fn new(device: wgpu::Device, queue: wgpu::Queue, size: (u32, u32), format: wgpu::TextureFormat) -> Gfx {
        let rune = Renderer::new(&device, wgpu::TextureFormat::Rgba16Float);
        let light = LightRenderer::new(&device, &queue, size.0, size.1, format);
        let base = layer(&device, "ui base", size);
        let text = layer(&device, "ui text", size);
        Gfx { device, queue, rune, ts: TextSystem::default(), light, base, text, size, format }
    }

    fn resize(&mut self, size: (u32, u32)) {
        if size == self.size {
            return;
        }
        self.size = size;
        self.light = LightRenderer::new(&self.device, &self.queue, size.0, size.1, self.format);
        self.base = layer(&self.device, "ui base", size);
        self.text = layer(&self.device, "ui text", size);
    }

    /// Same path as the headless demo: rune draws the UI base and text layers, rune-light lights and composites.
    fn render(&mut self, frame: Frame, target: &wgpu::TextureView) {
        let (prims, base, text, clear, lights, mut s) = frame;
        s.seed = 0.0;
        let bv = self.base.create_view(&Default::default());
        let tv = self.text.create_view(&Default::default());
        self.rune.prepare(&self.device, &self.queue, &mut self.ts.atlas, &base, self.size);
        let mut enc = self.device.create_command_encoder(&Default::default());
        self.rune.draw(&mut enc, &bv, [clear.r, clear.g, clear.b, 1.0]);
        self.queue.submit([enc.finish()]);
        self.rune.prepare(&self.device, &self.queue, &mut self.ts.atlas, &text, self.size);
        let mut enc = self.device.create_command_encoder(&Default::default());
        self.rune.draw(&mut enc, &tv, [0.0; 4]);
        self.queue.submit([enc.finish()]);
        let mut enc = self.device.create_command_encoder(&Default::default());
        let scene = Scene { prims: &prims, lights: &lights, settings: s, emission: None };
        self.light.render(&self.device, &self.queue, &mut enc, &scene, &bv, &tv, target);
        self.queue.submit([enc.finish()]);
    }
}

fn request_device(instance: &wgpu::Instance, surface: Option<&wgpu::Surface<'_>>) -> (wgpu::Adapter, wgpu::Device, wgpu::Queue) {
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: surface,
        apply_limit_buckets: false,
    }))
    .expect("adapter");
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("rune-light live"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: Default::default(),
        memory_hints: wgpu::MemoryHints::Performance,
        trace: Default::default(),
    }))
    .expect("device");
    (adapter, device, queue)
}

// ---------------------------------------------------------------- interaction state

#[derive(Clone, Copy, PartialEq)]
enum Which {
    Flashlight,
    Chat,
}

struct State {
    which: Which,
    start: Instant,
    /// Pointer in points.
    cursor: [f32; 2],
    /// The followed point: torch aim (flashlight) or lamp (chat), in points.
    x: Track,
    y: Track,
    target: [f32; 2],
    lamp: [f32; 2],
    direct: bool,
    radius: f32,
    rays: bool,
    press: Option<[f32; 2]>,
    dragging: bool,
}

impl State {
    fn new(which: Which) -> State {
        let mut s = State {
            which: Which::Flashlight,
            start: Instant::now(),
            cursor: [640.0, 400.0],
            x: Track::at(640.0),
            y: Track::at(400.0),
            target: [640.0, 400.0],
            lamp: [1085.0, 250.0],
            direct: false,
            radius: 1.0,
            rays: false,
            press: None,
            dragging: false,
        };
        s.switch(which);
        s
    }

    fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    fn jump(&mut self, p: [f32; 2]) {
        self.x = Track::at(p[0]);
        self.y = Track::at(p[1]);
        self.target = p;
    }

    fn switch(&mut self, which: Which) {
        if self.which == Which::Chat {
            self.lamp = self.target;
        }
        self.which = which;
        self.dragging = false;
        match which {
            Which::Flashlight => self.jump(self.cursor),
            Which::Chat => self.jump(self.lamp),
        }
    }

    fn follow(&mut self, p: [f32; 2], spring: Spring) {
        self.target = p;
        if self.direct {
            self.jump(p);
        } else {
            let t = self.now();
            self.x.retarget(t, p[0], spring);
            self.y.retarget(t, p[1], spring);
        }
    }

    fn pointer_move(&mut self, p: [f32; 2]) {
        self.cursor = p;
        if let Some(down) = self.press
            && ((p[0] - down[0]).powi(2) + (p[1] - down[1]).powi(2)).sqrt() > 4.0
        {
            self.dragging = true;
        }
        match self.which {
            Which::Flashlight => self.follow(p, FOLLOW),
            Which::Chat if self.dragging => self.follow(p, LAMP),
            Which::Chat => {}
        }
    }

    fn button(&mut self, pressed: bool) {
        if pressed {
            self.press = Some(self.cursor);
            self.dragging = false;
        } else {
            if self.press.is_some() && !self.dragging {
                self.rays = !self.rays;
            }
            self.press = None;
            self.dragging = false;
        }
    }

    fn position(&self, t: f64) -> ([f32; 2], bool) {
        let (x, vx) = self.x.sample(t);
        let (y, vy) = self.y.sample(t);
        let moving = (x - self.target[0]).abs() > 0.05 || (y - self.target[1]).abs() > 0.05 || vx.abs() > 0.5 || vy.abs() > 0.5;
        ([x, y], moving)
    }

    /// Builds the frame with the same scene builders as the headless demos, then applies the live knobs.
    fn frame(&self, ts: &mut TextSystem) -> (Frame, bool) {
        let t = self.now();
        let (p, moving) = self.position(t);
        let mut f = match self.which {
            Which::Flashlight => flashlight_scene(ts, t as f32, p),
            Which::Chat => chat_scene(ts, t as f32, p),
        };
        if let Some(l) = f.4.first_mut() {
            l.pos[3] *= self.radius;
            l.extra[3] *= self.radius.sqrt();
            if self.rays {
                l.extra[0] = 1.5;
            }
        }
        if self.rays {
            f.5.shafts_over = f.5.shafts_over.max(0.12);
        }
        (f, moving)
    }
}

// ---------------------------------------------------------------- window

struct Live {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    gfx: Gfx,
}

struct App {
    state: State,
    live: Option<Live>,
}

impl Live {
    /// Device px per point: the window's scale at the native 1280×800 pt size, fitted when resized.
    fn k(&self) -> f32 {
        (self.config.width as f32 / PT_W).min(self.config.height as f32 / PT_H).max(0.1)
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.live.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("rune-light")
            .with_inner_size(LogicalSize::new(PT_W as f64, PT_H as f64))
            .with_min_inner_size(LogicalSize::new(480.0, 300.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("window"));
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(window.clone()).expect("surface");
        let (adapter, device, queue) = request_device(&instance, Some(&surface));
        let phys = window.inner_size();
        let mut config = surface.get_default_config(&adapter, phys.width.max(1), phys.height.max(1)).expect("surface config");
        let caps = surface.get_capabilities(&adapter);
        config.format = caps.formats.iter().copied().find(|f| f.is_srgb()).unwrap_or(config.format);
        config.present_mode = wgpu::PresentMode::AutoVsync;
        config.desired_maximum_frame_latency = 2;
        surface.configure(&device, &config);
        let gfx = Gfx::new(device, queue, (config.width, config.height), config.format);
        window.request_redraw();
        self.live = Some(Live { window, surface, config, gfx });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(l) = self.live.as_mut() else { return };
        let st = &mut self.state;
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                l.config.width = size.width.max(1);
                l.config.height = size.height.max(1);
                l.surface.configure(&l.gfx.device, &l.config);
                l.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => l.window.request_redraw(),
            WindowEvent::CursorMoved { position, .. } => {
                let k = l.k();
                st.pointer_move([position.x as f32 / k, position.y as f32 / k]);
                l.window.request_redraw();
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                st.button(state == ElementState::Pressed);
                l.window.request_redraw();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let steps = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 20.0,
                };
                st.radius = (st.radius * 1.12f32.powf(steps)).clamp(0.1, 10.0);
                l.window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                match event.logical_key.as_ref() {
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    Key::Character("1") => st.switch(Which::Flashlight),
                    Key::Character("2") => st.switch(Which::Chat),
                    Key::Character("d") | Key::Character("D") => st.direct = !st.direct,
                    Key::Character("g") | Key::Character("G") => st.rays = !st.rays,
                    Key::Character("r") | Key::Character("R") => st.radius = 1.0,
                    _ => {}
                }
                l.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                l.gfx.resize((l.config.width, l.config.height));
                set_scale(l.k());
                let surface_tex = match l.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
                    wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                        l.surface.configure(&l.gfx.device, &l.config);
                        l.window.request_redraw();
                        return;
                    }
                    _ => {
                        l.window.request_redraw();
                        return;
                    }
                };
                let view = surface_tex.texture.create_view(&Default::default());
                let (frame, moving) = st.frame(&mut l.gfx.ts);
                l.gfx.render(frame, &view);
                l.window.pre_present_notify();
                l.gfx.queue.present(surface_tex);
                if moving {
                    l.window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- headless snapshot

fn snap(path: &str, which: Which, at: Option<[f32; 2]>, rays: bool) {
    let (w, h) = (2560u32, 1600u32);
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let (_, device, queue) = request_device(&instance, None);
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut gfx = Gfx::new(device, queue, (w, h), format);
    set_scale(2.0);
    let mut st = State::new(which);
    if let Some(p) = at {
        st.cursor = p;
        st.switch(which);
        st.jump(p);
    }
    st.rays = rays;
    let out = gfx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("snap"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let (frame, _) = st.frame(&mut gfx.ts);
    gfx.render(frame, &out.create_view(&Default::default()));
    let buf = gfx.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (w * 4 * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = gfx.device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: &out, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(h) } },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    gfx.queue.submit([enc.finish()]);
    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |r| r.expect("map"));
    gfx.device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
    let px = slice.get_mapped_range().expect("range").to_vec();
    rune::render::write_png(std::path::Path::new(path), w, h, &px).unwrap();
    eprintln!("wrote {path}");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let which = if arg("--scene").as_deref() == Some("chat") { Which::Chat } else { Which::Flashlight };
    if let Some(path) = arg("--snap") {
        let at = arg("--at").and_then(|s| {
            let (x, y) = s.split_once(',')?;
            Some([x.parse().ok()?, y.parse().ok()?])
        });
        snap(&path, which, at, args.iter().any(|a| a == "--rays"));
        return;
    }
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App { state: State::new(which), live: None };
    event_loop.run_app(&mut app).expect("run");
}

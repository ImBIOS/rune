//! Overlays: observational surfaces beside the app's window (a recording HUD, meeting
//! capsules). Each has its own state type and `Runtime`, is fed from any thread through an
//! `OverlayHandle`, and is drawn transparent above every app, centred on the top or bottom
//! edge of the primary screen. An overlay never takes focus and the pointer passes through it.
//!
//! - Wayland: a layer-shell surface on the overlay layer (`layer_shell.rs`), keyboard none,
//!   empty input region. Hidden means destroyed. Without layer-shell the overlay is unavailable.
//! - X11: an override-redirect notification window with an empty input shape.
//! - macOS: a borderless window at status level on every space, ignoring mouse events,
//!   ordered front without becoming key or activating the app.
//! - Windows: a topmost tool window with `WS_EX_NOACTIVATE`, `WS_EX_TRANSPARENT` and
//!   `WS_EX_LAYERED`, shown with `SW_SHOWNOACTIVATE`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowId, WindowLevel};

use crate::el::{El, Tx};
use crate::render::{Gpu, Renderer};
use crate::runtime::{Cx, Env, Runtime, Waker};
use crate::text::TextSystem;
use crate::tooling::AppSpec;
use crate::window::{AnyHost, WakeEvent};

/// The screen edge an overlay is centred on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScreenEdge {
    #[default]
    Top,
    Bottom,
}

pub struct OverlaySpec<T: 'static> {
    /// The window title, or the layer-shell namespace on Wayland.
    pub name: &'static str,
    /// In points.
    pub size: (f32, f32),
    pub edge: ScreenEdge,
    /// Distance from the edge, in points.
    pub margin: f32,
    /// The overlay's view. Its root should have no fill: whatever it leaves clear stays
    /// transparent.
    pub view: fn(&T, &mut Cx<T>) -> El,
}

/// Whether the platform could show the overlay, known after its first `show`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    Unknown,
    Available,
    /// Wayland without layer-shell, or the surface couldn't be created.
    Unavailable,
}

type Update<T> = Box<dyn FnOnce(&mut T, &mut Tx) + Send>;

struct Requests<T> {
    visible: Option<bool>,
    edge: Option<ScreenEdge>,
    margin: Option<f32>,
    updates: Vec<Update<T>>,
}

impl<T> Default for Requests<T> {
    fn default() -> Self {
        Requests { visible: None, edge: None, margin: None, updates: Vec::new() }
    }
}

struct Shared<T> {
    requests: Mutex<Requests<T>>,
    availability: AtomicU8,
    /// The event loop's wake-up for this overlay; empty until the loop runs.
    wake: Mutex<Option<Waker>>,
}

impl<T> Shared<T> {
    fn wake(&self) {
        let wake = self.wake.lock().unwrap().clone();
        if let Some(wake) = wake {
            wake();
        }
    }
}

/// Shows, hides, moves and feeds an overlay from any thread; every call wakes the event loop.
pub struct OverlayHandle<T: 'static> {
    shared: Arc<Shared<T>>,
}

impl<T> Clone for OverlayHandle<T> {
    fn clone(&self) -> Self {
        OverlayHandle { shared: self.shared.clone() }
    }
}

impl<T: 'static> OverlayHandle<T> {
    pub fn show(&self) {
        self.request(|q| q.visible = Some(true));
    }

    pub fn hide(&self) {
        self.request(|q| q.visible = Some(false));
    }

    pub fn set_edge(&self, edge: ScreenEdge) {
        self.request(|q| q.edge = Some(edge));
    }

    /// Moves the overlay to `edge`, `margin` points in.
    pub fn place(&self, edge: ScreenEdge, margin: f32) {
        self.request(|q| {
            q.edge = Some(edge);
            q.margin = Some(margin);
        });
    }

    /// Change the overlay's state on the UI thread, in call order, shown or not.
    pub fn update(&self, f: impl FnOnce(&mut T, &mut Tx) + Send + 'static) {
        self.request(|q| q.updates.push(Box::new(f)));
    }

    pub fn availability(&self) -> Availability {
        match self.shared.availability.load(Ordering::Acquire) {
            1 => Availability::Available,
            2 => Availability::Unavailable,
            _ => Availability::Unknown,
        }
    }

    fn request(&self, f: impl FnOnce(&mut Requests<T>)) {
        f(&mut self.shared.requests.lock().unwrap());
        self.shared.wake();
    }
}

/// An overlay before the event loop runs: the shell gives it a wake-up and the GPU pool.
pub(crate) trait PendingOverlay: AnyHost {
    fn connect(&mut self, wake: Waker, gpu: Rc<RefCell<Weak<Gpu>>>);
}

enum Backing {
    Window(Arc<Window>),
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
    Layer(crate::layer_shell::LayerSurface),
}

/// A shown overlay. The wgpu surface drops before what it draws into.
struct Live {
    alpha: Option<crate::surface_alpha::AlphaSurface>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    backing: Backing,
    scale: f32,
}

pub(crate) struct Overlay<T: 'static> {
    name: &'static str,
    size: (f32, f32),
    edge: ScreenEdge,
    margin: f32,
    shared: Arc<Shared<T>>,
    rt: Runtime<T>,
    live: Option<Live>,
    /// Kept while hidden where the platform can hide a window.
    hidden: Option<Arc<Window>>,
    renderer: Option<(Renderer, wgpu::TextureFormat)>,
    gpu: Option<Arc<Gpu>>,
    gpu_pool: Rc<RefCell<Weak<Gpu>>>,
    dark: bool,
    dirty: bool,
    idle: bool,
    last: Instant,
}

impl<T: 'static> Overlay<T> {
    pub(crate) fn new<S>(spec: OverlaySpec<T>, state: T, app: &AppSpec<S>) -> (Self, OverlayHandle<T>) {
        let shared = Arc::new(Shared { requests: Mutex::default(), availability: AtomicU8::new(0), wake: Mutex::new(None) });
        let text = TextSystem::new(app.fonts, app.families);
        let mut rt = Runtime::with_text(state, spec.view, spec.size, Env { system_dark: true, scale: 1.0 }, text);
        rt.wall_start = Some(Instant::now());
        let overlay = Overlay {
            name: spec.name,
            size: spec.size,
            edge: spec.edge,
            margin: spec.margin,
            shared: shared.clone(),
            rt,
            live: None,
            hidden: None,
            renderer: None,
            gpu: None,
            gpu_pool: Rc::default(),
            dark: true,
            dirty: true,
            idle: true,
            last: Instant::now(),
        };
        (overlay, OverlayHandle { shared })
    }

    fn set_availability(&self, a: Availability) {
        let v = match a {
            Availability::Unknown => 0,
            Availability::Available => 1,
            Availability::Unavailable => 2,
        };
        self.shared.availability.store(v, Ordering::Release);
    }

    fn show(&mut self, event_loop: &ActiveEventLoop) {
        self.rt.set_system_dark(self.dark);
        if self.live.is_some() {
            return;
        }
        if let Some(window) = self.hidden.take() {
            if let Some(p) = position(event_loop, self.size, self.edge, self.margin) {
                window.set_outer_position(p);
            }
            show_window(&window);
            let scale = window.scale_factor() as f32;
            match self.attach(Backing::Window(window), scale) {
                Some(live) => self.live = Some(live),
                None => self.set_availability(Availability::Unavailable),
            }
            return;
        }
        let made = if crate::window::is_wayland(event_loop) { self.layer(event_loop) } else { self.window(event_loop) };
        match made {
            Some(live) => {
                self.live = Some(live);
                self.set_availability(Availability::Available);
                self.rt.settle_all();
                self.dirty = true;
                self.idle = true;
            }
            None => self.set_availability(Availability::Unavailable),
        }
    }

    fn hide(&mut self) {
        let Some(live) = self.live.take() else { return };
        let Live { alpha, surface, backing, .. } = live;
        drop(alpha);
        drop(surface);
        match backing {
            Backing::Window(window) => {
                window.set_visible(false);
                self.hidden = Some(window);
            }
            #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
            Backing::Layer(layer) => drop(layer),
        }
    }

    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
    fn layer(&mut self, event_loop: &ActiveEventLoop) -> Option<Live> {
        use winit::raw_window_handle::HasDisplayHandle;
        let display = event_loop.display_handle().ok()?.as_raw();
        // A buffer scale covers the densest output; the compositor corrects it on entry.
        let scale = event_loop.available_monitors().map(|m| m.scale_factor()).fold(1.0f64, f64::max).ceil() as i32;
        let size = (self.size.0.ceil() as u32, self.size.1.ceil() as u32);
        let mut layer = crate::layer_shell::LayerSurface::new(display, self.name, size, self.edge, self.margin.round() as i32)?;
        layer.set_buffer_scale(scale);
        self.attach(Backing::Layer(layer), scale as f32)
    }

    #[cfg(not(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))))]
    fn layer(&mut self, _event_loop: &ActiveEventLoop) -> Option<Live> {
        None
    }

    fn window(&mut self, event_loop: &ActiveEventLoop) -> Option<Live> {
        let mut attrs = Window::default_attributes()
            .with_title(self.name)
            .with_inner_size(LogicalSize::new(self.size.0 as f64, self.size.1 as f64))
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(true)
            .with_active(false)
            .with_visible(false)
            .with_window_level(WindowLevel::AlwaysOnTop);
        if let Some(p) = position(event_loop, self.size, self.edge, self.margin) {
            attrs = attrs.with_position(p);
        }
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        {
            use winit::platform::x11::{WindowAttributesExtX11, WindowType};
            attrs = attrs.with_override_redirect(true).with_x11_window_type(vec![WindowType::Notification]);
            if let Some(id) = crate::tooling::app_id() {
                attrs = attrs.with_name(id, id);
            }
        }
        #[cfg(target_os = "windows")]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs = attrs.with_skip_taskbar(true).with_undecorated_shadow(false);
        }
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attrs = attrs.with_has_shadow(false);
        }
        let window = Arc::new(event_loop.create_window(attrs).ok()?);
        let _ = window.set_cursor_hittest(false);
        prepare_window(&window);
        show_window(&window);
        let scale = window.scale_factor() as f32;
        self.attach(Backing::Window(window), scale)
    }

    /// Creates the wgpu surface (and the GPU device, if this is the first surface).
    fn attach(&mut self, backing: Backing, scale: f32) -> Option<Live> {
        let make = |instance: &wgpu::Instance| -> Option<wgpu::Surface<'static>> {
            match &backing {
                Backing::Window(w) => instance.create_surface(w.clone()).ok(),
                #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
                Backing::Layer(layer) => {
                    let (display, window) = layer.raw_handles();
                    // SAFETY: the layer surface outlives this wgpu surface (`Live` drops the
                    // surface first), and winit keeps the display alive.
                    unsafe {
                        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                            raw_display_handle: Some(display),
                            raw_window_handle: window,
                        })
                    }
                    .ok()
                }
            }
        };
        let pooled = self.gpu.clone().or_else(|| self.gpu_pool.borrow().upgrade());
        let surface = match pooled {
            Some(gpu) => {
                let surface = make(&gpu.instance)?;
                self.gpu = Some(gpu);
                surface
            }
            None => {
                let instance = Gpu::instance();
                let surface = make(&instance)?;
                let gpu = Arc::new(Gpu::new(instance, Some(&surface)));
                *self.gpu_pool.borrow_mut() = Arc::downgrade(&gpu);
                self.gpu = Some(gpu);
                surface
            }
        };
        let gpu = self.gpu.clone()?;
        let (w, h) = ((self.size.0 * scale).round() as u32, (self.size.1 * scale).round() as u32);
        let mut config = surface.get_default_config(&gpu.adapter, w.max(1), h.max(1))?;
        let caps = surface.get_capabilities(&gpu.adapter);
        config.format = caps.formats.iter().copied().find(|f| f.is_srgb()).unwrap_or(config.format);
        config.alpha_mode = [wgpu::CompositeAlphaMode::PreMultiplied, wgpu::CompositeAlphaMode::PostMultiplied, wgpu::CompositeAlphaMode::Inherit]
            .into_iter()
            .find(|m| caps.alpha_modes.contains(m))
            .unwrap_or(config.alpha_mode);
        config.present_mode = wgpu::PresentMode::AutoVsync;
        config.desired_maximum_frame_latency = 2;
        surface.configure(&gpu.device, &config);
        if self.renderer.as_ref().is_none_or(|(_, f)| *f != config.format) {
            self.renderer = Some((Renderer::new(&gpu.device, config.format), config.format));
            self.rt.text.atlas.invalidate();
            self.rt.images.invalidate();
        }
        // Core Animation composites premultiplied pixels even in wgpu's PostMultiplied mode;
        // other straight-alpha surfaces get the conversion pass.
        let alpha = (config.alpha_mode == wgpu::CompositeAlphaMode::PostMultiplied && gpu.adapter.get_info().backend != wgpu::Backend::Metal)
            .then(|| crate::surface_alpha::AlphaSurface::new(&gpu.device, config.format, (config.width, config.height)));
        self.rt.transparent = true;
        self.rt.set_viewport(self.size, scale);
        Some(Live { alpha, surface, config, backing, scale })
    }

    fn rescale(&mut self, gpu: &Gpu, scale: f32) {
        let Some(live) = self.live.as_mut() else { return };
        if (live.scale - scale).abs() < f32::EPSILON {
            return;
        }
        live.scale = scale;
        live.config.width = (self.size.0 * scale).round().max(1.0) as u32;
        live.config.height = (self.size.1 * scale).round().max(1.0) as u32;
        live.surface.configure(&gpu.device, &live.config);
        if let Some(alpha) = &mut live.alpha {
            alpha.resize(&gpu.device, (live.config.width, live.config.height));
        }
        self.rt.set_viewport(self.size, scale);
        self.dirty = true;
    }
}

impl<T: 'static> Overlay<T> {
    /// Applies requests from other threads: updates, edge, show and hide.
    fn sync(&mut self, event_loop: &ActiveEventLoop) {
        let requests = std::mem::take(&mut *self.shared.requests.lock().unwrap());
        for f in requests.updates {
            self.rt.update(f);
            self.rt.shell_request = None;
            self.dirty = true;
        }
        let margin = requests.margin.filter(|m| *m != self.margin);
        if let Some(margin) = margin {
            self.margin = margin;
        }
        if let Some(edge) = requests.edge.filter(|e| *e != self.edge).or(margin.map(|_| self.edge)) {
            self.edge = edge;
            match self.live.as_mut().map(|l| &mut l.backing) {
                Some(Backing::Window(w)) => {
                    if let Some(p) = position(event_loop, self.size, edge, self.margin) {
                        w.set_outer_position(p);
                    }
                }
                #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
                Some(Backing::Layer(layer)) => {
                    layer.place(edge, self.margin.round() as i32);
                    layer.commit();
                }
                None => {}
            }
        }
        match requests.visible {
            Some(true) => self.show(event_loop),
            Some(false) => self.hide(),
            None => {}
        }
    }

    /// Draws a frame if one is due; true while it needs more (animating).
    fn draw(&mut self) -> bool {
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        if let Some(Live { backing: Backing::Layer(layer), .. }) = self.live.as_mut() {
            let changes = layer.dispatch();
            if changes.closed {
                self.hide();
                return false;
            }
            if let Some(scale) = changes.scale {
                layer.set_buffer_scale(scale);
                if let Some(gpu) = self.gpu.clone() {
                    self.rescale(&gpu, scale as f32);
                }
            }
        }
        let (Some(live), Some((renderer, _)), Some(gpu)) = (self.live.as_mut(), self.renderer.as_mut(), self.gpu.as_ref()) else {
            return false;
        };
        if !self.dirty && self.idle {
            return false;
        }
        let frame = match live.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                live.surface.configure(&gpu.device, &live.config);
                return true;
            }
            _ => return true,
        };
        let now = Instant::now();
        let dt = if self.idle { 1.0 / 120.0 } else { (now - self.last).as_secs_f64().min(1.0 / 30.0) };
        self.last = now;
        let animating = self.rt.frame(dt);
        self.rt.shell_request = None;
        let view = frame.texture.create_view(&Default::default());
        renderer.prepare(&gpu.device, &gpu.queue, &mut self.rt.text.atlas, &self.rt.instances, (live.config.width, live.config.height));
        renderer.upload_images(&gpu.device, &gpu.queue, &mut self.rt.images);
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        renderer.draw(&mut enc, live.alpha.as_ref().map_or(&view, |a| &a.view), [0.0, 0.0, 0.0, 0.0]);
        if let Some(alpha) = &live.alpha {
            alpha.draw(&mut enc, &view);
        }
        gpu.queue.submit([enc.finish()]);
        #[allow(irrefutable_let_patterns)]
        if let Backing::Window(w) = &live.backing {
            w.pre_present_notify();
        }
        gpu.queue.present(frame);
        self.dirty = false;
        self.idle = !(animating || self.rt.has_pending_input());
        !self.idle
    }
}

impl<T: 'static> AnyHost for Overlay<T> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.sync(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: WakeEvent) {
        if let WakeEvent::App(_) = event {
            self.sync(event_loop);
        }
    }

    fn owns(&self, id: WindowId) -> bool {
        let window = match self.live.as_ref().map(|l| &l.backing) {
            Some(Backing::Window(w)) => Some(w),
            _ => self.hidden.as_ref(),
        };
        window.is_some_and(|w| w.id() == id)
    }

    fn window_event(&mut self, _event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(gpu) = self.gpu.clone() {
                    self.rescale(&gpu, scale_factor as f32);
                }
            }
            WindowEvent::RedrawRequested | WindowEvent::Resized(_) => self.dirty = true,
            WindowEvent::ThemeChanged(theme) => {
                self.dark = theme == winit::window::Theme::Dark;
                self.rt.set_system_dark(self.dark);
                self.dirty = true;
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) -> bool {
        self.draw()
    }

    fn keeps_loop(&self) -> bool {
        false
    }
}

impl<T: 'static> PendingOverlay for Overlay<T> {
    fn connect(&mut self, wake: Waker, gpu: Rc<RefCell<Weak<Gpu>>>) {
        self.gpu_pool = gpu;
        *self.shared.wake.lock().unwrap() = Some(wake.clone());
        // Requests made before the loop existed.
        wake();
    }
}

/// Centred on the primary screen's top or bottom edge, `margin` points in.
fn position(event_loop: &ActiveEventLoop, size: (f32, f32), edge: ScreenEdge, margin: f32) -> Option<PhysicalPosition<i32>> {
    let monitor = event_loop.primary_monitor().or_else(|| event_loop.available_monitors().next())?;
    let scale = monitor.scale_factor();
    let (origin, area) = (monitor.position(), monitor.size());
    let (w, h) = ((size.0 as f64 * scale).round() as i32, (size.1 as f64 * scale).round() as i32);
    let m = (margin as f64 * scale).round() as i32;
    let x = origin.x + (area.width as i32 - w) / 2;
    #[allow(unused_mut)]
    let mut bottom = origin.y + area.height as i32;
    // Above the taskbar on Windows' primary display.
    #[cfg(target_os = "windows")]
    if event_loop.primary_monitor().as_ref() == Some(&monitor)
        && let Some(work) = crate::win32::work_area_bottom()
    {
        bottom = work;
    }
    let y = match edge {
        ScreenEdge::Top => origin.y + m,
        ScreenEdge::Bottom => bottom - h - m,
    };
    Some(PhysicalPosition::new(x, y))
}

/// Platform window flags winit has no attribute for.
fn prepare_window(window: &Window) {
    #[cfg(target_os = "macos")]
    crate::macos::prepare_overlay(window);
    #[cfg(target_os = "windows")]
    crate::win32::prepare_overlay(window);
    let _ = window;
}

/// Puts the window on screen without focusing it or activating the app.
fn show_window(window: &Window) {
    #[cfg(target_os = "macos")]
    crate::macos::show_overlay(window);
    #[cfg(target_os = "windows")]
    crate::win32::show_overlay(window);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    window.set_visible(true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_requests_collapse_and_updates_keep_their_order() {
        let shared = Arc::new(Shared::<u32> { requests: Mutex::default(), availability: AtomicU8::new(0), wake: Mutex::new(None) });
        let handle = OverlayHandle { shared: shared.clone() };
        assert_eq!(handle.availability(), Availability::Unknown);
        handle.show();
        handle.update(|n, _| *n += 1);
        handle.place(ScreenEdge::Bottom, 24.0);
        handle.update(|n, _| *n *= 3);
        handle.hide();
        let q = std::mem::take(&mut *shared.requests.lock().unwrap());
        assert_eq!(q.visible, Some(false), "the latest show/hide wins");
        assert_eq!((q.edge, q.margin), (Some(ScreenEdge::Bottom), Some(24.0)));
        let mut n = 0;
        for f in q.updates {
            f(&mut n, &mut Tx::default());
        }
        assert_eq!(n, 3);
        shared.availability.store(2, Ordering::Release);
        assert_eq!(handle.availability(), Availability::Unavailable);
    }
}

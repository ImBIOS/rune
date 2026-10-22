//! Overlays: surfaces beside the app's window (a recording HUD, meeting capsules). Each has
//! its own state type and `Runtime`, is fed from any thread through an `OverlayHandle`, and is
//! drawn transparent above every app at the top or bottom edge of the primary screen (centred
//! or in a corner). An overlay never takes keyboard focus or activates the app; the pointer
//! passes through it unless it is `interactive`, in which case clicks reach its view.
//!
//! - Wayland: a layer-shell surface on the overlay layer (`layer_shell.rs`), keyboard none,
//!   empty input region. Hidden means destroyed. Without layer-shell the overlay is
//!   unavailable; interactive overlays are unavailable there (no pointer on the private queue).
//! - X11: an override-redirect notification window, with an empty input shape unless
//!   interactive.
//! - macOS: winit's view moved into a non-activating `NSPanel` at status level on every space,
//!   ignoring the mouse unless interactive, ordered front without becoming key.
//! - Windows: a topmost tool window with `WS_EX_NOACTIVATE` (plus `WS_EX_TRANSPARENT` and
//!   `WS_EX_LAYERED` unless interactive), shown with `SW_SHOWNOACTIVATE`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowId, WindowLevel};

use crate::el::{Align, El, Tx};
use crate::render::{Gpu, Renderer};
use crate::runtime::{Cx, Env, Runtime, Waker};
use crate::text::TextSystem;
use crate::tooling::AppSpec;
use crate::window::{AnyHost, WakeEvent};

/// The screen edge an overlay sits on.
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
    /// Along the edge: centred, or at the start (left) or end (right) `inset` points in.
    pub align: Align,
    pub inset: f32,
    /// Clicks reach the view (meeting capsule buttons); otherwise they pass through.
    pub interactive: bool,
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

/// Where an overlay sits and how big it is, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Placement {
    pub size: (f32, f32),
    pub edge: ScreenEdge,
    pub margin: f32,
    pub align: Align,
    pub inset: f32,
}

struct Requests<T> {
    visible: Option<bool>,
    edge: Option<ScreenEdge>,
    margin: Option<f32>,
    align: Option<(Align, f32)>,
    size: Option<(f32, f32)>,
    updates: Vec<Update<T>>,
}

impl<T> Default for Requests<T> {
    fn default() -> Self {
        Requests { visible: None, edge: None, margin: None, align: None, size: None, updates: Vec::new() }
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

    /// Moves the overlay along its edge: centred, or `inset` points from the start or end.
    pub fn set_align(&self, align: Align, inset: f32) {
        self.request(|q| q.align = Some((align, inset)));
    }

    /// Resizes the overlay (points), keeping its edge and alignment.
    pub fn resize(&self, size: (f32, f32)) {
        self.request(|q| q.size = Some(size));
    }

    /// Change the overlay's state on the UI thread, in call order, shown or not.
    /// `Tx::wake_after` only redraws; the view re-runs on `update` (no poll callback yet).
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
    at: Placement,
    interactive: bool,
    shared: Arc<Shared<T>>,
    rt: Runtime<T>,
    /// The non-activating panel holding winit's view (macOS), kept with the window. Declared
    /// before the window so the view goes back to it first.
    #[cfg(target_os = "macos")]
    panel: Option<crate::macos::Panel>,
    live: Option<Live>,
    /// Kept while hidden where the platform can hide a window.
    hidden: Option<Arc<Window>>,
    mods: winit::keyboard::ModifiersState,
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
            at: Placement { size: spec.size, edge: spec.edge, margin: spec.margin, align: spec.align, inset: spec.inset },
            interactive: spec.interactive,
            shared: shared.clone(),
            rt,
            live: None,
            hidden: None,
            #[cfg(target_os = "macos")]
            panel: None,
            mods: Default::default(),
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
            self.place_window(event_loop, &window);
            self.show_window(&window);
            let scale = window.scale_factor() as f32;
            match self.attach(Backing::Window(window), scale) {
                Some(live) => self.live = Some(live),
                None => self.set_availability(Availability::Unavailable),
            }
            return;
        }
        let made = match (crate::window::is_wayland(event_loop), self.interactive) {
            (true, true) => None,
            (true, false) => self.layer(event_loop),
            (false, _) => self.window(event_loop),
        };
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
                #[cfg(target_os = "macos")]
                if let Some(panel) = &self.panel {
                    panel.hide();
                }
                window.set_visible(false);
                self.rt.pointer_leave();
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
        let mut layer = crate::layer_shell::LayerSurface::new(display, self.name, self.at)?;
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
            .with_inner_size(LogicalSize::new(self.at.size.0 as f64, self.at.size.1 as f64))
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(true)
            .with_active(false)
            .with_visible(false)
            .with_window_level(WindowLevel::AlwaysOnTop);
        if let Some(p) = position(event_loop, self.at) {
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
        let scale = window.scale_factor() as f32;
        // The surface first: on macOS it binds winit's view and its layer, which then move
        // into the panel.
        let live = self.attach(Backing::Window(window.clone()), scale)?;
        #[cfg(target_os = "macos")]
        {
            self.panel = Some(crate::macos::Panel::adopt(&window, self.at, self.interactive)?);
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = window.set_cursor_hittest(self.interactive);
            #[cfg(target_os = "windows")]
            crate::win32::prepare_overlay(&window, self.interactive);
        }
        self.place_window(event_loop, &window);
        self.show_window(&window);
        Some(live)
    }

    fn place_window(&self, event_loop: &ActiveEventLoop, window: &Window) {
        #[cfg(target_os = "macos")]
        if let Some(panel) = &self.panel {
            panel.place(self.at);
            return;
        }
        if let Some(p) = position(event_loop, self.at) {
            window.set_outer_position(p);
        }
    }

    /// Puts the window on screen without focusing it or activating the app.
    fn show_window(&self, window: &Window) {
        #[cfg(target_os = "macos")]
        if let Some(panel) = &self.panel {
            panel.show();
            return;
        }
        #[cfg(target_os = "windows")]
        crate::win32::show_overlay(window);
        #[cfg(not(target_os = "windows"))]
        window.set_visible(true);
    }

    fn resized(&mut self, event_loop: &ActiveEventLoop) {
        let Some(live) = self.live.as_mut() else { return };
        match &mut live.backing {
            Backing::Window(w) => {
                let _ = w.request_inner_size(LogicalSize::new(self.at.size.0 as f64, self.at.size.1 as f64));
            }
            #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
            Backing::Layer(layer) => {
                layer.place(self.at);
                layer.commit();
            }
        }
        if let Some(gpu) = self.gpu.clone() {
            let scale = live.scale;
            live.scale = 0.0;
            self.rescale(&gpu, scale);
        }
        if let Some(Backing::Window(w)) = self.live.as_ref().map(|l| &l.backing) {
            let w = w.clone();
            self.place_window(event_loop, &w);
        }
    }

    /// Creates the wgpu surface (and the GPU device, if this is the first surface).
    fn attach(&mut self, backing: Backing, scale: f32) -> Option<Live> {
        #[cfg(target_os = "macos")]
        let moved = self.panel.as_ref().map(|p| p.raw_handles());
        let make = |instance: &wgpu::Instance| -> Option<wgpu::Surface<'static>> {
            match &backing {
                Backing::Window(w) => {
                    #[cfg(target_os = "macos")]
                    if let Some((display, window)) = moved {
                        // SAFETY: the panel keeps the view (and its layer) alive while the
                        // overlay holds this surface; AppKit's display handle carries nothing.
                        return unsafe {
                            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                                raw_display_handle: Some(display),
                                raw_window_handle: window,
                            })
                        }
                        .ok();
                    }
                    instance.create_surface(w.clone()).ok()
                }
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
                let gpu = Arc::new(Gpu::try_new(instance, Some(&surface)).ok()?);
                *self.gpu_pool.borrow_mut() = Arc::downgrade(&gpu);
                self.gpu = Some(gpu);
                surface
            }
        };
        let gpu = self.gpu.clone()?;
        let (w, h) = ((self.at.size.0 * scale).round() as u32, (self.at.size.1 * scale).round() as u32);
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
        self.rt.set_viewport(self.at.size, scale);
        Some(Live { alpha, surface, config, backing, scale })
    }

    fn rescale(&mut self, gpu: &Gpu, scale: f32) {
        let Some(live) = self.live.as_mut() else { return };
        if (live.scale - scale).abs() < f32::EPSILON {
            return;
        }
        live.scale = scale;
        live.config.width = (self.at.size.0 * scale).round().max(1.0) as u32;
        live.config.height = (self.at.size.1 * scale).round().max(1.0) as u32;
        live.surface.configure(&gpu.device, &live.config);
        if let Some(alpha) = &mut live.alpha {
            alpha.resize(&gpu.device, (live.config.width, live.config.height));
        }
        self.rt.set_viewport(self.at.size, scale);
        self.dirty = true;
    }
}

impl<T: 'static> Overlay<T> {
    fn apply_updates(&mut self, updates: Vec<Update<T>>) {
        if updates.is_empty() { return; }
        for f in updates { self.rt.update(f); }
        // Register timer requests even before the first show; hidden overlays never draw.
        self.advance();
    }

    fn advance(&mut self) {
        self.rt.frame(0.0);
        after_frame(&mut self.rt);
        self.dirty = true;
    }

    fn wake_due(&mut self, now: Instant) {
        if self.wake_deadline().is_some_and(|deadline| deadline <= now) {
            // Drain independently of surface acquisition (hidden, minimized, or unavailable).
            self.advance();
        }
    }

    /// Applies requests from other threads: updates, edge, show and hide.
    fn sync(&mut self, event_loop: &ActiveEventLoop) {
        let requests = std::mem::take(&mut *self.shared.requests.lock().unwrap());
        self.apply_updates(requests.updates);
        let mut at = self.at;
        at.edge = requests.edge.unwrap_or(at.edge);
        at.margin = requests.margin.unwrap_or(at.margin);
        if let Some((align, inset)) = requests.align {
            (at.align, at.inset) = (align, inset);
        }
        let resized = requests.size.is_some_and(|size| size != at.size);
        at.size = requests.size.unwrap_or(at.size);
        if at != self.at {
            self.at = at;
            if resized {
                self.resized(event_loop);
            } else {
                match self.live.as_mut().map(|l| &mut l.backing) {
                    Some(Backing::Window(w)) => {
                        let w = w.clone();
                        self.place_window(event_loop, &w);
                    }
                    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
                    Some(Backing::Layer(layer)) => {
                        layer.place(at);
                        layer.commit();
                    }
                    None => {}
                }
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
        after_frame(&mut self.rt);
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
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::ResumeTimeReached { .. } | StartCause::Poll) {
            self.wake_due(Instant::now());
        }
    }

    fn wake_deadline(&self) -> Option<Instant> {
        self.rt.wall_start.and_then(|start| {
            start.checked_add(std::time::Duration::try_from_secs_f64(self.rt.wake_deadline()?).ok()?)
        })
    }

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
            WindowEvent::CursorMoved { position, .. } if self.interactive => {
                let scale = self.live.as_ref().map_or(1.0, |l| l.scale);
                self.rt.pointer_move(position.x as f32 / scale, position.y as f32 / scale);
                self.dirty = true;
            }
            WindowEvent::CursorLeft { .. } if self.interactive => {
                self.rt.pointer_leave();
                self.dirty = true;
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::MouseInput { state, button: winit::event::MouseButton::Left, .. } if self.interactive => {
                if let Some((x, y)) = self.rt.pointer() {
                    match state {
                        winit::event::ElementState::Pressed => self.rt.pointer_down(x, y),
                        winit::event::ElementState::Released => self.rt.pointer_up(x, y),
                    }
                }
                self.dirty = true;
            }
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

/// Carries out a frame's clipboard request; window requests (`Tx::close_window`) don't apply
/// to overlays.
fn after_frame<T>(rt: &mut Runtime<T>) {
    rt.shell_request = None;
    if let Some(text) = rt.copy_request.take()
        && let Ok(mut clipboard) = arboard::Clipboard::new()
    {
        let _ = clipboard.set_text(text);
    }
}

/// On the primary screen's top or bottom edge, `margin` points in, centred or `inset` points
/// from a side.
fn position(event_loop: &ActiveEventLoop, at: Placement) -> Option<PhysicalPosition<i32>> {
    let monitor = event_loop.primary_monitor().or_else(|| event_loop.available_monitors().next())?;
    let scale = monitor.scale_factor();
    let (origin, area) = (monitor.position(), monitor.size());
    let px = |v: f32| (v as f64 * scale).round() as i32;
    let (w, h) = (px(at.size.0), px(at.size.1));
    let x = match at.align {
        Align::Start => origin.x + px(at.inset),
        Align::Center => origin.x + (area.width as i32 - w) / 2,
        Align::End => origin.x + area.width as i32 - w - px(at.inset),
    };
    #[allow(unused_mut)]
    let mut bottom = origin.y + area.height as i32;
    // Above the taskbar on Windows' primary display.
    #[cfg(target_os = "windows")]
    if event_loop.primary_monitor().as_ref() == Some(&monitor)
        && let Some(work) = crate::win32::work_area_bottom()
    {
        bottom = work;
    }
    let y = match at.edge {
        ScreenEdge::Top => origin.y + px(at.margin),
        ScreenEdge::Bottom => bottom - h - px(at.margin),
    };
    Some(PhysicalPosition::new(x, y))
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
        handle.set_align(Align::End, 12.0);
        handle.resize((300.0, 80.0));
        handle.update(|n, _| *n *= 3);
        handle.hide();
        let q = std::mem::take(&mut *shared.requests.lock().unwrap());
        assert_eq!(q.visible, Some(false), "the latest show/hide wins");
        assert_eq!((q.edge, q.margin), (Some(ScreenEdge::Bottom), Some(24.0)));
        assert_eq!((q.align, q.size), (Some((Align::End, 12.0)), Some((300.0, 80.0))));
        let mut n = 0;
        for f in q.updates {
            f(&mut n, &mut Tx::default());
        }
        assert_eq!(n, 3);
        shared.availability.store(2, Ordering::Release);
        assert_eq!(handle.availability(), Availability::Unavailable);
    }

    #[test]
    fn hidden_overlay_update_wakes_once_without_a_surface() {
        use std::time::Duration;
        fn view(_: &Vec<f64>, _: &mut Cx<Vec<f64>>) -> El { crate::el() }
        let app = AppSpec {
            title: "timer test", size: (20.0, 20.0), min_size: None,
            init: Vec::<f64>::new, view, scenes: &[], fonts: &[], families: ("sans-serif", "monospace"),
            setup: None, poll: None, on_key: None,
        };
        let spec = OverlaySpec {
            name: "timer test", size: (20.0, 20.0), edge: ScreenEdge::Top,
            margin: 0.0, align: Align::Center, inset: 0.0, interactive: false, view,
        };
        let (mut overlay, handle) = Overlay::new(spec, Vec::new(), &app);
        // The runtime's virtual clock makes scheduling exact; the host maps it to wall time.
        overlay.rt.wall_start = None;
        overlay.rt.poll = Some(|seen, now, _| { seen.push(now); false });
        handle.update(|_, tx| tx.wake_after(Duration::from_millis(250)));
        let updates = std::mem::take(&mut overlay.shared.requests.lock().unwrap().updates);
        overlay.apply_updates(updates);
        assert_eq!(overlay.rt.state, [0.0]);
        assert_eq!(overlay.rt.wake_deadline(), Some(0.25));
        let start = Instant::now();
        overlay.rt.wall_start = Some(start);
        assert_eq!(overlay.wake_deadline(), Some(start + Duration::from_millis(250)));
        overlay.wake_due(start + Duration::from_millis(249));
        assert_eq!(overlay.rt.state.len(), 1, "not before the deadline");
        overlay.rt.wall_start = Some(Instant::now() - Duration::from_millis(250));
        overlay.wake_due(Instant::now());
        assert_eq!(overlay.rt.state.len(), 2);
        assert!(overlay.rt.state[1] >= 0.25);
        assert_eq!(overlay.wake_deadline(), None);
        overlay.wake_due(Instant::now());
        assert!(!overlay.draw(), "hidden overlays do not acquire or animate a surface");
        assert_eq!(overlay.rt.state.len(), 2, "no polling after the one wake");
        assert!(overlay.live.is_none() && overlay.gpu.is_none());
    }
}

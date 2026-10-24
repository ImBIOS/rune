//! Overlays: surfaces beside the app's window (a recording HUD, meeting capsules). Each has
//! its own state type and `Runtime`, is fed from any thread through an `OverlayHandle`, and is
//! drawn transparent above other apps. Bare specs preserve edge placement on the primary
//! display and never request keyboard focus. `OverlaySpec::options` adds display targeting,
//! full-display/rect placement, regional input and explicit nonactivating keyboard policy.
//!
//! - Wayland: layer-shell (`layer_shell.rs`) with output-local input regions and a private
//!   seat queue. Hidden means destroyed. No global pointer observation or capture exclusion.
//! - X11: an override-redirect notification window, with an empty input shape unless
//!   interactive.
//! - macOS: winit's view moved into a non-activating `NSPanel` at status level on every space,
//!   observing pointer movement even through transparent regions, ordered front without key
//!   focus unless explicitly requested. Its window number is available for capture filters.
//! - Windows: a topmost tool window with `WS_EX_NOACTIVATE` (plus `WS_EX_TRANSPARENT` and
//!   `WS_EX_LAYERED` unless interactive), shown with `SW_SHOWNOACTIVATE`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowId, WindowLevel};

use crate::el::{Align, El, Tx};
use crate::compose::{Compositor, Frame};
use crate::frame_clock::FrameClock;
use crate::render::{Gpu, Renderer};
use crate::runtime::{Cx, Env, Runtime, Waker};
use crate::text::TextSystem;
use crate::tooling::AppSpec;
use crate::window::{AnyHost, WakeEvent};
use crate::{ConfiguredOverlay, DisplayTarget, KeyPolicy, OverlayInput, OverlayOptions, OverlayPlacement, OverlaySupport};

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
    input: Option<OverlayInput>,
    follow_pointer: Option<bool>,
    placement: Option<Option<OverlayPlacement>>,
    display: Option<DisplayTarget>,
    request_keys: bool,
    visible: Option<bool>,
    edge: Option<ScreenEdge>,
    margin: Option<f32>,
    align: Option<(Align, f32)>,
    size: Option<(f32, f32)>,
    /// How long the `visible` change fades the window's alpha (macOS).
    fade: Option<Duration>,
    macos_level: Option<isize>,
    /// The `setup` waker fired.
    woken: bool,
    updates: Vec<Update<T>>,
}

impl<T> Default for Requests<T> {
    fn default() -> Self {
        Requests { input: None, follow_pointer: None, placement: None, display: None, request_keys: false, visible: None, edge: None, margin: None, align: None, size: None, fade: None, macos_level: None, woken: false, updates: Vec::new() }
    }
}

struct Shared<T> {
    native: Mutex<(Option<u32>, crate::OverlayCapabilities)>,
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
    /// Takes effect on the next event-loop turn, or after the current owned drag releases.
    pub fn set_input(&self, input: OverlayInput) { self.request(|q| q.input = Some(input)); }
    pub fn set_follow_pointer(&self, follow: bool) { self.request(|q| q.follow_pointer = Some(follow)); }
    /// None restores the spec's legacy edge placement.
    pub fn set_placement(&self, placement: Option<OverlayPlacement>) { self.request(|q| q.placement = Some(placement)); }
    pub fn set_display(&self, display: DisplayTarget) { self.request(|q| q.display = Some(display)); }

    /// Request keyboard focus without application activation, where supported.
    pub fn request_keys(&self) { self.request(|q| q.request_keys = true); }

    /// macOS panel CGWindowID for an explicit ScreenCaptureKit exclusion filter. Available
    /// at native creation (see `on_created`/`prewarm`), stable across hide/show until drop.
    pub fn window_number(&self) -> Option<u32> { self.shared.native.lock().unwrap().0 }

    /// Result of the native capture-exclusion request. Linux reports Unsupported.
    pub fn capture_exclusion(&self) -> OverlaySupport { self.capabilities().capture_exclusion }
    pub fn capabilities(&self) -> crate::OverlayCapabilities { self.shared.native.lock().unwrap().1 }

    /// Reveal the overlay. Prewarmed macOS overlays refresh hidden changes before reveal,
    /// preserving motion elapsed since each commit, even when show arrives in a later turn.
    /// Unretained hosts keep their settled first-frame behavior.
    pub fn show(&self) {
        self.request(|q| { q.visible = Some(true); q.fade = None; });
    }

    pub fn hide(&self) {
        self.request(|q| { q.visible = Some(false); q.fade = None; });
    }

    /// Like `show`, fading the window's alpha in from zero over `duration` on macOS
    /// (AppKit's default ease). Elsewhere the overlay appears at once.
    pub fn fade_in(&self, duration: Duration) {
        self.request(|q| { q.visible = Some(true); q.fade = Some(duration); });
    }

    /// Like `hide`, fading the window's alpha out over `duration` on macOS before it orders
    /// out. Input stops at once, but the overlay keeps drawing until the fade ends, so motion
    /// started in the same turn (an exit transition, a poll stepping its own animation) plays
    /// out during it. Showing again cancels the fade, and that motion carries on from where it
    /// is. Elsewhere the overlay hides at once.
    pub fn fade_out(&self, duration: Duration) {
        self.request(|q| { q.visible = Some(false); q.fade = Some(duration); });
    }

    /// Change the macOS window level (`NSWindow.Level`, e.g. 3 for `.floating`), shown or not.
    /// Overrides `OverlayOptions::level`; other platforms ignore it.
    pub fn set_macos_level(&self, level: isize) {
        self.request(|q| q.macos_level = Some(level));
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
    /// `Tx::wake_after` invokes the configured poll at its deadline, including while hidden.
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
    fn connect(&mut self, wake: Waker, frame_wake: Waker, gpu: Rc<RefCell<Weak<Gpu>>>);
}

enum Backing {
    Window(Arc<Window>),
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
    Layer(crate::layer_shell::LayerSurface),
}

/// A shown overlay. The wgpu surface drops before what it draws into.
struct Live {
    #[cfg(target_os = "macos")]
    pacing: Option<crate::display_link::DisplayLink>,
    /// `El::system_material` views behind the Metal layer.
    #[cfg(target_os = "macos")]
    materials: crate::system_material::macos::Materials,
    alpha: Option<crate::surface_alpha::AlphaSurface>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    backing: Backing,
    scale: f32,
}

pub(crate) struct Overlay<T: 'static> {
    on_created: Option<Box<dyn FnOnce(crate::OverlayCreated)>>,
    setup: Option<fn(&mut T, Waker)>,
    pending_input: Option<OverlayInput>,
    visible: bool,
    /// When a fade-out started by `OverlayHandle::fade_out` ends. Until then the hidden
    /// overlay takes no input but keeps drawing, and isn't treated as occluded.
    fading_out: Option<Instant>,
    options: OverlayOptions,
    canvas_size: (f32, f32),
    name: &'static str,
    at: Placement,
    /// Where `place_window` last put the window, to carry the pointer across a move.
    origin: Option<winit::dpi::PhysicalPosition<i32>>,
    interactive: bool,
    /// `ConfiguredOverlay::macos_level` or the last `set_macos_level`.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    macos_level: Option<isize>,
    shared: Arc<Shared<T>>,
    rt: Runtime<T>,
    clipboard: crate::clipboard::Client,
    popup: crate::popup_menu::Presenter,
    compositor: Option<Box<dyn Compositor<T>>>,
    /// The non-activating panel holding winit's view (macOS), kept with the window. Declared
    /// before the window so the view goes back to it first.
    #[cfg(target_os = "macos")]
    panel: Option<crate::macos::Panel>,
    #[cfg(target_os = "windows")]
    native: Option<crate::win32::OverlayWindow>,
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
    x11: Option<crate::x11_overlay::InputWindow>,
    live: Option<Live>,
    cursor: Option<crate::os_cursor::OsCursor>,
    mods: winit::keyboard::ModifiersState,
    renderer: Option<(Renderer, wgpu::TextureFormat)>,
    gpu: Option<Arc<Gpu>>,
    gpu_pool: Rc<RefCell<Weak<Gpu>>>,
    dark: bool,
    dirty: bool,
    idle: bool,
    clock: FrameClock,
    /// The panel is shown but none of it is on screen (covered, or the displays asleep).
    #[cfg(target_os = "macos")]
    occluded: bool,
    /// When to try a frame again that had no drawable, or the next frame of motion with no
    /// display link to pace it.
    retry: Option<Instant>,
    /// The next occluded re-check's wait, backing off while the surface stays occluded.
    occluded_wait: Duration,
    #[cfg(target_os = "macos")]
    frame_wake: Option<Waker>,
}

impl<T: 'static> Overlay<T> {
    pub(crate) fn new<S>(configured: impl Into<ConfiguredOverlay<T>>, state: T, app: &AppSpec<S>) -> (Self, OverlayHandle<T>) {
        let ConfiguredOverlay { spec, options, on_key, on_created, poll, on_focus, compositor, setup, srgb_blend, macos_level } = configured.into();
        let srgb_blend = if compositor.is_some() && srgb_blend {
            eprintln!("rune: srgb_blend is ignored when a compositor is set");
            false
        } else {
            srgb_blend
        };
        let shared = Arc::new(Shared { native: Mutex::default(), requests: Mutex::default(), availability: AtomicU8::new(0), wake: Mutex::new(None) });
        let text = TextSystem::new(app.fonts, app.families);
        let mut rt = Runtime::with_text(state, spec.view, spec.size, Env::default(), text);
        rt.set_srgb_blend(srgb_blend);
        rt.set_system_reduced_motion(crate::preferences::reduced_motion());
        rt.wall_start = Some(Instant::now());
        rt.native_overlay = true;
        rt.set_os_drag(crate::drag_out::SUPPORTED);
        rt.on_key = on_key;
        rt.on_focus = on_focus;
        rt.set_live_visible(false);
        // App and overlay may use different state types. Downcast the function pointer
        // once, without a per-frame adapter or changing App::overlay's generic API.
        rt.poll = poll.or_else(|| app.poll.and_then(|f| {
            (&f as &dyn std::any::Any).downcast_ref::<fn(&mut T, f64, &mut Tx) -> bool>().copied()
        }));
        let overlay = Overlay {
            on_created,
            setup,
            pending_input: None,
            visible: false,
            fading_out: None,
            options,
            canvas_size: spec.size,
            name: spec.name,
            at: Placement { size: spec.size, edge: spec.edge, margin: spec.margin, align: spec.align, inset: spec.inset },
            origin: None,
            interactive: spec.interactive,
            macos_level,
            shared: shared.clone(),
            rt,
            clipboard: Default::default(),
            popup: Default::default(),
            compositor,
            live: None,
            cursor: None,
            #[cfg(target_os = "macos")]
            panel: None,
            #[cfg(target_os = "windows")]
            native: None,
            #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
            x11: None,
            mods: Default::default(),
            renderer: None,
            gpu: None,
            gpu_pool: Rc::default(),
            dark: true,
            dirty: true,
            idle: true,
            clock: FrameClock::new(Instant::now()),
            #[cfg(target_os = "macos")]
            occluded: false,
            retry: None,
            occluded_wait: crate::window::OCCLUDED_RETRY,
            #[cfg(target_os = "macos")]
            frame_wake: None,
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

    fn show_fading(&mut self, event_loop: &ActiveEventLoop, fade: Duration) {
        self.rt.set_system_dark(self.dark);
        if self.visible { return; }
        // Shown again mid fade-out: the window is still up with current pixels, and its motion
        // carries on rather than settling.
        let fading = self.fading_out.take().is_some();
        let size = self.at.size;
        self.resolve_size(event_loop);
        if self.live.is_none() { self.prepare(event_loop); }
        if self.live.is_none() { return; }
        self.resume_live();
        if size != self.at.size { self.resized(event_loop); }
        if !fading { self.settle_before_reveal(); }
        if let Some(Backing::Window(window)) = self.live.as_ref().map(|l| &l.backing) {
            let window = window.clone();
            self.place_window(event_loop, &window);
            if let Some(gpu) = self.gpu.clone() { self.rescale(&gpu, window.scale_factor() as f32); }
            // Retained alpha-zero hosts already have pixels. A hidden update must reach
            // the surface before exposing it, rather than flashing the old frame.
            if !fading { self.refresh_for_reveal_at(Instant::now()); }
            self.show_window(&window, fade);
        }
        self.visible = true;
        self.occluded_wait = crate::window::OCCLUDED_RETRY;
        // Shown onto sleeping displays or under another window, the panel stays occluded.
        #[cfg(target_os = "macos")]
        {
            self.occluded = false;
            self.sync_occlusion();
            if !self.occluded { self.ensure_pacing(); }
        }
        if !self.retained() { self.dirty = true; }
        self.idle &= !(self.rt.animating || self.compositor.as_ref().is_some_and(|c| c.animating()));
    }

    fn theme_changed(&mut self, dark: bool) {
        self.dark = dark;
        self.rt.set_system_dark(dark);
        self.dirty = true;
    }

    fn prepare(&mut self, event_loop: &ActiveEventLoop) {
        if self.live.is_some() { return; }
        self.resolve_size(event_loop);
        let made = if crate::window::is_wayland(event_loop) { self.layer(event_loop) } else { self.window(event_loop) };
        match made {
            Some(live) => {
                self.live = Some(live);
                self.set_availability(Availability::Available);
                self.rt.settle_all();
                self.dirty = true;
                self.idle = true;
                if self.options.prewarm { self.warm(); }
                if let Some(created) = self.on_created.take() {
                    let (window_number, capabilities) = *self.shared.native.lock().unwrap();
                    created(crate::OverlayCreated { window_number, capabilities });
                }
            }
            None => self.set_availability(Availability::Unavailable),
        }
    }

    // A retained host can be prepared while hidden. Resume sampling before warm/reveal so
    // hidden publications reach that first visible frame, without waking hidden hosts.
    fn resume_live(&mut self) {
        self.rt.set_live_visible(true);
        if self.rt.live_pending() { self.dirty = true; }
    }

    fn settle_before_reveal(&mut self) {
        // Initial retained pixels were settled by warm(). Leaving a settle flag pending here
        // would snap changes arriving after that frame, in the same turn as show().
        if !self.retained() { self.rt.settle_all(); }
    }

    /// macOS keeps a prewarmed host's alpha-zero surface, and its pixels, while hidden.
    fn retained(&self) -> bool { cfg!(target_os = "macos") && self.options.prewarm }

    fn refresh_for_reveal_at(&mut self, now: Instant) {
        if self.retained() && (self.dirty || self.rt.animating) {
            self.advance_at(now);
            self.warm_frame();
        }
    }

    fn hide(&mut self) { self.hide_fading(Duration::ZERO) }

    /// Shown, or fading out: frames still draw.
    fn drawing(&self) -> bool { self.visible || self.fading_out.is_some() }

    /// A hidden overlay draws nothing more: its display link, timer frames and live values stop.
    fn stop_drawing(&mut self) {
        self.fading_out = None;
        #[cfg(target_os = "macos")]
        if let Some(live) = &mut self.live && let Some(pacing) = &mut live.pacing { pacing.stop(); }
        self.rt.set_live_visible(false);
        self.retry = None;
    }

    /// Ends a fade-out that has run its course.
    fn end_fade_out(&mut self, now: Instant) {
        if self.fading_out.is_some_and(|end| end <= now) { self.stop_drawing(); }
    }

    fn hide_fading(&mut self, fade: Duration) {
        // Only macOS fades the window; elsewhere it goes at once, and so does drawing.
        let fading = cfg!(target_os = "macos") && !fade.is_zero() && (self.visible || self.fading_out.is_some());
        if fading {
            self.fading_out = Some(Instant::now() + fade);
        } else {
            self.stop_drawing();
        }
        self.visible = false;
        self.popup.cancel(&mut self.rt);
        self.rt.cancel_pointer();
        self.apply_cursor();
        let Some(live) = &self.live else { return };
        match &live.backing {
            Backing::Window(window) => {
                #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
                if let Some(x11) = &self.x11 { x11.hide(); }
                #[cfg(target_os = "windows")]
                if let Some(native) = &mut self.native { native.hide(); }
                #[cfg(target_os = "macos")]
                if let Some(panel) = &self.panel {
                    panel.hide(fade);
                }
                window.set_visible(false);
            }
            #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
            Backing::Layer(_) => { self.live.take(); }
        }
    }

    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
    fn layer(&mut self, event_loop: &ActiveEventLoop) -> Option<Live> {
        use winit::raw_window_handle::HasDisplayHandle;
        use winit::platform::wayland::MonitorHandleExtWayland;
        let display = event_loop.display_handle().ok()?.as_raw();
        let monitor = monitor(event_loop, &self.options.display);
        let scale = monitor.as_ref().map_or(1.0, |m| m.scale_factor()).ceil() as i32;
        // With no target, the compositor chooses an output; global pointer querying is forbidden.
        let output = match &self.options.display {
            DisplayTarget::Id(id) if event_loop.available_monitors().any(|m| m == id.0) => Some(id.0.native_id()),
            DisplayTarget::Native(crate::NativeDisplayId::Wayland(id)) if event_loop.available_monitors().any(|m| m.native_id() == *id) => Some(*id),
            _ => None,
        };
        let mut layer = crate::layer_shell::LayerSurface::new(display, self.name, self.at, self.interactive, &self.options, output)?;
        layer.set_buffer_scale(scale);
        if let Some((w, h)) = layer.configured_size() {
            if w > 0 { self.at.size.0 = w as f32; }
            if h > 0 { self.at.size.1 = h as f32; }
        }
        self.shared.native.lock().unwrap().1 = crate::OverlayCapabilities {
            capture_exclusion: OverlaySupport::Unsupported, global_pointer: OverlaySupport::Unsupported,
            nonactivating_keys: if layer.supports_keys() { OverlaySupport::Supported } else { OverlaySupport::Unsupported },
        };
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
        if let Some(p) = position(event_loop, self.at, &self.options) {
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
        self.dark = window.theme().is_none_or(|t| t == winit::window::Theme::Dark);
        self.rt.set_system_dark(self.dark);
        if self.cursor.is_none() { self.cursor = Some(crate::os_cursor::OsCursor::new(event_loop)); }
        let scale = window.scale_factor() as f32;
        // The surface first: on macOS it binds winit's view and its layer, which then move
        // into the panel.
        let live = self.attach(Backing::Window(window.clone()), scale)?;
        #[cfg(target_os = "macos")]
        {
            let wake = self.shared.wake.lock().unwrap().clone();
            self.panel = Some(crate::macos::Panel::adopt(&window, self.at, self.interactive, &self.options, wake)?);
            let panel = self.panel.as_ref().unwrap();
            if let Some(level) = self.macos_level { panel.set_level(level); }
            let native = (Some(panel.window_number()), crate::OverlayCapabilities {
                capture_exclusion: panel.capture_exclusion(), nonactivating_keys: OverlaySupport::Supported, global_pointer: OverlaySupport::Supported,
            });
            *self.shared.native.lock().unwrap() = native;
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = window.set_cursor_hittest(self.options.input.resolved(self.interactive) == OverlayInput::All);
            #[cfg(all(unix, not(target_os = "ios"), not(target_os = "android")))]
            {
                self.x11 = crate::x11_overlay::InputWindow::new(&window, self.shared.wake.lock().unwrap().clone(), self.options.input.resolved(self.interactive), self.options.follow_pointer);
                if self.options.input == OverlayInput::Regions && self.x11.is_none() { return None; }
                self.shared.native.lock().unwrap().1 = crate::OverlayCapabilities {
                    capture_exclusion: OverlaySupport::Unsupported, nonactivating_keys: OverlaySupport::Unsupported, global_pointer: OverlaySupport::Supported,
                };
            }
            #[cfg(target_os = "windows")]
            {
                let wake = self.shared.wake.lock().unwrap().clone();
                self.native = Some(crate::win32::OverlayWindow::new(&window, self.interactive, &self.options, wake)?);
                self.shared.native.lock().unwrap().1 = crate::OverlayCapabilities {
                    capture_exclusion: self.native.as_ref().unwrap().capture_exclusion(), nonactivating_keys: OverlaySupport::Unsupported, global_pointer: OverlaySupport::Supported,
                };
            }
        }
        self.place_window(event_loop, &window);
        Some(live)
    }

    /// Moving the window under a still pointer (`place`, `set_placement`, a resize that keeps
    /// the far edge) keeps the runtime's pointer and a held drag in step, so a drag that moves
    /// its own overlay reports pointer travel. Hosts that place asynchronously catch up on the
    /// next pointer event.
    fn place_window(&mut self, event_loop: &ActiveEventLoop, window: &Window) {
        self.position_window(event_loop, window);
        let Ok(origin) = window.outer_position() else { return };
        if let Some(before) = self.origin.replace(origin) && before != origin {
            let scale = window.scale_factor();
            let points = |a: i32, b: i32| (f64::from(a - b) / scale) as f32;
            self.rt.window_moved(points(origin.x, before.x), points(origin.y, before.y));
        }
    }

    fn position_window(&self, event_loop: &ActiveEventLoop, window: &Window) {
        #[cfg(target_os = "macos")]
        if let Some(panel) = &self.panel {
            panel.place(self.at, &self.options);
            return;
        }
        if let Some(p) = position(event_loop, self.at, &self.options) {
            window.set_outer_position(p);
        }
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        if let Some(x11) = &self.x11 { x11.update_geometry(); }
    }

    /// Puts the window on screen without focusing it or activating the app.
    fn show_window(&mut self, window: &Window, fade: Duration) {
        #[cfg(target_os = "macos")]
        if let Some(panel) = &self.panel {
            panel.show(fade);
            return;
        }
        let _ = fade;
        #[cfg(target_os = "windows")]
        {
            if let Some(native) = &mut self.native { native.show(); }
            crate::win32::show_overlay(window);
        }
        #[cfg(not(target_os = "windows"))]
        window.set_visible(true);
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        if let Some(x11) = &self.x11 { x11.show(self.options.level); }
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
            if let Some(gpu) = self.gpu.clone() { self.rescale(&gpu, w.scale_factor() as f32); }
        }
    }

    fn resolve_size(&mut self, event_loop: &ActiveEventLoop) {
        let at = Placement { size: self.canvas_size, ..self.at };
        #[cfg(target_os = "macos")]
        if let Some(rect) = self.panel.as_ref().map_or_else(|| crate::macos::overlay_rect(at, &self.options), |panel| panel.rect(at, &self.options)) {
            self.at.size = (rect.size.width as f32, rect.size.height as f32);
            return;
        }
        if let Some(monitor) = monitor(event_loop, &self.options.display) {
            let area = monitor.size().to_logical::<f32>(monitor.scale_factor());
            let rect = crate::overlay_options::local_rect(at, self.options.placement, (area.width, area.height));
            self.at.size = (rect[2], rect[3]);
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn follow_pointer(&mut self, event_loop: &ActiveEventLoop, point: (i32, i32)) {
        if !self.options.follow_pointer || self.options.display != DisplayTarget::UnderPointer { return; }
        let Some(Backing::Window(window)) = self.live.as_ref().map(|l| &l.backing) else { return };
        let target = event_loop.available_monitors().find(|m| {
            let (p, s) = (m.position(), m.size());
            point.0 >= p.x && point.1 >= p.y && i64::from(point.0) < i64::from(p.x) + i64::from(s.width)
                && i64::from(point.1) < i64::from(p.y) + i64::from(s.height)
        });
        if let Some(target) = target && window.current_monitor().as_ref() != Some(&target) {
            // Pin the sampled target across size and position resolution, then resume following.
            self.options.display = DisplayTarget::Id(crate::DisplayId::from_monitor(target, crate::window::is_wayland(event_loop)));
            self.resolve_size(event_loop);
            self.resized(event_loop);
            self.options.display = DisplayTarget::UnderPointer;
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
        let features = self.compositor.as_ref().map_or(wgpu::Features::empty(), |c| c.features());
        let pooled = self.gpu.clone().or_else(|| self.gpu_pool.borrow().upgrade().filter(|gpu| {
            gpu.device.features().contains(features & gpu.adapter.features())
        }));
        let surface = match pooled {
            Some(gpu) => {
                let surface = make(&gpu.instance)?;
                self.gpu = Some(gpu);
                surface
            }
            None => {
                let instance = Gpu::instance();
                let surface = make(&instance)?;
                let gpu = Arc::new(Gpu::try_with_features(instance, Some(&surface), features).ok()?);
                *self.gpu_pool.borrow_mut() = Arc::downgrade(&gpu);
                self.gpu = Some(gpu);
                surface
            }
        };
        let gpu = self.gpu.clone()?;
        let (w, h) = ((self.at.size.0 * scale).round() as u32, (self.at.size.1 * scale).round() as u32);
        let (mut config, translucent) = crate::window::surface_config(&surface, &gpu.adapter, (w, h), self.rt.srgb_blend())?;
        config.alpha_mode = translucent.unwrap_or(config.alpha_mode);
        surface.configure(&gpu.device, &config);
        let target_format = if self.rt.srgb_blend() { config.format.remove_srgb_suffix() } else { config.format };
        if self.renderer.as_ref().is_none_or(|(_, f)| *f != target_format) {
            let mut renderer = Renderer::new(&gpu.device, target_format);
            renderer.set_srgb_blend(self.rt.srgb_blend());
            self.renderer = Some((renderer, target_format));
            self.rt.text.atlas.invalidate();
            self.rt.images.invalidate();
        } else if let Some((renderer, _)) = &mut self.renderer {
            renderer.set_srgb_blend(self.rt.srgb_blend());
        }
        // Core Animation composites premultiplied pixels even in wgpu's PostMultiplied mode;
        // other straight-alpha surfaces get the conversion pass.
        let alpha = (config.alpha_mode == wgpu::CompositeAlphaMode::PostMultiplied && gpu.adapter.get_info().backend != wgpu::Backend::Metal)
            .then(|| crate::surface_alpha::AlphaSurface::new(&gpu.device, target_format, (config.width, config.height)));
        self.rt.transparent = true;
        self.rt.set_viewport(self.at.size, scale);
        #[cfg(target_os = "macos")]
        let pacing = self.frame_wake.clone().and_then(|wake| {
            let Backing::Window(window) = &backing;
            crate::display_link::DisplayLink::new(window, wake)
        });
        Some(Live {
            #[cfg(target_os = "macos")]
            pacing,
            #[cfg(target_os = "macos")]
            materials: crate::system_material::macos::Materials::new(),
            alpha, surface, config, backing, scale,
        })
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
    /// The runtime's cursor while the pointer is over a hit area of a shown overlay that takes
    /// input; the arrow once when it leaves, so `Cursor::Hidden` never outlives the overlay or
    /// the hover, and a click-through area leaves the cursor to the window under it.
    fn apply_cursor(&mut self) {
        let input = self.options.input.resolved(self.interactive);
        let over = self.rt.pointer().is_some_and(|(x, y)| {
            x >= 0.0 && y >= 0.0 && x < self.at.size.0 && y < self.at.size.1
                && (input != OverlayInput::Regions || self.rt.pointer_captured()
                    || self.rt.hit_regions().iter().any(|r| crate::overlay_options::contains(*r, x, y)))
        });
        let active = self.visible && input != OverlayInput::None && over;
        let want = self.rt.cursor();
        let (Some(Live { backing: Backing::Window(window), .. }), Some(cursor)) = (&self.live, &mut self.cursor) else { return };
        cursor.apply(window, want, active);
        #[cfg(target_os = "macos")]
        if let Some(panel) = &self.panel { panel.set_cursor(active.then_some(want)); }
    }

    fn observe_pointer(&mut self, x: f32, y: f32) {
        if self.rt.pointer_captured() || (x >= 0.0 && y >= 0.0 && x < self.at.size.0 && y < self.at.size.1) {
            self.dirty |= self.rt.observe_pointer(x, y);
            self.begin_drag_out();
        } else if self.rt.pointer().is_some() {
            self.rt.pointer_leave();
            self.dirty = true;
        }
    }

    /// Starts the OS drag a pointer move asked for, before the move event returns.
    fn begin_drag_out(&mut self) {
        let Some(start) = self.rt.take_drag_out() else { return };
        self.dirty = true;
        let (sink, path) = (self.rt.drag_end_sink(), start.path.clone());
        #[allow(irrefutable_let_patterns)]
        let Some(Live { backing: Backing::Window(window), config, scale, .. }) = &self.live else {
            sink.borrow_mut().push((path, crate::DragOutcome::Cancelled));
            return;
        };
        let image = match (&self.gpu, &self.renderer) {
            (Some(gpu), Some((renderer, _))) if start.item.drag_image().is_none() =>
                crate::drag_out::snapshot(gpu, renderer, (config.width, config.height), *scale, self.rt.clear_color(), start.rect),
            _ => None,
        };
        let wake = self.shared.wake.lock().unwrap().clone();
        crate::drag_out::begin(window, start, image, Box::new(move |outcome| {
            sink.borrow_mut().push((path, outcome));
            if let Some(wake) = wake { wake(); }
        }));
        // The system drag swallows the button release, so let go of the panel's capture now
        // rather than after its missed-release grace: the panel turns click-through outside
        // its regions (so drops land on the windows below) and deferred input changes apply.
        #[cfg(target_os = "macos")]
        if let Some(panel) = &self.panel { panel.pointer_released(); }
        self.apply_input();
        self.apply_cursor();
    }
    /// A left press or release, at the panel's click point when it has one.
    fn left_button(&mut self, pressed: bool, point: Option<(f32, f32)>) {
        if pressed {
            if let Some((x, y)) = point { self.rt.pointer_move(x, y); }
            if let Some((x, y)) = self.rt.pointer() {
                self.rt.pointer_down(x, y);
                self.rt.physical_input(crate::PhysicalInput::Modifiers(crate::window::map_mods(self.mods)));
            }
        } else if let Some((x, y)) = point.or(self.rt.pointer()) {
            // `pointer_up` moves to the release point itself, after disarming a drag-out: a
            // release never starts an OS drag.
            self.rt.pointer_up(x, y);
        }
        self.dirty = true;
    }

    /// Delivers drag ends a host reported. An `on_drag_start` may have hidden the overlay;
    /// its `on_drag_end` still runs.
    fn drag_ends_due(&mut self) {
        if !self.rt.drag_ends_pending() { return; }
        self.dirty = true;
        if self.frameless() { self.advance(); }
    }

    fn publish_regions(&mut self) {
        let Some(live) = &self.live else { return };
        #[cfg(target_os = "macos")]
        if let Some(panel) = &self.panel { panel.set_regions(self.rt.hit_regions()); }
        #[cfg(target_os = "windows")]
        if let Some(native) = &self.native { native.set_regions(self.rt.hit_regions(), live.scale); }
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        if self.options.input.resolved(self.interactive) == OverlayInput::Regions && let Some(x11) = &mut self.x11 { x11.regions(self.rt.hit_regions(), live.scale); }
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        if let Backing::Layer(layer) = &live.backing { layer.input_regions(self.rt.hit_regions(), self.interactive); }
        let _ = live;
    }

    /// Warm glyphs/pipelines and, on macOS, the retained alpha-zero host's actual surface.
    fn warm(&mut self) {
        self.rt.frame(0.0);
        self.rt.settle_all();
        self.rt.frame(0.0);
        self.clock.hold(Instant::now());
        self.warm_frame();
    }

    fn warm_frame(&mut self) {
        self.publish_regions();
        #[cfg(target_os = "macos")]
        if let Some(panel) = &self.panel {
            let _prewarm = panel.prewarm();
            self.dirty = true;
            self.draw_frame(true);
            if let Some(gpu) = &self.gpu { let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely()); }
            return;
        }
        let Some(live) = &self.live else { return };
        self.warm_offscreen((live.config.width, live.config.height));
    }

    fn warm_offscreen(&mut self, size: (u32, u32)) {
        let (Some((renderer, format)), Some(gpu)) = (&mut self.renderer, &self.gpu) else { return };
        let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("overlay prewarm"), size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
            format: *format, usage: wgpu::TextureUsages::RENDER_ATTACHMENT, view_formats: &[],
        });
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        render_frame(&mut Frame {
            device: &gpu.device, queue: &gpu.queue, encoder: &mut enc,
            target: &target.create_view(&Default::default()), format: *format, size,
            dt: 0.0, update_ms: 0.0, rt: &mut self.rt, ui: renderer,
        }, &mut self.compositor);
        renderer.submit(&gpu.queue, enc);
        let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
    }

    fn apply_input(&mut self) {
        if self.rt.pointer_captured() { return; }
        #[cfg(target_os = "macos")]
        if self.panel.as_ref().is_some_and(|panel| panel.pointer_captured()) { return; }
        let Some(input) = self.pending_input.take() else { return };
        self.options.input = input;
        let resolved = input.resolved(self.interactive);
        #[cfg(target_os = "macos")]
        if let Some(panel) = &self.panel { panel.set_input(resolved); }
        #[cfg(target_os = "windows")]
        if let Some(native) = &mut self.native { native.set_input(resolved); }
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        {
            if let Some(x11) = &mut self.x11 { x11.set_input(resolved); }
            if let Some(Live { backing: Backing::Layer(layer), .. }) = &mut self.live { layer.set_input(resolved); }
        }
        self.publish_regions();
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        if let Some(Live { backing: Backing::Layer(layer), .. }) = &mut self.live { layer.commit(); }
    }

    fn advance(&mut self) {
        self.advance_at(Instant::now());
    }

    /// Handle updates, and the `setup` waker (new data arrived) re-runs the view. A shown
    /// overlay draws a frame, which polls; a hidden one runs a frame without drawing, as for a
    /// due timer.
    fn apply_wake(&mut self, woken: bool, updates: Vec<Update<T>>) {
        let updated = !updates.is_empty();
        for f in updates { self.rt.update(f); }
        if woken { self.rt.invalidate(); }
        if !woken && !updated { return; }
        self.clipboard.flush(&mut self.rt);
        self.dirty = true; // Queued updates already invalidate the cache before polling.
        if self.frameless() {
            // Register timer requests even before the first show; hidden or occluded overlays
            // don't draw.
            self.advance();
        }
    }

    /// Hidden, or shown but not on screen: no frame will come to run what's due.
    fn frameless(&self) -> bool {
        !self.drawing() || self.is_occluded()
    }

    fn advance_at(&mut self, now: Instant) {
        // Covered or asleep, a shown overlay only polls. What it would commit waits for the
        // first uncovered frame, which resumes running motion and starts new motion fresh
        // (`step`). Input and drag ends still run their handlers now.
        if self.visible && self.is_occluded() && !self.rt.has_pending_input() && !self.rt.drag_ends_pending() {
            self.rt.poll_hidden();
            self.rt.shell_request = None;
            self.rt.window_frame = None;
            self.clipboard.flush(&mut self.rt);
            self.open_popup();
            self.dirty = true;
            return;
        }
        let commits = self.rt.stats.commits;
        if self.retained() {
            self.rt.frame_split(now.saturating_duration_since(self.clock.last()).as_secs_f64(), 0.0);
            self.clock.hold(now);
        } else {
            self.rt.frame(0.0);
        }
        self.rt.shell_request = None;
        // Overlays size through `OverlayHandle::resize`, not `Tx::resize_window`.
        self.rt.window_frame = None;
        self.clipboard.flush(&mut self.rt);
        self.open_popup();
        // Even a transition that finished in this step needs its current pixels submitted.
        // A scheduling-only poll with no motion still preserves the prepared frame.
        if self.rt.stats.commits != commits || self.rt.animating || !self.idle {
            self.dirty = true;
            self.idle = false;
        }
    }

    fn wake_due(&mut self, now: Instant) {
        self.end_fade_out(now);
        let deadline = crate::window::wall_wake_deadline(&self.rt);
        if deadline.is_some_and(|deadline| deadline <= now) {
            if !self.frameless() {
                // Like the window host, poll once in the upcoming frame, after acquiring
                // the drawable. A false poll result does not keep the display loop alive.
                self.dirty = true;
            } else {
                // No frame will come to run it: a hidden or occluded overlay steps without
                // drawing, so its timers fire (and may hide it) on time.
                self.advance();
            }
        }
        if self.retry.is_some_and(|retry| retry <= now) {
            self.retry = None;
            self.dirty = true;
        }
    }

    fn is_occluded(&self) -> bool {
        #[cfg(target_os = "macos")]
        return self.occluded;
        #[cfg(not(target_os = "macos"))]
        false
    }

    /// A frame with no drawable still runs what it would have (updates, poll, due timers)
    /// without drawing, as a hidden overlay does, and the next try waits.
    fn missed(&mut self, missed: crate::window::Missed, paced: bool) {
        let now = Instant::now();
        self.advance_at(now);
        let wait = self.occluded_wait;
        if missed == crate::window::Missed::Occluded {
            self.occluded_wait = (wait * 2).min(crate::window::OCCLUDED_MAX);
        }
        self.retry = match missed.next(paced, now, wait) {
            crate::window::NextFrame::WaitUntil(at) => Some(at),
            crate::window::NextFrame::Idle => None,
        };
    }

    /// Applies requests from other threads: updates, edge, show and hide.
    fn sync(&mut self, event_loop: &ActiveEventLoop) {
        if self.clipboard.receive(&mut self.rt) { self.dirty = true; self.advance(); }
        if self.popup.answer(&mut self.rt) {
            self.dirty = true;
            if self.frameless() { self.advance(); }
        }
        if self.rt.live_pending() { self.dirty = true; }
        self.drag_ends_due();
        let requests = std::mem::take(&mut *self.shared.requests.lock().unwrap());
        if let Some(input) = requests.input { self.pending_input = Some(input); }
        self.apply_input();
        if let Some(follow) = requests.follow_pointer {
            self.options.follow_pointer = follow;
            #[cfg(target_os = "macos")]
            if let Some(panel) = &self.panel { panel.set_follow_pointer(follow); }
            #[cfg(target_os = "windows")]
            if let Some(native) = &mut self.native { native.set_follow_pointer(follow); }
            #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
            if let Some(x11) = &self.x11 { x11.set_follow_pointer(follow); }
        }
        let options_changed = requests.placement.is_some() || requests.display.is_some();
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        let recreate_layer = options_changed && matches!(self.live.as_ref().map(|l| &l.backing), Some(Backing::Layer(_)));
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        let layer_was_visible = self.visible;
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        if recreate_layer { self.hide(); }
        if let Some(placement) = requests.placement { self.options.placement = placement; }
        if let Some(display) = requests.display { self.options.display = display; }
        self.apply_wake(requests.woken, requests.updates);
        let mut at = self.at;
        at.edge = requests.edge.unwrap_or(at.edge);
        at.margin = requests.margin.unwrap_or(at.margin);
        if let Some((align, inset)) = requests.align {
            (at.align, at.inset) = (align, inset);
        }
        let old_size = self.at.size;
        if let Some(size) = requests.size { self.canvas_size = size; }
        at.size = requests.size.unwrap_or(at.size);
        if at != self.at || options_changed {
            self.at = at;
            self.resolve_size(event_loop);
            if self.at.size != old_size {
                self.resized(event_loop);
            } else {
                match self.live.as_mut().map(|l| &mut l.backing) {
                    Some(Backing::Window(w)) => {
                        let w = w.clone();
                        self.place_window(event_loop, &w);
                        if let Some(gpu) = self.gpu.clone() { self.rescale(&gpu, w.scale_factor() as f32); }
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
        if self.options.prewarm && self.shared.availability.load(Ordering::Acquire) == 0 { self.prepare(event_loop); }
        if let Some(level) = requests.macos_level {
            self.macos_level = Some(level);
            #[cfg(target_os = "macos")]
            if let Some(panel) = &self.panel { panel.set_level(level); }
        }
        let fade = requests.fade.unwrap_or_default();
        match requests.visible {
            Some(true) => self.show_fading(event_loop, fade),
            Some(false) => self.hide_fading(fade),
            None => {}
        }
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        if recreate_layer && self.live.is_none() {
            if layer_was_visible && requests.visible != Some(false) { self.show_fading(event_loop, Duration::ZERO); }
            else if self.options.prewarm { self.prepare(event_loop); }
        }
        self.apply_input();
        if self.visible {
            #[cfg(target_os = "macos")]
            if let Some(panel) = &self.panel { self.shared.native.lock().unwrap().1.global_pointer = panel.global_pointer(); }
            #[cfg(target_os = "windows")]
            if let Some(native) = &self.native { self.shared.native.lock().unwrap().1.global_pointer = native.global_pointer(); }
            #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
            if let Some(x11) = &self.x11 { self.shared.native.lock().unwrap().1.global_pointer = x11.global_pointer(); }
        }
        #[cfg(target_os = "macos")]
        if requests.request_keys && self.visible && self.options.keys == KeyPolicy::OnDemand
            && let Some(panel) = &self.panel { panel.request_keys(); }
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        if requests.request_keys && self.visible && self.options.keys == KeyPolicy::OnDemand
            && let Some(Live { backing: Backing::Layer(layer), .. }) = &self.live { layer.request_keys(); }
    }

    /// Draws a frame if one is due; true while it needs more (animating).
    fn draw(&mut self) -> bool {
        self.end_fade_out(Instant::now());
        if !self.drawing() { return false; }
        let more = self.draw_frame(false);
        self.open_popup();
        more
    }

    /// Opens the pop-up menu a handler asked for over the overlay's panel. A hidden or occluded
    /// overlay answers `None`, committed with its next frame. One anchored to an element waits
    /// for the frame that commits what changed since the last.
    fn open_popup(&mut self) {
        let window = match self.live.as_ref().map(|l| &l.backing) {
            Some(Backing::Window(w)) if self.visible && !self.is_occluded() => Some(w.clone()),
            _ => None,
        };
        if self.popup.open(&mut self.rt, window.as_ref(), true) { self.dirty = true; }
    }

    fn draw_frame(&mut self, prewarm: bool) -> bool {
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        if let Some(Live { backing: Backing::Layer(layer), .. }) = self.live.as_mut() {
            let changes = layer.dispatch();
            if changes.closed {
                self.hide();
                return false;
            }
            for input in changes.input {
                use crate::layer_shell::Input;
                match input {
                    Input::Move(x, y) => { self.dirty |= self.rt.observe_pointer(x, y); continue; }
                    Input::Leave => self.rt.pointer_leave(),
                    Input::Button(down) => if let Some((x, y)) = self.rt.pointer() {
                        if down { self.rt.pointer_down(x, y); } else { self.rt.pointer_up(x, y); }
                    },
                    Input::Key(key) => self.rt.key(key),
                    Input::Physical(event, fallback) => {
                        self.dirty |= self.rt.physical_with_fallback(event, fallback);
                        continue;
                    }
                }
                self.dirty = true;
            }
            if let Some((w, h)) = changes.size {
                let size = (if w == 0 { self.at.size.0 } else { w as f32 }, if h == 0 { self.at.size.1 } else { h as f32 });
                if size != self.at.size {
                    self.at.size = size;
                    if let Some(live) = self.live.as_mut() { live.scale = 0.0; }
                }
            }
            if let Some(scale) = changes.scale {
                if let Some(Live { backing: Backing::Layer(layer), .. }) = self.live.as_mut() { layer.set_buffer_scale(scale); }
                if let Some(gpu) = self.gpu.clone() {
                    self.rescale(&gpu, scale as f32);
                }
            }
            if self.live.as_ref().is_some_and(|l| l.scale == 0.0) && let Some(gpu) = self.gpu.clone() {
                self.rescale(&gpu, self.rt.env.scale);
            }
        }
        self.apply_input();
        if self.live.is_none() || self.renderer.is_none() || self.gpu.is_none() {
            return false;
        }
        // Even an idle overlay looks again on its retry while occluded: the occlusion
        // notification that should uncover it may never come.
        #[cfg(target_os = "macos")]
        if !prewarm {
            self.sync_occlusion();
            // Until it's uncovered, only the retry (or a due timer, in `wake_due`) steps it,
            // however often other windows turn the loop.
            if self.occluded {
                if self.retry.is_none_or(|retry| retry <= Instant::now()) { self.missed(crate::window::Missed::Occluded, false); }
                return false;
            }
        }
        if !self.dirty && self.idle { return false; }
        if !prewarm { self.retry = None; }
        let (Some(live), Some((renderer, _)), Some(gpu)) = (self.live.as_mut(), self.renderer.as_mut(), self.gpu.as_ref()) else {
            return false;
        };
        #[cfg(target_os = "macos")]
        let target = if prewarm { None } else if let Some(pacing) = &mut live.pacing {
            let Some(target) = pacing.next_frame() else { return false };
            Some(target)
        } else { None };
        #[cfg(target_os = "macos")]
        let paced = live.pacing.as_ref().is_some_and(|p| p.running());
        #[cfg(not(target_os = "macos"))]
        let paced = false;
        let frame = match crate::window::acquire(&live.surface, &gpu.device, &live.config, if prewarm { 2 } else { 1 }) {
            Ok(frame) => frame,
            Err(missed) => {
                if !prewarm {
                    #[cfg(target_os = "macos")]
                    if missed == crate::window::Missed::Occluded { self.set_occluded(true); }
                    self.missed(missed, paced);
                }
                return false;
            }
        };
        let now = Instant::now();
        #[cfg(not(target_os = "macos"))]
        let frame_time = now;
        #[cfg(target_os = "macos")]
        let frame_time = target.unwrap_or(now);
        #[cfg(target_os = "macos")]
        { self.rt.native_materials = live.materials.native(); }
        let (mut animating, dt) = step(&mut self.rt, &mut self.clock, &self.compositor, frame_time, prewarm);
        #[cfg(target_os = "macos")]
        let frame = {
            let Backing::Window(window) = &live.backing;
            if live.materials.update(self.rt.transparent, !self.rt.materials.is_empty(), window, &live.surface) {
                // This drawable presents without the views: build again natively into a new one,
                // so prewarm and reveal show the material from this frame.
                drop(frame);
                self.rt.native_materials = true;
                animating |= self.rt.frame(0.0);
                match crate::window::acquire(&live.surface, &gpu.device, &live.config, if prewarm { 2 } else { 1 }) {
                    Ok(frame) => frame,
                    Err(_) => return true,
                }
            } else { frame }
        };
        let update_ms = if self.compositor.is_some() { now.elapsed().as_secs_f64() * 1000.0 } else { 0.0 };
        self.rt.shell_request = None;
        // Overlays size through `OverlayHandle::resize`, not `Tx::resize_window`.
        self.rt.window_frame = None;
        self.clipboard.flush(&mut self.rt);
        // Publish the geometry just rendered, never a layout cached from the preceding frame.
        #[cfg(target_os = "macos")]
        if let Some(panel) = &self.panel { panel.set_regions(self.rt.hit_regions()); }
        #[cfg(target_os = "windows")]
        if let Some(native) = &self.native { native.set_regions(self.rt.hit_regions(), live.scale); }
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        if self.options.input == OverlayInput::Regions && let Some(x11) = &mut self.x11 { x11.regions(self.rt.hit_regions(), live.scale); }
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        if let Backing::Layer(layer) = &live.backing { layer.input_regions(self.rt.hit_regions(), self.interactive); }
        let target_format = if self.rt.srgb_blend() { live.config.format.remove_srgb_suffix() } else { live.config.format };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
            format: (target_format != live.config.format).then_some(target_format),
            ..Default::default()
        });
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        animating |= render_frame(&mut Frame {
            device: &gpu.device, queue: &gpu.queue, encoder: &mut enc,
            target: live.alpha.as_ref().map_or(&view, |a| &a.view), format: target_format,
            size: (live.config.width, live.config.height), dt, update_ms,
            rt: &mut self.rt, ui: renderer,
        }, &mut self.compositor);
        if let Some(alpha) = &live.alpha {
            alpha.draw(&mut enc, &view);
        }
        renderer.submit(&gpu.queue, enc);
        #[allow(irrefutable_let_patterns)]
        if let Backing::Window(w) = &live.backing {
            w.pre_present_notify();
        }
        #[cfg(target_os = "macos")]
        live.materials.present(&self.rt.materials, self.rt.env.scale, || gpu.queue.present(frame));
        #[cfg(not(target_os = "macos"))]
        gpu.queue.present(frame);
        self.dirty = false;
        #[cfg(target_os = "macos")]
        let period = live.pacing.as_ref().map_or(crate::window::TIMER_FRAME, |p| Duration::from_secs_f64(1.0 / p.refresh_hz()));
        #[cfg(not(target_os = "macos"))]
        let period = crate::window::TIMER_FRAME;
        self.idle = idle_after(&self.rt, animating, Instant::now() + period);
        if !prewarm { self.occluded_wait = crate::window::OCCLUDED_RETRY; }
        // The display link wakes this overlay, never ControlFlow::Poll. Without one (it can't
        // be made while the displays sleep, or didn't start), a timer paces its motion.
        #[cfg(target_os = "macos")]
        {
            let idle = self.idle;
            let paced = live.pacing.as_mut().is_some_and(|pacing| {
                pacing.presented(Instant::now());
                if prewarm || idle { pacing.stop(); }
                pacing.running()
            });
            if !paced && !prewarm && !idle { self.retry = Some(now + crate::window::TIMER_FRAME); }
            false
        }
        #[cfg(not(target_os = "macos"))]
        !self.idle
    }

    /// Records whether the shown panel is on screen. Occluded, it stops its display link,
    /// ambient loops and live values; uncovered, it redraws once and resumes them.
    #[cfg(target_os = "macos")]
    fn occluded(&mut self, occluded: bool) {
        self.occluded = occluded;
        self.idle = true;
        if occluded {
            if let Some(live) = &mut self.live && let Some(pacing) = &mut live.pacing { pacing.stop(); }
            self.rt.set_live_visible(false);
        } else if self.drawing() {
            self.dirty = true;
            self.occluded_wait = crate::window::OCCLUDED_RETRY;
            self.rt.set_live_visible(true);
            self.ensure_pacing();
        }
    }

    /// Reads the panel's occlusion directly: one shown onto covered or sleeping displays gets
    /// no event saying so.
    #[cfg(target_os = "macos")]
    fn sync_occlusion(&mut self) {
        let occluded = self.panel.as_ref().is_some_and(|p| p.occluded());
        self.set_occluded(occluded);
    }

    /// Applies an occlusion reading. A fading-out panel reads as covered once its alpha nears
    /// zero, but it's still drawing its exit, so it never becomes occluded mid-fade.
    #[cfg(target_os = "macos")]
    fn set_occluded(&mut self, occluded: bool) {
        if occluded && self.fading_out.is_some() { return; }
        if occluded != self.occluded { self.occluded(occluded); }
    }

    /// A display link made while the displays slept fails; try again once they may be back. One
    /// that exists follows the panel to the display it is on now.
    #[cfg(target_os = "macos")]
    fn ensure_pacing(&mut self) {
        let (Some(live), Some(wake)) = (&mut self.live, &self.frame_wake) else { return };
        let Backing::Window(window) = &live.backing;
        match &mut live.pacing {
            Some(pacing) => pacing.update_display(window),
            None => live.pacing = crate::display_link::DisplayLink::new(window, wake.clone()),
        }
    }
}

/// Whether nothing asks for the frame after one just presented, due by `next_frame`. A timer
/// due by then counts as asking: a poll stepping its own animation with
/// `Tx::wake_after(Duration::ZERO)` gets every frame at the display's rate, instead of the
/// display link stopping after each frame and a timer starting it again.
fn idle_after<T: 'static>(rt: &Runtime<T>, animating: bool, next_frame: Instant) -> bool {
    !(animating || rt.has_pending_input() || crate::window::wall_wake_deadline(rt).is_some_and(|due| due <= next_frame))
}

/// `draw_frame`'s animation step, apart from the surface so tests can drive it.
fn step<T: 'static>(rt: &mut Runtime<T>, clock: &mut FrameClock, compositor: &Option<Box<dyn Compositor<T>>>, frame_time: Instant, prewarm: bool) -> (bool, f64) {
    if prewarm {
        clock.hold(frame_time);
        return (rt.frame(0.0), 0.0);
    }
    clock.advance(rt, frame_time, compositor.as_ref().is_some_and(|c| c.animating()))
}

/// Both native surface frames and offscreen prewarming use the same compositor path.
fn render_frame<T: 'static>(f: &mut Frame<'_, T>, compositor: &mut Option<Box<dyn Compositor<T>>>) -> bool {
    f.ui.upload_images(f.device, f.queue, &mut f.rt.images);
    f.ui.prepare_surfaces(f.device, f.queue, &f.rt.surfaces, f.size);
    if let Some(c) = compositor {
        c.render(f);
        c.animating()
    } else {
        f.ui.prepare(f.device, f.queue, &mut f.rt.text.atlas, &f.rt.instances, f.size);
        f.ui.draw(f.encoder, f.target, [0.0; 4]);
        false
    }
}

impl<T: 'static> AnyHost for Overlay<T> {
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::ResumeTimeReached { .. } | StartCause::Poll) {
            let now = Instant::now();
            self.wake_due(now);
        }
    }

    fn wake_deadline(&self) -> Option<Instant> {
        let deadline = crate::window::wall_wake_deadline(&self.rt);
        // A running display link consumes due runtime timers. Keep the independent
        // pointer deadline, but do not spin on an overdue animation timer.
        #[cfg(target_os = "macos")]
        let deadline = deadline.filter(|_| !self.live.as_ref().and_then(|l| l.pacing.as_ref()).is_some_and(|p| p.running()));
        #[cfg(target_os = "macos")]
        let deadline = deadline.into_iter().chain(self.panel.as_ref().filter(|_| !self.occluded).and_then(|p| p.pointer_deadline())).min();
        deadline.into_iter().chain(self.retry.filter(|_| self.drawing())).chain(self.fading_out).min()
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.sync(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: WakeEvent) {
        #[cfg(target_os = "macos")]
        if matches!(event, WakeEvent::DisplayTick(_))
            && self.live.as_ref().and_then(|l| l.pacing.as_ref()).is_some_and(|p| p.running()) {
            self.draw();
        }
        if let WakeEvent::App(_) = event {
            self.sync(event_loop);
        }
    }

    fn owns(&self, id: WindowId) -> bool {
        let window = match self.live.as_ref().map(|l| &l.backing) {
            Some(Backing::Window(w)) => Some(w),
            _ => None,
        };
        window.is_some_and(|w| w.id() == id)
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        #[cfg(target_os = "macos")]
        if matches!(event, WindowEvent::Moved(_) | WindowEvent::ScaleFactorChanged { .. } | WindowEvent::Occluded(_))
            && let Some(live) = &mut self.live && let Some(pacing) = &mut live.pacing {
            let Backing::Window(window) = &live.backing;
            pacing.update_display(window);
        }
        // The panel's own state, which winit's source window forwards; kept while hidden too.
        #[cfg(target_os = "macos")]
        if let WindowEvent::Occluded(occluded) = event {
            let occluded = self.panel.as_ref().map_or(occluded, |p| p.occluded());
            if occluded && self.fading_out.is_some() { return; }
            self.occluded(occluded);
            return;
        }
        if let WindowEvent::Focused(focused) = &event {
            #[cfg(target_os = "macos")]
            if self.panel.as_ref().is_some_and(|p| p.is_key() != *focused) { return; }
            self.rt.window_focus(*focused);
            match self.rt.shell_request.take() {
                Some(crate::el::Shell::Close) => self.hide(),
                Some(crate::el::Shell::Quit) => event_loop.exit(),
                None => {}
            }
        }
        // Hidden too, so the retained surface redraws in the new theme before it's revealed.
        if let WindowEvent::ThemeChanged(theme) = event {
            return self.theme_changed(theme == winit::window::Theme::Dark);
        }
        if !self.visible { return; }
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        if matches!(event, WindowEvent::Moved(_) | WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. })
            && let Some(x11) = &self.x11 { x11.update_geometry(); }
        match event {
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(gpu) = self.gpu.clone() {
                    self.rescale(&gpu, scale_factor as f32);
                }
            }
            WindowEvent::RedrawRequested | WindowEvent::Resized(_) => self.dirty = true,
            WindowEvent::CursorMoved { position, .. } if self.options.input.resolved(self.interactive) != OverlayInput::None => {
                let scale = self.live.as_ref().map_or(1.0, |l| l.scale);
                self.dirty |= self.rt.observe_pointer(position.x as f32 / scale, position.y as f32 / scale);
                self.begin_drag_out();
            }
            WindowEvent::CursorLeft { .. } if self.options.input.resolved(self.interactive) != OverlayInput::None => {
                self.rt.pointer_leave();
                self.dirty = true;
            }
            WindowEvent::ModifiersChanged(m) => {
                self.mods = m.state();
                self.dirty |= self.rt.physical_observation(crate::PhysicalInput::Modifiers(crate::window::map_mods(self.mods)));
            }
            WindowEvent::Focused(focused) => {
                self.rt.set_system_reduced_motion(crate::preferences::reduced_motion());
                if !focused {
                    self.rt.physical_input(crate::PhysicalInput::FocusLost);
                    self.rt.cancel_composition();
                }
                self.dirty = true;
            }
            WindowEvent::KeyboardInput { event, .. } if self.options.keys == KeyPolicy::OnDemand => {
                let pressed = event.state.is_pressed();
                let fallback = pressed.then(|| crate::window::map_key(&event.logical_key, self.mods)).flatten();
                if let winit::keyboard::PhysicalKey::Code(key) = event.physical_key {
                    self.dirty |= self.rt.physical_with_fallback(crate::PhysicalInput::Key {
                        key, mods: crate::window::map_mods(self.mods), pressed, repeat: event.repeat,
                    }, fallback);
                } else if let Some(key) = fallback {
                    if event.repeat { self.rt.repeat_key(key); } else { self.rt.key(key); }
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput { state, button: winit::event::MouseButton::Left, .. } => {
                #[cfg(target_os = "macos")]
                let point = self.panel.as_ref().and_then(|p| p.take_click(state.is_pressed()));
                if self.options.input.resolved(self.interactive) != OverlayInput::None {
                    #[cfg(target_os = "windows")]
                    let point = self.native.as_ref().and_then(|p| p.take_click(state.is_pressed()));
                    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                    let point: Option<(f32, f32)> = None;
                    self.left_button(state.is_pressed(), point);
                }
                #[cfg(target_os = "macos")]
                if !state.is_pressed() && let Some(panel) = &self.panel { panel.pointer_released(); }
                self.apply_input();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) -> bool {
        // A fading-out overlay takes no input; it only draws its exit.
        if !self.visible { return self.draw(); }
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        if let Some(live) = &self.live && let Some(x11) = &self.x11 && let Some((point, desktop)) = x11.take_pointer(live.scale) {
            self.observe_pointer(point.0, point.1);
            self.follow_pointer(_event_loop, desktop);
        }
        #[cfg(target_os = "windows")]
        if self.live.is_some() && let Some(point) = self.native.as_ref().and_then(|p| p.take_pointer()) && self.rt.pointer() != Some(point) {
            self.observe_pointer(point.0, point.1);
            if let Some(point) = crate::win32::pointer_position() { self.follow_pointer(_event_loop, point); }
        }
        // Covering or sleeping displays can come without an event the overlay sees.
        #[cfg(target_os = "macos")]
        if self.live.is_some() { self.sync_occlusion(); }
        #[cfg(target_os = "macos")]
        if self.live.is_some() && let Some(panel) = &self.panel {
            // Covered or asleep, the 120 Hz regions poll rests; input events still sample.
            panel.observe_pointer(Instant::now(), !self.occluded);
            if let Some((x, y)) = panel.take_recovered_release() && self.rt.pointer_captured() {
                self.rt.pointer_up(x, y);
                self.dirty = true;
            }
            if let Some((x, y)) = panel.take_pointer() && self.rt.pointer() != Some((x, y)) {
                self.observe_pointer(x, y);
                if self.options.follow_pointer && self.options.display == DisplayTarget::UnderPointer {
                    let size = self.at.size;
                    self.resolve_size(_event_loop);
                    if size != self.at.size { self.resized(_event_loop); }
                    else if let Some(panel) = &self.panel { panel.place(self.at, &self.options); }
                }
            }
        }
        let more = self.draw();
        self.apply_cursor();
        more
    }

    fn keeps_loop(&self) -> bool {
        false
    }

    fn cancel_menus(&mut self) { self.popup.cancel(&mut self.rt); }
}

impl<T: 'static> PendingOverlay for Overlay<T> {
    fn connect(&mut self, wake: Waker, _frame_wake: Waker, gpu: Rc<RefCell<Weak<Gpu>>>) {
        #[cfg(target_os = "macos")]
        { self.frame_wake = Some(_frame_wake); }
        // All live sources (including Pixels) must share this registration: a second
        // set_live_waker call replaces the wake lane and resets its subscriptions.
        self.rt.set_live_waker(wake.clone());
        self.clipboard.connect(wake.clone());
        self.popup.connect(wake.clone());
        self.gpu_pool = gpu;
        *self.shared.wake.lock().unwrap() = Some(wake.clone());
        if let Some(setup) = self.setup.take() {
            let shared = self.shared.clone();
            setup(&mut self.rt.state, Arc::new(move || {
                let first = !std::mem::replace(&mut shared.requests.lock().unwrap().woken, true);
                if first { shared.wake(); }
            }));
        }
        // Requests made before the loop existed.
        wake();
    }
}

/// On the primary screen's top or bottom edge, `margin` points in, centred or `inset` points
/// from a side.
fn position(event_loop: &ActiveEventLoop, at: Placement, options: &OverlayOptions) -> Option<PhysicalPosition<i32>> {
    let monitor = monitor(event_loop, &options.display)?;
    let scale = monitor.scale_factor();
    let (origin, area) = (monitor.position(), monitor.size());
    if options.placement.is_some() {
        let r = crate::overlay_options::local_rect(at, options.placement, (area.width as f32 / scale as f32, area.height as f32 / scale as f32));
        return Some(PhysicalPosition::new(origin.x + (r[0] as f64 * scale).round() as i32, origin.y + (r[1] as f64 * scale).round() as i32));
    }
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

fn monitor(event_loop: &ActiveEventLoop, target: &DisplayTarget) -> Option<winit::monitor::MonitorHandle> {
    monitor_at(event_loop, target, None)
}

pub(crate) fn monitor_at(event_loop: &ActiveEventLoop, target: &DisplayTarget, sampled: Option<(i32, i32)>) -> Option<winit::monitor::MonitorHandle> {
    let monitors: Vec<_> = event_loop.available_monitors().collect();
    let primary = event_loop.primary_monitor().and_then(|p| monitors.iter().position(|m| *m == p));
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
    let resolved = if let DisplayTarget::Native(crate::NativeDisplayId::X11Output(output)) = target {
        if !crate::window::is_wayland(event_loop) { crate::x11_overlay::output_crtc(*output).map(|id| DisplayTarget::Native(crate::NativeDisplayId::X11Crtc(id))) } else { None }
    } else { None };
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
    let target = resolved.as_ref().unwrap_or(target);
    let requested = match target {
        DisplayTarget::Id(id) => monitors.iter().position(|m| *m == id.0),
        DisplayTarget::Native(id) => monitors.iter().position(|m| crate::DisplayId::from_monitor(m.clone(), crate::window::is_wayland(event_loop)).native_id() == Some(*id)),
        _ => None,
    };
    let bounds: Vec<_> = monitors.iter().map(|m| {
        let (p, s) = (m.position(), m.size());
        [f64::from(p.x), f64::from(p.y), f64::from(p.x) + f64::from(s.width), f64::from(p.y) + f64::from(s.height)]
    }).collect();
    #[cfg(target_os = "windows")]
    let pointer = (*target == DisplayTarget::UnderPointer).then(crate::win32::pointer_position).flatten();
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
    let pointer = if *target == DisplayTarget::UnderPointer && !crate::window::is_wayland(event_loop) { crate::x11_overlay::pointer_position() } else { None };
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
    let pointer: Option<(i32, i32)> = None;
    let i = crate::overlay_options::choose_display(&bounds, primary, requested, sampled.or(pointer).map(|(x, y)| (f64::from(x), f64::from(y))))?;
    Some(monitors[i].clone())
}

impl<T: 'static> Drop for Overlay<T> {
    fn drop(&mut self) {
        *self.shared.native.lock().unwrap() = Default::default();
        self.set_availability(Availability::Unavailable);
        // The surface must disappear before returning the adopted view or removing subclasses.
        // Keep winit's Window alive until after those native owners have unwound, too.
        let backing = self.live.take().map(|live| {
            let Live { alpha, surface, backing, .. } = live;
            drop(alpha);
            drop(surface);
            backing
        });
        #[cfg(target_os = "macos")]
        self.panel.take();
        #[cfg(target_os = "windows")]
        self.native.take();
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        self.x11.take();
        drop(backing);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const FRAME: Duration = Duration::from_micros(16_667);

    fn width_overlay(name: &'static str) -> Overlay<f32> {
        fn view(width: &f32, _: &mut Cx<f32>) -> El {
            crate::el().child(crate::el().key("box").size(*width, 8.0))
        }
        let app = AppSpec::new(name, (400.0, 50.0), || 12.0, view);
        let (mut overlay, _) = Overlay::new(OverlaySpec::new(name, app.size, view).options(Default::default()), 12.0, &app);
        overlay.rt.wall_start = None;
        overlay
    }

    /// One `draw_frame` without a surface: the frame step, then the idle bookkeeping after present.
    fn present<T: 'static>(overlay: &mut Overlay<T>, at: Instant) {
        let (animating, _) = step(&mut overlay.rt, &mut overlay.clock, &overlay.compositor, at, false);
        overlay.dirty = false;
        overlay.idle = idle_after(&overlay.rt, animating, at + FRAME);
    }

    fn width<T: 'static>(overlay: &Overlay<T>, key: &str) -> f32 {
        overlay.rt.presented(key).unwrap().0[2]
    }

    /// A spring started at `FRAME`, presented at `times` (from `start`); `before` runs ahead of
    /// the frame at `at`. Returns the width after each frame.
    fn widths(times: &[Duration], at: Duration, before: fn(&mut Overlay<f32>)) -> Vec<(Duration, f32)> {
        let mut overlay = width_overlay("late frame");
        let start = Instant::now();
        times.iter().map(|&t| {
            if t == FRAME { overlay.rt.update(|width, _| *width = 212.0); }
            if t == at { before(&mut overlay); }
            present(&mut overlay, start + t);
            (t, width(&overlay, "box"))
        }).collect()
    }

    /// A 49 ms frame mid-spring lands where an unbroken 16.7 ms cadence is at that time, and so
    /// do the frames after it: no hitch followed by a slow catch-up. `before` runs just ahead
    /// of the late frame.
    fn assert_late_frame_keeps_time(before: fn(&mut Overlay<f32>), what: &str) {
        let late = 6 * FRAME + Duration::from_micros(49_300);
        let mut steady: Vec<_> = (0..=8).map(|k| k * FRAME).collect();
        let after = [late, late + FRAME, late + 2 * FRAME];
        steady.extend(after);
        let mut hitched: Vec<_> = (0..=6).map(|k| k * FRAME).collect();
        hitched.extend(after);
        let steady = widths(&steady, Duration::MAX, |_| {});
        let hitched = widths(&hitched, late, before);
        for t in after {
            let expected = steady.iter().find(|w| w.0 == t).unwrap().1;
            let got = hitched.iter().find(|w| w.0 == t).unwrap().1;
            assert!(expected < 211.0, "the spring must still be moving at {t:?}: {expected}");
            assert!((got - expected).abs() < 0.01, "{what}: at {t:?} width={got}, unbroken cadence={expected}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_fading_out_overlay_draws_its_exit_until_the_fade_ends() {
        let mut overlay = width_overlay("fade out");
        overlay.visible = true;
        overlay.rt.set_live_visible(true);
        let start = Instant::now();
        present(&mut overlay, start);
        // The handler that fades the overlay out starts its exit in the same turn.
        overlay.rt.update(|width, _| *width = 212.0);
        overlay.dirty = true;
        overlay.hide_fading(Duration::from_millis(150));
        assert!(!overlay.visible, "input stops at once");
        assert!(overlay.drawing() && !overlay.frameless(), "frames keep coming during the fade");
        assert!(overlay.rt.live_visible());
        overlay.set_occluded(true);
        assert!(!overlay.occluded, "a panel fading toward alpha zero isn't occluded");
        let end = overlay.fading_out.expect("fading out");
        assert!(overlay.wake_deadline().is_some_and(|due| due <= end), "the loop wakes to end the fade");
        let mut last = width(&overlay, "box");
        for k in 1..=6 {
            present(&mut overlay, start + k * FRAME);
            let now = width(&overlay, "box");
            assert!(now > last, "frame {k}: the exit moves ({last} -> {now})");
            last = now;
        }
        assert!(!overlay.idle);
        overlay.apply_wake(true, Vec::new());
        assert!(overlay.dirty && !overlay.idle, "a wake mid-fade waits for the next drawn frame");
        overlay.wake_due(end);
        assert!(!overlay.drawing() && overlay.frameless(), "drawing stops when the fade ends");
        assert!(!overlay.rt.live_visible());
        assert_eq!(overlay.wake_deadline(), None);
    }

    #[test]
    fn hiding_without_a_fade_stops_drawing_at_once() {
        let mut overlay = width_overlay("hide");
        overlay.visible = true;
        overlay.rt.set_live_visible(true);
        overlay.hide_fading(Duration::ZERO);
        assert!(!overlay.drawing() && overlay.frameless() && !overlay.rt.live_visible());
        overlay.hide_fading(Duration::from_millis(150));
        assert!(!overlay.drawing(), "fading out an overlay that's already hidden draws nothing");
    }

    #[test]
    fn a_poll_asking_for_the_next_frame_keeps_a_shown_overlay_stepping() {
        fn view(_: &u32, _: &mut Cx<u32>) -> El { crate::el().key("box").size(10.0, 10.0) }
        // Steps its own animation from the frame clock, as a port of a display-link loop does.
        fn poll(steps: &mut u32, _: f64, tx: &mut Tx) -> bool {
            *steps += 1;
            if *steps < 4 { tx.wake_after(Duration::ZERO); }
            true
        }
        let app = AppSpec { poll: Some(poll), ..AppSpec::new("frame poll", (100.0, 50.0), || 0, view) };
        let (mut overlay, _) = Overlay::new(OverlaySpec::new("frame poll", app.size, view).options(Default::default()), 0, &app);
        overlay.visible = true;
        let start = Instant::now();
        let idle: Vec<_> = (1..=5).map(|k| {
            present(&mut overlay, start + k * FRAME);
            overlay.idle
        }).collect();
        assert_eq!(overlay.rt.state, 5);
        assert_eq!(idle, [false, false, false, true, true], "the display link runs while the poll asks for frames, and stops after");
    }

    #[test]
    fn a_hidden_overlay_takes_a_theme_change_and_redraws_before_reveal() {
        let mut overlay = width_overlay("theme");
        present(&mut overlay, Instant::now());
        assert!(!overlay.visible && !overlay.dirty);
        let dark = !overlay.rt.env.system_dark;
        overlay.theme_changed(dark);
        assert_eq!(overlay.rt.env.system_dark, dark);
        assert!(overlay.dirty, "a retained surface redraws in the new theme before it's revealed");
        assert_eq!(overlay.dark, dark, "show reapplies this theme");
    }

    #[test]
    fn overlay_late_frame_lands_where_an_unbroken_cadence_would() {
        assert_late_frame_keeps_time(|_| {}, "late frame");
    }

    #[test]
    fn overlay_idle_flag_left_set_mid_motion_cannot_shrink_the_step() {
        // The motion trace's case: something marks the host idle while a spring is in flight.
        assert_late_frame_keeps_time(|overlay| overlay.idle = true, "idle flag left set");
        // The source on macOS: an unoccluded notice with no occlusion before it.
        #[cfg(target_os = "macos")]
        assert_late_frame_keeps_time(|overlay| overlay.occluded(false), "Occluded(false) mid-motion");
    }

    #[test]
    fn overlay_motion_starting_after_idle_takes_one_frame_step() {
        let first_pose = |idle: Duration| {
            let mut overlay = width_overlay("after idle");
            let start = Instant::now();
            present(&mut overlay, start);
            present(&mut overlay, start + FRAME);
            assert!(overlay.idle);
            overlay.rt.update(|width, _| *width = 212.0);
            present(&mut overlay, start + FRAME + idle);
            width(&overlay, "box")
        };
        let short = first_pose(FRAME);
        let long = first_pose(Duration::from_secs(2));
        assert!(short > 12.0 && short < 20.0, "the first frame shows one step of motion: {short}");
        assert!((long - short).abs() < 0.001, "idle time must not age motion that starts after it: {long} vs {short}");
    }

    #[test]
    fn overlay_resume_keeps_running_motion_and_starts_new_motion_fresh() {
        #[derive(Clone, Copy)]
        struct Pair { a: f32, b: f32 }
        fn view(s: &Pair, _: &mut Cx<Pair>) -> El {
            crate::el().children([crate::el().key("a").size(s.a, 8.0), crate::el().key("b").size(s.b, 8.0)])
        }
        let app = AppSpec::new("resume", (500.0, 50.0), || Pair { a: 12.0, b: 12.0 }, view);
        // `a` springs from FRAME; `pause` runs after 3 frames, and `b` starts on the frame
        // presented `gap` later.
        let run_with = |options: OverlayOptions, pause: fn(&mut Overlay<Pair>), resume: fn(&mut Overlay<Pair>), gap: Duration| {
            let (mut overlay, _) = Overlay::new(OverlaySpec::new("resume", app.size, view).options(options), Pair { a: 12.0, b: 12.0 }, &app);
            overlay.rt.wall_start = None;
            let start = Instant::now();
            present(&mut overlay, start);
            overlay.rt.update(|s, _| s.a = 212.0);
            for k in 1..=3 { present(&mut overlay, start + k * FRAME); }
            pause(&mut overlay);
            overlay.rt.update(|s, _| s.b = 212.0);
            resume(&mut overlay);
            present(&mut overlay, start + 3 * FRAME + gap);
            (width(&overlay, "a"), width(&overlay, "b"))
        };
        let run = |pause: fn(&mut Overlay<Pair>), resume: fn(&mut Overlay<Pair>), gap: Duration| run_with(Default::default(), pause, resume, gap);
        let fresh = {
            let mut overlay = width_overlay("fresh");
            let start = Instant::now();
            present(&mut overlay, start);
            overlay.rt.update(|width, _| *width = 212.0);
            present(&mut overlay, start + FRAME);
            width(&overlay, "box")
        };
        // Running motion keeps real time up to the resume cap.
        let (capped, _) = run(|_| {}, |_| {}, Duration::from_secs_f64(crate::frame_clock::MAX_GAP));
        type Case = (&'static str, fn(&mut Overlay<Pair>), fn(&mut Overlay<Pair>));
        let hidden: Case = ("hidden", |o| o.hide(), |o| o.visible = true);
        #[cfg(target_os = "macos")]
        let cases: [Case; 2] = [hidden, ("occluded", |o| o.occluded(true), |o| o.occluded(false))];
        #[cfg(not(target_os = "macos"))]
        let cases: [Case; 1] = [hidden];
        for (what, pause, resume) in cases {
            let (a, b) = run(pause, resume, Duration::from_secs(3));
            // `b` sits beside `a`, so its edges snap to a different 1/64 pt grid than `fresh`.
            assert!((b - fresh).abs() < 0.02, "{what}: motion starting on the resume frame must not jump: b={b}, fresh={fresh}");
            assert!(capped < 211.0 && (a - capped).abs() < 0.01, "{what}: running motion resumes {:?} along: a={a}, expected={capped}", crate::frame_clock::MAX_GAP);
        }
        // A missed frame while covered or asleep polls but commits nothing, retained or not: `b`
        // starts on the first uncovered frame, as above.
        #[cfg(target_os = "macos")]
        for prewarm in [false, true] {
            let options = OverlayOptions { prewarm, ..Default::default() };
            let (a, b) = run_with(options, |o| { o.visible = true; o.occluded(true); },
                |o| { o.missed(crate::window::Missed::Occluded, false); o.occluded(false); }, Duration::from_secs(3));
            assert!((b - fresh).abs() < 0.02, "missed while occluded (prewarm: {prewarm}): b={b}, fresh={fresh}");
            assert!((a - capped).abs() < 0.01, "missed while occluded (prewarm: {prewarm}): a={a}, expected={capped}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn overlay_hidden_commit_keeps_elapsed_motion_on_later_reveal() {
        use std::time::Duration;
        struct State { width: f32, enter: bool, live: crate::LiveText }
        fn init() -> State { State { width: 12.0, enter: false, live: crate::LiveText::new("00") } }
        fn view(s: &State, _: &mut Cx<State>) -> El {
            crate::el().children([
                crate::el().key("box").size(s.width, 8.0),
                crate::text(if s.width == 12.0 { "A" } else { "B" }).key("content"),
                crate::text("00").key("live").live_text(&s.live),
            ])
        }
        fn poll(s: &mut State, _: f64, _: &mut Tx) -> bool {
            if !std::mem::take(&mut s.enter) { return false; }
            s.width = 20.0;
            true
        }
        let app = AppSpec { poll: Some(poll), ..AppSpec::new("delayed entry", (100.0, 50.0), init, view) };
        for delay in [Duration::from_millis(16), Duration::from_secs(2)] {
            let spec = OverlaySpec::new("delayed entry", app.size, view)
                .options(OverlayOptions { prewarm: true, ..Default::default() });
            let (mut overlay, handle) = Overlay::new(spec, init(), &app);
            overlay.rt.wall_start = None;
            overlay.warm();
            overlay.dirty = false; // Successful submission of A, without a native window.
            let commit = overlay.clock.last() + Duration::from_secs(10);
            overlay.rt.state.enter = true;
            overlay.advance_at(commit); // Hidden turn 1 commits B; no show request yet.
            assert_eq!(handle.shared.requests.lock().unwrap().visible, None);
            assert_eq!(overlay.rt.presented("box").unwrap().0[2], 12.0, "idle before B must not age B");
            assert!(!overlay.draw(), "hidden motion must not drive frames");
            assert_eq!(overlay.wake_deadline(), None);
            overlay.rt.state.live.set("01");
            handle.show(); // Turn 2, after B has been committed.
            overlay.resume_live();
            overlay.settle_before_reveal();
            overlay.refresh_for_reveal_at(commit + delay);
            let width = overlay.rt.presented("box").unwrap().0[2];
            if delay.as_secs() == 0 {
                assert!(overlay.rt.animating && width > 12.0 && width < 14.0,
                    "16ms reveal must be near A and moving: width={width}, moving={}", overlay.rt.animating);
            } else {
                assert_eq!(width, 20.0, "2s reveal must have reached B");
                assert!(!overlay.rt.animating);
            }
            let elements = overlay.rt.elements();
            assert_eq!(elements.iter().find(|e| e.key.as_deref() == Some("content")).unwrap().text.as_deref(), Some("B"));
            assert_eq!(elements.iter().find(|e| e.key.as_deref() == Some("live")).unwrap().text.as_deref(), Some("01"));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn overlay_hidden_retarget_samples_old_motion_before_committing_new_target() {
        use std::time::Duration;
        fn view(width: &f32, _: &mut Cx<f32>) -> El {
            crate::el().child(crate::el().key("box").size(*width, 8.0))
        }
        let app = AppSpec::new("retarget", (100.0, 50.0), || 12.0, view);
        for (scale, millis) in [(0.0, 16), (0.25, 16), (1.0, 16), (0.0, 2000), (0.25, 2000), (1.0, 2000)] {
            let delay = Duration::from_millis(millis);
            let spec = OverlaySpec::new("retarget", app.size, view)
                .options(OverlayOptions { prewarm: true, ..Default::default() });
            let (mut overlay, _) = Overlay::new(spec, 12.0, &app);
            overlay.rt.wall_start = None;
            overlay.rt.time_scale = scale;
            overlay.warm();
            overlay.dirty = false;
            let commit = overlay.clock.last() + Duration::from_secs(10);
            overlay.rt.update(|width, _| *width = 20.0);
            overlay.advance_at(commit);
            overlay.advance_at(commit + Duration::from_millis(8)); // Unchanged intervening poll.
            overlay.rt.update(|width, _| *width = 30.0); // Newly observed at reveal, not 2s ago.
            overlay.refresh_for_reveal_at(commit + delay);

            // A visible reference ages B first and only then commits C. Compare the next
            // moving frame too: retargeting must preserve velocity, not just position.
            let mut reference = Runtime::new(12.0, view, app.size, Env::default());
            reference.time_scale = scale;
            reference.frame(0.0);
            reference.update(|width, _| *width = 20.0);
            reference.frame(0.0);
            reference.frame(delay.as_secs_f64());
            reference.update(|width, _| *width = 30.0);
            reference.frame(0.0);
            for dt in [0.0, 0.016] {
                overlay.rt.frame(dt);
                reference.frame(dt);
                let width = overlay.rt.presented("box").unwrap().0[2];
                let expected = reference.presented("box").unwrap().0[2];
                assert!((width - expected).abs() < 0.001, "scale={scale}: width={width}, expected={expected}");
                assert!(width < 30.0, "the fresh target must not inherit the hidden gap");
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn overlay_hidden_poll_can_show_and_animate_from_the_cached_pose() {
        #[derive(Default)]
        struct State { width: f32, enter: bool, instant: bool, handle: Option<OverlayHandle<State>> }
        fn view(s: &State, _: &mut Cx<State>) -> El {
            crate::el().child(crate::el().key("box").size(s.width, 8.0))
        }
        fn poll(s: &mut State, _: f64, tx: &mut Tx) -> bool {
            if !std::mem::take(&mut s.enter) { return false; }
            s.width = 20.0;
            if s.instant { tx.instant(); }
            s.handle.as_ref().unwrap().show();
            true
        }
        let app = AppSpec { poll: Some(poll), ..AppSpec::new("entry", (100.0, 50.0), State::default, view) };
        for (prewarm, stale, instant, queued) in [
            (true, false, false, false), (true, true, false, false),
            (false, false, false, false), (true, false, true, false),
            (true, false, false, true),
        ] {
            let spec = OverlaySpec::new("entry", app.size, view)
                .options(OverlayOptions { prewarm, ..Default::default() });
            let (mut overlay, handle) = Overlay::new(spec, State { width: 12.0, instant, ..Default::default() }, &app);
            overlay.rt.state.handle = Some(handle.clone());
            overlay.warm();
            // No native window in this test: model successful submission of the warmed pose.
            overlay.dirty = false;
            assert_eq!(overlay.rt.presented("box").unwrap().0[2], 12.0);
            overlay.advance();
            assert!(!overlay.dirty, "an unchanged hidden poll preserves the prepared frame");
            if stale { overlay.apply_wake(false, vec![Box::new(|s, _| s.width = 16.0)]); }
            overlay.rt.state.enter = true; // External event observed by the next hidden poll.
            if queued {
                // Scrawl's original kick: a no-op update invalidates the prepared frame
                // before the hidden poll commits B and requests show.
                handle.update(|_, _| {});
                let updates = std::mem::take(&mut handle.shared.requests.lock().unwrap().updates);
                overlay.apply_wake(false, updates);
                assert!(overlay.dirty);
            } else {
                overlay.advance();
            }
            assert_eq!(handle.shared.requests.lock().unwrap().visible, Some(true));
            overlay.settle_before_reveal();
            overlay.refresh_for_reveal_at(Instant::now());
            if queued {
                assert!(overlay.rt.animating && overlay.rt.presented("box").unwrap().0[2] < 20.0,
                    "a queued update kick must not settle the entry during reveal");
            }
            if prewarm && !stale { assert!(!overlay.idle, "the new target needs a visible frame even with instant motion"); }
            let moving = overlay.rt.frame(1.0 / 60.0);
            let width = overlay.rt.presented("box").unwrap().0[2];
            if prewarm && !instant {
                assert!(moving && (12.0..20.0).contains(&width),
                    "same-poll show must animate from A: width={width}, moving={moving}");
            } else {
                assert_eq!(width, 20.0, "instant changes and unretained frames land at their target");
                assert!(!moving);
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn overlay_cached_reveal_does_not_settle_a_same_tick_update() {
        fn view(width: &f32, _: &mut Cx<f32>) -> El {
            crate::el().child(crate::el().key("box").size(*width, 8.0))
        }
        let app = AppSpec::new("reveal", (100.0, 50.0), || 8.0, view);
        for prewarm in [true, false] {
            let spec = OverlaySpec::new("reveal", app.size, view)
                .options(OverlayOptions { prewarm, ..Default::default() });
            let (mut overlay, _) = Overlay::new(spec, 8.0, &app);
            overlay.rt.frame(0.0);
            overlay.rt.update(|width, _| *width = 12.0);
            overlay.warm();
            assert_eq!(overlay.rt.presented("box").unwrap().0[2], 12.0, "hidden changes settle in prewarm");
            overlay.settle_before_reveal();
            // A show and update can arrive in one event-loop turn, after the cached frame.
            overlay.rt.update(|width, _| *width = 20.0);
            let moving = overlay.rt.frame(1.0 / 60.0);
            let width = overlay.rt.presented("box").unwrap().0[2];
            if prewarm {
                assert!(moving && width < 20.0, "cached reveal erased new motion: width={width}, moving={moving}");
            } else { assert_eq!(width, 20.0, "ordinary show retains its settled first-frame contract"); }
        }
    }

    #[test]
    fn window_panel_overlay_focus_callback_is_connected_before_first_show() {
        fn view(_: &Vec<bool>, _: &mut Cx<Vec<bool>>) -> El { crate::el() }
        let app = AppSpec::new("focus", (100.0, 100.0), Vec::new, view);
        let spec = OverlaySpec::new("focus", app.size, view).options(Default::default())
            .on_focus(|s, focused, tx| { s.push(focused); if !focused { tx.close_window(); } });
        let (mut overlay, _) = Overlay::new(spec, Vec::new(), &app);
        overlay.rt.window_focus(true);
        overlay.rt.window_focus(false);
        assert_eq!(overlay.rt.state, [true, false]);
        assert_eq!(overlay.rt.shell_request, Some(crate::el::Shell::Close));
        assert!(overlay.live.is_none());
    }

    #[test]
    fn overlay_live_values_resume_before_retained_reveal_and_stop_on_hide() {
        use std::sync::atomic::AtomicUsize;
        fn init() -> crate::LiveText { crate::LiveText::new("00") }
        fn view(value: &crate::LiveText, _: &mut Cx<crate::LiveText>) -> El {
            crate::text("00").live_text(value)
        }
        let app = AppSpec::new("live overlay", (80.0, 40.0), init, view);
        let spec = OverlaySpec::new("live overlay", app.size, view).options(OverlayOptions {
            prewarm: true, keys: KeyPolicy::OnDemand, ..Default::default()
        });
        let value = init();
        let (mut overlay, _) = Overlay::new(spec, value.clone(), &app);
        let wakes = Arc::new(AtomicUsize::new(0));
        let count = wakes.clone();
        overlay.rt.set_live_waker(Arc::new(move || { count.fetch_add(1, Ordering::Relaxed); }));
        overlay.rt.frame(0.0);
        value.set("01");
        assert_eq!(wakes.load(Ordering::Relaxed), 0, "prewarming isn't visibility");
        overlay.dirty = false;
        overlay.resume_live();
        assert!(overlay.dirty, "the retained surface must redraw before reveal");
        overlay.rt.frame(0.0);
        assert_eq!(overlay.rt.elements()[0].text.as_deref(), Some("01"));
        overlay.hide();
        let before = wakes.load(Ordering::Relaxed);
        value.set("02");
        assert_eq!(wakes.load(Ordering::Relaxed), before);
        overlay.dirty = false;
        overlay.resume_live();
        assert!(overlay.dirty);
        overlay.rt.frame(0.0);
        assert_eq!(overlay.rt.elements()[0].text.as_deref(), Some("02"));
    }

    #[test]
    fn overlay_input_switch_waits_for_owned_drag_release() {
        fn view(_: &Vec<crate::Phase>, _: &mut Cx<Vec<crate::Phase>>) -> El {
            crate::el().w_full().h_full().hit().on_drag(|phases: &mut Vec<crate::Phase>, drag, _| phases.push(drag.phase))
        }
        let app = AppSpec {
            title: "input switch", size: (100.0, 100.0), min_size: None, init: Vec::new, view,
            scenes: &[], fonts: &[], families: ("sans-serif", "monospace"), setup: None, poll: None, on_key: None,
        };
        let spec = OverlaySpec::new("input switch", app.size, view).options(OverlayOptions { input: OverlayInput::All, ..Default::default() });
        let (mut overlay, handle) = Overlay::new(spec, Vec::new(), &app);
        overlay.rt.frame(0.0);
        overlay.rt.pointer_down(10.0, 10.0);
        handle.set_input(OverlayInput::Regions);
        handle.set_input(OverlayInput::None);
        overlay.pending_input = overlay.shared.requests.lock().unwrap().input.take();
        overlay.apply_input();
        assert_eq!(overlay.options.input, OverlayInput::All, "an owned gesture retains its old native mode");
        overlay.rt.pointer_move(150.0, 150.0);
        overlay.rt.pointer_up(150.0, 150.0);
        overlay.apply_input();
        overlay.rt.frame(0.0);
        assert_eq!(overlay.options.input, OverlayInput::None);
        assert_eq!(overlay.rt.state.first(), Some(&crate::Phase::Down));
        assert_eq!(overlay.rt.state.last(), Some(&crate::Phase::Up), "release is delivered even outside the canvas");
        assert!(!overlay.rt.pointer_captured());
    }

    #[test]
    fn handle_requests_collapse_and_updates_keep_their_order() {
        let shared = Arc::new(Shared::<u32> { native: Mutex::default(), requests: Mutex::default(), availability: AtomicU8::new(0), wake: Mutex::new(None) });
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
        let app = AppSpec { families: ("sans-serif", "monospace"), ..AppSpec::new("timer test", (20.0, 20.0), Vec::<f64>::new, view) };
        let spec = OverlaySpec {
            name: "timer test", size: (20.0, 20.0), edge: ScreenEdge::Top,
            margin: 0.0, align: Align::Center, inset: 0.0, interactive: false, view,
        };
        let (mut overlay, handle) = Overlay::new(spec.options(Default::default()).poll(|seen, now, _| { seen.push(now); false }), Vec::new(), &app);
        // The runtime's virtual clock makes scheduling exact; the host maps it to wall time.
        overlay.rt.wall_start = None;
        handle.update(|_, tx| tx.wake_after(Duration::from_millis(250)));
        let updates = std::mem::take(&mut overlay.shared.requests.lock().unwrap().updates);
        overlay.apply_wake(false, updates);
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

    #[test]
    fn overlay_poll_inherits_overrides_and_wakes_visible_state_once() {
        use std::time::Duration;
        #[derive(Default)]
        struct State { polls: usize, value: f32 }
        fn view(s: &State, _: &mut Cx<State>) -> El {
            crate::el().child(crate::el().key("value").size(10.0 + s.value, 10.0))
        }
        fn poll(s: &mut State, _: f64, tx: &mut Tx) -> bool {
            s.polls += 1;
            if s.polls == 1 { tx.wake_after(Duration::from_millis(250)); return false; }
            if s.polls == 2 { s.value = 7.0; tx.instant(); return true; }
            false
        }
        let app = AppSpec { poll: Some(poll), ..AppSpec::new("poll", (30.0, 20.0), State::default, view) };
        let (mut overlay, _) = Overlay::new(OverlaySpec::new("poll", app.size, view), State::default(), &app);
        overlay.visible = true;
        overlay.rt.wall_start = None;
        assert!(!overlay.rt.frame(0.0));
        assert_eq!(overlay.rt.state.polls, 1);
        assert_eq!(overlay.rt.wake_deadline(), Some(0.25), "false still registers wake_after");
        overlay.dirty = false;
        let start = Instant::now() - Duration::from_millis(251);
        overlay.rt.wall_start = Some(start);
        overlay.wake_due(start + Duration::from_millis(249));
        assert!(!overlay.dirty);
        overlay.wake_due(Instant::now());
        assert!(overlay.dirty, "visible wake schedules the frame");
        assert_eq!(overlay.rt.state.polls, 1, "do not poll twice: the drawable frame owns the poll");
        assert!(!overlay.rt.frame(0.0), "true means changed state, not continuous animation");
        assert_eq!(overlay.rt.state.polls, 2);
        assert_eq!(overlay.rt.presented("value").unwrap().0[2], 17.0);
        assert_eq!(overlay.wake_deadline(), None);
        overlay.dirty = false;
        overlay.wake_due(Instant::now());
        assert!(!overlay.dirty, "no timer or animation means no more scheduled frames");

        let configured = OverlaySpec::new("override", app.size, view).options(Default::default())
            .poll(|s, _, _| { s.polls += 10; false });
        let (mut other, _) = Overlay::new(configured, State::default(), &app);
        other.rt.frame(0.0);
        assert_eq!(other.rt.state.polls, 10, "builder overrides inherited poll");
        assert_eq!(other.wake_deadline(), None);

        fn other_view(_: &u32, _: &mut Cx<u32>) -> El { crate::el() }
        let (mut different, _) = Overlay::new(OverlaySpec::new("different", app.size, other_view), 0_u32, &app);
        assert!(different.rt.poll.is_none(), "never apply a poll to a different state type");
        different.rt.frame(0.0);
        assert_eq!(different.rt.state, 0);
    }

    /// Covered, or shown while the displays sleep: no frame comes, yet `wake_after` still fires
    /// and can hide the overlay, and a missed frame waits instead of polling.
    #[cfg(target_os = "macos")]
    #[test]
    fn overlay_occluded_runs_due_timers_without_frames() {
        use crate::window::{Missed, OCCLUDED_MAX, OCCLUDED_RETRY};
        #[derive(Default)]
        struct State { polls: usize, handle: Option<OverlayHandle<State>> }
        fn view(_: &State, _: &mut Cx<State>) -> El { crate::el() }
        fn poll(s: &mut State, _: f64, tx: &mut Tx) -> bool {
            s.polls += 1;
            if s.polls == 1 { tx.wake_after(Duration::from_millis(250)); } else { s.handle.as_ref().unwrap().hide(); }
            false
        }
        let app = AppSpec { poll: Some(poll), ..AppSpec::new("occluded", (30.0, 20.0), State::default, view) };
        let (mut overlay, handle) = Overlay::new(OverlaySpec::new("occluded", app.size, view), State::default(), &app);
        overlay.rt.state.handle = Some(handle.clone());
        overlay.visible = true;
        overlay.rt.wall_start = None;
        // Shown onto sleeping displays: the first frame misses, and still polls.
        overlay.occluded(true);
        overlay.missed(Missed::Occluded, false);
        assert_eq!(overlay.rt.state.polls, 1);
        assert_eq!(overlay.rt.wake_deadline(), Some(0.25));
        overlay.retry = None;
        overlay.occluded_wait = OCCLUDED_RETRY;
        let start = Instant::now() - Duration::from_millis(251);
        overlay.rt.wall_start = Some(start);
        assert!(overlay.wake_deadline().is_some_and(|at| at <= Instant::now()), "the timer stays armed while occluded");
        overlay.dirty = false;
        overlay.wake_due(Instant::now());
        assert_eq!(overlay.rt.state.polls, 2, "the due timer runs without waiting for a drawable");
        assert_eq!(handle.shared.requests.lock().unwrap().visible, Some(false), "and its hide() goes through");
        assert_eq!(overlay.wake_deadline(), None);

        // Each missed frame waits, longer while it stays occluded; a timeout waits for the
        // display link that is still running.
        let mut waits = Vec::new();
        for _ in 0..6 {
            let before = Instant::now();
            overlay.missed(Missed::Occluded, false);
            let retry = overlay.retry.expect("an occluded miss re-checks later");
            assert_eq!(overlay.wake_deadline(), Some(retry));
            waits.push(retry - before);
        }
        assert!(waits[0] >= OCCLUDED_RETRY && waits.windows(2).all(|w| w[1] + Duration::from_millis(1) >= w[0]), "{waits:?}");
        assert!(waits[5] >= OCCLUDED_MAX && waits.iter().all(|&w| w <= OCCLUDED_MAX + Duration::from_millis(50)), "{waits:?}");
        overlay.missed(Missed::Timeout, true);
        assert_eq!(overlay.retry, None);
        overlay.occluded(false);
        assert!(overlay.dirty, "uncovered, it redraws once");
        assert_eq!(overlay.occluded_wait, OCCLUDED_RETRY);

        // An update arriving while it is covered arms its timer at once, not on a later retry.
        overlay.occluded(true);
        overlay.apply_wake(false, vec![Box::new(|_: &mut State, tx: &mut Tx| tx.wake_after(Duration::from_secs(5)))]);
        let deadline = overlay.wake_deadline().expect("the update's timer is armed while occluded");
        assert!(deadline > Instant::now() + Duration::from_secs(4), "{:?}", deadline - Instant::now());
    }

    #[test]
    fn overlay_setup_runs_once_with_a_waker_that_redraws_the_overlay() {
        use std::sync::atomic::AtomicUsize;
        use std::time::Duration;
        #[derive(Default)]
        struct State { setups: usize, polls: usize, cache: Option<crate::image::ImageCache>, icon: Option<crate::image::Image>, saw_icon: std::cell::Cell<bool> }
        fn view(s: &State, _: &mut Cx<State>) -> El {
            s.saw_icon.set(s.icon.as_ref().is_some_and(|i| i.is_ready()));
            crate::el()
        }
        let app = AppSpec { families: ("sans-serif", "monospace"), ..AppSpec::new("setup", (30.0, 20.0), State::default, view) };
        let configured = OverlaySpec::new("setup", app.size, view).options(Default::default())
            .setup(|s, waker| { s.setups += 1; s.cache = Some(crate::image::ImageCache::new(waker)); })
            .poll(|s, _, _| { s.polls += 1; false });
        let (mut overlay, _) = Overlay::new(configured, State::default(), &app);
        overlay.rt.wall_start = None;
        assert_eq!(overlay.rt.state.setups, 0, "not before the event loop");
        let loop_wakes = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = std::sync::mpsc::channel();
        let counter = loop_wakes.clone();
        let wake: Waker = Arc::new(move || { counter.fetch_add(1, Ordering::SeqCst); let _ = tx.send(()); });
        overlay.connect(wake.clone(), Arc::new(|| {}), Rc::default());
        overlay.connect(wake, Arc::new(|| {}), Rc::default());
        assert_eq!(overlay.rt.state.setups, 1, "setup runs once");
        while rx.try_recv().is_ok() {}
        let take = |overlay: &mut Overlay<State>| std::mem::take(&mut *overlay.shared.requests.lock().unwrap());
        let _ = take(&mut overlay);

        // Hidden: the overlay has laid out without the icon; a background decode wakes the
        // loop, and the overlay re-runs the view in a frame without drawing.
        overlay.advance();
        assert!(!overlay.rt.state.saw_icon.get());
        let state = &mut overlay.rt.state;
        state.icon = Some(state.cache.as_ref().unwrap().rgba(1, 1, vec![20, 40, 60, 255]));
        rx.recv_timeout(Duration::from_secs(5)).expect("the decode wakes the event loop");
        let requests = take(&mut overlay);
        assert!(requests.woken);
        overlay.apply_wake(requests.woken, requests.updates);
        assert!(overlay.rt.state.saw_icon.get(), "the view saw the decoded image");
        let polls = overlay.rt.state.polls;

        // Shown: the next wake schedules a frame that re-runs the view.
        overlay.visible = true;
        overlay.rt.state.icon = None;
        overlay.rt.invalidate();
        overlay.advance();
        assert!(!overlay.rt.state.saw_icon.get());
        overlay.dirty = false;
        overlay.idle = true;
        let polls_shown = overlay.rt.state.polls;
        let state = &mut overlay.rt.state;
        state.icon = Some(state.cache.as_ref().unwrap().rgba(1, 1, vec![60, 40, 20, 255]));
        rx.recv_timeout(Duration::from_secs(5)).expect("the decode wakes the event loop");
        let requests = take(&mut overlay);
        overlay.apply_wake(requests.woken, requests.updates);
        assert!(overlay.dirty, "a shown overlay draws a frame");
        assert_eq!(overlay.rt.state.polls, polls_shown, "the drawn frame owns the poll");
        overlay.advance(); // the drawn frame
        assert!(overlay.rt.state.saw_icon.get(), "the view saw the decoded image");
        assert!(polls >= 1);

        // Wakes before the loop drains them coalesce into one loop wake.
        while rx.try_recv().is_ok() {}
        let before = loop_wakes.load(Ordering::SeqCst);
        let state = &mut overlay.rt.state;
        let cache = state.cache.as_ref().unwrap();
        let _a = cache.rgba(1, 1, vec![1, 2, 3, 255]);
        let _b = cache.rgba(1, 1, vec![4, 5, 6, 255]);
        rx.recv_timeout(Duration::from_secs(5)).expect("the decodes wake the event loop");
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(loop_wakes.load(Ordering::SeqCst), before + 1, "one loop wake until the overlay takes it");
        assert!(take(&mut overlay).woken);
        assert!(loop_wakes.load(Ordering::SeqCst) >= 2);
    }

    #[test]
    fn overlay_compositor_observes_prewarm_and_each_rendered_frame() {
        use crate::render::{Offscreen, OFFSCREEN_FORMAT};
        struct Trace(Rc<RefCell<Vec<f32>>>);
        impl Compositor<f32> for Trace {
            fn render(&mut self, f: &mut Frame<'_, f32>) {
                f.draw_ui();
                let element = f.rt.elements().into_iter().find(|e| e.key.as_deref() == Some("box")).unwrap();
                self.0.borrow_mut().push(element.rect[2]);
            }
            fn animating(&self) -> bool { self.0.borrow().len() < 3 }
        }
        fn view(width: &f32, _: &mut Cx<f32>) -> El {
            crate::el().size(32.0, 16.0).child(crate::el().key("box").size(*width, 8.0).fill(crate::Color::WHITE))
        }
        let app = AppSpec::new("trace", (32.0, 16.0), || 8.0, view);
        let trace = Rc::new(RefCell::new(Vec::new()));
        let spec = OverlaySpec::new("trace", app.size, view).options(OverlayOptions { prewarm: true, ..Default::default() })
            .compositor(Trace(trace.clone()));
        let (mut overlay, _) = Overlay::new(spec, 8.0, &app);
        let gpu = Arc::new(Gpu::headless());
        overlay.renderer = Some((Renderer::new(&gpu.device, OFFSCREEN_FORMAT), OFFSCREEN_FORMAT));
        overlay.gpu = Some(gpu.clone());
        overlay.rt.transparent = true;
        overlay.rt.frame(0.0);
        overlay.warm_offscreen((32, 16));
        assert_eq!(*trace.borrow(), [8.0], "offscreen prewarm must use the compositor too");
        assert!(!overlay.visible);

        let target = Offscreen::new(&gpu.device, 32, 16);
        for (width, more) in [(12.0, true), (20.0, false)] {
            overlay.rt.update(|s, tx| { *s = width; tx.instant(); });
            overlay.rt.frame(1.0 / 60.0);
            let renderer = &mut overlay.renderer.as_mut().unwrap().0;
            let mut enc = gpu.device.create_command_encoder(&Default::default());
            let animating = render_frame(&mut Frame {
                device: &gpu.device, queue: &gpu.queue, encoder: &mut enc, target: &target.view,
                format: OFFSCREEN_FORMAT, size: (32, 16), dt: 1.0 / 60.0, update_ms: 0.0,
                rt: &mut overlay.rt, ui: renderer,
            }, &mut overlay.compositor);
            assert_eq!(animating, more, "honour compositor-driven animation and stop when it settles");
            target.copy(&mut enc);
            renderer.submit(&gpu.queue, enc);
            let pixels = target.read(&gpu.device);
            assert_eq!(&pixels[4..8], &[255; 4], "draw_ui renders the overlay content");
            assert_eq!(&pixels[(15*32+31)*4..(15*32+32)*4], &[0; 4], "draw_ui preserves the transparent clear");
        }
        assert_eq!(*trace.borrow(), [8.0, 12.0, 20.0], "one observation per rendered frame, with current geometry");
    }

    #[derive(Default)]
    struct Drags { starts: u32, ended: Vec<crate::DragOutcome> }

    fn drag_overlay(name: &'static str) -> Overlay<Drags> {
        fn view(_: &Drags, _: &mut Cx<Drags>) -> El {
            crate::el().child(crate::el().key("tile").size(100.0, 40.0)
                .drag_out(|_: &Drags| Some(crate::DragItem::new().text("tile")))
                .on_drag_start(|s: &mut Drags, _| s.starts += 1)
                .on_drag_end(|s: &mut Drags, outcome, _| s.ended.push(outcome)))
        }
        let app = AppSpec::new(name, (200.0, 50.0), Drags::default, view);
        let (mut overlay, _) = Overlay::new(OverlaySpec::new(name, app.size, view).options(Default::default()), Drags::default(), &app);
        overlay.rt.wall_start = None;
        present(&mut overlay, Instant::now());
        overlay
    }

    #[test]
    fn a_release_past_the_threshold_does_not_start_a_drag_out() {
        let mut overlay = drag_overlay("flick");
        overlay.rt.pointer_move(10.0, 10.0);
        overlay.left_button(true, None);
        overlay.left_button(false, Some((60.0, 10.0)));
        assert!(overlay.rt.take_drag_out().is_none());
        present(&mut overlay, Instant::now());
        assert_eq!(overlay.rt.state.starts, 0);
    }

    #[test]
    fn a_hidden_overlay_still_runs_on_drag_end() {
        let mut overlay = drag_overlay("hidden drag end");
        assert!(!overlay.visible);
        overlay.rt.drag_end_sink().borrow_mut().push(("#0/tile".into(), crate::DragOutcome::Copied));
        overlay.drag_ends_due();
        assert_eq!(overlay.rt.state.ended, [crate::DragOutcome::Copied]);
    }
}

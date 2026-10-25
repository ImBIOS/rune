//! One event loop, independent window lifetime hosts. App/AppHandle and multi-window
//! factories share the same input, rendering, activation and close/reopen paths.

use std::sync::{Arc, Mutex, Weak, atomic::{AtomicBool, AtomicU32, Ordering}};
use std::{cell::RefCell, rc::Rc};
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalPosition};
use winit::event::{ElementState, Ime as WIme, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};
use winit::window::{Theme, Window, WindowId, WindowLevel};
use winit::raw_window_handle::HasWindowHandle;

use crate::app::{App, Shared, WindowRequest};
use crate::compose::{Compositor, Frame};
use crate::frame_clock::FrameClock;
use crate::edit::{Ime, Key, KeyEvent, Mods};
use crate::el::{Shell, Tx};
use crate::render::{Gpu, Renderer};
use crate::runtime::{Env, Runtime, TitlebarAction, Waker};
use crate::surface_stats::{Pacing, SurfaceMeter, SurfaceStats};
use crate::tooling::{AppSpec, Args, Chrome, runtime_for};
use crate::windows::{AnyWindowSpec, Display, WindowOptions, WindowSpec};

/// The open window. Fields drop in order: the surface before its window.
struct Win {
    first_frame: FirstFrame,
    #[cfg(target_os = "macos")]
    pacing: Option<crate::display_link::DisplayLink>,
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
    activator: Option<crate::activation::Activator>,
    /// Reports when each drawable reached the screen while tracing.
    #[cfg(target_os = "macos")]
    present: Option<crate::present_time::metal::LayerHook>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    #[cfg(target_os = "macos")]
    panel: Option<crate::macos::Panel>,
    /// `El::system_material` views behind the Metal layer.
    #[cfg(target_os = "macos")]
    materials: crate::system_material::macos::Materials,
    window: Arc<Window>,
    /// False while hidden (platforms that can hide a window keep it for the next open).
    visible: bool,
    occluded: bool,
    /// When to redraw after a frame that had no drawable, or the next frame of motion when no
    /// display link paces it (macOS).
    retry: Option<Instant>,
    /// The next occluded re-check's wait, backing off while the surface stays occluded.
    occluded_wait: Duration,
    /// A hidden launcher's next try after a refresh got no drawable, backing off likewise.
    hidden_wait: Duration,
    /// When a hidden launcher renders the changes it hasn't yet.
    hidden_pace: HiddenPace,
    cursor: crate::os_cursor::OsCursor,
    alpha: Option<crate::surface_alpha::AlphaSurface>,
    #[cfg(target_os = "macos")]
    help: crate::macos_help::NativeHelp,
    /// Fn presses, which winit doesn't send, while the app wants modifier keys.
    #[cfg(target_os = "macos")]
    fn_key: Option<crate::fn_key::FnKey>,
}

impl Win {
    /// Matches the drawable to the window's size; false when it already did.
    fn resize_surface(&mut self, device: &wgpu::Device, size: winit::dpi::PhysicalSize<u32>) -> bool {
        let (width, height) = (size.width.max(1), size.height.max(1));
        if (self.config.width, self.config.height) == (width, height) { return false; }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(device, &self.config);
        if let Some(alpha) = &mut self.alpha { alpha.resize(device, (width, height)); }
        true
    }
}

/// A reveal requested before the hidden frame completes is held behind the fence, as is a
/// reveal of cached pixels older than the state. Hiding cancels that intent, including when
/// the GPU frame completes later. `deadline` is (next retry, give up) while it prepares.
#[derive(Default)]
struct FirstFrame { ready: bool, reveal: bool, notified: bool, deadline: Option<(Instant, Instant)> }
impl FirstFrame {
    /// Whether to show the cached frame now. One already on screen shows at once; its next
    /// frame (or retry) draws a stale state, with no first-frame deadline.
    fn request_reveal(&mut self, stale: bool, visible: bool) -> bool {
        if visible { return true; }
        if stale { self.ready = false; }
        if self.ready { return true; }
        self.reveal = true;
        if self.deadline.is_none() { self.prepare(); }
        false
    }
    /// After readiness, a canceled refresh keeps the cached frame and stops retrying.
    fn hide(&mut self) {
        self.reveal = false;
        if self.notified {
            self.ready = true;
            self.deadline = None;
        }
    }
    /// The drawable deadline passed. The first frame stays hidden (the host reports it); a
    /// refresh returns whether to reveal the cached frame rather than nothing.
    fn expire(&mut self) -> bool {
        self.deadline = None;
        if !self.notified { return false; }
        self.ready = true;
        std::mem::take(&mut self.reveal)
    }
    /// A hidden frame that got no drawable leaves the cached one stale. Unless the deadline
    /// retries it, the next try waits `wait`, which doubles up to `OCCLUDED_MAX`.
    fn missed(&mut self, now: Instant, wait: &mut Duration) -> Option<Instant> {
        self.ready = false;
        if self.deadline.is_some() { return None; }
        let at = now + *wait;
        *wait = (*wait * 2).min(OCCLUDED_MAX);
        Some(at)
    }
    fn prepare(&mut self) {
        let now = Instant::now();
        self.deadline = Some((now, now + Duration::from_secs(2)));
    }
    /// Hidden frames settle, except the refresh a reveal waits on: its new motion runs on.
    fn settles(&self) -> bool { !(self.reveal && self.notified) }
    /// A finished frame still owes the first readiness, or a refresh owes its reveal.
    fn pending(&self) -> bool { self.ready && (!self.notified || self.reveal) }
    fn complete(&mut self) -> bool {
        self.ready = true;
        self.deadline = None;
        std::mem::take(&mut self.reveal)
    }
}

#[test]
fn launcher_first_frame_holds_reveal_and_hide_cancels_late_completion() {
    let mut frame = FirstFrame::default();
    frame.prepare();
    assert!(!frame.request_reveal(false, false));
    assert!(!frame.ready);
    assert!(frame.complete());
    assert!(frame.ready && frame.deadline.is_none());
    assert!(!frame.complete(), "a second callback must not reopen a hidden panel");
    let mut canceled = FirstFrame::default();
    canceled.prepare();
    assert!(!canceled.request_reveal(false, false));
    canceled.hide();
    assert!(!canceled.complete(), "hide before readiness cancels the pending show");
    assert!(canceled.ready);
    assert!(canceled.request_reveal(false, false), "the prepared frame can be revealed without another preparation");
    assert!(canceled.deadline.is_none());
}

#[test]
fn launcher_stale_reveal_waits_for_an_unsettled_hidden_frame() {
    let mut frame = FirstFrame { ready: true, notified: true, ..Default::default() };
    assert!(!frame.complete());
    assert!(frame.request_reveal(false, false), "unchanged state reveals the cached frame at once");
    assert!(!frame.pending() && frame.settles());
    assert!(!frame.request_reveal(true, false), "changed state renders hidden first");
    assert!(frame.deadline.is_some() && !frame.settles(), "the refresh keeps its new motion");
    frame.ready = true; // The hidden frame's fence completed.
    assert!(frame.pending() && frame.complete());
    assert!(!frame.pending() && frame.settles());
    assert!(!frame.request_reveal(true, false));
    frame.hide();
    assert!(frame.settles(), "a canceled refresh is an ordinary hidden frame");
    assert!(frame.ready && frame.deadline.is_none(), "hide stops the refresh retries");
    assert!(!frame.pending() && !frame.complete(), "a late frame does not reveal");

    assert!(!frame.request_reveal(true, false));
    assert!(frame.expire(), "a refresh with no drawable reveals the cached frame");
    assert!(frame.ready && !frame.pending() && frame.deadline.is_none());
    let mut first = FirstFrame::default();
    first.prepare();
    assert!(!first.request_reveal(true, false));
    assert!(!first.expire() && !first.ready, "the first frame stays hidden and reports instead");
    first.ready = true;
    assert!(first.pending() && first.complete(), "a late first frame still reveals");
}

#[test]
fn launcher_refresh_with_no_drawable_retries_and_never_reveals_stale_pixels() {
    let now = Instant::now();
    let mut frame = FirstFrame { ready: true, notified: true, ..Default::default() };
    let mut wait = TIMEOUT_RETRY;
    assert_eq!(frame.missed(now, &mut wait), Some(now + TIMEOUT_RETRY), "a hidden refresh that got no drawable tries again");
    assert!(!frame.ready, "its cached frame no longer counts as ready");
    assert_eq!(frame.missed(now, &mut wait), Some(now + TIMEOUT_RETRY * 2), "and backs off");
    for _ in 0..10 { frame.missed(now, &mut wait); }
    assert_eq!(wait, OCCLUDED_MAX);
    assert!(!frame.request_reveal(false, false), "a reveal waits for the refresh even with nothing new to commit");
    assert!(frame.deadline.is_some());
    assert_eq!(frame.missed(now, &mut wait), None, "the first-frame deadline owns its retries");

    // One already on screen shows at once and redraws, rather than retrying a first frame.
    let mut shown = FirstFrame { ready: true, notified: true, ..Default::default() };
    assert!(shown.request_reveal(true, true));
    assert!(shown.deadline.is_none() && shown.ready);
}

impl Win {
    fn show(&self, focus: bool) {
        #[cfg(target_os = "macos")]
        {
            if let Some(panel) = &self.panel {
                panel.show(std::time::Duration::ZERO);
                if focus { panel.request_keys(); }
                return;
            }
            if !focus { crate::macos::show_without_focus(&self.window); return; }
        }
        self.window.set_visible(true);
        if focus { self.window.focus_window(); }
    }

    /// The runtime's cursor while the pointer is in the window, the arrow once it leaves.
    /// `Hidden` needs focus, so it can't outlive the window being active.
    fn apply_cursor<S>(&mut self, rt: &Runtime<S>, focused: bool) {
        let want = match rt.cursor() { crate::el::Cursor::Hidden if !focused => crate::el::Cursor::Default, c => c };
        self.cursor.apply(&self.window, want, rt.pointer().is_some());
    }

    fn hide(&self) {
        #[cfg(target_os = "macos")]
        if let Some(panel) = &self.panel { panel.hide(std::time::Duration::ZERO); return; }
        self.window.set_visible(false);
    }

    /// Whether the display link drives redraws. Other platforms redraw on request.
    fn paced(&self) -> bool {
        #[cfg(target_os = "macos")]
        return self.pacing.as_ref().is_some_and(|p| p.running());
        #[cfg(not(target_os = "macos"))]
        false
    }

    /// Whether something other than events paces the window's redraws: the display link on
    /// macOS, the present queue's vsync elsewhere.
    fn paced_for_stats(&self) -> bool { !cfg!(target_os = "macos") || self.paced() }

    /// Waiting for a missed frame's retry, or for the timer pacing unpaced motion. A hidden
    /// launcher's first-frame deadline owns its own retries.
    fn backing_off(&self, now: Instant) -> bool {
        self.retry.is_some_and(|retry| retry > now) && self.first_frame.deadline.is_none()
    }

    /// What paces the window's frames, for its stats.
    fn pacing_state(&self, moving: bool) -> Pacing {
        #[cfg(target_os = "macos")]
        return crate::display_link::pacing(self.pacing.as_ref(), moving);
        #[cfg(not(target_os = "macos"))]
        if moving { Pacing::Vsync } else { Pacing::Idle }
    }

    /// Whether none of the window is on screen, read from AppKit (a prewarm acquisition reads
    /// as visible).
    #[cfg(target_os = "macos")]
    fn natively_occluded(&self) -> bool {
        match &self.panel {
            Some(panel) => panel.occluded(),
            None => crate::macos::window_occluded(&self.window),
        }
    }

    fn stop_pacing(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(pacing) = &mut self.pacing { pacing.stop(); }
        #[cfg(target_os = "macos")]
        self.present_with_transaction(false);
    }

    /// Presents drawables inside Core Animation transactions, while the frame moves.
    #[cfg(target_os = "macos")]
    fn present_with_transaction(&mut self, on: bool) {
        self.materials.set_moving(&self.surface, on);
    }

    fn focus(&self) {
        #[cfg(target_os = "macos")]
        if let Some(panel) = &self.panel { panel.request_keys(); return; }
        self.window.focus_window();
    }
}

pub(crate) enum WakeEvent {
    DisplayTick(usize),
    App(usize), Live(usize), Window(usize, WindowRequest), Focus(usize),
    Position(usize, (i32, i32)), Level(usize, bool),
    Place(usize, crate::windows::PlaceRequest),
    Frame(usize, crate::window_frame::FrameRequest),
}

/// What `Windows` needs of a host, whatever its state type: an app window or an overlay.
pub(crate) trait AnyHost {
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, _cause: StartCause) {}
    fn wake_deadline(&self) -> Option<Instant> { None }
    fn resumed(&mut self, event_loop: &ActiveEventLoop);
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: WakeEvent);
    fn owns(&self, id: WindowId) -> bool;
    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent);
    /// Draws what has no redraw event (a layer surface); true while it needs more frames.
    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) -> bool { false }
    fn finished(&self) -> bool { false }
    /// The OS is ending the app without Rune asking: run close hooks before the hosts drop.
    fn terminate(&mut self) {}
    /// Ends a tracking pop-up menu and answers waiting ones `None`, before the host drops or
    /// the loop exits.
    fn cancel_menus(&mut self) {}
    /// False for hosts that must not keep the loop running alone (overlays).
    fn keeps_loop(&self) -> bool { true }
}

type Request = Box<dyn FnOnce(usize) -> WakeEvent + Send>;

#[derive(Default)]
struct NativeWindow {
    window: Mutex<Weak<Window>>,
    number: AtomicU32,
    ready: AtomicBool,
    /// What the host's meter last published.
    stats: Arc<Mutex<SurfaceStats>>,
    /// The event loop and slot once the host exists, and the requests made before it did.
    route: Mutex<(Option<(EventLoopProxy<WakeEvent>, usize)>, Vec<Request>)>,
}

/// A window's native reference, refreshed across App close/reopen without owning the window.
#[derive(Clone)]
pub struct WindowHandle {
    native: Arc<NativeWindow>,
    key: Arc<str>,
}
impl WindowHandle {
    pub(crate) fn new(key: &str) -> Self { Self { native: Arc::default(), key: key.into() } }

    /// Routes requests to the window's host, starting with those queued before it existed.
    fn bind(&self, proxy: EventLoopProxy<WakeEvent>, slot: usize) {
        let mut route = self.native.route.lock().unwrap();
        for request in std::mem::take(&mut route.1) { let _ = proxy.send_event(request(slot)); }
        route.0 = Some((proxy, slot));
    }

    /// A headless run never binds a host; it takes the queued requests instead.
    pub(crate) fn take_headless(&self) -> Vec<WakeEvent> {
        let queued = std::mem::take(&mut self.native.route.lock().unwrap().1);
        queued.into_iter().map(|request| request(0)).collect()
    }

    pub(crate) fn headless_pending(&self) -> bool {
        !self.native.route.lock().unwrap().1.is_empty()
    }

    fn send(&self, request: impl FnOnce(usize) -> WakeEvent + Send + 'static) {
        let mut route = self.native.route.lock().unwrap();
        match &route.0 {
            Some((proxy, slot)) => { let _ = proxy.send_event(request(*slot)); }
            None => route.1.push(Box::new(request)),
        }
    }

    /// The first frame has completed for the current native lifetime. For a launcher
    /// this confirms GPU completion of an alpha-zero frame before reveal. False after destruction.
    pub fn is_ready(&self) -> bool { self.window().is_ok() && self.native.ready.load(Ordering::Acquire) }
    pub fn key(&self) -> &str { &self.key }
    /// The window's frame counts: redraws, presents, failed drawable acquisitions, loop turns
    /// and pacing: current totals, and per-second rates updated once a second while it draws.
    pub fn surface_stats(&self) -> SurfaceStats { crate::surface_stats::read(&self.native.stats) }
    fn window(&self) -> Result<Arc<Window>, String> {
        self.native.window.lock().unwrap().upgrade().ok_or_else(|| "window is closed".into())
    }
    /// macOS CGWindowID for ScreenCaptureKit exclusion; None before registration or after destruction.
    pub fn window_number(&self) -> Option<u32> {
        let _window = self.window().ok()?;
        let number = self.native.number.load(Ordering::Acquire);
        (number != 0).then_some(number)
    }
    pub fn set_click_through(&self, click_through: bool) -> Result<(), String> {
        self.window()?.set_cursor_hittest(!click_through).map_err(|e| e.to_string())
    }
    pub fn set_always_on_top(&self, on: bool) {
        self.send(move |slot| WakeEvent::Level(slot, on));
    }
    /// Physical desktop pixels; retained across recreation. Unsupported on Wayland.
    pub fn set_position(&self, position: (i32, i32)) {
        self.send(move |slot| WakeEvent::Position(slot, position));
    }
    pub fn place(&self, display: crate::DisplayTarget, placement: crate::OverlayPlacement) {
        let place = crate::windows::PlaceRequest::new(display, placement);
        self.send(move |slot| WakeEvent::Place(slot, place));
    }
    /// [`Tx::resize_window`] from outside a handler, on `tok::motion::DEFAULT`.
    pub fn resize(&self, size: (f32, f32)) {
        self.frame(crate::window_frame::FrameRequest { size: Some(size), animate: true, ..Default::default() });
    }
    /// [`Tx::resize_window_instant`] from outside a handler.
    pub fn resize_instant(&self, size: (f32, f32)) {
        self.frame(crate::window_frame::FrameRequest { size: Some(size), ..Default::default() });
    }
    /// [`Tx::center_window`] from outside a handler.
    pub fn center(&self) {
        self.frame(crate::window_frame::FrameRequest { center: true, ..Default::default() });
    }
    fn frame(&self, request: crate::window_frame::FrameRequest) {
        self.send(move |slot| WakeEvent::Frame(slot, request));
    }
    /// Show/recreate, or hide while keeping state. A launcher takes keys and reveals cached
    /// pixels; an ordinary window shows without requesting focus.
    pub fn set_visible(&self, visible: bool) {
        let request = if visible { WindowRequest::Show } else { WindowRequest::Hide };
        self.send(move |slot| WakeEvent::Window(slot, request));
    }
    pub fn show(&self) { self.set_visible(true); }
    pub fn hide(&self) { self.set_visible(false); }
    /// Focus/activate an already-visible window, restoring it if minimized. Wayland requests attention.
    pub fn focus(&self) { self.send(WakeEvent::Focus); }
    /// Show, restore and activate, creating the window if needed (see `AppHandle::open`).
    /// A default close destroys its host; use App::keep_running to reopen.
    /// Windows added with `App::window` always reopen. Requests made before the loop runs
    /// apply once the window's host starts.
    pub fn open(&self) { self.send(|slot| WakeEvent::Window(slot, WindowRequest::Open(None))); }
    pub fn close(&self) { self.send(|slot| WakeEvent::Window(slot, WindowRequest::Close)); }
    /// Borrow a raw native handle while alive. Platform thread restrictions still apply;
    /// do not retain an extracted pointer beyond the callback.
    pub fn with_raw_handle<R>(&self, f: impl for<'a> FnOnce(winit::raw_window_handle::WindowHandle<'a>) -> R) -> Result<R, String> {
        let window = self.window()?;
        let handle = window.window_handle().map_err(|e| e.to_string())?;
        Ok(f(handle))
    }
}

#[cfg(target_os = "macos")]
fn native_window_number(window: &Window) -> Option<u32> {
    // SAFETY: called on winit's event thread.
    u32::try_from(unsafe { crate::macos::ns_window(window)?.windowNumber() }).ok().filter(|n| *n != 0)
}
#[cfg(not(target_os = "macos"))]
fn native_window_number(_: &Window) -> Option<u32> { None }

/// Builds every host, attaching its shortcut and menu guards, before any resumes: a window's
/// first updates drain on resume and may register on a later window.
fn start_all<T, H>(items: Vec<T>, mut build: impl FnMut(usize, T) -> Option<H>, resume: impl FnMut(&mut H)) -> Vec<Option<H>> {
    let mut hosts: Vec<_> = items.into_iter().enumerate().map(|(slot, item)| build(slot, item)).collect();
    hosts.iter_mut().flatten().for_each(resume);
    hosts
}

type Factory = Box<dyn FnOnce(&[Display]) -> Vec<AnyWindowSpec>>;

struct Windows {
    factory: Option<Factory>,
    args: Args,
    proxy: EventLoopProxy<WakeEvent>,
    hosts: Vec<Option<Box<dyn AnyHost>>>,
    // Weak pool preserves release_gpu_when_closed when the last GPU owner closes.
    gpu: Rc<RefCell<Weak<Gpu>>>,
}
impl Windows {
    fn cleanup(&mut self, event_loop: &ActiveEventLoop) {
        for host in &mut self.hosts {
            if let Some(h) = host.as_mut().filter(|h| h.finished()) {
                h.cancel_menus();
                *host = None;
            }
        }
        // Another window's quit: a menu still tracking would hold the loop open.
        if event_loop.exiting() {
            for host in self.hosts.iter_mut().flatten() { host.cancel_menus(); }
        }
        if !self.hosts.iter().flatten().any(|h| h.keeps_loop()) {
            // The app ends with its last main window: windows still open close as their close
            // button would. A quit already asked to exit and skips their hooks.
            if !event_loop.exiting() {
                for host in self.hosts.iter_mut().flatten() { host.terminate(); }
            }
            event_loop.exit();
        }
    }
}

/// What a window needs from the shell to start, whatever its state type.
pub(crate) struct Start<'a> {
    proxy: EventLoopProxy<WakeEvent>,
    slot: usize,
    gpu: Rc<RefCell<Weak<Gpu>>>,
    wayland: bool,
    /// Whether this window records `--native` traces.
    traced: bool,
    overlays: &'a mut Vec<Box<dyn crate::overlay::PendingOverlay>>,
}

/// Starts one window's host, or renders it headless when its own arguments ask for that.
pub(crate) fn start<S: 'static>(mut spec: WindowSpec<S>, handle: Option<WindowHandle>, keeps_loop: bool, mut inherited: Args, cx: Start<'_>) -> Option<Box<dyn AnyHost>> {
    if inherited.native && !cx.traced {
        inherited.trace = None;
        inherited.frames = None;
    }
    let args = spec.app.resolve_args(inherited);
    if args.headless() {
        if let Some((w, h)) = spec.options.physical_size {
            let scale = args.scale.unwrap_or(2.0);
            spec.app.spec.size = (w as f32 / scale, h as f32 / scale);
        }
        let transparent = spec.options.transparent;
        spec.app.window_options = spec.options;
        crate::tooling::headless_configured(spec.app, args, transparent);
        return None;
    }
    crate::video::enable_playback();
    cx.overlays.append(&mut spec.app.overlays);
    let handle = handle.unwrap_or_else(|| WindowHandle::new(&spec.key));
    let mut host = Host::new(spec, handle, args, cx.proxy, cx.slot, cx.gpu, cx.wayland);
    host.keeps_loop = keeps_loop;
    Some(Box::new(host))
}

impl ApplicationHandler<WakeEvent> for Windows {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        for host in self.hosts.iter_mut().flatten() { host.new_events(event_loop, cause); }
        // Before resumed there are no hosts to clean up.
        if self.factory.is_none() { self.cleanup(event_loop); }
    }
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let Some(factory) = self.factory.take() else { return };
        let primary = event_loop.primary_monitor();
        let displays: Vec<_> = event_loop.available_monitors().map(|m| Display {
            id: Some(crate::DisplayId::from_monitor(m.clone(), is_wayland(event_loop))),
            name: m.name(), position: (m.position().x, m.position().y),
            size: (m.size().width, m.size().height), scale: m.scale_factor(), primary: primary.as_ref() == Some(&m),
        }).collect();
        let specs = crate::windows::flatten(factory(&displays));
        crate::windows::validate(&specs).expect("window keys");
        let trace_window = self.args.window.clone().or_else(|| specs.first().map(|s| s.key().to_owned()));
        let mut overlays = Vec::new();
        let wayland = is_wayland(event_loop);
        self.hosts = start_all(specs, |slot, spec| {
            let traced = trace_window.as_deref() == Some(spec.key());
            spec.start(self.args.clone(), Start { proxy: self.proxy.clone(), slot, gpu: self.gpu.clone(), wayland, traced, overlays: &mut overlays })
        }, |host| host.resumed(event_loop));
        for mut overlay in overlays {
            let (slot, proxy) = (self.hosts.len(), self.proxy.clone());
            let frame_proxy = proxy.clone();
            overlay.connect(Arc::new(move || { let _ = proxy.send_event(WakeEvent::App(slot)); }),
                Arc::new(move || { let _ = frame_proxy.send_event(WakeEvent::DisplayTick(slot)); }), self.gpu.clone());
            overlay.resumed(event_loop);
            self.hosts.push(Some(overlay));
        }
        self.cleanup(event_loop);
    }
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: WakeEvent) {
        let slot = match &event {
            WakeEvent::DisplayTick(i) | WakeEvent::App(i) | WakeEvent::Live(i) | WakeEvent::Window(i, _) | WakeEvent::Focus(i)
                | WakeEvent::Position(i, _) | WakeEvent::Level(i, _) | WakeEvent::Place(i, _) | WakeEvent::Frame(i, _) => *i,
        };
        if let Some(Some(host)) = self.hosts.get_mut(slot) {
            host.user_event(event_loop, event);
        }
        self.cleanup(event_loop);
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if let Some(host) = self.hosts.iter_mut().flatten().find(|h| h.owns(id)) {
            host.window_event(event_loop, id, event);
        }
        self.cleanup(event_loop);
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let mut animating = false;
        for host in self.hosts.iter_mut().flatten() {
            animating |= host.about_to_wait(event_loop);
        }
        // Layer surfaces have no redraw events: keep turning while one animates (its vsync
        // paces the loop). Hosts whose only motion is an ambient loop sleep until its next step.
        let deadline = self.hosts.iter().flatten().filter_map(|host| host.wake_deadline()).min();
        event_loop.set_control_flow(if animating { ControlFlow::Poll }
            else { deadline.map_or(ControlFlow::Wait, ControlFlow::WaitUntil) });
    }
    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        // Rune didn't ask to exit, so the OS is ending the app: macOS Quit, Cmd-Q, the Dock's
        // Quit, logout. Windows close as their close button would, before the clipboard stops.
        if !event_loop.exiting() {
            for host in self.hosts.iter_mut().flatten() { host.terminate(); }
        }
        for host in self.hosts.iter_mut().flatten() { host.cancel_menus(); }
        // Surfaces go while winit's display connection is still open (layer surfaces live on it).
        self.hosts.clear();
        crate::clipboard::shutdown();
    }
}

struct Host<S: 'static> {
    chrome: Chrome,
    options: WindowOptions,
    placement: Option<crate::windows::PlaceRequest>,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    macos_window: crate::MacWindowOptions,
    native_handle: WindowHandle,
    on_created: Option<Box<dyn FnOnce(WindowHandle)>>,
    on_window: Option<crate::app::OnWindow>,
    on_ready: Option<crate::app::OnWindow>,
    finished: bool,
    /// False for windows `App::window` added: they live as long as the loop, never keep it.
    keeps_loop: bool,
    spec: AppSpec<S>,
    args: Args,
    keep_running: bool,
    on_close: Option<fn(&mut S, &mut Tx)>,
    release_gpu: bool,
    open_at_start: bool,
    shared: Arc<Shared<S>>,
    shortcuts: crate::shortcuts::Guard,
    menus: crate::native_menu::Guard<S>,
    popup: crate::popup_menu::Presenter,
    rt: Runtime<S>,
    /// Before `win` and `gpu`: fields drop in order, and the trace waits for the window's last
    /// present times.
    trace: Option<crate::tooling::Trace>,
    trace_start: Option<Instant>,
    trace_frames: usize,
    meter: SurfaceMeter,
    win: Option<Win>,
    gpu: Option<(Arc<Gpu>, Renderer, wgpu::TextureFormat)>,
    gpu_pool: Rc<RefCell<Weak<Gpu>>>,
    /// A window has shown before: the next one opens settled.
    shown: bool,
    started: bool,
    clock: FrameClock,
    idle: bool,
    mods: ModifiersState,
    control_click: bool,
    clipboard: crate::clipboard::Client,
    secure_input: crate::secure_input::SecureInput,
    ime_allowed: bool,
    comp: Option<Box<dyn Compositor<S>>>,
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    wayland: bool,
    /// The window's frame moving to where a handler asked (`Tx::resize_window`), top-down
    /// screen points on the host's frame clock. macOS only; elsewhere frames change at once.
    frame_anim: Option<crate::window_frame::FrameAnim>,
    /// The window origin `frame_anim` last set, to notice a drag mid-flight.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    frame_origin: Option<[f32; 2]>,
    /// The content size a resize request jumps to, so its Resized event lays out like a step
    /// of a resize animation rather than a user's resize.
    frame_jump: Option<(f32, f32)>,
    /// Present times of every window this host opens while tracing.
    #[cfg(target_os = "macos")]
    present_log: Option<Arc<crate::present_time::metal::Log>>,
    #[cfg(target_os = "macos")]
    frame_wake: Waker,
}

pub fn run<S: 'static>(app: App<S>, args: Args) -> Result<(), String> {
    let accessory = app.accessory;
    run_configured(Box::new(move |_| vec![WindowSpec::from_app("main", app).into()]), args, accessory)
}

pub(crate) fn run_windows(factory: Factory, args: Args, options: crate::RunOptions) -> Result<(), String> {
    run_configured(factory, args, options.accessory)
}

/// Without a display (no `WAYLAND_DISPLAY` or `DISPLAY`, a refused X11 connection) the event
/// loop can't be built.
fn event_loop_error(error: winit::error::EventLoopError) -> String {
    format!("event loop: {error}")
}

/// Whether this process can reach the window server. Over ssh to a Mac no one is logged in to,
/// AppKit starts but never finishes launching, so the app would wait forever.
#[cfg(target_os = "macos")]
fn window_server_session() -> bool {
    use std::ffi::c_void;
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" { fn CGSessionCopyCurrentDictionary() -> *const c_void; }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" { fn CFRelease(cf: *const c_void); }
    // SAFETY: a Copy-rule CF function; a non-null result is released exactly once.
    unsafe {
        let session = CGSessionCopyCurrentDictionary();
        if session.is_null() { return false; }
        CFRelease(session);
    }
    true
}

fn run_configured(factory: Factory, args: Args, accessory: bool) -> Result<(), String> {
    let _ = accessory;
    #[cfg(target_os = "macos")]
    if !window_server_session() {
        return Err("event loop: no window server session (no one is logged in to this Mac)".into());
    }
    #[allow(unused_mut)]
    let mut builder = EventLoop::<WakeEvent>::with_user_event();
    #[cfg(target_os = "macos")]
    if accessory {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        builder
            .with_activation_policy(ActivationPolicy::Accessory)
            .with_default_menu(false)
            .with_activate_ignoring_other_apps(false);
    }
    let event_loop = builder.build().map_err(event_loop_error)?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let mut windows = Windows { factory: Some(factory), args, proxy, hosts: Vec::new(), gpu: Rc::default() };
    let result = event_loop.run_app(&mut windows);
    crate::clipboard::shutdown(); // Also cover an event-loop error before `exiting`.
    result.map_err(|error| format!("run: {error}"))
}

#[test]
fn an_event_loop_that_cant_start_is_an_error_naming_the_event_loop() {
    let error = event_loop_error(winit::error::EventLoopError::RecreationAttempt);
    assert!(error.starts_with("event loop: ") && error.contains("recreated"), "{error}");
}

impl<S: 'static> Host<S> {
    fn new(window_spec: WindowSpec<S>, native_handle: WindowHandle, args: Args, proxy: EventLoopProxy<WakeEvent>, slot: usize, gpu_pool: Rc<RefCell<Weak<Gpu>>>, wayland: bool) -> Self {
        let WindowSpec { mut app, options, key: _, on_created } = window_spec;
        let mut args = app.resolve_args(args);
        let on_window = app.on_window.take();
        let on_ready = app.on_ready.take();
        let macos_window = std::mem::take(&mut app.macos_window);
        native_handle.bind(proxy.clone(), slot);
        let live_proxy = proxy.clone();
        #[cfg(target_os = "macos")]
        let frame_proxy = proxy.clone();
        let waker: Waker = Arc::new(move || {
            let _ = proxy.send_event(WakeEvent::App(slot));
        });
        let (open, keep_running, on_close, release_gpu) = (app.open, app.keep_running, app.on_close, app.release_gpu);
        #[cfg(target_os = "macos")]
        let present_log = args.trace.as_ref().map(|_| Arc::default());
        let mut app = app;
        let comp = app.compositor.take();
        let on_focus = app.on_focus;
        let chrome = app.chrome;
        let srgb_blend = if comp.is_some() && app.srgb_blend {
            eprintln!("rune: srgb_blend is ignored when a compositor is set");
            false
        } else {
            app.srgb_blend
        };
        #[cfg(all(feature = "dev", debug_assertions))]
        let dev = app.dev.take();
        let modifier_keys = app.modifier_keys;
        let fit_height = app.fit_content_height;
        let app_stats = app.dev_stats;
        let (mut spec, state, shared) = app.into_parts(args.scene.as_deref());
        let mut size = args.size.unwrap_or(spec.size);
        // The window reports the system appearance and scale when it opens.
        let env = Env::new(args.theme.as_deref() != Some("light"), 1.0);
        let mut rt = runtime_for(&spec, state, size, env, setup_waker(&shared));
        if fit_height {
            spec.min_size = options.fit_min_size(spec.min_size);
            // Opens at the content's height; the first frame corrects any difference its
            // real scale makes.
            rt.fit_height = true;
            let (min, max) = options.size_limits(spec.min_size);
            size = crate::window_frame::clamp_size((size.0, rt.content_height()), min, max);
            rt.set_viewport(size, env.scale);
            spec.size = size;
            args.size = None;
        }
        rt.on_focus = on_focus;
        rt.set_modifier_keys(modifier_keys);
        rt.set_srgb_blend(srgb_blend);
        rt.set_system_reduced_motion(crate::preferences::reduced_motion());
        rt.set_full_keyboard_access(crate::preferences::full_keyboard_access());
        rt.trace_channels = args.trace.as_ref().map(|_| Default::default());
        rt.set_live_waker(Arc::new(move || { let _ = live_proxy.send_event(WakeEvent::Live(slot)); }));
        rt.set_live_visible(false);
        rt.set_os_drag(crate::drag_out::SUPPORTED);
        rt.transparent = options.transparent;
        rt.track_marks = comp.is_some();
        rt.wall_start = Some(Instant::now());
        let meter = SurfaceMeter::new(native_handle.key(), native_handle.native.stats.clone());
        rt.surface_stats = Some(meter.shared());
        rt.app_stats = app_stats;
        #[cfg(all(feature = "dev", debug_assertions))]
        if let Some(dev) = dev { crate::dev::launch(&mut rt, &spec, dev, waker.clone()); }
        let mut clipboard = crate::clipboard::Client::default();
        clipboard.connect(waker.clone());
        shared.connect(waker.clone());
        let popup = crate::popup_menu::Presenter::new(waker.clone());
        let shortcuts = crate::shortcuts::Guard::attach(&shared, waker, false, wayland);
        let menus = crate::native_menu::Guard::attach(&shared, false);
        Host {
            chrome,
            options,
            placement: None,
            macos_window,
            native_handle,
            on_created,
            on_window,
            on_ready,
            finished: false,
            keeps_loop: true,
            spec,
            args,
            keep_running,
            on_close,
            release_gpu,
            open_at_start: open,
            shared,
            shortcuts,
            menus,
            popup,
            rt,
            win: None,
            gpu: None,
            gpu_pool,
            shown: false,
            started: false,
            clock: FrameClock::new(Instant::now()),
            idle: true,
            mods: ModifiersState::empty(),
            control_click: false,
            clipboard,
            secure_input: Default::default(),
            ime_allowed: false,
            comp,
            wayland,
            frame_anim: None,
            frame_origin: None,
            frame_jump: None,
            trace: None,
            trace_start: None,
            trace_frames: 0,
            meter,
            #[cfg(target_os = "macos")]
            present_log,
            #[cfg(target_os = "macos")]
            frame_wake: Arc::new(move || { let _ = frame_proxy.send_event(WakeEvent::DisplayTick(slot)); }),
        }
    }
}

/// The runtime's next timer, on the wall clock that started with the host.
pub(crate) fn wall_wake_deadline<S>(rt: &Runtime<S>) -> Option<Instant> {
    rt.wall_start?.checked_add(Duration::try_from_secs_f64(rt.wake_deadline()?).ok()?)
}

/// The runtime's next timer as a host with no display link to take it waits for it: hidden,
/// covered, faded out or unpaced. A timer due within a frame interval of the last poll
/// (`Tx::wake_after(Duration::ZERO)` asks for the next frame) waits for that interval, so a
/// poll that keeps asking steps at most once per `TIMER_FRAME` and never spins the loop.
pub(crate) fn timer_deadline<S>(rt: &Runtime<S>) -> Option<Instant> {
    let due = wall_wake_deadline(rt)?;
    let polled = rt.wall_start.and_then(|t0| t0.checked_add(Duration::try_from_secs_f64(rt.polled_at()).ok()?));
    Some(polled.map_or(due, |polled| due.max(polled + TIMER_FRAME)))
}

/// Paces the frame a hidden host keeps ready to show (a launcher's cached pixels, a retained
/// overlay's commit). A change after a quiet spell renders at once; later ones wait until
/// `HIDDEN_REFRESH` has passed since the last render, so state that changes many times a
/// second costs a few builds a second while nobody sees them.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct HiddenPace { last: Option<Instant>, waiting: bool }

pub(crate) const HIDDEN_REFRESH: Duration = Duration::from_millis(250);

impl HiddenPace {
    /// Whether a change at `now` renders now. Otherwise it waits for `deadline`.
    pub(crate) fn changed(&mut self, now: Instant) -> bool {
        self.waiting = !self.ready(now);
        !self.waiting
    }
    /// Whether a change at `now` would render at once.
    pub(crate) fn ready(&self, now: Instant) -> bool {
        self.last.is_none_or(|last| now >= last + HIDDEN_REFRESH)
    }
    /// When the change that waits renders.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.last.filter(|_| self.waiting).map(|last| last + HIDDEN_REFRESH)
    }
    /// Whether the change that waits is due at `now`; it stops waiting either way it renders.
    pub(crate) fn take_due(&mut self, now: Instant) -> bool {
        let due = self.deadline().is_some_and(|at| at <= now);
        if due { self.waiting = false; }
        due
    }
    /// A hidden frame rendered what had changed.
    pub(crate) fn rendered(&mut self, now: Instant) {
        *self = HiddenPace { last: Some(now), waiting: false };
    }
    /// Shown, the host draws its own frames; hidden again, the first change renders at once.
    pub(crate) fn reset(&mut self) { *self = HiddenPace::default(); }
}

#[test]
fn hidden_pace_renders_a_change_at_once_then_at_most_every_refresh() {
    let t0 = Instant::now();
    let mut pace = HiddenPace::default();
    assert!(pace.changed(t0), "the first change renders at once");
    pace.rendered(t0);
    assert_eq!(pace.deadline(), None);
    assert!(!pace.changed(t0 + Duration::from_millis(100)));
    assert!(!pace.changed(t0 + Duration::from_millis(200)));
    assert_eq!(pace.deadline(), Some(t0 + HIDDEN_REFRESH), "later ones wait for one render");
    assert!(!pace.take_due(t0 + Duration::from_millis(249)));
    assert!(pace.take_due(t0 + HIDDEN_REFRESH));
    assert_eq!(pace.deadline(), None, "a due render asked for doesn't wake the loop again");
    assert!(pace.changed(t0 + HIDDEN_REFRESH));
    pace.rendered(t0 + HIDDEN_REFRESH);
    assert_eq!(pace.deadline(), None);
    assert!(pace.changed(t0 + Duration::from_secs(5)), "after a quiet spell, at once again");
    pace.reset();
    assert!(pace.changed(t0 + Duration::from_secs(5)));
}

/// Whether a timer asks for the frame after one just presented. Only a surface whose display
/// link paces it (`next_frame` is its next tick) takes a timer due by then as a frame request;
/// anywhere else the timer waits in `timer_deadline`, so it never turns the loop by itself.
pub(crate) fn timer_wants_frame<S>(rt: &Runtime<S>, next_frame: Option<Instant>) -> bool {
    next_frame.is_some_and(|next| wall_wake_deadline(rt).is_some_and(|due| due <= next))
}

/// Why a surface gave no drawable this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Missed {
    /// Nothing of the window is on screen (covered, or its displays asleep).
    Occluded,
    /// No drawable was free, or acquisition failed some other passing way.
    Timeout,
    /// Outdated or lost, and reconfiguring didn't bring it back.
    Outdated,
}

/// How a host waits after a frame it couldn't draw. There is no `ControlFlow::Poll` here:
/// retrying at once is what spins a core while a window is occluded or its displays sleep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NextFrame {
    WaitUntil(Instant),
    /// Sleep until an event: a display tick, a notification, input or a timer.
    Idle,
}

/// The first re-check of an occluded surface. Its occlusion notification usually redraws it
/// first; this catches one that never arrives. Consecutive misses back off to `OCCLUDED_MAX`.
pub(crate) const OCCLUDED_RETRY: Duration = Duration::from_millis(250);
pub(crate) const OCCLUDED_MAX: Duration = Duration::from_secs(2);
/// An unpaced surface's retry after a drawable timeout: about one frame.
pub(crate) const TIMEOUT_RETRY: Duration = Duration::from_millis(16);
/// Motion's frame interval when no display link paces a surface.
pub(crate) const TIMER_FRAME: Duration = Duration::from_micros(16_667);

impl Missed {
    fn of(status: wgpu::CurrentSurfaceTexture) -> Result<wgpu::SurfaceTexture, Missed> {
        use wgpu::CurrentSurfaceTexture as S;
        match status {
            S::Success(f) | S::Suboptimal(f) => Ok(f),
            S::Occluded => Err(Missed::Occluded),
            S::Outdated | S::Lost => Err(Missed::Outdated),
            S::Timeout | S::Validation => Err(Missed::Timeout),
        }
    }

    /// What follows a frame this surface couldn't draw. A display link that is still running
    /// retries a timeout on its next tick. `occluded_wait` is the caller's backoff, held to
    /// `OCCLUDED_RETRY..=OCCLUDED_MAX`.
    pub(crate) fn next(self, paced: bool, now: Instant, occluded_wait: Duration) -> NextFrame {
        match self {
            Missed::Occluded => NextFrame::WaitUntil(now + occluded_wait.clamp(OCCLUDED_RETRY, OCCLUDED_MAX)),
            Missed::Timeout if paced => NextFrame::Idle,
            Missed::Timeout => NextFrame::WaitUntil(now + TIMEOUT_RETRY),
            Missed::Outdated => NextFrame::WaitUntil(now + OCCLUDED_RETRY),
        }
    }
}

/// The next drawable. An outdated or lost surface is reconfigured and tried again, up to
/// `reconfigures` times, synchronously; every other miss returns at once. On macOS a surface
/// wgpu would call occluded is never asked: each failed `get_current_texture` leaks a texture
/// id in wgpu-core 30.0.1.
pub(crate) fn acquire(surface: &wgpu::Surface, device: &wgpu::Device, config: &wgpu::SurfaceConfiguration, mut reconfigures: u32,
    meter: &mut SurfaceMeter) -> Result<wgpu::SurfaceTexture, Missed> {
    #[cfg(target_os = "macos")]
    if crate::macos::surface_occluded(surface) {
        meter.skipped();
        return Err(Missed::Occluded);
    }
    loop {
        let status = Missed::of(surface.get_current_texture());
        if let Err(missed) = status { meter.failed(missed); }
        match status {
            Err(Missed::Outdated) if reconfigures > 0 => {
                surface.configure(device, config);
                reconfigures -= 1;
            }
            Err(Missed::Outdated) => {
                surface.configure(device, config);
                return Err(Missed::Outdated);
            }
            other => return other,
        }
    }
}

#[test]
fn missed_frames_never_poll() {
    use wgpu::CurrentSurfaceTexture as S;
    let now = Instant::now();
    for status in [S::Occluded, S::Timeout, S::Outdated, S::Lost, S::Validation] {
        let missed = Missed::of(status).err().expect("a miss");
        for paced in [false, true] {
            for wait in [Duration::ZERO, OCCLUDED_RETRY, Duration::from_secs(60)] {
                // `NextFrame` has no Poll; a deadline at or before now would spin the same way.
                let flow = match missed.next(paced, now, wait) {
                    NextFrame::WaitUntil(at) => { assert!(at > now, "{missed:?} retries later, not now"); ControlFlow::WaitUntil(at) }
                    NextFrame::Idle => ControlFlow::Wait,
                };
                assert_ne!(flow, ControlFlow::Poll, "{missed:?} (paced: {paced}) must not poll");
            }
        }
    }
    let occluded = |wait| Missed::Occluded.next(true, now, wait);
    assert_eq!(occluded(Duration::ZERO), NextFrame::WaitUntil(now + OCCLUDED_RETRY), "the safety net is slow");
    assert_eq!(occluded(Duration::from_secs(60)), NextFrame::WaitUntil(now + OCCLUDED_MAX));
    assert_eq!(Missed::Timeout.next(true, now, Duration::ZERO), NextFrame::Idle, "a running display link retries on its tick");
    assert_eq!(Missed::Timeout.next(false, now, Duration::ZERO), NextFrame::WaitUntil(now + TIMEOUT_RETRY));
}

/// A Metal surface whose window reports itself covered, with no window server needed: the layer's
/// delegate stands in for the backing view, and is its own window.
#[cfg(all(test, target_os = "macos"))]
mod occluded_surface {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use objc2_video::{ClassType, define_class, msg_send, rc::Retained, runtime::{AnyClass, AnyObject, NSObject}};

    /// The fake window's `occlusionState`: 0 covered, 2 (`NSWindowOcclusionStateVisible`) shown.
    static OCCLUSION: AtomicUsize = AtomicUsize::new(0);

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "RuneFakeOccludedView"]
        struct FakeView;

        impl FakeView {
            #[unsafe(method(window))]
            fn window(&self) -> *const Self { self }

            #[unsafe(method(occlusionState))]
            fn occlusion_state(&self) -> usize { OCCLUSION.load(Ordering::Relaxed) }
        }
    );

    /// An hour covered costs no failed `get_current_texture` (each leaks a texture id in
    /// wgpu-core 30.0.1) and a re-check at most every two seconds; uncovered, it draws.
    #[test]
    fn an_occluded_surface_never_asks_wgpu_and_draws_once_visible() {
        let view: Retained<FakeView> = unsafe { msg_send![FakeView::class(), new] };
        let layer: Retained<AnyObject> = unsafe { msg_send![AnyClass::get(c"CAMetalLayer").expect("QuartzCore"), new] };
        let _: () = unsafe { msg_send![&*layer, setDelegate: &*view] };
        let instance = Gpu::instance();
        let target = wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(Retained::as_ptr(&layer) as *mut std::ffi::c_void);
        let surface = unsafe { instance.create_surface_unsafe(target) }.expect("a Metal layer surface");
        let gpu = match Gpu::try_new(instance, Some(&surface)) {
            Ok(gpu) => gpu,
            Err(error) => return eprintln!("skipped: {error}"),
        };
        let Some((config, _)) = surface_config(&surface, &gpu.adapter, (64, 64), false) else {
            return eprintln!("skipped: the adapter can't present to a Metal layer");
        };
        surface.configure(&gpu.device, &config);

        OCCLUSION.store(0, Ordering::Relaxed);
        assert!(unsafe { crate::macos::layer_occluded(&layer) });
        let mut meter = SurfaceMeter::new("occluded", Arc::default());
        let start = Instant::now();
        let (mut at, mut wait, mut checks) = (start, OCCLUDED_RETRY, 0u64);
        while at < start + Duration::from_secs(3600) {
            if let Ok(_frame) = acquire(&surface, &gpu.device, &config, 1, &mut meter) { panic!("an occluded surface got a drawable"); }
            checks += 1;
            let NextFrame::WaitUntil(next) = Missed::Occluded.next(false, at, wait) else { panic!("occlusion re-checks on a deadline") };
            wait = (wait * 2).min(OCCLUDED_MAX);
            at = next;
        }
        let totals = meter.totals();
        assert_eq!(totals.failed(), 0, "wgpu was asked for drawables it refuses as occluded");
        assert_eq!(totals.skipped, checks);
        assert!(checks <= 3600 / 2 + 3, "{checks} re-checks in an hour occluded");

        OCCLUSION.store(2, Ordering::Relaxed);
        let frame = acquire(&surface, &gpu.device, &config, 1, &mut meter).expect("a visible surface gets a drawable");
        let target = frame.texture.create_view(&Default::default());
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target, depth_slice: None, resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            ..Default::default()
        });
        gpu.queue.submit([encoder.finish()]);
        gpu.queue.present(frame);
        meter.presented();
        assert_eq!((meter.totals().presents, meter.totals().failed()), (1, 0));
    }
}

/// `setup`'s waker: `AppHandle::wake`, so a drain that also holds updates that changed nothing
/// still polls and redraws for it.
fn setup_waker<S: 'static>(shared: &Arc<crate::app::Shared<S>>) -> Waker {
    let handle = crate::AppHandle { shared: shared.clone() };
    Arc::new(move || handle.wake())
}

/// Whether a drain of other threads' requests asks the window for a frame. A hidden launcher
/// renders a change into its cached frame (paced); a shown launcher redraws for a change or
/// as it shows; any other shown window redraws, unless only updates that changed nothing
/// (`quiet`) woke it.
fn drain_frame(launcher: bool, visible: bool, was_shown: bool, changed: bool, quiet: bool) -> bool {
    if launcher && !visible { return changed; }
    !quiet && if launcher { changed || was_shown && visible } else { visible }
}

#[test]
fn updates_that_change_nothing_ask_no_window_for_a_frame() {
    use crate::{App, AppSpec, el};
    let app = App::new(AppSpec::new("t", (10.0, 10.0), || 0u32, |_, _| el()));
    let (handle, shared) = (app.handle(), app.shared.clone());
    let mut rt = Runtime::new(0u32, |_, _| el(), (10.0, 10.0), crate::Env::default());
    rt.frame(0.0);
    // As `Host::drain`, for 100 timer ticks that found nothing new.
    for _ in 0..100 {
        handle.update_if(|_, _| false);
        let mut requests = shared.take();
        let mut updated = false;
        for f in std::mem::take(&mut requests.updates) { updated |= rt.update_if(f); }
        let quiet = !updated && requests.only_updates() && !rt.patch_pending() && !rt.needs_commit();
        for (launcher, visible) in [(false, true), (true, true), (true, false), (false, false)] {
            assert!(!drain_frame(launcher, visible, visible, updated, quiet), "launcher {launcher} visible {visible}");
        }
    }
    handle.update_if(|_, _| false);
    handle.wake();
    assert!(!shared.take().only_updates(), "`wake` still runs a shown window's poll");
    handle.update_if(|_, _| false);
    setup_waker(&shared)();
    assert!(!shared.take().only_updates(), "so does `setup`'s waker");
    assert!(drain_frame(false, true, true, false, false) && drain_frame(true, false, false, true, false));
}

/// A stream of updates that change nothing, faster than a frame, on a host that neither polls
/// nor draws for them, doesn't push back a due timer, and one's own `wake_after(ZERO)` after
/// idle is due at once.
#[test]
fn updates_that_change_nothing_do_not_delay_due_timers() {
    let mut rt = Runtime::new(0u32, |_, _| crate::el(), (10.0, 10.0), crate::Env::default());
    rt.wall_start = Some(Instant::now());
    rt.frame(0.0);
    // A second idle since that frame.
    rt.wall_start = rt.wall_start.map(|t| t - Duration::from_secs(1));
    let polled = rt.polled_at();
    rt.update_if(|_, tx| { tx.wake_after(Duration::ZERO); false });
    rt.schedule_pending_wakes();
    let due = timer_deadline(&rt).expect("armed");
    assert!(due <= Instant::now(), "due now, not a frame interval from now");
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(2));
        rt.update_if(|_, _| false);
        rt.schedule_pending_wakes();
        assert_eq!(timer_deadline(&rt), Some(due));
    }
    assert_eq!(rt.polled_at(), polled, "the clock stays at the last poll");
}

/// `wake_after(Duration::ZERO)` from the poll: a running display link takes it as the next
/// frame; any host without one waits a frame interval from the poll, never `now`.
#[test]
fn a_timer_asking_for_the_next_frame_waits_a_frame_without_a_display_link() {
    fn poll(polls: &mut u32, _: f64, tx: &mut Tx) -> bool { *polls += 1; tx.wake_after(Duration::ZERO); false }
    let mut rt = Runtime::new(0u32, |_, _| crate::el(), (10.0, 10.0), crate::Env::default());
    rt.poll = Some(poll);
    let start = Instant::now() - Duration::from_secs(1);
    rt.wall_start = Some(start);
    rt.frame(0.0);
    assert_eq!(rt.state, 1);
    let due = wall_wake_deadline(&rt).expect("armed");
    let polled = start + Duration::from_secs_f64(rt.polled_at());
    assert!(due <= polled + Duration::from_micros(1), "due at once on the runtime's clock");
    let wait = timer_deadline(&rt).expect("armed");
    assert!(wait >= polled + TIMER_FRAME - Duration::from_micros(1) && wait <= polled + TIMER_FRAME + Duration::from_micros(1));
    assert!(timer_wants_frame(&rt, Some(polled + Duration::from_millis(8))), "a paced surface draws it next tick");
    assert!(!timer_wants_frame(&rt, None), "an unpaced one doesn't keep the loop turning");
    // A timer longer than a frame keeps its own time.
    let mut rt = Runtime::new(0u32, |_, _| crate::el(), (10.0, 10.0), crate::Env::default());
    rt.wall_start = Some(start);
    rt.update(|_, tx| tx.wake_after(Duration::from_millis(100)));
    rt.frame(0.0);
    assert_eq!(timer_deadline(&rt), wall_wake_deadline(&rt));
}

/// A vsynced sRGB (or non-sRGB when `srgb_blend` is set) configuration for the surface, and the
/// first alpha mode it supports that composites translucent pixels.
pub(crate) fn surface_config(surface: &wgpu::Surface, adapter: &wgpu::Adapter, (width, height): (u32, u32), srgb_blend: bool)
    -> Option<(wgpu::SurfaceConfiguration, Option<wgpu::CompositeAlphaMode>)> {
    let mut config = surface.get_default_config(adapter, width.max(1), height.max(1))?;
    let caps = surface.get_capabilities(adapter);
    let srgb = caps.formats.iter().copied().find(|f| f.is_srgb()).unwrap_or(config.format);
    config.format = if srgb_blend {
        let non_srgb = srgb.remove_srgb_suffix();
        if caps.formats.contains(&non_srgb) {
            non_srgb
        } else {
            config.view_formats = vec![non_srgb];
            srgb
        }
    } else {
        srgb
    };
    config.present_mode = wgpu::PresentMode::AutoVsync;
    config.desired_maximum_frame_latency = 2;
    let translucent = [wgpu::CompositeAlphaMode::PreMultiplied, wgpu::CompositeAlphaMode::PostMultiplied, wgpu::CompositeAlphaMode::Inherit]
        .into_iter().find(|mode| caps.alpha_modes.contains(mode));
    Some((config, translucent))
}

pub(crate) fn is_wayland(event_loop: &ActiveEventLoop) -> bool {
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
    return winit::platform::wayland::ActiveEventLoopExtWayland::is_wayland(event_loop);
    #[cfg(not(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))))]
    {
        let _ = event_loop;
        false
    }
}

/// A window's content top-left in points from the primary display's top-left (see
/// `El::on_screen_layout`), the space an overlay's `OverlayPlacement::Rect` on
/// `DisplayTarget::Primary` is in. Elsewhere than macOS that's desktop pixels from the
/// primary's origin over the window's scale, as `overlay::position` converts the other way.
/// Wayland hides it.
pub(crate) fn screen_origin(window: &Window) -> Option<(f32, f32)> {
    #[cfg(target_os = "macos")]
    return crate::macos::content_origin(window);
    #[cfg(not(target_os = "macos"))]
    {
        let p = window.inner_position().ok()?;
        let o = window.primary_monitor().map_or(winit::dpi::PhysicalPosition::new(0, 0), |m| m.position());
        let scale = window.scale_factor();
        Some(((f64::from(p.x - o.x) / scale) as f32, (f64::from(p.y - o.y) / scale) as f32))
    }
}

/// The scale an overlay draws at. On macOS, read from the panel AppKit shows, and before it's
/// ordered in from the display holding most of its frame, where it will appear.
pub(crate) fn backing_scale(window: &Window) -> f32 {
    #[cfg(target_os = "macos")]
    if let Some(scale) = crate::macos::backing_scale(window) { return scale; }
    window.scale_factor() as f32
}

/// Native titlebar behaviour the framework owns: traffic-light placement, drag regions and the
/// double-click action. Elsewhere only the portable parts apply.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn uses_drag_regions(chrome: Chrome, borderless: bool, panel: bool) -> bool {
    chrome.drag_regions && (borderless || chrome.full_size_content) && !panel
}

#[test]
fn a_window_s_first_updates_can_register_on_a_later_window() {
    use crate::{App, AppSpec, MenuItem, el};
    type S = Vec<String>;
    let apps: Vec<App<S>> = ["first", "second"].map(|title| App::new(AppSpec::new(title, (10.0, 10.0), Vec::new, |_, _| el()))).into();
    let second = apps[1].handle();
    apps[0].handle().update(move |s: &mut S, _| {
        s.push(format!("{:?}", second.register_shortcut("ctrl+alt+shift+f18".parse().unwrap(), |_, _| {}).map(|_| ())));
        s.push(format!("{:?}", second.set_menu([MenuItem::new("Item", |_: &mut S, _| {})])));
    });
    // As Host::new and Host::resumed: attach the guards, then drain the first updates.
    let hosts = start_all(apps.into(), |_, app| {
        let shortcuts = crate::shortcuts::Guard::attach(&app.shared, Arc::new(|| {}), true, false);
        let menus = crate::native_menu::Guard::attach(&app.shared, true);
        Some((app, shortcuts, menus, S::new()))
    }, |(app, _, _, state)| for f in app.shared.take().updates { f(state, &mut Tx::default()); });
    assert_eq!(hosts[0].as_ref().unwrap().3, ["Ok(())", "Ok(())"]);
}

#[test]
fn drag_region_policy_preserves_titled_windows_and_panels() {
    for enabled in [false, true] {
        for full_size in [false, true] {
            let chrome = Chrome { drag_regions: enabled, full_size_content: full_size, ..Chrome::DEFAULT };
            assert_eq!(uses_drag_regions(chrome, false, false), enabled && full_size);
            assert_eq!(uses_drag_regions(chrome, true, false), enabled);
            assert!(!uses_drag_regions(chrome, false, true));
            assert!(!uses_drag_regions(chrome, true, true));
        }
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
    use objc2::{msg_send, sel, ClassType};
    use objc2_app_kit::{NSEvent, NSPanel, NSWindowButton, NSWindowStyleMask};
    use objc2_foundation::{NSPoint, NSString, NSUserDefaults};
    use winit::window::Window;
    use crate::macos::{content_view, ns_window};

    unsafe extern "C" fn cannot_move_window(_: &AnyObject, _: Sel) -> Bool {
        Bool::NO
    }

    /// The content view stops AppKit from moving the window when the titlebar band is pressed;
    /// only `.window_drag()` regions move it. Install an equal-sized subclass on this instance,
    /// never a method on winit's shared class. Other windows keep their original class.
    pub fn own_window_dragging(window: &Window) {
        let Some(view) = content_view(window) else { return };
        let Some(win) = view.window() else { return };
        let panel: Bool = unsafe { msg_send![&*win, isKindOfClass: NSPanel::class()] };
        let full_size = win.styleMask().contains(NSWindowStyleMask::FullSizeContentView);
        let borderless = !win.styleMask().contains(NSWindowStyleMask::Titled);
        let chrome = super::Chrome { full_size_content: full_size, drag_regions: true, ..super::Chrome::DEFAULT };
        if !super::uses_drag_regions(chrome, borderless, panel.as_bool()) { return; }
        unsafe { scope_drag_regions(&view) };
    }

    /// Main-thread native views only. The subclass adds no ivars, preserving winit's layout,
    /// methods and deallocation. Registered classes live for the process lifetime.
    unsafe fn scope_drag_regions(view: &AnyObject) {
        let base = view.class();
        if base.name().starts_with("RuneDragRegions_") { return; }
        let name = format!("RuneDragRegions_{}", base.name());
        let class = match ClassBuilder::new(&name, base) {
            Some(mut builder) => {
                // SAFETY: the signature matches -[NSView mouseDownCanMoveWindow].
                unsafe { builder.add_method(sel!(mouseDownCanMoveWindow), cannot_move_window as unsafe extern "C" fn(_, _) -> _) };
                builder.register()
            }
            None => AnyClass::get(&name).expect("register Rune drag-region view"),
        };
        // SAFETY: called on a live view on the main thread. Only its isa changes, to a subclass
        // without ivars; the original class is never modified.
        unsafe { AnyObject::set_class(view, class) };
    }

    #[test]
    fn drag_subclass_changes_only_the_selected_instance() {
        // NSObject exercises the Objective-C class operation without constructing a window
        // or requiring an AppKit event loop on the test thread.
        use objc2::{ffi, rc::Retained};
        let selected = objc2_foundation::NSObject::new();
        let sibling = objc2_foundation::NSObject::new();
        unsafe {
            let selected_ptr = Retained::as_ptr(&selected).cast_mut().cast();
            let sibling_ptr = Retained::as_ptr(&sibling).cast_mut().cast();
            let base = ffi::object_getClass(selected_ptr);
            let selector = sel!(mouseDownCanMoveWindow).as_ptr();
            let original = ffi::class_getInstanceMethod(base, selector);
            scope_drag_regions(&selected);
            let scoped = ffi::object_getClass(selected_ptr);
            assert_ne!(scoped, base);
            assert_eq!(ffi::class_getSuperclass(scoped), base);
            assert_eq!(ffi::class_getInstanceSize(scoped), ffi::class_getInstanceSize(base));
            assert_eq!(ffi::object_getClass(sibling_ptr), base);
            assert_eq!(ffi::class_getInstanceMethod(base, selector), original);
            let moves: Bool = msg_send![&*selected, mouseDownCanMoveWindow];
            assert!(!moves.as_bool());
            scope_drag_regions(&selected);
            assert_eq!(ffi::object_getClass(selected_ptr), scoped);
        }
    }

    /// Electron's `trafficLightPosition`: the titlebar container grows to the buttons' height
    /// plus twice `y`, and the buttons sit `x, y` from the top-left at AppKit's own pitch.
    /// AppKit lays the titlebar out again on resize, so the shell calls this after each.
    pub fn place_traffic_lights(window: &Window, (x, y): (f32, f32)) {
        let Some(win) = ns_window(window) else { return };
        if win.styleMask().contains(NSWindowStyleMask::FullScreen) {
            return;
        }
        let buttons = [NSWindowButton::NSWindowCloseButton, NSWindowButton::NSWindowMiniaturizeButton, NSWindowButton::NSWindowZoomButton]
            .map(|b| win.standardWindowButton(b));
        let [Some(close), Some(mini), Some(zoom)] = buttons else { return };
        // SAFETY: AppKit's own view hierarchy, on the main thread.
        let Some(container) = (unsafe { close.superview().and_then(|v| v.superview()) }) else { return };
        let (x, y) = (f64::from(x), f64::from(y));
        let pitch = mini.frame().origin.x - close.frame().origin.x;
        let mut frame = container.frame();
        frame.size.height = close.frame().size.height + 2.0 * y;
        frame.origin.y = win.frame().size.height - frame.size.height;
        unsafe {
            container.setFrame(frame);
            for (i, b) in [close, mini, zoom].iter().enumerate() {
                b.setFrameOrigin(NSPoint::new(x + i as f64 * pitch, y));
            }
        }
    }

    pub fn double_click_interval() -> f64 {
        unsafe { NSEvent::doubleClickInterval() }
    }

    /// System Settings › Desktop & Dock › "Double-click a window's title bar to".
    pub fn titlebar_double_click(window: &Window) {
        // SAFETY: reading a user default on the main thread.
        let action = unsafe { NSUserDefaults::standardUserDefaults().stringForKey(&NSString::from_str("AppleActionOnDoubleClick")) };
        match action.map(|s| s.to_string()).as_deref() {
            Some("Minimize") => window.set_minimized(true),
            Some("None") => {}
            _ => window.set_maximized(!window.is_maximized()),
        }
    }
}

fn place_chrome(window: &Window, chrome: Chrome) {
    #[cfg(target_os = "macos")]
    if let Some(p) = chrome.traffic_lights {
        mac::place_traffic_lights(window, p);
    }
    let _ = (window, chrome);
}

fn titlebar_double_click(window: &Window) {
    #[cfg(target_os = "macos")]
    mac::titlebar_double_click(window);
    #[cfg(not(target_os = "macos"))]
    window.set_maximized(!window.is_maximized());
}

pub(crate) fn map_mods(m: ModifiersState) -> Mods {
    Mods { shift: m.shift_key(), cmd: if cfg!(target_os = "macos") { m.super_key() } else { m.control_key() },
        alt: m.alt_key(), ctrl: m.control_key(), logo: m.super_key() }
}

pub(crate) fn map_key(key: &WKey, m: ModifiersState) -> Option<KeyEvent> {
    let mods = map_mods(m);
    let key = match key {
        WKey::Named(n) => match n {
            NamedKey::Enter => Key::Enter,
            NamedKey::Backspace => Key::Backspace,
            NamedKey::Delete => Key::Delete,
            NamedKey::ArrowLeft => Key::Left,
            NamedKey::ArrowRight => Key::Right,
            NamedKey::ArrowUp => Key::Up,
            NamedKey::ArrowDown => Key::Down,
            NamedKey::Home => Key::Home,
            NamedKey::End => Key::End,
            NamedKey::Escape => Key::Escape,
            NamedKey::Tab => Key::Tab,
            NamedKey::Space => Key::Text(" ".into()),
            NamedKey::F1 => Key::Function(1),
            NamedKey::F2 => Key::Function(2),
            NamedKey::F3 => Key::Function(3),
            NamedKey::F4 => Key::Function(4),
            NamedKey::F5 => Key::Function(5),
            NamedKey::F6 => Key::Function(6),
            NamedKey::F7 => Key::Function(7),
            NamedKey::F8 => Key::Function(8),
            NamedKey::F9 => Key::Function(9),
            NamedKey::F10 => Key::Function(10),
            NamedKey::F11 => Key::Function(11),
            NamedKey::F12 => Key::Function(12),
            NamedKey::F13 => Key::Function(13),
            NamedKey::F14 => Key::Function(14),
            NamedKey::F15 => Key::Function(15),
            NamedKey::F16 => Key::Function(16),
            NamedKey::F17 => Key::Function(17),
            NamedKey::F18 => Key::Function(18),
            NamedKey::F19 => Key::Function(19),
            NamedKey::F20 => Key::Function(20),
            NamedKey::F21 => Key::Function(21),
            NamedKey::F22 => Key::Function(22),
            NamedKey::F23 => Key::Function(23),
            NamedKey::F24 => Key::Function(24),
            _ => return None,
        },
        WKey::Character(c) => {
            if mods.cmd {
                Key::Command(c.chars().next()?.to_ascii_lowercase())
            } else {
                Key::Text(c.to_string())
            }
        }
        _ => return None,
    };
    Some(KeyEvent { key, mods })
}

impl<S: 'static> Host<S> {
    fn launcher(&self) -> bool { cfg!(target_os = "macos") && self.macos_window.launcher }

    fn reveal_launcher(&mut self, event_loop: &ActiveEventLoop) {
        self.apply_placement(event_loop);
        if self.win.is_none() { self.create(event_loop, None, false); }
        let stale = self.rt.needs_commit();
        let Some(w) = &mut self.win else { return };
        w.hidden_pace.reset();
        let visible = w.visible;
        if !w.first_frame.request_reveal(stale, visible) {
            w.window.request_redraw();
            return;
        }
        // Already showing: its next frame draws the new state, or a covered one's retry does.
        if visible && stale { w.window.request_redraw(); }
        self.show_cached();
    }

    fn show_cached(&mut self) {
        let Some(w) = &mut self.win else { return };
        w.show(true);
        w.visible = true;
        w.occluded = false;
        self.rt.set_live_visible(true);
        // No settle, frame, surface configure or acquisition on a cached reveal, unless
        // the refreshed frame left motion running.
        if !self.idle { w.window.request_redraw(); }
    }

    fn first_frame_ready(&mut self, event_loop: &ActiveEventLoop) {
        let Some(w) = &mut self.win else { return };
        if !w.first_frame.pending() { return; }
        let reveal = w.first_frame.complete();
        self.native_handle.native.ready.store(true, Ordering::Release);
        *self.shared.window_error.lock().unwrap() = None;
        if !std::mem::replace(&mut w.first_frame.notified, true)
            && let Some(ready) = self.on_ready.as_mut() { ready(&self.native_handle); }
        if reveal { self.reveal_launcher(event_loop); }
        self.publish();
    }

    fn place(&mut self, event_loop: &ActiveEventLoop, request: crate::windows::PlaceRequest) {
        self.options.position = None;
        self.placement = Some(request);
        self.apply_placement(event_loop);
    }

    fn apply_placement(&mut self, event_loop: &ActiveEventLoop) {
        let (Some(request), Some(w)) = (&self.placement, &self.win) else { return };
        if is_wayland(event_loop) { return; }
        #[cfg(target_os = "macos")]
        {
            let _ = request.pointer;
            crate::macos::place_window(&w.window, self.rt.viewport, request);
        }
        #[cfg(not(target_os = "macos"))]
        if let Some(monitor) = crate::overlay::monitor_at(event_loop, &request.display, request.pointer) {
            let scale = monitor.scale_factor();
            let area = monitor.size();
            let rect = crate::overlay_options::local_rect(crate::windows::PlaceRequest::at(self.rt.viewport), Some(request.placement),
                (area.width as f32 / scale as f32, area.height as f32 / scale as f32));
            let origin = monitor.position();
            let _ = w.window.request_inner_size(LogicalSize::new(rect[2], rect[3]));
            w.window.set_outer_position(PhysicalPosition::new(origin.x + (f64::from(rect[0]) * scale).round() as i32,
                origin.y + (f64::from(rect[1]) * scale).round() as i32));
        }
        self.sync_origin();
    }

    /// Seconds on the host's frame clock, which frame animations sample.
    fn frame_secs(&self, at: Instant) -> f64 {
        self.rt.wall_start.map_or(0.0, |start| at.saturating_duration_since(start).as_secs_f64())
    }

    /// Takes the frame change handlers asked for. A painting macOS window moves over its next
    /// frames on the request's spring, from `start`; anywhere else the frame jumps. Sizes stay
    /// inside the window's minimum and maximum. A size outlives the window: one created later
    /// opens at it.
    fn take_frame_request(&mut self, start: f64, painting: bool) {
        self.rt.fit_held = false;
        let Some(mut request) = self.rt.window_frame.take() else { return };
        let (min, max) = self.options.size_limits(self.spec.min_size);
        request.size = request.size.map(|size| crate::window_frame::clamp_size(size, min, max));
        if let Some(size) = request.size {
            self.spec.size = size;
            self.args.size = None;
        }
        let Some(w) = &self.win else { return };
        #[cfg(target_os = "macos")]
        {
            use crate::window_frame::{FrameAnim, snap, target};
            let Some((frame, visible, scale)) = crate::macos::window_geometry(&w.window) else { return };
            // Resizing again mid-flight keeps the edge the first resize keeps.
            let base = self.frame_anim.map_or(frame, |a| a.to);
            let outer = request.size.and_then(|size| crate::macos::outer_size(&w.window, size));
            let to = snap(target(base, outer, request.center, visible), scale);
            // Centring alone carries on with a resize already moving.
            let spring = request.spring.or(self.frame_anim.and_then(|a| a.spring).filter(|_| request.size.is_none()));
            if painting {
                self.frame_anim = Some(FrameAnim::retarget(self.frame_anim.as_ref(), spring, start, frame, to));
                self.frame_origin = Some([frame[0], frame[1]]);
            } else {
                self.frame_anim = None;
                crate::macos::set_window_frame(&w.window, to);
                self.sync_origin();
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (start, painting);
            let window = &w.window;
            // Read before the resize request, which may apply at once.
            let geometry = (window.current_monitor(), window.outer_position(), window.outer_size(), window.inner_size());
            if let Some((width, height)) = request.size {
                self.frame_jump = Some((width, height));
                let _ = window.request_inner_size(LogicalSize::new(width, height));
            }
            // Wayland windows can't see or set their position.
            if self.wayland { return; }
            let (Some(monitor), Ok(position), outer, inner) = geometry else { return };
            let scale = window.scale_factor() as f32;
            let size = request.size.map(|(width, height)| (
                width * scale + outer.width.saturating_sub(inner.width) as f32,
                height * scale + outer.height.saturating_sub(inner.height) as f32,
            ));
            let current = [position.x as f32, position.y as f32, outer.width as f32, outer.height as f32];
            let (origin, area) = (monitor.position(), monitor.size());
            let visible = [origin.x as f32, origin.y as f32, area.width as f32, area.height as f32];
            let [x, y, ..] = crate::window_frame::target(current, size, request.center, visible);
            if [x, y] != [current[0], current[1]] {
                window.set_outer_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
                self.sync_origin();
            }
        }
    }

    /// Moves the frame one display frame along, before the frame's drawable is acquired, and
    /// lays the content out at the new size: content follows the edge in the same frame, with
    /// no stretched or blank strip.
    fn step_frame(&mut self, at: Instant) {
        let now = self.frame_secs(at);
        self.take_frame_request(now, true);
        #[cfg(target_os = "macos")]
        {
            use crate::window_frame::snap;
            // The window server shows a move on the first frame after an idle spell before the
            // frame drawn for it, leaving a stale strip. A request taken then starts here and
            // moves from the next frame.
            let resting = self.idle;
            let moving = self.frame_anim.is_some();
            let (Some(w), Some((gpu, ..))) = (self.win.as_mut(), self.gpu.as_ref()) else { return };
            w.present_with_transaction(moving);
            if resting { return; }
            let Some(anim) = self.frame_anim.as_mut() else { return };
            let Some((frame, _, scale)) = crate::macos::window_geometry(&w.window) else { return };
            // Someone dragged the window mid-flight: the drag wins. The window stays where they
            // put it and the size carries on from there.
            if let Some([x, y]) = self.frame_origin && ((frame[0] - x).abs() > 0.5 || (frame[1] - y).abs() > 0.5) {
                anim.shift(frame[0] - x, frame[1] - y);
            }
            let rect = snap(anim.sample(now).0, scale);
            crate::macos::set_window_frame(&w.window, rect);
            self.frame_origin = Some([rect[0], rect[1]]);
            if anim.done(now) { self.frame_anim = None; }
            place_chrome(&w.window, self.chrome);
            let size = w.window.inner_size();
            w.resize_surface(&gpu.device, size);
            let scale = w.window.scale_factor() as f32;
            self.rt.step_viewport((size.width as f32 / scale, size.height as f32 / scale), scale);
            // In this frame's layout, not the next one when `Moved` arrives.
            self.sync_origin();
        }
    }

    /// A window that stops painting mid-move lands where it was going.
    fn land_frame(&mut self) {
        let anim = self.frame_anim.take();
        #[cfg(target_os = "macos")]
        if let Some(w) = &mut self.win {
            w.present_with_transaction(false);
            if let Some(anim) = anim { crate::macos::set_window_frame(&w.window, anim.to); }
        }
        #[cfg(target_os = "macos")]
        self.sync_origin();
        #[cfg(not(target_os = "macos"))]
        let _ = anim;
    }

    fn window_request(&mut self, event_loop: &ActiveEventLoop, request: WindowRequest) {
        match request {
            WindowRequest::Open(token) => self.open(event_loop, token),
            WindowRequest::Show => self.show(event_loop),
            WindowRequest::Hide => self.hide(event_loop),
            WindowRequest::Close => self.close(event_loop),
        }
    }

    fn set_position(&mut self, event_loop: &ActiveEventLoop, position: (i32, i32)) {
        self.placement = None;
        self.options.position = Some(position);
        if !is_wayland(event_loop) && let Some(w) = &self.win {
            w.window.set_outer_position(PhysicalPosition::new(position.0, position.1));
        }
        self.sync_origin();
    }

    fn set_always_on_top(&mut self, event_loop: &ActiveEventLoop, on: bool) {
        self.options.always_on_top = on;
        self.macos_window.level = None;
        if !is_wayland(event_loop) && let Some(w) = &self.win {
            w.window.set_window_level(if on { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal });
        }
    }

    fn show(&mut self, event_loop: &ActiveEventLoop) {
        if self.launcher() { self.reveal_launcher(event_loop); return; }
        let built = self.win.as_ref().is_some_and(|w| !w.visible) && self.build_for_show();
        self.apply_placement(event_loop);
        let Some(w) = self.win.as_mut() else { self.create(event_loop, None, false); return };
        if !w.visible {
            w.show(false);
        }
        w.visible = true;
        w.occluded = false;
        self.rt.set_live_visible(!w.window.is_minimized().unwrap_or(false));
        self.idle = true;
        self.native_handle.native.number.store(native_window_number(&w.window).unwrap_or(0), Ordering::Release);
        if !built { self.rt.settle_all(); }
        w.window.request_redraw();
    }

    /// A hidden or released window that fits its content builds what changed while it was
    /// away and takes its new height before it is ordered in (`Runtime::build_for_show`).
    /// Returns whether it built, settled.
    fn build_for_show(&mut self) -> bool {
        if !self.rt.build_for_show() { return false; }
        self.clipboard.flush(&mut self.rt);
        self.take_frame_request(0.0, false);
        // macOS sets the frame at once: the first drawable and layout take the new size.
        // Elsewhere the window's `Resized` brings it.
        #[cfg(target_os = "macos")]
        if let (Some(w), Some((gpu, ..))) = (self.win.as_mut(), self.gpu.as_ref()) {
            let size = w.window.inner_size();
            w.resize_surface(&gpu.device, size);
            let scale = w.window.scale_factor() as f32;
            self.rt.set_viewport((size.width as f32 / scale, size.height as f32 / scale), scale);
        }
        true
    }

    fn focus(&self, event_loop: &ActiveEventLoop) {
        let Some(w) = self.win.as_ref().filter(|w| w.visible) else { return };
        if w.window.is_minimized() == Some(true) { w.window.set_minimized(false); }
        if is_wayland(event_loop) {
            w.window.request_user_attention(Some(winit::window::UserAttentionType::Informational));
            return;
        }
        w.focus();
    }

    /// Applies everything other threads asked for since the last wake-up. A `wake` polls a
    /// hidden window first, so a reveal in the same turn sees what `poll` changed.
    fn drain(&mut self, event_loop: &ActiveEventLoop, wake: bool) {
        let shown = self.win.as_ref().is_some_and(|w| w.visible);
        let stale_before = self.rt.needs_commit();
        let clipboard = self.clipboard.receive(&mut self.rt);
        let answered = self.popup.answer(&mut self.rt);
        self.shortcuts.drain();
        let mut requests = self.shared.take();
        self.menus.drain();
        let updates = std::mem::take(&mut requests.updates);
        let had_updates = !updates.is_empty();
        let mut updated = false;
        for f in updates {
            updated |= self.rt.update_if(f);
            self.menus.drain();
        }
        let changed = clipboard || answered || updated || self.rt.drag_ends_pending();
        // Only updates that changed nothing (`AppHandle::update_if`) woke it: no poll, no frame.
        let quiet = had_updates && !changed && requests.only_updates() && !self.rt.patch_pending()
            && (stale_before || !self.rt.needs_commit());
        self.clipboard.flush(&mut self.rt);
        if requests.quit {
            // A tracking menu holds the event loop until it ends.
            self.popup.cancel(&mut self.rt);
            event_loop.exit();
            return;
        }
        #[cfg(target_os = "macos")]
        if let Some(visible) = requests.dock {
            crate::macos::set_dock_icon(visible);
        }
        let polled = wake && !quiet && self.poll_hidden(event_loop);
        if let Some(position) = requests.position { self.set_position(event_loop, position); }
        if let Some(place) = requests.place { self.place(event_loop, place); }
        if let Some(request) = requests.window { self.window_request(event_loop, request); }
        if requests.focus { self.focus(event_loop); }
        self.shell(event_loop);
        // A pop-up a quiet update asked for may have been answered at once, running its callback.
        let answered = quiet && !stale_before && self.rt.needs_commit();
        if quiet { self.clipboard.flush(&mut self.rt); }
        let (changed, quiet) = (changed || answered, quiet && !answered);
        #[cfg(all(feature = "dev", debug_assertions))]
        if self.rt.dev.is_some() && self.win.as_ref().is_none_or(|w| !w.visible || w.occluded || w.window.is_minimized().unwrap_or(false)) {
            // A hidden/windowless app still services its dev channel without a GPU or redraw.
            if self.launcher() && let Some(w) = &self.win { w.window.request_redraw(); }
            else { self.rt.dev_hidden_frame(); self.shell(event_loop); }
        }
        // A dev command may change a shown launcher, which otherwise redraws only for updates.
        #[cfg(all(feature = "dev", debug_assertions))]
        let (changed, quiet) = (changed || self.rt.dev.is_some(), quiet && self.rt.dev.is_none());
        // A launcher's cached reveal draws nothing; a hidden one renders its changes hidden,
        // paced (`refresh_hidden`). A covered or minimised one runs them without asking for a
        // frame it wouldn't draw.
        let launcher = self.launcher();
        let framed = match self.win.as_ref().filter(|w| drain_frame(launcher, w.visible, shown, changed, quiet)) {
            None => false,
            Some(w) if launcher && !w.visible => self.refresh_hidden(),
            Some(w) if (!launcher && w.occluded) || w.window.is_minimized().unwrap_or(false) => {
                self.covered_frame();
                self.shell(event_loop);
                true
            }
            Some(w) => { w.window.request_redraw(); true }
        };
        // Neither a poll nor a frame comes to schedule the updates' `wake_after`s.
        if !polled && !framed { self.rt.schedule_pending_wakes(); }
    }

    /// A hidden, keep-running or windowless window still reads its inbox when woken: `poll` runs
    /// with no drawable, present or animation, and its changes render when the window is next
    /// shown. A hidden launcher renders poll's changes hidden, paced, keeping its cached frame
    /// fresh. Returns whether it polled.
    fn poll_hidden(&mut self, event_loop: &ActiveEventLoop) -> bool {
        if event_loop.exiting() || self.win.as_ref().is_some_and(|w| w.visible) { return false; }
        // Its hidden frames already poll.
        #[cfg(all(feature = "dev", debug_assertions))]
        if self.rt.dev.is_some() { return false; }
        let changed = self.rt.poll_hidden();
        self.clipboard.flush(&mut self.rt);
        self.shell(event_loop);
        if changed && self.launcher() { self.refresh_hidden(); }
        true
    }

    /// A hidden launcher renders what changed into its cached frame: at once after a quiet
    /// spell, then at most every `HIDDEN_REFRESH` (`HiddenPace`). A show of state newer than
    /// that frame renders it hidden first (`reveal_launcher`), as for an update in the same
    /// turn. A frame being prepared, or one a reveal waits on, renders at once. Returns whether
    /// it asked for a frame.
    fn refresh_hidden(&mut self) -> bool {
        let now_due = self.hidden_launcher_renders();
        let Some(w) = &mut self.win else { return false };
        let render = now_due || w.first_frame.deadline.is_some() || w.hidden_pace.changed(Instant::now());
        if render { w.window.request_redraw(); }
        render
    }

    /// What a hidden launcher's frame must render as it commits: queued input, drag ends and
    /// dev commands. Committed without a render, they'd leave a cached frame that looks fresh.
    fn hidden_launcher_renders(&self) -> bool {
        #[cfg(all(feature = "dev", debug_assertions))]
        if self.rt.dev.is_some() { return true; }
        self.rt.has_pending_input() || self.rt.drag_ends_pending()
    }

    /// Carries out what a handler or update asked with `Tx::close_window` / `Tx::quit`, then
    /// opens the pop-up menu it asked for, if the window is still there to open it over.
    fn shell(&mut self, event_loop: &ActiveEventLoop) {
        for _ in 0..8 {
            match self.rt.shell_request.take() {
                Some(Shell::Quit) => {
                    self.popup.cancel(&mut self.rt);
                    event_loop.exit();
                }
                Some(Shell::Close) => self.close(event_loop),
                None => {}
            }
            if self.finished || event_loop.exiting() {
                self.popup.cancel(&mut self.rt);
                return;
            }
            let shown = self.win.as_ref().filter(|w| w.visible);
            let paints = shown.is_some_and(|w| !w.occluded && !w.window.is_minimized().unwrap_or(false));
            // A painting macOS window moves its frame over its next frames (`redraw`).
            if self.rt.window_frame.is_some() && !(paints && cfg!(target_os = "macos")) {
                self.take_frame_request(self.frame_secs(Instant::now()), false);
            }
            let shown = self.win.as_ref().filter(|w| w.visible);
            let window = shown.map(|w| &w.window);
            if self.popup.open(&mut self.rt, window, paints) && let Some(w) = window { w.request_redraw(); }
            // A callback that ran (a menu that couldn't open) may have asked to close or quit.
            if self.rt.shell_request.is_none() { return; }
        }
    }

    fn close(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(w) = &mut self.win { w.first_frame.hide(); }
        self.secure_input.set(false);
        if !self.closes() { return; }
        self.close_hook();
        self.rt.set_live_visible(false);
        // While the window still shows its last frames.
        if let Some(trace) = &mut self.trace { trace.settle_presents(crate::present_time::SETTLE); }
        if self.rt.shell_request.take() == Some(Shell::Quit) {
            self.popup.cancel(&mut self.rt);
            event_loop.exit();
            return;
        }
        if !self.keep_running {
            self.popup.cancel(&mut self.rt);
            self.rt.cancel_pointer();
            if let Some(w) = &mut self.win { w.apply_cursor(&self.rt, false); w.hide(); }
            self.finished = true;
            return;
        }
        self.hide(event_loop);
    }

    /// Whether a close runs `on_close`: a window is shown, or hidden without `keep_running`.
    fn closes(&self) -> bool { self.win.as_ref().is_some_and(|w| w.visible || !self.keep_running) }

    fn close_hook(&mut self) {
        if let Some(f) = self.on_close {
            self.rt.update(f);
        }
        // Before the window hides or the app exits: a lock or clear must reach the worker.
        self.clipboard.flush(&mut self.rt);
    }

    fn hide(&mut self, event_loop: &ActiveEventLoop) {
        self.land_frame();
        // Before the window can go: a hide need not report a blur. `about_to_wait` delivers it,
        // after any Fn change the monitor saw first.
        #[cfg(target_os = "macos")]
        self.fn_key();
        self.rt.keys_lost();
        if let Some(w) = &mut self.win { w.first_frame.hide(); }
        self.secure_input.set(false);
        self.ime_allowed = false;
        if let Some(w) = &self.win { w.window.set_ime_allowed(false); }
        self.rt.set_live_visible(false);
        self.popup.cancel(&mut self.rt);
        self.rt.cancel_pointer();
        if let Some(w) = &mut self.win { w.apply_cursor(&self.rt, false); }
        self.mods = ModifiersState::empty();
        if let Some(w) = &self.win { w.hide(); }
        let launcher = self.launcher();
        if self.release_gpu && !launcher {
            self.win = None;
            self.native_handle.native.ready.store(false, Ordering::Release);
            self.native_handle.native.number.store(0, Ordering::Release);
            self.gpu = None;
            self.rt.text.atlas.invalidate();
            self.rt.images.invalidate();
        } else if is_wayland(event_loop) {
            // Wayland has no hidden toplevel: unmap by destroying, recreate on open.
            self.win = None;
            self.native_handle.native.ready.store(false, Ordering::Release);
            self.native_handle.native.number.store(0, Ordering::Release);
        } else if let Some(w) = self.win.as_mut() {
            w.stop_pacing();
            w.visible = false;
            w.retry = None;
            // Its hidden frame delivers the focus loss.
            if launcher { w.window.request_redraw(); }
        }
    }

    fn open(&mut self, event_loop: &ActiveEventLoop, token: Option<String>) {
        if self.launcher() { self.reveal_launcher(event_loop); return; }
        let built = self.win.as_ref().is_some_and(|w| !w.visible) && self.build_for_show();
        self.apply_placement(event_loop);
        // The launcher's startup token belongs to the first window only.
        #[allow(unused_mut)]
        let mut from_env = None;
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        if !self.shown {
            use winit::platform::startup_notify::{EventLoopExtStartupNotify, reset_activation_token_env};
            from_env = event_loop.read_token_from_env().map(|t| t.into_raw());
            reset_activation_token_env();
        }
        let token = token.or(from_env);
        let Some(w) = self.win.as_mut() else {
            self.create(event_loop, token, true);
            // On macOS a new window becomes key without activating the app, so activate it
            // like a reopen does. The launch open leaves activation to the platform.
            #[cfg(target_os = "macos")]
            if self.started && let Some(w) = self.win.as_ref().filter(|w| w.visible) { w.focus(); }
            return;
        };
        if !w.visible {
            w.show(false);
            w.visible = true;
            if !built { self.rt.settle_all(); }
            self.idle = true;
        }
        if w.window.is_minimized() == Some(true) {
            w.window.set_minimized(false);
        }
        w.occluded = false;
        self.rt.set_live_visible(true);
        self.native_handle.native.number.store(native_window_number(&w.window).unwrap_or(0), Ordering::Release);
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        if is_wayland(event_loop) {
            match token {
                Some(token) => {
                    if w.activator.is_none() {
                        w.activator = crate::activation::Activator::new(&w.window);
                    }
                    if let Some(a) = w.activator.as_mut() {
                        a.activate(&token);
                    }
                }
                // Without a token the compositor may only mark the window urgent.
                None => w.window.request_user_attention(Some(winit::window::UserAttentionType::Informational)),
            }
            w.window.request_redraw();
            return;
        }
        let _ = token;
        w.focus();
        w.window.request_redraw();
    }

    /// Creates the window, or records why it can't be and leaves the app windowless; a later
    /// open tries again.
    fn create(&mut self, event_loop: &ActiveEventLoop, token: Option<String>, focus: bool) {
        let result = self.try_create(event_loop, token, focus);
        if let Err(error) = &result {
            eprintln!("rune: could not create the window {}: {error}", self.native_handle.key());
        }
        *self.shared.window_error.lock().unwrap() = result.err();
    }

    #[cfg_attr(not(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))), allow(unused_variables))]
    fn try_create(&mut self, event_loop: &ActiveEventLoop, token: Option<String>, focus: bool) -> Result<(), String> {
        // A released window reopens at the height its content reached while it was gone.
        let built = self.shown && self.build_for_show();
        let size = self.args.size.unwrap_or(self.spec.size);
        let launcher = self.launcher();
        let visible = !launcher && (self.started || self.options.visible);
        #[allow(unused_mut)]
        let mut attrs = self.options.attributes(self.spec.title, size, self.spec.min_size, visible).with_active(focus);
        #[cfg(target_os = "macos")]
        if self.macos_window.nonactivating_panel { attrs = attrs.with_visible(false).with_active(false); }
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
        {
            if let Some(id) = crate::tooling::app_id() {
                attrs = winit::platform::wayland::WindowAttributesExtWayland::with_name(attrs, id, id);
                attrs = winit::platform::x11::WindowAttributesExtX11::with_name(attrs, id, id);
            }
            if let Some(token) = token {
                let token = winit::window::ActivationToken::from_raw(token);
                attrs = winit::platform::startup_notify::WindowAttributesExtStartupNotify::with_activation_token(attrs, token);
            }
        }
        let chrome = self.chrome;
        #[cfg(target_os = "macos")]
        if !self.options.borderless && chrome.full_size_content {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attrs = attrs.with_titlebar_transparent(true).with_title_hidden(true).with_fullsize_content_view(true);
        }
        let window = Arc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);
        #[cfg(target_os = "macos")]
        let panel = if self.macos_window.nonactivating_panel {
            Some(crate::macos::Panel::adopt_window(&window, size, &self.macos_window).ok_or("could not create nonactivating panel")?)
        } else { None };
        #[cfg(target_os = "macos")]
        crate::macos::configure_window(&window, &self.macos_window);
        if self.options.click_through && let Err(error) = window.set_cursor_hittest(false) {
            eprintln!("click-through unavailable for {}: {error}", self.native_handle.key());
        }
        window.set_ime_allowed(false);
        self.rt.ime_reset = true;
        #[cfg(target_os = "macos")]
        if uses_drag_regions(chrome, self.options.borderless, false) {
            mac::own_window_dragging(&window);
        }
        place_chrome(&window, chrome);
        #[cfg(target_os = "macos")]
        {
            self.rt.double_click_interval = mac::double_click_interval();
            self.rt.set_native_help(true);
        }
        #[cfg(windows)]
        {
            let ms = unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() };
            self.rt.double_click_interval = f64::from(ms) / 1000.0;
            // A tooltip control's initial delay is the double-click time.
            self.rt.help_delay = self.rt.double_click_interval;
        }
        let (gpu, surface) = match self.gpu.take() {
            Some((gpu, renderer, format)) => {
                let surface = gpu.instance.create_surface(window.clone());
                self.gpu = Some((gpu, renderer, format));
                (None, surface.map_err(|e| e.to_string())?)
            }
            None => {
                let cached = self.gpu_pool.borrow().upgrade();
                let (gpu, surface) = if let Some(gpu) = cached {
                    let surface = gpu.instance.create_surface(window.clone()).map_err(|e| e.to_string())?;
                    (gpu, surface)
                } else {
                    let instance = Gpu::instance();
                    let surface = instance.create_surface(window.clone()).map_err(|e| e.to_string())?;
                    let features = self.comp.as_ref().map_or(wgpu::Features::empty(), |c| c.features());
                    let gpu = Arc::new(Gpu::try_with_features(instance, Some(&surface), features)?);
                    *self.gpu_pool.borrow_mut() = Arc::downgrade(&gpu);
                    (gpu, surface)
                };
                (Some(gpu), surface)
            }
        };
        let adapter = &gpu.as_ref().or(self.gpu.as_ref().map(|g| &g.0)).unwrap().adapter;
        let phys = window.inner_size();
        let (mut config, translucent) = surface_config(&surface, adapter, (phys.width, phys.height), self.rt.srgb_blend())
            .ok_or("the GPU adapter can't present to this window")?;
        if self.options.transparent {
            config.alpha_mode = translucent.unwrap_or_else(|| {
                eprintln!("transparency unavailable for {}; using an opaque window", self.native_handle.key());
                wgpu::CompositeAlphaMode::Opaque
            });
            self.rt.transparent = config.alpha_mode != wgpu::CompositeAlphaMode::Opaque;
        }
        let target_format = if self.rt.srgb_blend() { config.format.remove_srgb_suffix() } else { config.format };
        if let Some(gpu) = gpu {
            let mut renderer = Renderer::new(&gpu.device, target_format);
            renderer.set_srgb_blend(self.rt.srgb_blend());
            self.gpu = Some((gpu, renderer, target_format));
        }
        let (gpu, renderer, format) = self.gpu.as_mut().unwrap();
        if *format != target_format {
            *renderer = Renderer::new(&gpu.device, target_format);
            renderer.set_srgb_blend(self.rt.srgb_blend());
            *format = target_format;
            self.rt.text.atlas.invalidate();
            self.rt.images.invalidate();
        }
        renderer.set_srgb_blend(self.rt.srgb_blend());
        surface.configure(&gpu.device, &config);
        // Metal's PostMultiplied mode only makes CAMetalLayer non-opaque; Core Animation
        // still composites premultiplied pixels. Other backends follow wgpu's straight-alpha contract.
        let alpha = (self.options.transparent && config.alpha_mode == wgpu::CompositeAlphaMode::PostMultiplied
            && gpu.adapter.get_info().backend != wgpu::Backend::Metal)
            .then(|| crate::surface_alpha::AlphaSurface::new(&gpu.device, target_format, (config.width, config.height)));
        let scale = window.scale_factor() as f32;
        let dark = match self.args.theme.as_deref() {
            Some("light") => false,
            Some("dark") => true,
            _ => window.theme().is_none_or(|t| t == Theme::Dark),
        };
        self.rt.set_system_dark(dark);
        self.rt.set_viewport((phys.width as f32 / scale, phys.height as f32 / scale), scale);
        if (self.shown || launcher) && !built {
            // Reopening paints the settled state in the first frame (guarantee 8).
            self.rt.settle_all();
        }
        self.shown = true;
        self.idle = true;
        self.rt.set_live_visible(visible);
        if visible || launcher { window.request_redraw(); }
        *self.native_handle.native.window.lock().unwrap() = Arc::downgrade(&window);
        self.native_handle.native.number.store(native_window_number(&window).unwrap_or(0), Ordering::Release);
        #[cfg(target_os = "macos")]
        let present = self.present_log.clone().and_then(|log| crate::present_time::metal::LayerHook::attach(&surface, log));
        #[cfg(target_os = "macos")]
        if self.present_log.is_some() && present.is_none() { eprintln!("rune: trace: this surface reports no present times"); }
        #[cfg(target_os = "macos")]
        let fn_key = if self.rt.modifier_keys() { crate::fn_key::FnKey::new(&window) } else { None };
        self.win = Some(Win {
            first_frame: FirstFrame::default(),
            #[cfg(target_os = "macos")]
            pacing: crate::display_link::DisplayLink::new(&window, self.frame_wake.clone()),
            #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
            activator: None,
            #[cfg(target_os = "macos")]
            present,
            surface,
            config,
            #[cfg(target_os = "macos")]
            panel,
            #[cfg(target_os = "macos")]
            materials: crate::system_material::macos::Materials::new(),
            window,
            visible,
            occluded: false,
            retry: None,
            occluded_wait: OCCLUDED_RETRY,
            hidden_wait: TIMEOUT_RETRY,
            hidden_pace: HiddenPace::default(),
            cursor: crate::os_cursor::OsCursor::new(event_loop),
            alpha,
            #[cfg(target_os = "macos")]
            help: Default::default(),
            #[cfg(target_os = "macos")]
            fn_key,
        });
        if launcher { self.win.as_mut().unwrap().first_frame.prepare(); }
        self.apply_placement(event_loop);
        self.sync_origin();
        #[cfg(target_os = "macos")]
        if visible && let Some(w) = &self.win && w.panel.is_some() { w.show(focus); }
        if let Some(created) = self.on_created.take() { created(self.native_handle.clone()); }
        if let Some(on_window) = self.on_window.as_mut() { on_window(&self.native_handle); }
        Ok(())
    }

    /// Tells the runtime where the window's content is on screen, drawing a frame when that
    /// moves an `El::on_screen_layout` element.
    fn sync_origin(&mut self) {
        let Some(w) = &self.win else { return };
        if let Some(origin) = screen_origin(&w.window) && self.rt.set_window_origin(origin) {
            w.window.request_redraw();
        }
    }

    /// Publishes what `AppHandle::is_visible` reports.
    fn publish(&self) {
        let visible = !self.finished && self.win.as_ref().is_some_and(|w| w.visible);
        self.shared.visible.store(visible, Ordering::Release);
    }

    /// A shown frame with no drawable. Asking for a redraw here would turn the loop at once, for
    /// as long as the window is occluded or its displays sleep, so it waits for `retry`, with
    /// its display link stopped so no tick redraws sooner. `stepped` when this frame already
    /// ran the runtime.
    fn missed_frame(&mut self, missed: Missed, stepped: bool) {
        let Some(l) = self.win.as_mut() else { return };
        let now = Instant::now();
        let wait = l.occluded_wait;
        if missed == Missed::Occluded {
            // Covered or asleep with no notification yet: rest as if one came, and look
            // again on `retry`.
            l.occluded = true;
            l.occluded_wait = (wait * 2).min(OCCLUDED_MAX);
            self.rt.set_live_visible(false);
            self.secure_input.set(false);
            self.idle = true;
        }
        l.retry = match missed.next(l.paced(), now, wait) {
            NextFrame::WaitUntil(at) => Some(at),
            NextFrame::Idle => None,
        };
        if l.retry.is_some() { l.stop_pacing(); }
        self.meter.set_pacing(l.pacing_state(missed != Missed::Occluded));
        // What this frame would have run (updates, poll, due timers) still runs, without
        // drawing. Misses come at the retry's pace, not the display's.
        if !stepped { self.rt.hidden_timer_frame(); }
        self.clipboard.flush(&mut self.rt);
    }

    /// A frame for a window that's hidden, covered, asleep or minimised: what it would have
    /// run (updates, poll, due timers) still runs while shown, without drawing.
    fn covered_frame(&mut self) {
        let Some(l) = self.win.as_mut() else { return };
        l.stop_pacing();
        let shown = l.visible;
        self.land_frame();
        if shown {
            self.rt.hidden_timer_frame();
            self.clipboard.flush(&mut self.rt);
        }
    }

    fn redraw(&mut self) {
        let launcher = self.launcher();
        let (Some(l), Some(_)) = (self.win.as_mut(), self.gpu.as_ref()) else { return };
        let hidden = launcher && !l.visible;
        if (!launcher && (!l.visible || l.occluded)) || l.window.is_minimized().unwrap_or(false) {
            return self.covered_frame();
        }
        // A missed frame's retry, or the timer pacing unpaced motion: input waits for it, and
        // `new_events` runs due timers meanwhile.
        if l.backing_off(Instant::now()) { return; }
        // Starting the display link for a window that isn't on screen would wait for a tick that
        // sleeping displays never send, and a running link holds back its timers (`wake_deadline`).
        #[cfg(target_os = "macos")]
        if !hidden && l.natively_occluded() {
            self.meter.skipped();
            self.missed_frame(Missed::Occluded, false);
            return;
        }
        #[cfg(target_os = "macos")]
        let _prewarm = if hidden { l.panel.as_ref().map(|p| p.prewarm()) } else { None };
        #[cfg(target_os = "macos")]
        let target = if hidden { None } else if let Some(pacing) = &mut l.pacing {
            let Some(target) = pacing.next_frame() else { return };
            Some(target)
        } else { None };
        if self.native_handle.native.number.load(Ordering::Relaxed) == 0 {
            self.native_handle.native.number.store(native_window_number(&l.window).unwrap_or(0), Ordering::Release);
        }
        #[cfg(not(target_os = "macos"))]
        let target: Option<Instant> = None;
        if !hidden { self.meter.redraw(l.paced_for_stats()); }
        // The frame moves before the drawable is acquired, so it has the new size.
        if !hidden { self.step_frame(target.unwrap_or_else(Instant::now)); }
        let (Some(l), Some((gpu, renderer, _))) = (self.win.as_mut(), self.gpu.as_mut()) else { return };
        // Acquire before latching live values so a drawable wait cannot age the input.
        l.retry = None;
        let frame = match acquire(&l.surface, &gpu.device, &l.config, 1, &mut self.meter) {
            Ok(frame) => frame,
            Err(_) if hidden => { l.retry = l.first_frame.missed(Instant::now(), &mut l.hidden_wait); return; }
            Err(missed) => return self.missed_frame(missed, false),
        };
        if !hidden && !self.rt.live_visible() && { let size = l.window.inner_size(); size.width > 0 && size.height > 0 } {
            // Uncovered after a missed frame said it was occluded.
            self.rt.set_live_visible(true);
            self.rt.settle_all();
            #[cfg(target_os = "macos")]
            if l.pacing.is_none() { l.pacing = crate::display_link::DisplayLink::new(&l.window, self.frame_wake.clone()); }
        }
        #[cfg(target_os = "macos")]
        let drawable = l.present.as_ref().and(self.present_log.as_ref()).and_then(|log| log.acquired());
        #[cfg(not(target_os = "macos"))]
        let drawable = None;
        let now = Instant::now();
        if self.trace.is_none() && let Some(path) = &self.args.trace {
            let hz = l.window.current_monitor().and_then(|m| m.refresh_rate_millihertz()).map_or(60.0, |hz| hz as f64 / 1000.0);
            #[cfg(target_os = "macos")]
            let hz = l.pacing.as_ref().map_or(hz, |pacing| pacing.refresh_hz());
            #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
            let mut trace = crate::tooling::Trace::open_clock(path, self.rt.viewport, hz, l.window.scale_factor() as f32, "wall");
            #[cfg(target_os = "macos")]
            if let Some(log) = self.present_log.clone() && l.present.is_some() { trace.record_presents(log, now); }
            trace.record_surface(self.meter.shared(), self.rt.app_stats);
            self.trace = Some(trace);
            self.trace_start = Some(now);
            self.rt.scene.store_mut().audit_retargets(true);
        }
        let frame_time = target.unwrap_or(now);
        self.rt.window_resting = self.frame_anim.is_none() && self.rt.window_frame.is_none();
        let target_ms = target.zip(self.trace_start).map(|(target, start)| crate::present_time::ms_since(start, target));
        let rt = &mut self.rt;
        #[cfg(target_os = "macos")]
        { rt.native_materials = l.materials.native(); }
        if hidden && l.first_frame.settles() { rt.settle_all(); }
        let (mut animating, dt) = if hidden {
            self.clock.hold(frame_time);
            (rt.frame(0.0), 0.0)
        } else {
            self.clock.advance(rt, frame_time, self.comp.as_ref().is_some_and(|c| c.animating()))
        };
        #[cfg(target_os = "macos")]
        let frame = if l.materials.update(rt.transparent, !rt.materials.is_empty(), &l.window, &l.surface) {
            // This drawable presents without the views: build again natively into a new one.
            drop(frame);
            rt.native_materials = true;
            animating |= rt.frame(0.0);
            match acquire(&l.surface, &gpu.device, &l.config, 1, &mut self.meter) {
                Ok(frame) => frame,
                // Retrying at once is what spins while occluded; wait like any missed frame.
                Err(_) if hidden => { l.retry = l.first_frame.missed(Instant::now(), &mut l.hidden_wait); return; }
                Err(missed) => return self.missed_frame(missed, true),
            }
        } else { frame };
        l.occluded_wait = OCCLUDED_RETRY;
        l.hidden_wait = TIMEOUT_RETRY;
        if hidden { l.hidden_pace.rendered(now); }
        self.secure_input.set(rt.has_secure_focus() && l.window.has_focus());
        let update_ms = now.elapsed().as_secs_f64() * 1000.0;
        let ime_allowed = rt.ime_area.is_some() && !rt.has_secure_focus() && l.window.has_focus();
        if std::mem::take(&mut rt.ime_reset) || ime_allowed != self.ime_allowed {
            l.window.set_ime_allowed(false);
            l.window.set_ime_allowed(ime_allowed);
            self.ime_allowed = ime_allowed;
        }
        let focused = l.window.has_focus();
        l.apply_cursor(rt, focused);
        #[cfg(target_os = "macos")]
        l.help.sync(&l.window, rt.help_regions());
        self.clipboard.flush(rt);
        if let Some(a) = rt.ime_area {
            l.window.set_ime_cursor_area(
                LogicalPosition::new(a[0] as f64, a[1] as f64),
                LogicalSize::new(a[2] as f64, a[3] as f64),
            );
        }
        let target_format = if rt.srgb_blend() { l.config.format.remove_srgb_suffix() } else { l.config.format };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
            format: (target_format != l.config.format).then_some(target_format),
            ..Default::default()
        });
        renderer.upload_images(&gpu.device, &gpu.queue, &mut rt.images);
        renderer.prepare_surfaces(&gpu.device, &gpu.queue, &rt.surfaces, (l.config.width, l.config.height));
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        let target = l.alpha.as_ref().map_or(&view, |a| &a.view);
        if let Some(c) = self.comp.as_mut() {
            let mut f = Frame {
                device: &gpu.device,
                queue: &gpu.queue,
                encoder: &mut enc,
                target,
                format: target_format,
                size: (l.config.width, l.config.height),
                dt,
                update_ms,
                rt: &mut *rt,
                ui: &mut *renderer,
            };
            c.render(&mut f);
            animating |= c.animating();
        } else {
            renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (l.config.width, l.config.height));
            renderer.draw(&mut enc, target, rt.clear_color());
        }
        if let Some(alpha) = &l.alpha { alpha.draw(&mut enc, &view); }
        renderer.submit(&gpu.queue, enc);
        l.window.pre_present_notify();
        #[cfg(target_os = "macos")]
        l.materials.present(&rt.materials, rt.env.scale, || gpu.queue.present(frame));
        #[cfg(not(target_os = "macos"))]
        gpu.queue.present(frame);
        // The frame landed with this present; later ones needn't wait for a transaction.
        #[cfg(target_os = "macos")]
        if self.frame_anim.is_none() { l.present_with_transaction(false); }
        if !l.first_frame.ready
            && gpu.device.poll(wgpu::PollType::wait_indefinitely()).is_ok() {
            l.first_frame.ready = true;
        }
        let submitted = Instant::now();
        self.meter.presented();
        #[cfg(target_os = "macos")]
        if let Some(pacing) = &mut l.pacing { pacing.presented(submitted); }
        // A display link that couldn't be made while the displays slept: they're back.
        #[cfg(target_os = "macos")]
        if !hidden && l.pacing.is_none() { l.pacing = crate::display_link::DisplayLink::retry(&l.window, self.frame_wake.clone()); }
        if let Some(trace) = self.trace.as_mut() {
            // A frame count keeps the trace redrawing while idle, as below.
            let continuous = !self.idle || self.args.frames.is_some();
            let timing = crate::tooling::FrameTiming { target_ms, drawable, continuous };
            trace.frame_timed(rt, (submitted - self.trace_start.unwrap()).as_secs_f64() * 1000.0,
                now.elapsed().as_secs_f64() * 1000.0, timing);
            self.trace_frames += 1;
            if self.args.frames.is_some_and(|n| self.trace_frames >= n) {
                // While the window still shows its last frames.
                trace.settle_presents(crate::present_time::SETTLE);
                self.finished = true;
                return;
            }
        }
        // A poll asking for the next frame (`wake_after(Duration::ZERO)`) keeps a running display
        // link going; without one its timer waits a frame in `wake_deadline`.
        #[cfg(target_os = "macos")]
        let next_tick = l.pacing.as_ref().filter(|p| !hidden && p.running()).map(|p| now + Duration::from_secs_f64(1.0 / p.refresh_hz()));
        #[cfg(not(target_os = "macos"))]
        let next_tick: Option<Instant> = None;
        self.idle = !(animating || rt.has_pending_input() || rt.live_pending() || rt.window_frame.is_some() || self.frame_anim.is_some()
            || timer_wants_frame(rt, next_tick));
        if !hidden && (!self.idle || (self.trace.is_some() && self.args.frames.is_some())) {
            // Without a running display link, macOS hands out drawables faster than the
            // display shows them, so a timer paces the next frame instead of the loop.
            #[cfg(target_os = "macos")]
            if !l.paced() { l.retry = Some(now + TIMER_FRAME); }
            #[cfg(not(target_os = "macos"))]
            l.window.request_redraw();
            self.meter.set_pacing(l.pacing_state(true));
        } else {
            l.stop_pacing();
            self.meter.set_pacing(l.pacing_state(false));
        }
        // A frame change this frame's commit asked for starts with the layout springs it
        // started, which have shown a step unless they held for the window; the next frame
        // moves it.
        #[cfg(target_os = "macos")]
        if !hidden && self.rt.window_frame.is_some() {
            let shown = if std::mem::take(&mut self.rt.fit_held) { 0.0 } else { crate::frame_clock::RESUME_STEP };
            self.take_frame_request(self.frame_secs(frame_time) - shown, true);
        }
    }
}

impl<S: 'static> AnyHost for Host<S> {
    fn owns(&self, id: WindowId) -> bool {
        self.win.as_ref().is_some_and(|w| w.window.id() == id)
    }
    fn finished(&self) -> bool { self.finished }
    fn keeps_loop(&self) -> bool { self.keeps_loop }
    fn terminate(&mut self) {
        // Once: a window that already closed ran its hook then.
        if self.finished || !self.closes() { return; }
        self.finished = true;
        self.close_hook();
        self.popup.cancel(&mut self.rt);
    }

    fn cancel_menus(&mut self) { self.popup.cancel(&mut self.rt); }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        self.meter.turn(matches!(cause, StartCause::ResumeTimeReached { .. } | StartCause::Poll), crate::surface_stats::slept(cause));
        if let Some(w) = &mut self.win && let Some((retry, until)) = w.first_frame.deadline && retry <= Instant::now() {
            if until <= Instant::now() {
                let notified = w.first_frame.notified;
                if w.first_frame.expire() {
                    self.show_cached();
                    if let Some(w) = &self.win { w.window.request_redraw(); }
                    self.publish();
                } else if !notified {
                    *self.shared.window_error.lock().unwrap() = Some("launcher first frame was not ready within two seconds".into());
                }
            } else {
                // Bounded retries while the frame prepares, whatever stops each one (occlusion,
                // no drawable). A retry left due would turn the loop at once until it gives up.
                w.first_frame.deadline = Some((Instant::now() + Duration::from_millis(16), until));
                w.window.request_redraw();
            }
        }
        let launcher = self.launcher();
        if let Some(w) = &mut self.win && w.retry.is_some_and(|retry| retry <= Instant::now()) {
            w.retry = None;
            // A missed frame marked it occluded; look at the surface again.
            w.occluded = false;
            if launcher || w.visible { w.window.request_redraw(); }
        }
        if let Some(w) = &mut self.win && !w.visible && w.hidden_pace.take_due(Instant::now()) {
            w.window.request_redraw();
        }
        if matches!(cause, StartCause::ResumeTimeReached { .. } | StartCause::Poll)
            && timer_deadline(&self.rt).is_some_and(|deadline| deadline <= Instant::now())
        {
            let hidden = self.win.as_ref().is_none_or(|w| !w.visible);
            if hidden && !(launcher && self.hidden_launcher_renders()) {
                // Hidden or windowless: poll and due timers run, and the view waits for the
                // frame that shows the window. A launcher's cached frame follows, paced.
                let changed = self.rt.hidden_wake();
                self.clipboard.flush(&mut self.rt);
                self.shell(event_loop);
                if changed && launcher { self.refresh_hidden(); }
            // A redraw while the window backs off would draw nothing and leave the timer due.
            } else if let Some(w) = self.win.as_ref().filter(|w| (self.launcher() || (w.visible && !w.occluded)) && !w.window.is_minimized().unwrap_or(false)
                && !w.backing_off(Instant::now())) {
                w.window.request_redraw();
            } else {
                self.rt.hidden_timer_frame();
                self.clipboard.flush(&mut self.rt);
                self.shell(event_loop);
            }
        }
        #[cfg(all(feature = "dev", debug_assertions))]
        if self.rt.dev_hidden_deadline().is_some_and(|deadline| deadline <= Instant::now()) {
            if let Some(w) = self.win.as_ref().filter(|w| (self.launcher() || (w.visible && !w.occluded)) && !w.window.is_minimized().unwrap_or(false)) {
                w.window.request_redraw();
                self.rt.dev_hidden_stop();
            } else {
                self.rt.dev_hidden_frame();
                self.shell(event_loop);
            }
        }
    }

    fn wake_deadline(&self) -> Option<Instant> {
        // The next display tick consumes due timers too. Leaving an overdue timer
        // armed while its redraw waits for that tick would spin the event loop. Without a
        // running link, a timer due sooner than a frame waits a frame (`timer_deadline`).
        let runtime = timer_deadline(&self.rt).filter(|_| !self.win.as_ref().is_some_and(Win::paced));
        #[cfg(all(feature = "dev", debug_assertions))]
        let runtime = runtime.into_iter().chain(self.rt.dev_hidden_deadline()).min();
        let win = self.win.as_ref();
        runtime.into_iter()
            .chain(win.and_then(|w| w.first_frame.deadline.map(|(retry, _)| retry)))
            .chain(win.and_then(|w| w.retry))
            .chain(win.filter(|w| !w.visible).and_then(|w| w.hidden_pace.deadline()))
            .min()
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) -> bool {
        #[cfg(target_os = "macos")]
        self.fn_key();
        self.hidden_input(event_loop);
        // Process queued hide/close requests before completing a pending first reveal.
        self.first_frame_ready(event_loop);
        false
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.resume(event_loop);
        self.publish();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: WakeEvent) {
        self.meter.waited();
        self.handle_user_event(event_loop, event);
        self.publish();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if !matches!(event, WindowEvent::RedrawRequested) { self.meter.waited(); }
        self.handle_window_event(event_loop, id, event);
        self.publish();
    }
}

impl<S: 'static> Host<S> {
    fn resume(&mut self, event_loop: &ActiveEventLoop) {
        if self.started {
            return;
        }
        if self.launcher() {
            self.create(event_loop, None, false);
            if self.open_at_start && self.options.visible { self.reveal_launcher(event_loop); }
        } else if self.open_at_start {
            self.open(event_loop, None);
        }
        self.started = true;
        self.drain(event_loop, false);
    }

    /// A hidden window delivers queued input without drawing, such as the releases of the
    /// modifier keys held when it hid, rather than when it next shows. A shown window's
    /// redraw delivers it, as does a hidden launcher's.
    fn hidden_input(&mut self, event_loop: &ActiveEventLoop) {
        if !self.started || self.finished || self.win.as_ref().is_some_and(|w| w.visible || self.launcher()) { return; }
        if !self.rt.keys_queued() { return; }
        self.rt.hidden_timer_frame();
        self.clipboard.flush(&mut self.rt);
        self.shell(event_loop);
    }

    #[cfg(target_os = "macos")]
    fn fn_key(&mut self) {
        let Some(l) = &self.win else { return };
        let Some(changes) = l.fn_key.as_ref().map(|f| f.take()) else { return };
        let mut redraw = false;
        for pressed in changes {
            redraw |= self.rt.physical_with_fallback(crate::PhysicalInput::Key {
                key: winit::keyboard::KeyCode::Fn, mods: map_mods(self.mods), pressed, repeat: false,
            }, None);
        }
        if redraw { l.window.request_redraw(); }
    }

    fn handle_user_event(&mut self, event_loop: &ActiveEventLoop, event: WakeEvent) {
        if !self.started { return; }
        match event {
            WakeEvent::App(_) => {
                self.drain(event_loop, true);
            }
            WakeEvent::DisplayTick(_) => {
                if self.win.as_ref().is_some_and(Win::paced) {
                    self.redraw();
                    self.shell(event_loop);
                }
            }
            WakeEvent::Live(_) => {
                if self.rt.live_pending() && let Some(w) = self.win.as_ref().filter(|w| w.visible && !w.occluded) {
                    w.window.request_redraw();
                }
            }
            WakeEvent::Window(_, request) => self.window_request(event_loop, request),
            WakeEvent::Focus(_) => self.focus(event_loop),
            WakeEvent::Position(_, position) => self.set_position(event_loop, position),
            WakeEvent::Place(_, place) => self.place(event_loop, place),
            WakeEvent::Level(_, on) => self.set_always_on_top(event_loop, on),
            WakeEvent::Frame(_, mut request) => {
                request.spring = (request.animate && !self.rt.reduced_motion).then_some(crate::tok::motion::DEFAULT);
                self.rt.window_frame = Some(self.rt.window_frame.unwrap_or_default().merge(request));
                if let Some(w) = self.win.as_ref().filter(|w| w.visible) { w.window.request_redraw(); }
                self.shell(event_loop);
            }
        }
    }

    fn handle_window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if self.win.as_ref().is_none_or(|w| w.window.id() != id) {
            return;
        }
        #[cfg(target_os = "macos")]
        if matches!(event, WindowEvent::Moved(_) | WindowEvent::ScaleFactorChanged { .. } | WindowEvent::Occluded(_))
            && let Some(w) = &mut self.win && let Some(pacing) = &mut w.pacing {
            pacing.update_display(&w.window);
        }
        if matches!(&event, WindowEvent::ScaleFactorChanged { .. }) { self.apply_placement(event_loop); }
        match event {
            WindowEvent::Focused(focused) => {
                if self.launcher() && self.win.as_ref().is_some_and(|w| !w.first_frame.notified) { return; }
                #[cfg(target_os = "macos")]
                if self.win.as_ref().and_then(|w| w.panel.as_ref()).is_some_and(|p| p.is_key() != focused) {
                    // winit queues an initial false from its hidden source window before
                    // adoption. It must not dismiss an already-key panel on first open.
                    return;
                }
                // Deliver independently of drawable availability, including hide/occlusion.
                self.rt.window_focus(focused);
                self.shell(event_loop);
                if self.finished || self.win.is_none() { return; }
            }
            WindowEvent::CloseRequested => {
                self.close(event_loop);
                return;
            }
            WindowEvent::RedrawRequested => {
                self.redraw();
                self.shell(event_loop);
                return;
            }
            _ => {}
        }
        // Occluded or minimised mid-move: land, so the window comes back at its new size.
        let stops = match &event {
            WindowEvent::Occluded(occluded) => *occluded && !(self.macos_window.launcher && cfg!(target_os = "macos")),
            WindowEvent::Resized(size) => size.width == 0 || size.height == 0,
            _ => false,
        };
        if stops { self.land_frame(); }
        if matches!(event, WindowEvent::Moved(_) | WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. }) { self.sync_origin(); }
        let (Some(l), Some((gpu, ..))) = (self.win.as_mut(), self.gpu.as_ref()) else { return };
        let rt = &mut self.rt;
        let scale = l.window.scale_factor() as f32;
        let chrome = self.chrome;
        match event {
            WindowEvent::Resized(size) => {
                place_chrome(&l.window, chrome);
                rt.set_live_visible(l.visible && !l.occluded && size.width > 0 && size.height > 0 && !l.window.is_minimized().unwrap_or(false));
                if size.width == 0 || size.height == 0 {
                    self.secure_input.set(false);
                    l.stop_pacing();
                    self.idle = true; return;
                }
                // A frame Rune moved itself has its drawable and viewport already.
                let resized = l.resize_surface(&gpu.device, size);
                let viewport = (size.width as f32 / scale, size.height as f32 / scale);
                if !resized && viewport == rt.viewport && scale == rt.env.scale { return; }
                let asked = self.frame_jump.take().is_some_and(|(w, h)| (w - viewport.0).abs() < 1.0 && (h - viewport.1).abs() < 1.0);
                if asked { rt.step_viewport(viewport, scale) } else { rt.set_viewport(viewport, scale) }
                l.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let size = l.window.inner_size();
                rt.set_viewport((size.width as f32 / scale_factor as f32, size.height as f32 / scale_factor as f32), scale_factor as f32);
                place_chrome(&l.window, chrome);
                l.window.request_redraw();
            }
            WindowEvent::Occluded(occluded) => {
                // Alpha-zero launcher panels remain renderable on explicit invalidation.
                // Their native occlusion transitions must not settle or redraw a reveal.
                if self.macos_window.launcher && cfg!(target_os = "macos") { return; }
                l.occluded = occluded;
                l.retry = None;
                l.occluded_wait = OCCLUDED_RETRY;
                rt.set_live_visible(l.visible && !occluded && !l.window.is_minimized().unwrap_or(false));
                if occluded {
                    self.secure_input.set(false);
                    l.stop_pacing();
                    self.idle = true; return;
                }
                place_chrome(&l.window, chrome);
                // A window opened while the displays slept has no display link yet.
                #[cfg(target_os = "macos")]
                if l.pacing.is_none() { l.pacing = crate::display_link::DisplayLink::new(&l.window, self.frame_wake.clone()); }
                // Coming back paints the settled state in the first frame (guarantee 8).
                rt.settle_all();
                l.window.request_redraw();
            }
            WindowEvent::ThemeChanged(theme) => {
                if self.args.theme.is_none() {
                    rt.set_system_dark(theme == Theme::Dark);
                    l.window.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(m) => {
                self.mods = m.state();
                if rt.physical_observation(crate::PhysicalInput::Modifiers(map_mods(self.mods))) {
                    l.window.request_redraw();
                }
            }
            WindowEvent::Focused(focused) => {
                rt.set_system_reduced_motion(crate::preferences::reduced_motion());
                rt.set_full_keyboard_access(crate::preferences::full_keyboard_access());
                self.secure_input.set(focused && l.visible && !l.occluded && rt.has_secure_focus());
                place_chrome(&l.window, chrome);
                if !focused {
                    rt.cancel_composition();
                    l.window.set_ime_allowed(false);
                    self.ime_allowed = false;
                    // A minimized or occluded window skips redraw.
                    l.apply_cursor(rt, false);
                } else {
                    rt.ime_reset = true;
                }
                l.window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, is_synthetic: false, .. } => {
                let pressed = event.state == ElementState::Pressed;
                let logical = pressed.then(|| map_key(&event.logical_key, self.mods)).flatten();
                let mut redraw = false;
                if let winit::keyboard::PhysicalKey::Code(key) = event.physical_key {
                    redraw = rt.physical_with_fallback(crate::PhysicalInput::Key {
                        key, mods: map_mods(self.mods), pressed, repeat: event.repeat,
                    }, logical);
                } else if let Some(ev) = logical {
                    if ev.key == Key::Command('v') && rt.accepts_paste() { self.clipboard.read(rt); }
                    else if event.repeat { rt.repeat_key(ev); }
                    else { rt.key(ev); }
                    redraw = true;
                }
                if redraw { l.window.request_redraw(); }
            }
            WindowEvent::Ime(ime) => {
                match ime {
                    WIme::Preedit(s, cur) => rt.ime(Ime::Preedit(s, cur)),
                    WIme::Commit(s) => rt.ime(Ime::Commit(s)),
                    _ => {}
                }
                l.window.request_redraw();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if let Some((x, y)) = rt.pointer() {
                    let dy = match delta {
                        MouseScrollDelta::PixelDelta(p) => p.y as f32 / scale,
                        MouseScrollDelta::LineDelta(_, ly) => ly * 40.0,
                    };
                    rt.wheel(x, y, dy);
                    l.window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                rt.pointer_move(position.x as f32 / scale, position.y as f32 / scale);
                // AppKit needs the drag session started inside this mouse-dragged event.
                if let Some(start) = rt.take_drag_out() {
                    let image = self.gpu.as_ref().filter(|_| start.item.drag_image().is_none()).and_then(|(gpu, renderer, _)|
                        crate::drag_out::snapshot(gpu, renderer, (l.config.width, l.config.height), scale, rt.clear_color(), start.rect));
                    let (sink, path) = (rt.drag_end_sink(), start.path.clone());
                    let handle = self.native_handle.clone();
                    crate::drag_out::begin(&l.window, start, image, Box::new(move |outcome| {
                        sink.borrow_mut().push((path, outcome));
                        handle.send(WakeEvent::App);
                    }));
                    l.apply_cursor(rt, false);
                }
                l.window.request_redraw();
            }
            WindowEvent::CursorLeft { .. } => {
                rt.pointer_leave();
                l.window.request_redraw();
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                if state == ElementState::Pressed {
                    self.control_click = cfg!(target_os = "macos") && self.mods.control_key();
                }
                if let Some((x, y)) = rt.pointer() {
                    match (state, self.control_click) {
                        (ElementState::Pressed, true) => rt.secondary_down(x, y),
                        (ElementState::Released, true) => rt.secondary_up(x, y),
                        (ElementState::Pressed, false) => {
                            rt.pointer_down(x, y);
                            rt.physical_input(crate::PhysicalInput::Modifiers(map_mods(self.mods)));
                            // AppKit needs the drag started inside this mouse-down.
                            match rt.take_titlebar_action() {
                                Some(TitlebarAction::Drag) => {
                                    let _ = l.window.drag_window();
                                }
                                Some(TitlebarAction::DoubleClick) => titlebar_double_click(&l.window),
                                None => {}
                            }
                        }
                        (ElementState::Released, false) => rt.pointer_up(x, y),
                    }
                }
                if state == ElementState::Released { self.control_click = false; }
                l.window.request_redraw();
            }
            WindowEvent::MouseInput { state, button: MouseButton::Right, .. } => {
                if let Some((x, y)) = rt.pointer() {
                    match state {
                        ElementState::Pressed => rt.secondary_down(x, y),
                        ElementState::Released => rt.secondary_up(x, y),
                    }
                }
                l.window.request_redraw();
            }
            _ => {}
        }
    }
}

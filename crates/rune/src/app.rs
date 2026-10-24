//! The app's lifetime beside its window. `App` runs an `AppSpec` with or without a window and
//! decides what closing it means; `AppHandle` opens, focuses, closes and feeds the running app
//! from any thread.
//!
//! The state, scene and text live with the app, not the window: closing the window keeps them,
//! and reopening shows the same UI, settled. Closing hides the window where the platform can
//! (macOS, Windows, X11) and destroys it on Wayland, where a hidden toplevel doesn't exist;
//! the GPU device and renderer stay either way.
//!
//! ```ignore
//! let app = rune::App::new(spec).windowless().keep_running();
//! let handle = app.handle();
//! std::thread::spawn(move || {
//!     for request in requests() {
//!         match request {
//!             Request::Activate(token) => handle.activate(token),
//!             Request::Snapshot(s) => handle.update(move |ui, _| ui.snapshot = s),
//!             Request::Quit => handle.quit(),
//!         }
//!     }
//! });
//! app.run();
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::el::Tx;
use crate::tooling::{AppSpec, Args};

type Update<S> = Box<dyn FnOnce(&mut S, &mut Tx) + Send>;
/// `App::on_window`'s hook, run after each native window creation.
pub(crate) type OnWindow = Box<dyn FnMut(&crate::WindowHandle)>;

/// An `AppSpec` with its lifetime: by default it opens one window and quits when it closes,
/// like `rune::run`.
pub struct App<S: 'static> {
    pub(crate) spec: AppSpec<S>,
    pub(crate) chrome: crate::Chrome,
    init: Option<Box<dyn FnOnce() -> S>>,
    argv: Option<Vec<String>>,
    pub(crate) open: bool,
    pub(crate) release_gpu: bool,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) accessory: bool,
    pub(crate) keep_running: bool,
    pub(crate) on_close: Option<fn(&mut S, &mut Tx)>,
    pub(crate) on_focus: Option<fn(&mut S, bool, &mut Tx)>,
    pub(crate) shared: Arc<Shared<S>>,
    pub(crate) overlays: Vec<Box<dyn crate::overlay::PendingOverlay>>,
    pub(crate) windows: Vec<crate::AnyWindowSpec>,
    pub(crate) on_window: Option<OnWindow>,
    pub(crate) on_ready: Option<OnWindow>,
    pub(crate) macos_window: crate::MacWindowOptions,
    pub(crate) compositor: Option<Box<dyn crate::compose::Compositor<S>>>,
    pub(crate) window_options: crate::WindowOptions,
    pub(crate) fit_content_height: bool,
    pub(crate) modifier_keys: bool,
    pub(crate) srgb_blend: bool,
    pub(crate) dev_stats: Option<fn() -> String>,
    #[cfg(all(feature = "dev", debug_assertions))]
    pub(crate) dev: Option<crate::dev::Config<S>>,
}

impl<S: 'static> App<S> {
    pub fn new(spec: AppSpec<S>) -> Self {
        let shared: Arc<Shared<S>> = Arc::default();
        shared.shortcuts.named(spec.title);
        App {
            #[cfg(all(feature = "dev", debug_assertions))]
            dev: None,
            spec,
            chrome: crate::Chrome::DEFAULT,
            init: None,
            argv: None,
            open: true,
            release_gpu: false,
            accessory: false,
            keep_running: false,
            on_close: None,
            on_focus: None,
            shared,
            overlays: Vec::new(),
            windows: Vec::new(),
            on_window: None,
            on_ready: None,
            macos_window: crate::MacWindowOptions::default(),
            compositor: None,
            window_options: crate::WindowOptions::default(),
            fit_content_height: false,
            modifier_keys: false,
            srgb_blend: false,
            dev_stats: None,
        }
    }

    /// Composite this window's translucent fills, blurs, shadows, images and video in sRGB
    /// (gamma) space instead of linear light, matching CSS and Core Animation over dark
    /// backgrounds. Ignored with a warning when a `Compositor` is set.
    pub fn srgb_blend(mut self, enabled: bool) -> Self {
        self.srgb_blend = enabled;
        self
    }

    /// Configure this window's frame. Overlays retain their own native behaviour.
    pub fn chrome(mut self, chrome: crate::Chrome) -> Self {
        self.chrome = chrome;
        self
    }

    /// Native options for this app's window (size limits, resizability, ...). `WindowSpec::from_app`
    /// starts from them, and `WindowSpec::options` replaces them.
    pub fn window_options(mut self, options: crate::WindowOptions) -> Self {
        self.window_options = options;
        self
    }

    /// Size the window's height to its content, as SwiftUI's `.windowResizability(.contentSize)`
    /// does: the root lays out at its natural height for the window's width, the window opens
    /// at that height and follows it when the content changes, on the change's spring like
    /// `Tx::resize_window`. Give the root no fixed or `fill` height. Users still set the width.
    pub fn fit_content_height(mut self) -> Self {
        self.fit_content_height = true;
        self
    }

    /// Send modifier keys pressed and released on their own (`Key::Modifier`, with the side)
    /// to `on_key` and the focused input's key handler, for hold-to-talk on Fn or Right Option,
    /// or a Shift tap. Off by default, since handlers written without it may act on any key.
    pub fn modifier_keys(mut self) -> Self {
        self.modifier_keys = true;
        self
    }

    /// Native and portable options together, including `WindowOptions::launcher_panel()`.
    pub fn configured_options(mut self, options: crate::ConfiguredWindowOptions) -> Self {
        self.window_options = options.options;
        self.macos_window = options.native;
        self
    }

    /// Native window policy, also retained by a single-window App.
    pub fn macos_window(mut self, options: crate::MacWindowOptions) -> Self {
        self.macos_window = options;
        self
    }

    /// Called once per native focus transition, including while hidden or occluded.
    /// Headless scripts deliver the same callback with `{"focus": false}`.
    pub fn on_focus(mut self, f: fn(&mut S, bool, &mut Tx)) -> Self {
        self.on_focus = Some(f);
        self
    }

    /// Adds a transparent overlay with its own state, hidden until `OverlayHandle::show`.
    /// Bare specs preserve primary-display edge placement and never request key focus;
    /// `OverlaySpec::options` selects displays, placement and native input policy.
    /// Uses this app's fonts. Headless runs don't draw overlays.
    pub fn overlay<T: 'static>(&mut self, spec: impl Into<crate::ConfiguredOverlay<T>>, state: T) -> crate::overlay::OverlayHandle<T> {
        let (overlay, handle) = crate::overlay::Overlay::new(spec.into(), state, &self.spec);
        self.overlays.push(Box::new(overlay));
        handle
    }

    /// Adds an ordinary window with its own state type to this app's event loop, such as a
    /// settings window beside the main one. `spec.options` applies (`visible: false` creates it
    /// hidden; `App::windowless` on its app defers creation), and either handle opens it later.
    /// Focus, input, clocks and redraws stay with each window; quitting, the Dock icon and
    /// accessory mode stay with the process.
    ///
    /// Closing it hides it and runs its `on_close`, like `keep_running`: reopening shows the
    /// same state (Wayland destroys and recreates the window). It never keeps the app running
    /// alone: the app still ends with its own window, and `Tx::quit` from either quits both.
    /// Headless tools select it with `--window KEY`; otherwise `--scene` and `--size` are left
    /// to the primary window.
    pub fn window<T: 'static>(&mut self, spec: crate::WindowSpec<T>) -> (AppHandle<T>, crate::WindowHandle) {
        let (app, window) = (spec.app.handle(), crate::WindowHandle::new(&spec.key));
        self.windows.push(crate::AnyWindowSpec::secondary(spec, window.clone()));
        (app, window)
    }

    /// Build the first state with `f` instead of `AppSpec::init`, so it can capture what the
    /// host already made (a channel, a handle to its runtime). Scenes still apply on top.
    pub fn init(mut self, f: impl FnOnce() -> S + 'static) -> Self {
        self.init = Some(Box::new(f));
        self
    }

    /// Read Rune's flags from `argv` (without the program name) instead of the process's
    /// arguments. An app that owns its command line passes only what it means for Rune, often
    /// nothing.
    pub fn args(mut self, argv: impl IntoIterator<Item = String>) -> Self {
        self.argv = Some(argv.into_iter().collect());
        self
    }

    /// Start without a window; `AppHandle::open` or `activate` shows it. Implies `keep_running`.
    pub fn windowless(mut self) -> Self {
        self.open = false;
        self.keep_running = true;
        self
    }

    /// Closing the window keeps the app, its state and its event loop running until
    /// `AppHandle::quit` or `Tx::quit`.
    pub fn keep_running(mut self) -> Self {
        self.keep_running = true;
        self
    }

    /// Closing the window also releases the GPU device and renderer (and destroys the window
    /// everywhere), so an app idling without a window holds no UI GPU memory. Reopening
    /// recreates them, which costs a device creation.
    pub fn release_gpu_when_closed(mut self) -> Self {
        self.release_gpu = true;
        self
    }

    /// macOS: run as an accessory (menu-bar) app: no Dock icon, no default menu bar, and
    /// launching doesn't activate the app or take focus. The app installs its own menu if it
    /// wants one, and may change the activation policy later. No effect on other platforms.
    pub fn accessory(mut self) -> Self {
        self.accessory = true;
        self
    }

    /// Runs whenever the window closes (close button, `Tx::close_window`, `AppHandle::close`,
    /// macOS Quit from the menu, Cmd-Q or the Dock), before it goes: flush drafts, stop previews.
    /// Its `Tx::quit` quits.
    pub fn on_close(mut self, f: fn(&mut S, &mut Tx)) -> Self {
        self.on_close = Some(f);
        self
    }

    /// Runs on the UI thread each time the native window is created (again after a Wayland
    /// close or a GPU release): set its icon or native styles through
    /// `WindowHandle::with_raw_handle`.
    pub fn on_window(mut self, f: impl FnMut(&crate::WindowHandle) + 'static) -> Self {
        self.on_window = Some(Box::new(f));
        self
    }

    /// First-frame readiness, once per native creation. Launcher panels call this while
    /// hidden, after GPU completion of its cached surface frame.
    /// Not called by headless tooling. May queue a show through the supplied handle.
    pub fn on_ready(mut self, f: impl FnMut(&crate::WindowHandle) + 'static) -> Self {
        self.on_ready = Some(Box::new(f));
        self
    }

    /// Draw every frame of the app's window (and headless renders) through `compositor`
    /// instead of Rune's default pass (see `compose`).
    pub fn compositor(mut self, compositor: impl crate::compose::Compositor<S>) -> Self {
        self.compositor = Some(Box::new(compositor));
        self
    }

    /// A handle for other threads; requests made before `run` apply once it starts.
    pub fn handle(&self) -> AppHandle<S> {
        AppHandle {
            shared: self.shared.clone(),
        }
    }

    /// Opt into the knobs remote panel for this debug runtime.
    #[cfg(all(feature = "dev", debug_assertions))]
    pub fn dev_panel(mut self, config: crate::dev::Config<S>) -> Self {
        self.dev = Some(config);
        self
    }

    /// The app's own counters (memory, caches, queues), shown beside the window's frame stats
    /// in the dev panel (`App::dev_panel`) and in a native trace's summary. Called at most
    /// once a frame with the panel open, so keep it cheap. For example, with the `procwatch`
    /// crate: `.dev_stats(|| procwatch::snapshot().map_or_else(String::new, |s| format!("{} MB, {} fds", s.footprint >> 20, s.fds)))`.
    pub fn dev_stats(mut self, stats: fn() -> String) -> Self {
        self.dev_stats = Some(stats);
        self
    }

    /// Runs until the app quits. Agent-tooling arguments (`--snap`, `--tree`, ...) render
    /// headless instead: the window requests are ignored there, and updates apply each frame.
    /// Panics when the app can't start, such as without a display; `try_run` returns that error.
    pub fn run(self) {
        if let Err(error) = self.try_run() { panic!("{error}"); }
    }

    /// `run`, returning an error instead of panicking when the event loop can't start (no
    /// display over ssh or in CI, no Wayland or X11 server) or fails.
    pub fn try_run(mut self) -> Result<(), String> {
        let args = match self.argv.take() {
            Some(argv) => Args::parse_from(argv),
            None => Args::parse(),
        };
        #[cfg(feature = "hot")]
        if args.snap_on_patch.is_some() && self.compositor.is_none() {
            crate::hot::run_app(self, args);
            return Ok(());
        }
        if args.headless() {
            crate::tooling::try_headless(self, args)
        } else {
            crate::window::run(self, args)
        }
    }
}

impl<S: 'static> App<S> {
    pub(crate) fn resolve_args(&mut self, inherited: Args) -> Args {
        self.argv.take().map_or(inherited, Args::parse_from)
    }

    pub(crate) fn into_parts(self, scene: Option<&str>) -> (AppSpec<S>, S, Arc<Shared<S>>) {
        let state = match self.init {
            Some(init) => crate::tooling::with_scene(&self.spec, init(), scene),
            None => crate::tooling::initial_state(&self.spec, scene),
        };
        (self.spec, state, self.shared)
    }
}

/// Opens, focuses, closes and feeds a running `App` from any thread. Every call wakes the event
/// loop; nothing polls. Window requests collapse to the latest.
pub struct AppHandle<S: 'static> {
    pub(crate) shared: Arc<Shared<S>>,
}

impl<S> Clone for AppHandle<S> {
    fn clone(&self) -> Self {
        AppHandle {
            shared: self.shared.clone(),
        }
    }
}

impl<S: 'static> AppHandle<S> {
    /// Wake the event loop so `AppSpec::poll` runs, also while the window is hidden; for a
    /// background thread that queued work for `poll` to drain.
    pub fn wake(&self) {
        self.shared.wake();
    }

    /// Show the window (creating it if needed), restore it if minimized, and activate the app
    /// with it in front, including the first open of a window that doesn't exist yet. A
    /// nonactivating panel takes keys without activating. The window open at launch doesn't
    /// activate the app beyond what the platform does at launch.
    pub fn open(&self) {
        self.request(|q| q.window = Some(WindowRequest::Open(None)));
    }

    /// Show or recreate the window without requesting focus. A minimized window stays minimized;
    /// open() also restores and activates it. Window-manager policy still applies.
    pub fn show(&self) {
        self.request(|q| q.window = Some(WindowRequest::Show));
    }

    /// Hide the window, retaining its host and state. Does not invoke the close callback.
    pub fn hide(&self) {
        self.request(|q| q.window = Some(WindowRequest::Hide));
    }

    /// Focus/activate an already-visible window, restoring it if minimized.
    /// On Wayland this requests attention; use activate(token) for compositor-authorized focus.
    pub fn focus(&self) {
        self.request(|q| q.focus = true);
    }

    /// Set the outer position in physical desktop pixels (negative coordinates are valid).
    /// Retained across recreation. Wayland does not support application window placement.
    pub fn set_position(&self, position: (i32, i32)) {
        self.request(|q| { q.position = Some(position); q.place = None; });
    }

    /// Place in display-local points. UnderPointer samples the pointer at this call;
    /// disconnected displays fall back to primary when the UI thread applies the request.
    /// Wayland does not permit application placement.
    pub fn place(&self, display: crate::DisplayTarget, placement: crate::OverlayPlacement) {
        let place = crate::windows::PlaceRequest::new(display, placement);
        self.request(|q| { q.place = Some(place); q.position = None; });
    }

    /// Show and focus the window with an activation token another process forwarded
    /// (`XDG_ACTIVATION_TOKEN` on Wayland, `DESKTOP_STARTUP_ID` on X11), so the compositor
    /// lets it take focus. Where tokens don't exist this is `open`.
    pub fn activate(&self, token: impl Into<String>) {
        let token = token.into();
        self.request(|q| q.window = Some(WindowRequest::Open(Some(token).filter(|t| !t.is_empty()))));
    }

    /// Close the window as the close button would (`App::on_close` runs; without
    /// `keep_running` the app quits).
    pub fn close(&self) {
        self.request(|q| q.window = Some(WindowRequest::Close));
    }

    /// Quit the app: the window closes without `on_close`, and `run` returns.
    pub fn quit(&self) {
        self.request(|q| q.quit = true);
    }

    /// macOS: show or hide the app's Dock icon (and app menu) while it runs, switching between
    /// the regular and accessory activation policies. Does nothing elsewhere.
    pub fn set_dock_icon(&self, visible: bool) {
        self.request(|q| q.dock = Some(visible));
    }

    /// Whether the window is on screen (shown, not hidden or closed). Minimized and occluded
    /// windows count as shown.
    pub fn is_visible(&self) -> bool {
        self.shared.visible.load(Ordering::Acquire)
    }

    /// Why the window couldn't be created the last time it was asked for (no display, no GPU
    /// adapter or device), until a later creation succeeds. The app keeps running windowless.
    pub fn window_error(&self) -> Option<String> {
        self.shared.window_error.lock().unwrap().clone()
    }

    /// Change the state from this thread: `f` runs on the UI thread, in call order, even while no
    /// window shows, and the next frame commits it with the motion its `Tx` asks for.
    pub fn update(&self, f: impl FnOnce(&mut S, &mut Tx) + Send + 'static) {
        self.request(|q| q.updates.push(Box::new(f)));
    }

    fn request(&self, f: impl FnOnce(&mut Requests<S>)) {
        f(&mut self.shared.requests.lock().unwrap());
        self.shared.wake();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WindowRequest {
    /// Show and focus, with an activation token when one was forwarded.
    Open(Option<String>),
    Show,
    Hide,
    Close,
}

pub(crate) struct Requests<S> {
    pub window: Option<WindowRequest>,
    pub dock: Option<bool>,
    pub focus: bool,
    pub position: Option<(i32, i32)>,
    pub place: Option<crate::windows::PlaceRequest>,
    pub quit: bool,
    pub updates: Vec<Update<S>>,
}

impl<S> Default for Requests<S> {
    fn default() -> Self {
        Requests {
            window: None,
            dock: None,
            focus: false,
            position: None,
            place: None,
            quit: false,
            updates: Vec::new(),
        }
    }
}

pub(crate) struct Shared<S> {
    pub(crate) menus: crate::native_menu::Hub<S>,
    pub(crate) shortcuts: crate::shortcuts::Hub<S>,
    requests: Mutex<Requests<S>>,
    wake: Mutex<Option<crate::runtime::Waker>>,
    /// Published by the window host for `AppHandle::is_visible` / `window_error`.
    pub(crate) visible: AtomicBool,
    pub(crate) window_error: Mutex<Option<String>>,
}

impl<S> Default for Shared<S> {
    fn default() -> Self {
        Shared {
            menus: crate::native_menu::Hub::default(),
            shortcuts: crate::shortcuts::Hub::default(),
            requests: Mutex::default(),
            wake: Mutex::new(None),
            visible: AtomicBool::new(false),
            window_error: Mutex::new(None),
        }
    }
}

impl<S> Shared<S> {
    /// Installs the event loop's wake-up and wakes it once for requests made before it existed.
    pub fn connect(&self, wake: crate::runtime::Waker) {
        *self.wake.lock().unwrap() = Some(wake);
        self.wake();
    }

    pub fn wake(&self) {
        let wake = self.wake.lock().unwrap().clone();
        if let Some(wake) = wake {
            wake();
        }
    }

    pub fn take(&self) -> Requests<S> {
        std::mem::take(&mut self.requests.lock().unwrap())
    }

    /// A request a `--desktop` run applies to the window (dock and quit aside) waits for `take`.
    pub(crate) fn window_pending(&self) -> bool {
        let r = self.requests.lock().unwrap();
        r.window.is_some() || r.focus || r.position.is_some() || r.place.is_some() || !r.updates.is_empty()
    }

    pub fn take_place(&self) -> Option<crate::windows::PlaceRequest> {
        self.requests.lock().unwrap().place.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::el::{El, Shell, el};

    use crate::runtime::{Cx, Env, Runtime};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn spec() -> AppSpec<u32> {
        AppSpec::new("test", (100.0, 100.0), || 0, |_: &u32, _: &mut Cx<u32>| el())
    }

    #[test]
    fn window_panel_placement_and_pixel_position_are_last_request_wins() {
        let app = App::new(spec());
        let handle = app.handle();
        handle.set_position((10, 20));
        handle.place(crate::DisplayTarget::Primary, crate::OverlayPlacement::FullScreen);
        let q = app.shared.take();
        assert!(q.position.is_none());
        assert_eq!(q.place.unwrap().placement, crate::OverlayPlacement::FullScreen);
        handle.place(crate::DisplayTarget::Primary, crate::OverlayPlacement::FullScreen);
        handle.set_position((-10, 20));
        let q = app.shared.take();
        assert!(q.place.is_none());
        assert_eq!(q.position, Some((-10, 20)));
    }

    #[test]
    fn window_panel_focus_is_local_deduplicated_and_runs_without_frames() {
        let mut rt = Runtime::new(Vec::new(), |_: &Vec<bool>, _: &mut Cx<Vec<bool>>| el(), (100.0, 100.0), Env::default());
        rt.on_focus = Some(|seen, focused, tx| { seen.push(focused); if !focused { tx.close_window(); } });
        let mut other = Runtime::new(Vec::new(), |_: &Vec<bool>, _: &mut Cx<Vec<bool>>| el(), (100.0, 100.0), Env::default());
        other.on_focus = rt.on_focus;
        rt.window_focus(true);
        rt.window_focus(true);
        rt.window_focus(false);
        rt.window_focus(false);
        assert_eq!(rt.state, [true, false]);
        assert_eq!(rt.shell_request, Some(Shell::Close));
        assert!(other.state.is_empty());
        other.window_focus(false);
        assert_eq!(other.state, [false]);
    }

    #[test]
    fn handles_report_the_window_the_host_publishes() {
        let app = App::new(spec());
        let handle = app.handle();
        assert!(!handle.is_visible() && handle.window_error().is_none());
        app.shared.visible.store(true, Ordering::Release);
        *app.shared.window_error.lock().unwrap() = Some("no GPU adapter".into());
        assert!(handle.is_visible());
        assert_eq!(handle.window_error().as_deref(), Some("no GPU adapter"));
    }

    #[test]
    fn handles_cross_threads_whatever_the_state() {
        fn send_sync<T: Send + Sync + Clone>() {}
        send_sync::<AppHandle<std::rc::Rc<()>>>();
    }

    #[test]
    fn init_closures_replace_the_spec_init_and_scenes_apply_on_top() {
        let mut spec = spec();
        spec.scenes = &[("big", |n| *n += 100)];
        let captured = 7;
        let (_, state, _) = App::new(spec).init(move || captured).into_parts(Some("big"));
        assert_eq!(state, 107);
        let (_, state, _) = App::new(self::spec()).into_parts(None);
        assert_eq!(state, 0);
    }

    #[test]
    fn handle_requests_queue_before_the_loop_and_wake_it_after() {
        let app = App::new(spec()).windowless();
        assert!(!app.open && app.keep_running);
        let handle = app.handle();
        let other = handle.clone();
        std::thread::spawn(move || {
            other.update(|n, _| *n += 1);
            other.activate("token");
        })
        .join()
        .unwrap();
        handle.update(|n, _| *n *= 10);
        handle.close();
        let wakes = Arc::new(AtomicUsize::new(0));
        let w = wakes.clone();
        app.shared.connect(Arc::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        }));
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            1,
            "connecting wakes once for queued requests"
        );
        let requests = app.shared.take();
        assert_eq!(
            requests.window,
            Some(WindowRequest::Close),
            "window requests collapse to the latest"
        );
        assert!(!requests.quit);
        let mut n = 0;
        for f in requests.updates {
            f(&mut n, &mut Tx::default());
        }
        assert_eq!(n, 10, "updates apply in call order");
        handle.activate("");
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
        assert_eq!(
            app.shared.take().window,
            Some(WindowRequest::Open(None)),
            "an empty token is no token"
        );
    }

    #[test]
    fn tray_requests_keep_show_focus_and_geometry_together() {
        let app = App::new(spec()).windowless();
        let handle = app.handle();
        std::thread::spawn(move || {
            handle.set_position((10, 20));
            handle.hide();
            handle.set_position((-300, 40));
            handle.show();
            handle.focus();
        }).join().unwrap();
        let requests = app.shared.take();
        assert_eq!(requests.window, Some(WindowRequest::Show), "focus must not overwrite show");
        assert!(requests.focus);
        assert_eq!(requests.position, Some((-300, 40)));
        let drained = app.shared.take();
        assert!(!drained.focus && drained.window.is_none() && drained.position.is_none());
    }

    #[test]
    fn updates_commit_on_the_next_frame_and_carry_shell_requests() {
        fn view(n: &u32, _: &mut Cx<u32>) -> El {
            crate::el::row().child(el().key("box").size(10.0 + *n as f32, 10.0))
        }
        let mut rt = Runtime::new(
            0u32,
            view,
            (200.0, 200.0),
            Env {
                system_dark: true,
                scale: 1.0,
            },
        );
        rt.frame(0.0);
        rt.update(|n, tx| {
            *n = 30;
            tx.instant();
        });
        assert!(rt.shell_request.is_none());
        rt.frame(1.0 / 120.0);
        let b = rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some("box"))
            .unwrap();
        assert_eq!(b.rect[2], 40.0, "the update's instant commit lands at once");
        rt.update(|_, tx| tx.close_window());
        rt.update(|_, tx| tx.quit());
        rt.update(|_, tx| tx.close_window());
        assert_eq!(rt.shell_request.take(), Some(Shell::Quit), "quit outranks close");
    }

    #[test]
    fn a_hidden_window_drains_its_inbox_on_wake_and_shows_it_when_reopened() {
        #[derive(Default)]
        struct Lock { inbox: Option<std::sync::mpsc::Receiver<u32>>, locked: u32 }
        fn view(s: &Lock, _: &mut Cx<Lock>) -> El {
            crate::el::row().child(el().key("box").size(10.0 + s.locked as f32, 10.0))
        }
        let mut rt = Runtime::new(Lock::default(), view, (200.0, 200.0), Env { system_dark: true, scale: 1.0 });
        rt.poll = Some(|s: &mut Lock, _, tx: &mut Tx| {
            let Some(n) = s.inbox.as_ref().and_then(|inbox| inbox.try_iter().last()) else { return false };
            s.locked = n;
            tx.wake_after(std::time::Duration::from_secs(60));
            true
        });
        rt.wall_start = Some(std::time::Instant::now());
        let (send, inbox) = std::sync::mpsc::channel();
        rt.state.inbox = Some(inbox);
        rt.frame(0.0);
        let width = |rt: &Runtime<Lock>| rt.elements().into_iter().find(|e| e.key.as_deref() == Some("box")).unwrap().rect[2];
        assert_eq!(width(&rt), 10.0);

        // Hidden from here: something queues a lock and wakes the loop.
        send.send(30).unwrap();
        assert!(rt.poll_hidden(), "a hidden launcher renders when poll reports a change");
        assert!(!rt.poll_hidden());
        assert_eq!(rt.state.locked, 30, "poll drained the inbox without a frame");
        assert_eq!(width(&rt), 10.0, "nothing laid out, animated or presented while hidden");
        assert!(rt.wake_deadline().is_some(), "poll's own timers still schedule while hidden");

        // `show` lands everything on its first frame.
        rt.settle_all();
        rt.frame(1.0 / 60.0);
        assert_eq!(width(&rt), 40.0);
    }

    /// An update's `wake_after`, delivered while no frame draws (hidden, covered, asleep or
    /// minimised), arms its timer on the next hidden poll.
    #[test]
    fn an_update_arriving_without_frames_arms_its_timer() {
        let mut rt = Runtime::new(0_u32, |_: &u32, _: &mut Cx<u32>| el(), (100.0, 100.0), Env { system_dark: true, scale: 1.0 });
        rt.poll = Some(|polls: &mut u32, _, _: &mut Tx| { *polls += 1; false });
        rt.wall_start = Some(std::time::Instant::now());
        rt.frame(0.0);
        assert_eq!(rt.wake_deadline(), None);
        rt.update(|_, tx| tx.wake_after(std::time::Duration::from_secs(5)));
        rt.poll_hidden();
        let deadline = rt.wake_deadline().expect("the update's timer is armed without a frame");
        assert!((4.9..5.5).contains(&deadline), "it counts from the update: {deadline}");
        rt.frame(0.0);
        assert_eq!(rt.wake_deadline(), Some(deadline), "and the next frame doesn't arm it twice");
    }

    /// A hidden host's due timer runs `poll` and spends the timer without building the view,
    /// poll or not. The view waits, dirty, for the frame that shows the host.
    #[test]
    fn a_hidden_timer_polls_without_building_the_view() {
        use std::time::{Duration, Instant};
        for with_poll in [true, false] {
            let mut rt = Runtime::new(0_u32, |_: &u32, _: &mut Cx<u32>| el(), (100.0, 100.0), Env { system_dark: true, scale: 1.0 });
            if with_poll { rt.poll = Some(|polls: &mut u32, _, _: &mut Tx| { *polls += 1; true }); }
            let t0 = Instant::now();
            rt.wall_start = Some(t0);
            rt.frame(0.0);
            let commits = rt.stats.commits;
            rt.update(|_, tx| tx.wake_after(Duration::from_millis(10)));
            assert_eq!(rt.hidden_wake(), with_poll);
            assert!(rt.wake_deadline().is_some());
            rt.wall_start = Some(t0 - Duration::from_secs(1));
            assert_eq!(rt.hidden_wake(), with_poll);
            assert_eq!(rt.wake_deadline(), None, "the due timer is spent, so it doesn't wake the host again");
            assert_eq!(rt.stats.commits, commits, "no view or layout while hidden");
            assert!(rt.needs_commit());
            rt.frame(0.0);
            assert_eq!(rt.stats.commits, commits + 1, "the frame that shows it builds once");
        }
    }
}

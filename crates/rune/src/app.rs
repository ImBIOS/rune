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

use std::sync::{Arc, Mutex};

use crate::el::Tx;
use crate::tooling::{AppSpec, Args};

type Update<S> = Box<dyn FnOnce(&mut S, &mut Tx) + Send>;

/// An `AppSpec` with its lifetime: by default it opens one window and quits when it closes,
/// like `rune::run`.
pub struct App<S: 'static> {
    pub(crate) spec: AppSpec<S>,
    init: Option<Box<dyn FnOnce() -> S>>,
    argv: Option<Vec<String>>,
    pub(crate) open: bool,
    pub(crate) release_gpu: bool,
    pub(crate) keep_running: bool,
    pub(crate) on_close: Option<fn(&mut S, &mut Tx)>,
    pub(crate) shared: Arc<Shared<S>>,
}

impl<S: 'static> App<S> {
    pub fn new(spec: AppSpec<S>) -> Self {
        App {
            spec,
            init: None,
            argv: None,
            open: true,
            release_gpu: false,
            keep_running: false,
            on_close: None,
            shared: Arc::default(),
        }
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

    /// Runs whenever the window closes (close button, `Tx::close_window`, `AppHandle::close`),
    /// before it goes: flush drafts, stop previews. Its `Tx::quit` quits.
    pub fn on_close(mut self, f: fn(&mut S, &mut Tx)) -> Self {
        self.on_close = Some(f);
        self
    }

    /// A handle for other threads; requests made before `run` apply once it starts.
    pub fn handle(&self) -> AppHandle<S> {
        AppHandle {
            shared: self.shared.clone(),
        }
    }

    /// Runs until the app quits. Agent-tooling arguments (`--snap`, `--tree`, ...) render
    /// headless instead: the window requests are ignored there, and updates apply each frame.
    pub fn run(mut self) {
        let args = match self.argv.take() {
            Some(argv) => Args::parse_from(argv),
            None => Args::parse(),
        };
        if args.headless() {
            crate::tooling::headless(self, args);
        } else {
            crate::window::run(self, args);
        }
    }
}

impl<S: 'static> App<S> {
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
    shared: Arc<Shared<S>>,
}

impl<S> Clone for AppHandle<S> {
    fn clone(&self) -> Self {
        AppHandle {
            shared: self.shared.clone(),
        }
    }
}

impl<S: 'static> AppHandle<S> {
    /// Show the window (creating it if needed) and bring it forward.
    pub fn open(&self) {
        self.request(|q| q.window = Some(WindowRequest::Open(None)));
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
    Close,
}

pub(crate) struct Requests<S> {
    pub window: Option<WindowRequest>,
    pub quit: bool,
    pub updates: Vec<Update<S>>,
}

impl<S> Default for Requests<S> {
    fn default() -> Self {
        Requests {
            window: None,
            quit: false,
            updates: Vec::new(),
        }
    }
}

pub(crate) struct Shared<S> {
    requests: Mutex<Requests<S>>,
    wake: Mutex<Option<crate::runtime::Waker>>,
}

impl<S> Default for Shared<S> {
    fn default() -> Self {
        Shared {
            requests: Mutex::default(),
            wake: Mutex::new(None),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::el::{El, Shell, el};

    use crate::runtime::{Cx, Env, Runtime};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn spec() -> AppSpec<u32> {
        AppSpec {
            title: "test",
            size: (100.0, 100.0),
            init: || 0,
            view: |_: &u32, _: &mut Cx<u32>| el(),
            scenes: &[],
            fonts: &[],
            families: ("Inter", "Inter"),
            setup: None,
            poll: None,
            on_key: None,
        }
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
}

//! One OS clipboard worker shared by windows and overlays. No OS reads on the event thread.
//!
//! A secret copy hands back a `SecretCopy` the app can ask about or clear. The worker keeps a
//! keyed fingerprint of the secret, never its text, and watches the clipboard's change count
//! while it holds one. Tests can give runtimes a shared `memory()` clipboard instead; without
//! one, headless runtimes never touch the OS clipboard.
use std::hash::{BuildHasher, RandomState};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, mpsc};
use std::time::Duration;
use crate::time::Instant;
use zeroize::Zeroizing;
use crate::{Runtime, Waker};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> { m.lock().unwrap_or_else(|e| e.into_inner()) }

/// A pending clipboard write. Debug output never includes its contents.
#[derive(Clone)]
pub struct ClipboardWrite {
    text: Zeroizing<String>,
    clear_after: Option<Duration>,
    secret: Option<Arc<Unwritten>>,
}
impl ClipboardWrite {
    pub(crate) fn new(text: String, clear_after: Option<Duration>) -> Self {
        Self { text: Zeroizing::new(text), clear_after, secret: None }
    }
    pub(crate) fn secret(text: String, clear_after: Duration) -> (Self, SecretCopy) {
        let slot = Arc::new(Slot::default());
        let write = Self { text: Zeroizing::new(text), clear_after: Some(clear_after), secret: Some(Arc::new(Unwritten(slot.clone()))) };
        (write, SecretCopy(slot))
    }
    pub(crate) fn is_secret(&self) -> bool { self.clear_after.is_some() }
    fn slot(&self) -> Option<&Arc<Slot>> { self.secret.as_ref().map(|u| &u.0) }
    fn bind(&self, route: Route) {
        if let Some(slot) = self.slot() { let _ = slot.route.set(route); }
    }
}
impl std::ops::Deref for ClipboardWrite {
    type Target = str;
    fn deref(&self) -> &str { &self.text }
}
impl std::fmt::Debug for ClipboardWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardWrite").field("clear_after", &self.clear_after).finish_non_exhaustive()
    }
}

/// What became of a secret copy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SecretState {
    /// Not written yet.
    #[default]
    Pending,
    /// On the clipboard: the last copy Rune wrote, and the clipboard hasn't changed since.
    Holding,
    /// Cleared by Rune: its delay ran out, or `clear_now` or `Tx::clear_secret_clipboard` ran.
    Cleared,
    /// Something else took its place: a later copy, a change by another app, or a newer copy
    /// before this one was written. Its countdown should end quietly.
    Replaced,
    /// The write failed; the clipboard didn't change.
    Failed(String),
}

#[derive(Default)]
struct Slot {
    state: Mutex<SecretState>,
    /// `clear_now` ran: skip the write if it hasn't happened yet.
    cancel: AtomicBool,
    route: OnceLock<Route>,
}
impl Slot {
    fn set(&self, state: SecretState) { *lock(&self.state) = state; }
}

/// Settles a secret write that was dropped before reaching a clipboard (replaced in its
/// handler, cancelled, or left in a headless runtime with no clipboard).
struct Unwritten(Arc<Slot>);
impl Drop for Unwritten {
    fn drop(&mut self) {
        let mut state = lock(&self.0.state);
        if *state == SecretState::Pending {
            *state = if self.0.cancel.load(Ordering::SeqCst) { SecretState::Cleared } else { SecretState::Replaced };
        }
    }
}

/// A secret copy from `Tx::copy_secret`. It holds no text, and is cheap to clone and send.
/// Its state is updated by the clipboard worker; asking never reads the OS clipboard.
#[derive(Clone)]
pub struct SecretCopy(Arc<Slot>);
impl SecretCopy {
    /// A copy that is never written and stays `Pending`, for scenes and tests that show a copy
    /// in flight without a `Tx`.
    pub fn pending() -> Self { Self(Arc::default()) }
    pub fn state(&self) -> SecretState {
        if let Some(Route::Memory(m)) = self.0.route.get() { m.tick(); }
        lock(&self.0.state).clone()
    }
    /// Still the last copy Rune wrote, and the clipboard hasn't changed since. The OS
    /// clipboard is checked a few times a second while a secret is held.
    pub fn holds(&self) -> bool { self.state() == SecretState::Holding }
    /// Clear the clipboard now if this copy still holds it, on the clipboard worker. A copy not
    /// yet written is never written. Idempotent.
    pub fn clear_now(&self) {
        self.0.cancel.store(true, Ordering::SeqCst);
        let holding = *lock(&self.0.state) == SecretState::Holding;
        if holding && let Some(route) = self.0.route.get() { route.send(Request::Clear(Some(self.0.clone()))); }
    }
}
impl std::fmt::Debug for SecretCopy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SecretCopy").field(&*lock(&self.0.state)).finish()
    }
}

#[derive(Clone)]
enum Route { System(mpsc::Sender<Request>), Memory(MemoryClipboard) }
impl Route {
    fn send(&self, request: Request) {
        match self {
            Route::System(worker) => { let _ = worker.send(request); }
            Route::Memory(m) => m.handle(request),
        }
    }
}

struct Reply { target: Option<String>, text: Zeroizing<String> }
#[derive(Clone)]
struct Notify { changed: Arc<AtomicBool>, wake: Waker }
impl Notify {
    fn send(&self) { self.changed.store(true, Ordering::SeqCst); (self.wake)(); }
}
enum Request {
    Shutdown,
    Write(ClipboardWrite, Option<Notify>),
    /// Clear this secret, or whichever secret is held, if the clipboard still holds it.
    Clear(Option<Arc<Slot>>),
    Read { target: Option<String>, reply: mpsc::Sender<Reply>, wake: Waker },
}

pub(crate) struct Client {
    sender: mpsc::Sender<Reply>,
    replies: mpsc::Receiver<Reply>,
    wake: Waker,
    /// A secret copy's state changed on the worker: run a frame so the app's poll sees it.
    changed: Arc<AtomicBool>,
}
impl Default for Client {
    fn default() -> Self {
        let (sender, replies) = mpsc::channel();
        Self { sender, replies, wake: std::sync::Arc::new(|| {}), changed: Arc::default() }
    }
}
impl Client {
    pub fn connect(&mut self, wake: Waker) { self.wake = wake; }
    pub fn read<S: 'static>(&self, rt: &Runtime<S>) {
        if let Some(m) = &rt.clipboard {
            if let Some(text) = m.text() && self.sender.send(Reply { target: rt.paste_target(), text }).is_ok() { (self.wake)(); }
            return;
        }
        self.read_target(rt.paste_target());
    }
    fn read_target(&self, target: Option<String>) {
        let _ = worker().send(Request::Read { target, reply: self.sender.clone(), wake: self.wake.clone() });
    }
    pub fn receive<S: 'static>(&self, rt: &mut Runtime<S>) -> bool {
        let mut received = self.changed.swap(false, Ordering::SeqCst);
        for reply in self.replies.try_iter() { rt.paste_result(reply.target, reply.text); received = true; }
        received
    }
    pub fn flush<S: 'static>(&self, rt: &mut Runtime<S>) {
        if rt.clipboard.is_some() { return flush_memory(rt); }
        // Nothing to clear before the first write.
        if std::mem::take(&mut rt.clear_secret_request) && let Some(w) = WORKER.get() { let _ = w.sender.send(Request::Clear(None)); }
        if let Some(copy) = rt.copy_request.take() {
            let worker = worker();
            copy.bind(Route::System(worker.clone()));
            let _ = worker.send(Request::Write(copy, Some(Notify { changed: self.changed.clone(), wake: self.wake.clone() })));
        }
        if let Some(target) = rt.paste_request.take() { self.read_target(target); }
    }
}

/// Applies a runtime's clipboard requests to its memory clipboard, if it has one. Runs after
/// every frame, so headless runtimes write too.
pub(crate) fn flush_memory<S: 'static>(rt: &mut Runtime<S>) {
    let Some(m) = rt.clipboard.clone() else { return };
    if std::mem::take(&mut rt.clear_secret_request) { m.handle(Request::Clear(None)); }
    if let Some(copy) = rt.copy_request.take() {
        copy.bind(Route::Memory(m.clone()));
        m.handle(Request::Write(copy, None));
    }
    if let Some(target) = rt.paste_request.take() && let Some(text) = m.text() {
        rt.paste_result(target, text);
    }
}

impl<S: 'static> Runtime<S> {
    /// Use a shared in-memory clipboard (`clipboard::memory()`) instead of the OS one: copies,
    /// secret clears and pastes go to it, in headless runs too. For tests.
    pub fn use_clipboard(&mut self, clipboard: &MemoryClipboard) { self.clipboard = Some(clipboard.clone()); }
}

/// An in-memory clipboard for tests, shared by its clones and by every runtime given it with
/// `Runtime::use_clipboard`. It behaves like the OS clipboard, synchronously.
#[derive(Clone)]
pub struct MemoryClipboard(Arc<Mutex<Memory>>);
struct Memory { core: Core, board: Board }
#[derive(Default)]
struct Board { text: Option<Zeroizing<String>>, revision: u64, fail: Option<String> }

pub fn memory() -> MemoryClipboard {
    MemoryClipboard(Arc::new(Mutex::new(Memory { core: Core::default(), board: Board::default() })))
}
impl MemoryClipboard {
    pub fn text(&self) -> Option<Zeroizing<String>> { self.ticked(|_, board| board.text.clone()) }
    /// Changes the clipboard as another app would.
    pub fn set_text(&self, text: &str) {
        let mut m = lock(&self.0);
        let Memory { core, board } = &mut *m;
        board.text = Some(Zeroizing::new(text.into()));
        board.revision += 1;
        core.tick(board);
    }
    /// Makes later writes fail with `error`, or succeed again with `None`.
    pub fn fail_writes(&self, error: Option<&str>) { lock(&self.0).board.fail = error.map(Into::into); }
    fn tick(&self) { self.ticked(|_, _| ()) }
    fn handle(&self, request: Request) { self.ticked(|core, board| core.handle(board, request)) }
    /// Lets a held secret expire or notice a change first, as the OS worker does per request.
    fn ticked<R>(&self, f: impl FnOnce(&mut Core, &mut Board) -> R) -> R {
        let mut m = lock(&self.0);
        let Memory { core, board } = &mut *m;
        core.tick(board);
        f(core, board)
    }
}
impl std::fmt::Debug for MemoryClipboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_struct("MemoryClipboard").finish_non_exhaustive() }
}
impl Backend for Board {
    fn write(&mut self, copy: &ClipboardWrite) -> Result<(), String> {
        if let Some(error) = &self.fail { return Err(error.clone()); }
        self.text = Some(copy.text.clone());
        self.revision += 1;
        Ok(())
    }
    fn text(&mut self) -> Option<Zeroizing<String>> { self.text.clone() }
    fn revision(&self) -> Option<u64> { Some(self.revision) }
    fn clear(&mut self) { self.text = None; self.revision += 1; }
}

static WORKER: OnceLock<Worker> = OnceLock::new();
struct Worker {
    sender: mpsc::Sender<Request>,
    join: Mutex<Option<(std::thread::JoinHandle<()>, mpsc::Receiver<()>)>>,
}
#[cfg(not(target_arch = "wasm32"))]
impl Worker {
    fn spawn<B: Backend + 'static>(open: impl FnMut() -> Option<B> + Send + 'static) -> Self {
        let (sender, recv) = mpsc::channel();
        let (done, completed) = mpsc::channel();
        let handle = std::thread::Builder::new().name("rune-clipboard".into()).spawn(move || {
            run_worker(recv, open);
            // Backend dropped (including Linux handoff) before acknowledging completion.
            let _ = done.send(());
        }).expect("clipboard worker");
        Self { sender, join: Mutex::new(Some((handle, completed))) }
    }
    fn stop(&self) {
        let Some((handle, completed)) = self.join.lock().unwrap().take() else { return };
        let _ = self.sender.send(Request::Shutdown);
        // A hung clipboard owner must not hang application exit. Join only after completion.
        if completed.recv_timeout(Duration::from_millis(250)).is_ok() { let _ = handle.join(); }
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn worker() -> &'static mpsc::Sender<Request> {
    &WORKER.get_or_init(|| Worker::spawn(|| arboard::Clipboard::new().ok())).sender
}

#[cfg(target_arch = "wasm32")]
fn worker() -> &'static mpsc::Sender<Request> {
    panic!("native clipboard is unsupported on web")
}

/// Called by the shared event-loop exit path, including overlay-only applications.
pub(crate) fn shutdown() {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(worker) = WORKER.get() { worker.stop(); }
}

trait Backend {
    fn write(&mut self, copy: &ClipboardWrite) -> Result<(), String>;
    fn text(&mut self) -> Option<Zeroizing<String>>;
    fn revision(&self) -> Option<u64>;
    fn clear(&mut self);
}
#[cfg(not(target_arch = "wasm32"))]
impl Backend for arboard::Clipboard {
    fn write(&mut self, copy: &ClipboardWrite) -> Result<(), String> { write(self, copy).map_err(|e| e.to_string()) }
    fn text(&mut self) -> Option<Zeroizing<String>> { self.get_text().ok().map(Zeroizing::new) }
    fn revision(&self) -> Option<u64> { revision() }
    fn clear(&mut self) { let _ = arboard::Clipboard::clear(self); }
}

/// How often the worker checks that a held secret is still on the clipboard. Where there is no
/// change count (Linux) it compares the text's fingerprint, so it checks less often.
const WATCH: Duration = if cfg!(any(target_os = "macos", target_os = "windows")) {
    Duration::from_millis(250)
} else {
    Duration::from_secs(1)
};

fn run_worker<B: Backend>(recv: mpsc::Receiver<Request>, mut open: impl FnMut() -> Option<B>) {
    let mut clipboard: Option<B> = None;
    let mut core = Core::default();
    loop {
        let request = match core.next_check() {
            Some(at) => recv.recv_timeout(at.saturating_duration_since(Instant::now())),
            None => recv.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        if let Some(cb) = &mut clipboard { core.tick(cb); }
        let request = match request {
            Ok(Request::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(request) => request,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
        };
        if matches!(request, Request::Clear(_)) && core.held.is_none() { continue; }
        if clipboard.is_none() { clipboard = open(); }
        let Some(cb) = &mut clipboard else {
            if let Request::Write(copy, notify) = request && let Some(slot) = copy.slot() {
                slot.set(SecretState::Failed("no clipboard".into()));
                if let Some(n) = notify { n.send(); }
            }
            continue;
        };
        core.handle(cb, request);
    }
    if let Some(cb) = clipboard.as_mut() { core.clear(cb, None); }
    drop(clipboard);
}

#[cfg(not(target_arch = "wasm32"))]
fn write(cb: &mut arboard::Clipboard, copy: &ClipboardWrite) -> Result<(), arboard::Error> {
    if copy.clear_after.is_none() { return cb.set_text(copy.text.as_str()); }
    #[cfg(target_os = "macos")]
    { use arboard::SetExtApple; return cb.set().exclude_from_history().text(copy.text.as_str()); }
    #[cfg(target_os = "windows")]
    { use arboard::SetExtWindows; return cb.set().exclude_from_monitoring().text(copy.text.as_str()); }
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
    { use arboard::SetExtLinux; return cb.set().exclude_from_history().text(copy.text.as_str()); }
    #[allow(unreachable_code)]
    cb.set_text(copy.text.as_str())
}

/// The secret Rune wrote last, while the clipboard may still hold it.
struct Held {
    slot: Option<Arc<Slot>>,
    notify: Option<Notify>,
    deadline: Option<Instant>,
    revision: Option<u64>,
    /// A keyed hash of the text, so the text needn't stay in memory until the clear.
    print: u64,
}

/// The clipboard owner's bookkeeping, shared by the OS worker and the memory clipboard.
#[derive(Default)]
struct Core { keys: RandomState, held: Option<Held> }
impl Core {
    fn next_check(&self) -> Option<Instant> {
        let held = self.held.as_ref()?;
        let watch = Instant::now() + WATCH;
        Some(held.deadline.map_or(watch, |d| d.min(watch)))
    }

    fn handle<B: Backend>(&mut self, cb: &mut B, request: Request) {
        match request {
            Request::Write(copy, notify) => self.write(cb, copy, notify),
            Request::Clear(which) => self.clear(cb, which.as_ref()),
            Request::Read { target, reply, wake } => {
                if let Some(text) = cb.text() && reply.send(Reply { target, text }).is_ok() { wake(); }
            }
            Request::Shutdown => {}
        }
    }

    fn write<B: Backend>(&mut self, cb: &mut B, copy: ClipboardWrite, notify: Option<Notify>) {
        let slot = copy.slot().cloned();
        let notify = notify.filter(|_| slot.is_some());
        let cancelled = |slot: &Option<Arc<Slot>>| slot.as_ref().is_some_and(|s| s.cancel.load(Ordering::SeqCst));
        if cancelled(&slot) { return; } // `Unwritten` settles it as cleared.
        if let Err(error) = cb.write(&copy) {
            if let Some(s) = &slot { s.set(SecretState::Failed(error)); }
            if let Some(n) = &notify { n.send(); }
            return;
        }
        self.release(SecretState::Replaced);
        if let Some(delay) = copy.clear_after {
            if let Some(s) = &slot { s.set(SecretState::Holding); }
            self.held = Some(Held {
                slot: slot.clone(),
                notify: notify.clone(),
                deadline: Instant::now().checked_add(delay),
                revision: cb.revision(),
                print: self.keys.hash_one(copy.text.as_str()),
            });
            if let Some(n) = &notify { n.send(); }
            if cancelled(&slot) { self.clear(cb, slot.as_ref()); }
        }
    }

    fn release(&mut self, state: SecretState) {
        let Some(held) = self.held.take() else { return };
        if let Some(s) = &held.slot { s.set(state); }
        if let Some(n) = &held.notify { n.send(); }
    }

    fn unchanged<B: Backend>(&self, cb: &mut B) -> bool {
        let Some(held) = &self.held else { return false };
        held.revision == cb.revision() && cb.text().is_some_and(|t| self.keys.hash_one(t.as_str()) == held.print)
    }

    fn clear<B: Backend>(&mut self, cb: &mut B, which: Option<&Arc<Slot>>) {
        let Some(held) = &self.held else { return };
        if which.is_some_and(|w| !held.slot.as_ref().is_some_and(|s| Arc::ptr_eq(s, w))) { return; }
        if self.unchanged(cb) {
            cb.clear();
            self.release(SecretState::Cleared);
        } else {
            self.release(SecretState::Replaced);
        }
    }

    /// Expire the held secret, or let it go if the clipboard changed under it.
    fn tick<B: Backend>(&mut self, cb: &mut B) {
        let Some(held) = &self.held else { return };
        if held.deadline.is_some_and(|d| Instant::now() >= d) { return self.clear(cb, None); }
        let changed = match held.revision {
            Some(revision) => cb.revision() != Some(revision),
            None => !self.unchanged(cb),
        };
        if changed { self.release(SecretState::Replaced); }
    }
}

fn revision() -> Option<u64> {
    #[cfg(target_os = "macos")]
    { return Some(unsafe { objc2_app_kit::NSPasteboard::generalPasteboard().changeCount() } as u64); }
    #[cfg(target_os = "windows")]
    { return Some(unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() } as u64); }
    // Linux's portable clipboard API has no generation counter; compare the current text.
    #[allow(unreachable_code)]
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct State { text: String, revision: u64, clears: usize, drops: usize }
    struct Fake { state: Arc<Mutex<State>>, dropped: Option<mpsc::Sender<()>> }
    impl Backend for Fake {
        fn write(&mut self, copy: &ClipboardWrite) -> Result<(), String> {
            let mut s = self.state.lock().unwrap();
            s.text = copy.to_string(); s.revision += 1;
            Ok(())
        }
        fn text(&mut self) -> Option<Zeroizing<String>> { Some(Zeroizing::new(self.state.lock().unwrap().text.clone())) }
        fn revision(&self) -> Option<u64> { Some(self.state.lock().unwrap().revision) }
        fn clear(&mut self) { let mut s = self.state.lock().unwrap(); s.text.clear(); s.clears += 1; }
    }
    impl Drop for Fake {
        fn drop(&mut self) {
            self.state.lock().unwrap().drops += 1;
            if let Some(dropped) = &self.dropped { let _ = dropped.send(()); }
        }
    }

    const LONG: Duration = Duration::from_secs(3600);

    fn copy(m: &MemoryClipboard, text: &str, clear_after: Duration) -> SecretCopy {
        let (write, handle) = ClipboardWrite::secret(text.into(), clear_after);
        write.bind(Route::Memory(m.clone()));
        m.handle(Request::Write(write, None));
        handle
    }
    fn text(m: &MemoryClipboard) -> Option<String> { m.text().map(|t| t.to_string()) }

    #[test]
    fn shutdown_expires_only_our_pending_secret_before_dropping_backend() {
        for replacement in [None, Some("new value"), Some("fixture")] {
            let state = Arc::new(Mutex::new(State::default()));
            let shared = state.clone();
            let worker = Worker::spawn(move || Some(Fake { state: shared.clone(), dropped: None }));
            worker.sender.send(Request::Write(ClipboardWrite::new("fixture".into(), Some(LONG)), None)).unwrap();
            let (reply, received) = mpsc::channel();
            worker.sender.send(Request::Read { target: None, reply, wake: Arc::new(|| {}) }).unwrap();
            received.recv_timeout(Duration::from_secs(1)).unwrap(); // Write and revision recorded.
            if let Some(text) = replacement {
                let mut s = state.lock().unwrap(); s.text = text.into(); s.revision += 1;
            }
            worker.stop(); worker.stop();
            let s = state.lock().unwrap();
            assert_eq!(s.text, replacement.unwrap_or(""));
            assert_eq!(s.clears, usize::from(replacement.is_none()));
            assert_eq!(s.drops, 1, "normal shutdown joins after clipboard handoff/drop");
        }
    }

    #[test]
    fn shutdown_does_not_wait_indefinitely_for_a_blocked_backend() {
        let (entered, waiting) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (dropped, finished) = mpsc::channel();
        let worker = Worker::spawn(move || {
            entered.send(()).unwrap();
            released.recv().unwrap();
            Some(Fake { state: Arc::default(), dropped: Some(dropped.clone()) })
        });
        worker.sender.send(Request::Write(ClipboardWrite::new("fixture".into(), Some(LONG)), None)).unwrap();
        waiting.recv_timeout(Duration::from_secs(1)).unwrap();
        let start = Instant::now();
        worker.stop();
        assert!(start.elapsed() < Duration::from_secs(1));
        release.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    /// The OS worker reports the write, then notices another app's change on its own.
    #[test]
    fn worker_reports_the_write_and_watches_for_outside_changes() {
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        let worker = Worker::spawn(move || Some(Fake { state: shared.clone(), dropped: None }));
        let (woke, wakes) = mpsc::channel();
        let notify = Notify { changed: Arc::default(), wake: Arc::new(move || { let _ = woke.send(()); }) };
        let (write, handle) = ClipboardWrite::secret("fixture".into(), LONG);
        write.bind(Route::System(worker.sender.clone()));
        worker.sender.send(Request::Write(write, Some(notify.clone()))).unwrap();
        wakes.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(handle.holds() && notify.changed.load(Ordering::SeqCst));
        { let mut s = state.lock().unwrap(); s.text = "theirs".into(); s.revision += 1; }
        wakes.recv_timeout(WATCH * 8).unwrap();
        assert_eq!(handle.state(), SecretState::Replaced);
        handle.clear_now();
        worker.stop();
        assert_eq!(state.lock().unwrap().text, "theirs");
    }

    #[test]
    fn holds_until_a_later_copy_or_an_outside_change() {
        let m = memory();
        let first = copy(&m, "fixture", LONG);
        assert!(first.holds());
        let second = copy(&m, "fixture", LONG);
        assert_eq!(first.state(), SecretState::Replaced, "identical text from a later copy still supersedes");
        assert!(second.holds());
        m.set_text("theirs");
        assert_eq!(second.state(), SecretState::Replaced);
        let expired = copy(&m, "brief", Duration::ZERO);
        assert_eq!(expired.state(), SecretState::Cleared);
        assert_eq!(text(&m), None);
        assert!(!format!("{expired:?} {m:?}").contains("brief"));
    }

    #[test]
    fn clear_now_clears_only_while_it_holds() {
        let m = memory();
        let replaced = copy(&m, "secret", LONG);
        m.set_text("theirs");
        replaced.clear_now();
        assert_eq!(text(&m).as_deref(), Some("theirs"));
        let held = copy(&m, "secret", LONG);
        held.clear_now();
        held.clear_now();
        assert_eq!((held.state(), text(&m)), (SecretState::Cleared, None));
        let (write, unwritten) = ClipboardWrite::secret("never".into(), LONG);
        write.bind(Route::Memory(m.clone()));
        unwritten.clear_now();
        m.handle(Request::Write(write, None));
        assert_eq!((unwritten.state(), text(&m)), (SecretState::Cleared, None));
        let (write, dropped) = ClipboardWrite::secret("dropped".into(), LONG);
        drop(write);
        assert_eq!(dropped.state(), SecretState::Replaced);
    }

    #[test]
    fn a_failed_write_reports_and_keeps_the_earlier_copy() {
        let m = memory();
        let earlier = copy(&m, "earlier", LONG);
        m.fail_writes(Some("pasteboard denied"));
        let failed = copy(&m, "later", LONG);
        assert_eq!(failed.state(), SecretState::Failed("pasteboard denied".into()));
        assert!(earlier.holds());
        assert_eq!(text(&m).as_deref(), Some("earlier"));
    }

    #[test]
    fn two_runtimes_share_a_memory_clipboard() {
        use crate::{col, Cx, El, Env};
        fn view(_: &Option<SecretCopy>, _: &mut Cx<Option<SecretCopy>>) -> El { col() }
        let m = memory();
        let [mut a, mut b] = [0, 1].map(|_| {
            let mut rt = Runtime::new(None, view, (100.0, 100.0), Env::new(true, 1.0));
            rt.use_clipboard(&m);
            rt.frame(0.0);
            rt
        });
        a.update(|s, tx| *s = Some(tx.copy_secret("from a", LONG))); a.frame(0.0);
        assert!(a.state.as_ref().unwrap().holds());
        b.update(|s, tx| *s = Some(tx.copy_secret("from b", LONG))); b.frame(0.0);
        assert_eq!(text(&m).as_deref(), Some("from b"));
        assert_eq!(a.state.as_ref().unwrap().state(), SecretState::Replaced);
        a.update(|_, tx| tx.clear_secret_clipboard()); a.frame(0.0);
        assert_eq!(text(&m), None, "either window clears the latest secret");
        assert_eq!(b.state.as_ref().unwrap().state(), SecretState::Cleared);

        a.update(|s, tx| *s = Some(tx.copy_secret("no frame", LONG)));
        assert_eq!(text(&m).as_deref(), Some("no frame"), "update writes without waiting for a frame");

        let mut headless = Runtime::new(None, view, (100.0, 100.0), Env::new(true, 1.0));
        headless.update(|s, tx| *s = Some(tx.copy_secret("nowhere", LONG))); headless.frame(0.0);
        assert_eq!(headless.state.as_ref().unwrap().state(), SecretState::Pending, "no clipboard, no write");
    }

    /// `on_close` runs through `update`, and a hidden window only polls: both must reach the
    /// clipboard before any frame.
    #[test]
    fn clears_from_on_close_and_a_hidden_poll_need_no_frame() {
        use crate::{col, Cx, El, Env};
        fn view(_: &u8, _: &mut Cx<u8>) -> El { col() }
        let m = memory();
        let mut rt = Runtime::new(0, view, (100.0, 100.0), Env::new(true, 1.0));
        rt.use_clipboard(&m);
        rt.frame(0.0);
        let on_close: fn(&mut u8, &mut crate::Tx) = |_, tx| tx.clear_secret_clipboard();
        let held = copy(&m, "secret", LONG);
        rt.update(on_close);
        assert_eq!((held.state(), text(&m)), (SecretState::Cleared, None));

        rt.poll = Some(|_, _, tx| { tx.clear_secret_clipboard(); false });
        let held = copy(&m, "secret", LONG);
        rt.poll_hidden();
        assert_eq!((held.state(), text(&m)), (SecretState::Cleared, None));
    }

    #[test]
    fn a_clear_after_an_unwritten_copy_drops_the_stale_secret() {
        use crate::{col, Cx, El, Env};
        fn view(_: &Option<SecretCopy>, _: &mut Cx<Option<SecretCopy>>) -> El { col() }
        // No clipboard: the copy waits for the shell, as it does between window flushes.
        let mut rt = Runtime::new(None, view, (100.0, 100.0), Env::new(true, 1.0));
        rt.update(|s, tx| *s = Some(tx.copy_secret("stale", LONG)));
        rt.update(|_, tx| tx.clear_secret_clipboard());
        assert!(rt.copy_request.is_none() && rt.clear_secret_request);
        assert_eq!(rt.state.as_ref().unwrap().state(), SecretState::Replaced);

        // The same in one handler, and with a memory clipboard on the next flush.
        let m = memory();
        rt.use_clipboard(&m);
        rt.update(|s, tx| { *s = Some(tx.copy_secret("stale", LONG)); tx.clear_secret_clipboard(); });
        rt.frame(0.0);
        assert_eq!(text(&m), None);
        assert_eq!(rt.state.as_ref().unwrap().state(), SecretState::Replaced);
    }

    #[test]
    fn a_staged_pending_copy_stays_pending_and_clear_now_is_a_no_op() {
        let copy = SecretCopy::pending();
        let clone = copy.clone();
        drop(copy);
        assert_eq!(clone.state(), SecretState::Pending);
        clone.clear_now();
        assert_eq!(clone.state(), SecretState::Pending);
        assert!(!clone.holds());
    }
}

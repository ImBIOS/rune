//! Receiving links the system opens with this app (`myapp://...`): `App::on_open_url`.
//!
//! winit 0.30 doesn't forward them, so on macOS Rune listens for the get-URL Apple event itself
//! (`kInternetEventClass`/`kAEGetURL`). `App::run` installs the listener before the event loop
//! starts, so the link that launched the app arrives too; a window's start installs it for apps
//! run through `run_windows`, which is after launch. Only an app with a hook installs it, once
//! per process, and the first app to do so receives every link.
//!
//! A link becomes an `AppHandle::update` running the hook: it queues until the app has started
//! (its first window open, unless it's windowless), runs in arrival order on the main thread in a normal transaction
//! (state commits and the window redraws), and wakes an idle loop. The link is passed through as
//! the system gave it; it comes from outside the app, so the hook must parse and validate it.
//!
//! Other platforms never call the hook: iOS (winit owns the scene delegate) is a follow-up, and
//! Windows and Linux hand links to a new process as launch arguments.
// The web build runs no native window, so nothing here is reached there.
#![cfg_attr(target_arch = "wasm32", allow(dead_code))]
use std::sync::Mutex;

use crate::app::{App, AppHandle};
use crate::el::Tx;

/// `App::on_open_url`'s hook.
pub(crate) type OnOpenUrl<S> = fn(&mut S, &str, &mut Tx);

type Sink = Box<dyn Fn(String) + Send>;

/// Where links go: the first app that listened.
#[derive(Default)]
pub(crate) struct Inbox {
    sink: Mutex<Option<Sink>>,
}

impl Inbox {
    pub(crate) const fn new() -> Self { Inbox { sink: Mutex::new(None) } }

    /// Sends this inbox's links to `app`'s hook and runs `install` (the platform listener), unless
    /// the app has no hook or another app already listens. Returns whether it did.
    pub(crate) fn listen<S: 'static>(&self, app: &App<S>, install: impl FnOnce() -> bool) -> bool {
        let Some(hook) = app.on_open_url else { return false };
        let mut sink = self.sink.lock().unwrap();
        if sink.is_some() || !install() { return false; }
        let handle: AppHandle<S> = app.handle();
        *sink = Some(Box::new(move |url| handle.update(move |s, tx| hook(s, &url, tx))));
        true
    }

    /// A link arrived; dropped when no app listens.
    #[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
    pub(crate) fn receive(&self, url: String) {
        if let Some(sink) = self.sink.lock().unwrap().as_ref() { sink(url); }
    }
}

static INBOX: Inbox = Inbox::new();

/// Installs the platform's listener for `app` if it has a hook (see the module docs).
pub(crate) fn listen<S: 'static>(app: &App<S>) {
    INBOX.listen(app, platform::install);
}

#[cfg(target_os = "macos")]
use mac as platform;

#[cfg(not(target_os = "macos"))]
mod platform {
    pub(super) fn install() -> bool { false }
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, NSObject, Sel};
    use objc2::{ClassType, class, msg_send, sel};
    use objc2_foundation::{MainThreadMarker, NSString};

    /// `'GURL'`: `kInternetEventClass` and `kAEGetURL`.
    const GET_URL: u32 = u32::from_be_bytes(*b"GURL");
    /// `'----'`: `keyDirectObject`, the event's URL string.
    const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

    /// The URL string of a get-URL event.
    pub(super) fn url_of(event: &AnyObject) -> Option<String> {
        // SAFETY: `event` is an NSAppleEventDescriptor; both messages are nil-safe and return
        // an autoreleased object or nil.
        unsafe {
            let param: *mut AnyObject = msg_send![event, paramDescriptorForKeyword: DIRECT_OBJECT];
            let string: *mut NSString = msg_send![param.as_ref()?, stringValue];
            Some(string.as_ref()?.to_string())
        }
    }

    extern "C" fn get_url(_: &AnyObject, _: Sel, event: *mut AnyObject, _reply: *mut AnyObject) {
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // SAFETY: AppKit passes the event, or nil.
            if let Some(url) = unsafe { event.as_ref() }.and_then(url_of) { super::INBOX.receive(url); }
        }));
        if caught.is_err() { eprintln!("rune: open-url handler panicked"); }
    }

    /// Makes Rune's object the handler of get-URL Apple events. Main thread only.
    pub(super) fn install() -> bool {
        if MainThreadMarker::new().is_none() { return false; }
        const NAME: &str = "RuneOpenUrlHandler";
        let class = AnyClass::get(NAME).or_else(|| {
            let mut builder = ClassBuilder::new(NAME, NSObject::class())?;
            // SAFETY: the function matches `handleGetURLEvent:withReplyEvent:` (two objects, void).
            unsafe { builder.add_method(sel!(handleGetURLEvent:withReplyEvent:), get_url as extern "C" fn(_, _, _, _)); }
            Some(builder.register())
        });
        let Some(class) = class else { return false };
        // SAFETY: Foundation calls on the main thread. The handler is never released: the event
        // manager doesn't retain it, and it serves the whole process.
        unsafe {
            let handler: *mut AnyObject = msg_send![class, new];
            if handler.is_null() { return false; }
            let manager: *mut AnyObject = msg_send![class!(NSAppleEventManager), sharedAppleEventManager];
            let Some(manager) = manager.as_ref() else { return false };
            let _: () = msg_send![manager, setEventHandler: handler, andSelector: sel!(handleGetURLEvent:withReplyEvent:),
                forEventClass: GET_URL, andEventID: GET_URL];
        }
        true
    }
}

#[cfg(test)]
#[path = "open_url_in_tests.rs"]
mod tests;

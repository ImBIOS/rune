//! Reopening a running app: on macOS, a click on its Dock icon, or opening it again from Finder,
//! Spotlight or `open` while it runs. Without this an app whose windows Rune hid (⌘W with
//! `keep_running`, `hide`) stays invisible, because AppKit only brings back minimized windows.
//!
//! AppKit answers the reopen Apple event itself and asks the app delegate
//! `applicationShouldHandleReopen:hasVisibleWindows:`. winit's delegate doesn't implement it,
//! so Rune adds the method to the delegate's class once (`install`). The method only sends an
//! event to the loop; `App::on_reopen` hooks and the default (`target`) run from it, like any
//! other wake, never inside the AppKit callback.

use std::sync::atomic::{AtomicU64, Ordering};

/// One window as reopening sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Candidate {
    /// An app window that still exists and can come back: not a launcher or other
    /// nonactivating panel, an overlay, or a window that closed for good.
    pub reopenable: bool,
    /// On screen and not minimized.
    pub visible: bool,
    pub minimized: bool,
    /// When Rune last hid it (a close with `keep_running`, `hide`), as `stamp` counted.
    pub hidden_at: Option<u64>,
    /// A window of the app itself, not one `App::window` added.
    pub main: bool,
}

/// Whether any window reopening could bring back already shows.
pub(crate) fn has_visible(windows: &[Candidate]) -> bool {
    windows.iter().any(|w| w.reopenable && w.visible)
}

/// The window the default reopen shows, when none shows: a minimized one, as AppKit restores
/// them; else the one Rune hid last; else the first main window, which may never have shown.
pub(crate) fn target(windows: &[Candidate]) -> Option<usize> {
    if has_visible(windows) { return None; }
    let ok = |w: &&Candidate| w.reopenable;
    let indexed = || windows.iter().enumerate().filter(|(_, w)| ok(w));
    indexed().find(|(_, w)| w.minimized).map(|(i, _)| i)
        .or_else(|| indexed().filter_map(|(i, w)| Some((w.hidden_at?, i))).max().map(|(_, i)| i))
        .or_else(|| indexed().find(|(_, w)| w.main).map(|(i, _)| i))
}

static HIDES: AtomicU64 = AtomicU64::new(0);

/// A number larger than every earlier one, for the order windows hid in.
pub(crate) fn stamp() -> u64 { HIDES.fetch_add(1, Ordering::Relaxed) + 1 }

#[cfg(target_os = "macos")]
pub(crate) use mac::{accessory, install};

#[cfg(target_os = "macos")]
mod mac {
    use std::cell::RefCell;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool, Sel};
    use objc2::{ffi, msg_send_id, sel};
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::MainThreadMarker;

    thread_local! {
        static REOPEN: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
    }

    unsafe extern "C" fn should_handle_reopen(_: &AnyObject, _: Sel, _: *mut AnyObject, _: Bool) -> Bool {
        let sent = REOPEN.with(|reopen| reopen.borrow().as_ref().map(|f| f()).is_some());
        // Rune decides what a reopen shows; AppKit's own handling would only deminiaturize.
        Bool::new(!sent)
    }

    /// Calls `reopen` on the main thread each time the app is reopened. Adds the reopen method
    /// to the app delegate's class unless the class already answers it (a later winit, or an app's
    /// own delegate), in which case reopening stays with that delegate. Main thread only.
    pub(crate) fn install(reopen: impl Fn() + 'static) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: NSApplication's delegate getter, on the main thread.
        let delegate: Option<Retained<AnyObject>> = unsafe { msg_send_id![&app, delegate] };
        let Some(delegate) = delegate else { return };
        let selector = sel!(applicationShouldHandleReopen:hasVisibleWindows:);
        let class = delegate.class();
        let ours = REOPEN.with(|r| r.borrow().is_some());
        if !ours && class.responds_to(selector) { return; }
        REOPEN.with(|r| *r.borrow_mut() = Some(Box::new(reopen)));
        if ours { return; }
        // BOOL is `bool` on Apple silicon and `signed char` on Intel.
        let types = if cfg!(target_arch = "aarch64") { c"B@:@B" } else { c"c@:@c" };
        let imp = should_handle_reopen as unsafe extern "C" fn(&AnyObject, Sel, *mut AnyObject, Bool) -> Bool;
        // SAFETY: the function's signature matches the selector's and `types`; adding a method
        // the class lacks leaves its other methods and instances as they were.
        unsafe {
            ffi::class_addMethod(
                (class as *const objc2::runtime::AnyClass).cast_mut().cast(),
                selector.as_ptr(),
                Some(std::mem::transmute::<_, unsafe extern "C" fn()>(imp)),
                types.as_ptr(),
            );
        }
    }

    /// The app has no Dock icon now (`App::accessory`, `AppHandle::set_dock_icon(false)`).
    pub(crate) fn accessory() -> bool {
        let Some(mtm) = MainThreadMarker::new() else { return false };
        // SAFETY: reading the activation policy on the main thread.
        unsafe { NSApplication::sharedApplication(mtm).activationPolicy() != NSApplicationActivationPolicy::Regular }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(visible: bool, hidden_at: Option<u64>) -> Candidate {
        Candidate { reopenable: true, visible, minimized: false, hidden_at, main: false }
    }

    #[test]
    fn reopening_shows_the_window_hidden_last() {
        let windows = [Candidate { main: true, ..window(false, Some(1)) }, window(false, Some(3)), window(false, Some(2))];
        assert!(!has_visible(&windows));
        assert_eq!(target(&windows), Some(1));
    }

    #[test]
    fn a_visible_window_leaves_reopening_alone() {
        let windows = [window(true, None), window(false, Some(5))];
        assert!(has_visible(&windows));
        assert_eq!(target(&windows), None);
    }

    #[test]
    fn a_minimized_window_comes_back_first() {
        let windows = [window(false, Some(9)), Candidate { minimized: true, ..window(false, None) }];
        assert_eq!(target(&windows), Some(1));
    }

    #[test]
    fn without_a_hidden_window_the_main_window_shows() {
        let windows = [window(false, None), Candidate { main: true, ..window(false, None) }];
        assert_eq!(target(&windows), Some(1));
    }

    #[test]
    fn panels_and_closed_windows_never_come_back_or_count_as_visible() {
        let panel = Candidate { reopenable: false, ..window(true, Some(7)) };
        let closed = Candidate { reopenable: false, main: true, ..window(false, Some(8)) };
        assert!(!has_visible(&[panel, closed]));
        assert_eq!(target(&[panel, closed]), None);
        assert_eq!(target(&[panel, closed, window(false, Some(1))]), Some(2));
    }

    #[test]
    fn stamps_count_up() {
        let (a, b) = (stamp(), stamp());
        assert!(b > a);
    }
}

//! AppKit pieces winit 0.30 doesn't expose: the activation policy at runtime and overlay windows.

use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::MainThreadMarker;

/// Regular (Dock icon and app menu) or accessory (menu-bar only). Called on the main thread.
pub(crate) fn set_dock_icon(visible: bool) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let policy = if visible { NSApplicationActivationPolicy::Regular } else { NSApplicationActivationPolicy::Accessory };
    NSApplication::sharedApplication(mtm).setActivationPolicy(policy);
}

fn ns_window(window: &winit::window::Window) -> Option<objc2::rc::Retained<objc2_app_kit::NSWindow>> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else { return None };
    // SAFETY: winit's content view is alive as long as its window, which the caller holds.
    let view: &objc2_app_kit::NSView = unsafe { handle.ns_view.cast().as_ref() };
    view.window()
}

/// An overlay's window: status level on every space (also over full-screen apps), out of the
/// window cycle, transparent, ignoring the mouse, and never hidden when the app deactivates.
pub(crate) fn prepare_overlay(window: &winit::window::Window) {
    use objc2_app_kit::{NSColor, NSWindowCollectionBehavior};
    let Some(w) = ns_window(window) else { return };
    // kCGStatusWindowLevel: above floating panels and the menu bar's own windows.
    w.setLevel(25);
    w.setIgnoresMouseEvents(true);
    w.setOpaque(false);
    // SAFETY: plain AppKit property setters on the main thread.
    unsafe {
        w.setBackgroundColor(Some(&NSColor::clearColor()));
        w.setHidesOnDeactivate(false);
        w.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
    }
}

/// Orders the overlay front without making it key or activating the app.
pub(crate) fn show_overlay(window: &winit::window::Window) {
    let Some(w) = ns_window(window) else { return };
    // SAFETY: an AppKit call on the main thread.
    unsafe { w.orderFrontRegardless() };
}

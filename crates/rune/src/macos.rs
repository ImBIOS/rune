//! AppKit activation policy, overlay windows, and visibility without activation.

use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::MainThreadMarker;
pub(crate) use show_overlay as show_without_focus;

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

/// Orders a window front without making it key or activating the app.
#[allow(dead_code)]
pub(crate) fn show_overlay(window: &winit::window::Window) {
    let Some(w) = ns_window(window) else { return };
    // SAFETY: an AppKit call on the main thread.
    unsafe { w.orderFrontRegardless() };
}

/// An overlay's non-activating panel. winit's content view (with the Metal layer wgpu draws
/// into) moves into it, so clicks reach Rune through winit while the app never activates and
/// the panel never becomes key. winit's own window stays hidden and gets its view back on drop.
pub(crate) struct Panel {
    panel: objc2::rc::Retained<objc2_app_kit::NSPanel>,
    window: objc2::rc::Retained<objc2_app_kit::NSWindow>,
    view: objc2::rc::Retained<objc2_app_kit::NSView>,
}

impl Panel {
    pub fn adopt(window: &winit::window::Window, at: crate::overlay::Placement, interactive: bool) -> Option<Panel> {
        use objc2_app_kit::{NSBackingStoreType, NSColor, NSPanel, NSView, NSWindowCollectionBehavior, NSWindowStyleMask};
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let mtm = MainThreadMarker::new()?;
        let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else { return None };
        // SAFETY: winit's content view is alive while its window is, and we retain it.
        let view = unsafe { objc2::rc::Retained::retain(handle.ns_view.as_ptr().cast::<NSView>()) }?;
        let ns_window = view.window()?;
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(at.size.0 as f64, at.size.1 as f64));
        // SAFETY: AppKit calls on the main thread; the panel is retained by `Panel`.
        let panel = unsafe {
            NSPanel::initWithContentRect_styleMask_backing_defer(
                mtm.alloc(),
                rect,
                NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                NSBackingStoreType::NSBackingStoreBuffered,
                false,
            )
        };
        unsafe {
            panel.setReleasedWhenClosed(false);
            panel.setFloatingPanel(true);
            panel.setBecomesKeyOnlyIfNeeded(true);
            panel.setHidesOnDeactivate(false);
            panel.setBackgroundColor(Some(&NSColor::clearColor()));
            panel.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::Stationary
                    | NSWindowCollectionBehavior::IgnoresCycle
                    | NSWindowCollectionBehavior::FullScreenAuxiliary,
            );
        }
        panel.setLevel(25);
        panel.setOpaque(false);
        panel.setHasShadow(false);
        panel.setIgnoresMouseEvents(!interactive);
        ns_window.setContentView(None);
        panel.setContentView(Some(&view));
        Some(Panel { panel, window: ns_window, view })
    }

    /// On the primary screen (the one with the menu bar), in AppKit's bottom-left coordinates.
    pub fn place(&self, at: crate::overlay::Placement) {
        use objc2_app_kit::NSScreen;
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        let Some(mtm) = MainThreadMarker::new() else { return };
        // SAFETY: reading the screen list on the main thread.
        let Some(screen) = (unsafe { NSScreen::screens(mtm).firstObject() }) else { return };
        let frame = screen.frame();
        let (w, h) = (at.size.0 as f64, at.size.1 as f64);
        let x = match at.align {
            crate::el::Align::Start => frame.origin.x + at.inset as f64,
            crate::el::Align::Center => frame.origin.x + (frame.size.width - w) / 2.0,
            crate::el::Align::End => frame.origin.x + frame.size.width - w - at.inset as f64,
        };
        let y = match at.edge {
            crate::overlay::ScreenEdge::Top => frame.origin.y + frame.size.height - h - at.margin as f64,
            crate::overlay::ScreenEdge::Bottom => frame.origin.y + at.margin as f64,
        };
        self.panel.setFrame_display(NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)), true);
    }

    pub fn show(&self) {
        // SAFETY: an AppKit call on the main thread.
        unsafe { self.panel.orderFrontRegardless() };
    }

    /// The moved view, for a new wgpu surface (winit's window handle would look for the view
    /// in winit's window, where it no longer is).
    pub fn raw_handles(&self) -> (winit::raw_window_handle::RawDisplayHandle, winit::raw_window_handle::RawWindowHandle) {
        use winit::raw_window_handle::{AppKitDisplayHandle, AppKitWindowHandle, RawDisplayHandle, RawWindowHandle};
        let view = std::ptr::NonNull::from(&*self.view).cast();
        (RawDisplayHandle::AppKit(AppKitDisplayHandle::new()), RawWindowHandle::AppKit(AppKitWindowHandle::new(view)))
    }

    pub fn hide(&self) {
        self.panel.orderOut(None);
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        self.panel.orderOut(None);
        self.panel.setContentView(None);
        self.window.setContentView(Some(&self.view));
    }
}

//! `El::help` as AppKit's own tooltips. Each help element's visible rect is a tooltip rect on
//! winit's (flipped, point-sized) content view, so AppKit owns the delay, look, placement and
//! hiding, as for SwiftUI's `.help`. The rects are re-registered when layout or scrolling moves
//! them. A tooltip's owner is its text: AppKit shows the owner's `description` when it doesn't
//! answer `view:stringForToolTip:point:userData:`, and an `NSString` describes itself.
use objc2::msg_send;
use objc2::rc::Retained;
use objc2_app_kit::NSView;
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

#[derive(Default)]
pub(crate) struct NativeHelp {
    regions: Vec<([f32; 4], String)>,
    view: Option<Retained<NSView>>,
    /// AppKit doesn't retain tooltip owners: they live here until their rects are removed.
    owners: Vec<Retained<NSString>>,
}

impl NativeHelp {
    /// Registers `regions` (window points, x0 y0 x1 y1) with `window`'s content view when they
    /// changed since the last sync. Main thread.
    pub(crate) fn sync(&mut self, window: &winit::window::Window, regions: &[([f32; 4], String)]) {
        if regions == self.regions.as_slice() { return; }
        self.clear();
        let Some(view) = crate::macos::content_view(window) else { return };
        for (r, text) in regions {
            let owner = NSString::from_str(text);
            let rect = NSRect::new(NSPoint::new(r[0] as f64, r[1] as f64), NSSize::new((r[2] - r[0]) as f64, (r[3] - r[1]) as f64));
            // SAFETY: an AppKit call on the main thread; `owners` keeps the owner alive until
            // `clear` removes its rect.
            let _: isize = unsafe { msg_send![&view, addToolTipRect: rect, owner: &*owner, userData: std::ptr::null_mut::<std::ffi::c_void>()] };
            self.owners.push(owner);
        }
        self.view = Some(view);
        self.regions = regions.to_vec();
    }

    fn clear(&mut self) {
        if let Some(view) = self.view.take() {
            // SAFETY: an AppKit call on the main thread, before the owners it names drop.
            unsafe { let _: () = msg_send![&view, removeAllToolTips]; }
        }
        self.owners.clear();
        self.regions.clear();
    }
}

impl Drop for NativeHelp {
    fn drop(&mut self) { self.clear(); }
}

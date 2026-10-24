//! AppKit activation policy, overlay windows, and visibility without activation.

use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::MainThreadMarker;
use objc2::{ClassType, DeclaredClass, declare_class, msg_send, msg_send_id, mutability};
use objc2::rc::Retained;
use objc2_app_kit::{NSEvent, NSPanel, NSWindow, NSResponder, NSScreen, NSView};
use objc2_foundation::{NSObject, NSPoint, NSRect, NSSize};
use crate::{DisplayTarget, KeyPolicy, OverlayInput, OverlayLevel, OverlayOptions, OverlaySupport};
use std::{cell::{Cell, RefCell}, collections::VecDeque, rc::Rc, time::{Duration, Instant}};

struct PanelFlags { keys: bool, warming: Rc<Cell<bool>>, revealing: Rc<Cell<bool>>, constrain: bool }

// Initialized nonactivating panel. No application activation call.
declare_class!(
    struct OverlayPanel;
    unsafe impl ClassType for OverlayPanel {
        #[inherits(NSWindow, NSResponder, NSObject)]
        type Super = NSPanel;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "RuneOverlayPanel";
    }
    impl DeclaredClass for OverlayPanel { type Ivars = PanelFlags; }
    unsafe impl OverlayPanel {
        #[method(canBecomeKeyWindow)]
        fn can_become_key(&self) -> bool { self.ivars().keys }
        #[method(canBecomeMainWindow)]
        fn can_become_main(&self) -> bool { false }
        #[method(occlusionState)]
        fn occlusion_state(&self) -> usize {
            // wgpu skips nextDrawable for occluded windows. Permit only the explicit
            // alpha-zero prewarm acquisition and a fade in or out, which AppKit reports as
            // occluded while its alpha is near zero; other visibility readbacks stay native.
            if self.ivars().warming.get() || self.ivars().revealing.get() { 2 }
            else { unsafe { msg_send![super(self), occlusionState] } }
        }
        // AppKit keeps windows below the menu bar's level out from under it; an overlay goes
        // exactly where it's placed at any level.
        #[method(constrainFrameRect:toScreen:)]
        fn constrain_frame_rect(&self, frame: NSRect, screen: *mut NSScreen) -> NSRect {
            if self.ivars().constrain { unsafe { msg_send![super(self), constrainFrameRect: frame, toScreen: screen] } }
            else { frame }
        }
        #[method(sendEvent:)]
        fn send_event(&self, event: &NSEvent) {
            // Without its view the panel would swallow this click and every later one.
            if crate::macos_panel_bridge::reattach(self.as_super()) { eprintln!("rune: panel lost its view; reattached on input"); }
            unsafe { let _: () = msg_send![super(self), sendEvent: event]; }
        }
    }
);

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" { fn CGDisplayBounds(display: u32) -> NSRect; }

/// CoreGraphics queries are thread-safe; snapshot the display before queuing a UI request.
pub(crate) fn pointer_display() -> Option<u32> {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreate(source: *const std::ffi::c_void) -> *const std::ffi::c_void;
        fn CGEventGetLocation(event: *const std::ffi::c_void) -> NSPoint;
        fn CGGetDisplaysWithPoint(point: NSPoint, max: u32, displays: *mut u32, count: *mut u32) -> i32;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" { fn CFRelease(object: *const std::ffi::c_void); }
    // SAFETY: the created event is released exactly once; output buffers live through the call.
    unsafe {
        let event = CGEventCreate(std::ptr::null());
        if event.is_null() { return None; }
        let point = CGEventGetLocation(event);
        CFRelease(event);
        let (mut id, mut count) = (0, 0);
        (CGGetDisplaysWithPoint(point, 1, &mut id, &mut count) == 0 && count > 0).then_some(id)
    }
}

pub(crate) fn place_window(window: &winit::window::Window, size: (f32, f32), request: &crate::windows::PlaceRequest) {
    let options = OverlayOptions { display: request.display.clone(), placement: Some(request.placement), ..Default::default() };
    if let (Some(window), Some(rect)) = (ns_window(window), overlay_rect(crate::windows::PlaceRequest::at(size), &options))
        && window.frame() != rect {
        window.setFrame_display(rect, true);
    }
}

/// A window's outer frame, the visible area of its screen and its backing scale, in
/// top-down global points (AppKit's are bottom-up from the primary screen).
pub(crate) fn window_geometry(window: &winit::window::Window) -> Option<([f32; 4], [f32; 4], f32)> {
    let mtm = MainThreadMarker::new()?;
    let ns = ns_window(window)?;
    let screens = NSScreen::screens(mtm);
    let primary = screens.iter().next()?.frame().size.height;
    let screen = ns.screen().or_else(|| NSScreen::mainScreen(mtm))?;
    let flip = |r: NSRect| [r.origin.x as f32, (primary - r.origin.y - r.size.height) as f32, r.size.width as f32, r.size.height as f32];
    Some((flip(ns.frame()), flip(screen.visibleFrame()), ns.backingScaleFactor() as f32))
}

/// A window's content top-left in top-down global points: the panel's once one adopts it.
pub(crate) fn content_origin(window: &winit::window::Window) -> Option<(f32, f32)> {
    let mtm = MainThreadMarker::new()?;
    let ns = ns_window(window)?;
    let primary = NSScreen::screens(mtm).iter().next()?.frame().size.height;
    let r = ns.contentRectForFrameRect(ns.frame());
    Some((r.origin.x as f32, (primary - r.origin.y - r.size.height) as f32))
}

/// The backing scale `window` draws at: its screen's while it's on one, else that of the screen
/// holding most of its frame, where ordering it in will put it.
pub(crate) fn backing_scale(window: &winit::window::Window) -> Option<f32> {
    let mtm = MainThreadMarker::new()?;
    let ns = ns_window(window)?;
    if ns.isVisible() { return Some(ns.backingScaleFactor() as f32); }
    let f = ns.frame();
    let screens: Vec<_> = NSScreen::screens(mtm).iter().map(|s| {
        let r = s.frame();
        ([r.origin.x, r.origin.y, r.size.width, r.size.height], s.backingScaleFactor())
    }).collect();
    let scale = crate::overlay_options::scale_under([f.origin.x, f.origin.y, f.size.width, f.size.height], &screens);
    Some(scale.unwrap_or_else(|| ns.backingScaleFactor()) as f32)
}

/// The outer size of a window whose content is `content` points.
pub(crate) fn outer_size(window: &winit::window::Window, (w, h): (f32, f32)) -> Option<(f32, f32)> {
    let ns = ns_window(window)?;
    // SAFETY: plain geometry on a live window.
    let frame = unsafe { ns.frameRectForContentRect(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(f64::from(w), f64::from(h)))) };
    Some((frame.size.width as f32, frame.size.height as f32))
}

/// Moves a window's outer frame to `rect` (top-down global points) without drawing: the
/// caller draws its content at the new size in the same frame.
pub(crate) fn set_window_frame(window: &winit::window::Window, [x, y, w, h]: [f32; 4]) {
    let (Some(mtm), Some(ns)) = (MainThreadMarker::new(), ns_window(window)) else { return };
    let Some(primary) = NSScreen::screens(mtm).iter().next().map(|s| s.frame().size.height) else { return };
    let rect = NSRect::new(NSPoint::new(f64::from(x), primary - f64::from(y) - f64::from(h)), NSSize::new(f64::from(w), f64::from(h)));
    if ns.frame() != rect { ns.setFrame_display(rect, false); }
}

/// Presents the surface's drawables inside the current Core Animation transaction, so a
/// window frame change and the frame drawn for it reach the screen together. Costs a
/// wait for the GPU to schedule each frame, so it is on only while a frame moves.
pub(crate) fn present_with_transaction(surface: &wgpu::Surface, on: bool) {
    use objc2::runtime::{AnyObject, Bool};
    // SAFETY: a live CAMetalLayer, on the main thread, between frames.
    unsafe {
        let Some(hal) = surface.as_hal::<wgpu::hal::api::Metal>() else { return };
        let layer = hal.render_layer().lock();
        // wgpu's objc2 may be another version than ours; the object is the same.
        let layer = &*std::ptr::from_ref(&**layer).cast::<AnyObject>();
        let _: () = msg_send![layer, setPresentsWithTransaction: Bool::new(on)];
    }
}

pub(crate) fn display_bounds(id: u32) -> [f64; 4] {
    let r = unsafe { CGDisplayBounds(id) };
    [r.origin.x, r.origin.y, r.size.width, r.size.height]
}

pub(crate) fn overlay_rect(at: crate::overlay::Placement, options: &OverlayOptions) -> Option<NSRect> {
    overlay_rect_on(at, options, None)
}

fn screen_id(screen: &NSScreen) -> u32 {
    unsafe {
        let description: *mut objc2::runtime::AnyObject = msg_send![screen, deviceDescription];
        let key = objc2_foundation::NSString::from_str("NSScreenNumber");
        let number: *mut objc2::runtime::AnyObject = msg_send![description, objectForKey: &*key];
        msg_send![number, unsignedIntValue]
    }
}

fn contains_pointer(screen: &NSScreen, pointer: NSPoint) -> bool {
    contains_appkit_rect(screen.frame(), pointer)
}

fn contains_appkit_rect(r: NSRect, pointer: NSPoint) -> bool {
    pointer.x >= r.origin.x && pointer.x < r.origin.x + r.size.width
        && pointer.y > r.origin.y && pointer.y <= r.origin.y + r.size.height
}

fn overlay_rect_on(at: crate::overlay::Placement, options: &OverlayOptions, previous: Option<u32>) -> Option<NSRect> {
    use winit::platform::macos::MonitorHandleExtMacOS;
    let mtm = MainThreadMarker::new()?;
    // SAFETY: AppKit screen and pointer queries on the event thread.
    let screens = NSScreen::screens(mtm);
    let pointer = unsafe { NSEvent::mouseLocation() };
    let screen = screens.iter().find(|screen| match &options.display {
        DisplayTarget::UnderPointer => {
            contains_pointer(screen, pointer)
        }
        DisplayTarget::Id(_) | DisplayTarget::Native(_) => {
            let native = screen_id(screen);
            match options.display {
                DisplayTarget::Id(ref id) => native == id.0.native_id(),
                DisplayTarget::Native(crate::NativeDisplayId::MacOS(id)) => native == id,
                _ => false,
            }
        },
        DisplayTarget::Primary => false,
    }).or_else(|| screens.iter().find(|s| options.display == DisplayTarget::UnderPointer && previous == Some(screen_id(s))))
        .or_else(|| screens.iter().next())?;
    let frame = screen.frame();
    let [x, y, w, h] = crate::overlay_options::local_rect(at, options.placement, (frame.size.width as f32, frame.size.height as f32));
    Some(NSRect::new(NSPoint::new(frame.origin.x + x as f64, frame.origin.y + frame.size.height - (y + h) as f64), NSSize::new(w as f64, h as f64)))
}

#[derive(Default)]
pub(crate) struct PointerState {
    input: Cell<OverlayInput>,
    regions: RefCell<Vec<[f32; 4]>>,
    pending: Cell<Option<(f32, f32)>>,
    captured: Cell<bool>,
    last: Cell<Option<(f32, f32)>>,
    inside: Cell<bool>,
    display: Cell<Option<u32>>,
    follow: Cell<bool>,
    clicks: RefCell<VecDeque<(bool, f32, f32)>>,
    release_since: Cell<Option<(Instant, (f32, f32))>>,
    recovered_release: Cell<Option<(f32, f32)>>,
    /// The cursor the overlay shows while the pointer is over its hit area.
    cursor: Cell<Option<crate::el::Cursor>>,
}

impl PointerState {
    fn trace(&self, panel: &NSPanel, source: &str, event: Option<&NSEvent>) {
        static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if !*ENABLED.get_or_init(|| std::env::var_os("RUNE_OVERLAY_TRACE_INPUT").is_some()) { return; }
        let cursor = unsafe { NSEvent::mouseLocation() };
        let frame = panel.frame();
        let local = (cursor.x - frame.origin.x, frame.origin.y + frame.size.height - cursor.y);
        let kind: usize = event.map_or(0, |event| unsafe { msg_send![event, type] });
        let event_point: NSPoint = event.map_or(NSPoint::new(0.0, 0.0), |event| unsafe { msg_send![event, locationInWindow] });
        let buttons: usize = unsafe { msg_send![NSEvent::class(), pressedMouseButtons] };
        eprintln!("overlay-input source={source} window={} kind={kind} event={event_point:?} cursor={cursor:?} local={local:?} frame={frame:?} input={:?} buttons={buttons} captured={} ignore={} regions={:?}",
            unsafe { panel.windowNumber() }, self.input.get(), self.captured.get(), unsafe { panel.ignoresMouseEvents() }, self.regions.borrow());
    }
    pub(crate) fn hit(&self, x: f32, y: f32) -> bool {
        self.captured.get() || match self.input.get() {
            OverlayInput::All => true,
            OverlayInput::Regions => self.regions.borrow().iter().any(|r| crate::overlay_options::contains(*r, x, y)),
            _ => false,
        }
    }
    fn record_click(&self, down: bool, x: f32, y: f32) {
        if !self.hit(x, y) { return; }
        if down { self.captured.set(true); self.release_since.set(None); }
        self.clicks.borrow_mut().push_back((down, x, y));
    }
    fn released(&self) {
        // Native callbacks can queue another Down before winit dispatches this Up.
        self.captured.set(self.clicks.borrow().iter().any(|(down, _, _)| *down));
        self.release_since.set(None);
    }
    fn observe_release(&self, buttons: usize, now: Instant, point: (f32, f32)) {
        if !self.captured.get() || buttons & 1 != 0 {
            self.release_since.set(None);
            return;
        }
        // winit dispatches a queued Up within the turn. One still queued seconds later was
        // never delivered (the panel lost its view), and would otherwise hold capture forever.
        let queued_up = self.clicks.borrow().iter().any(|(down, _, _)| !down);
        let grace = if queued_up { Duration::from_secs(2) } else { Duration::from_millis(100) };
        let (since, released_at) = self.release_since.get().unwrap_or_else(|| {
            self.release_since.set(Some((now, point)));
            (now, point)
        });
        if now.duration_since(since) < grace { return; }
        self.captured.set(false);
        self.clicks.borrow_mut().clear();
        self.release_since.set(None);
        self.recovered_release.set(Some(released_at));
    }
    /// winit's view was out of the panel, so the clicks recorded meanwhile never reached it.
    fn view_lost(&self) {
        self.captured.set(false);
        self.clicks.borrow_mut().clear();
        self.release_since.set(None);
        if let Some(last) = self.last.get() { self.recovered_release.set(Some(last)); }
    }
    /// Sets the cursor again after a mouse event, which may have let another window's cursor
    /// rects replace it. The arrow once the pointer is off the hit area.
    fn reapply_cursor(&self, panel: &NSPanel) {
        let Some(cursor) = self.cursor.get() else { return };
        // SAFETY: monitors run on AppKit's main thread.
        let pointer = unsafe { NSEvent::mouseLocation() };
        let frame = panel.frame();
        let (x, y) = ((pointer.x - frame.origin.x) as f32, (frame.origin.y + frame.size.height - pointer.y) as f32);
        let inside = x >= 0.0 && y >= 0.0 && x < frame.size.width as f32 && y < frame.size.height as f32;
        if inside && self.hit(x, y) { set_ns_cursor(cursor); }
        else { self.cursor.set(None); set_ns_cursor(crate::el::Cursor::Default); }
    }
    fn observe(&self, panel: &NSPanel, wake: Option<&crate::Waker>) {
        // SAFETY: monitors and frame publication run on AppKit's main thread.
        let pointer = unsafe { NSEvent::mouseLocation() };
        let buttons: usize = unsafe { msg_send![NSEvent::class(), pressedMouseButtons] };
        let frame = panel.frame();
        let (x, y) = ((pointer.x - frame.origin.x) as f32, (frame.origin.y + frame.size.height - pointer.y) as f32);
        self.observe_release(buttons, Instant::now(), (x, y));
        let ignore = !self.hit(x, y);
        if unsafe { panel.ignoresMouseEvents() } != ignore {
            panel.setIgnoresMouseEvents(ignore);
            self.trace(panel, "flip", None);
        }
        let inside = x >= 0.0 && y >= 0.0 && x < frame.size.width as f32 && y < frame.size.height as f32;
        let was_inside = self.inside.replace(inside);
        let display_changed = self.follow.get() && MainThreadMarker::new().is_some_and(|mtm| {
            NSScreen::screens(mtm).iter().find(|s| contains_pointer(s, pointer))
                .is_some_and(|s| self.display.replace(Some(screen_id(s))) != Some(screen_id(s)))
        });
        if (inside || was_inside || self.captured.get() || display_changed) && self.last.replace(Some((x, y))) != Some((x, y)) {
            self.pending.set(Some((x, y)));
            if let Some(wake) = wake { wake(); }
        }
    }
}

/// `[NSCursor set]` with winit's shape for each cursor. Main thread.
fn set_ns_cursor(cursor: crate::el::Cursor) {
    use crate::el::Cursor as C;
    thread_local! {
        static TRANSPARENT: Option<Retained<NSObject>> = image_cursor(None);
        static MOVE: Option<Retained<NSObject>> = image_cursor(Some("move"));
    }
    let Some(class) = objc2::runtime::AnyClass::get("NSCursor") else { return };
    #[cfg(feature = "macos-background-cursor")]
    sets_cursor_in_background();
    // SAFETY: NSCursor class methods and -set on the main thread.
    unsafe {
        let image = match cursor {
            C::Hidden => TRANSPARENT.with(Clone::clone),
            C::Move => MOVE.with(Clone::clone),
            _ => None,
        };
        let ns: Option<Retained<NSObject>> = match (image, cursor) {
            (Some(image), _) => Some(image),
            (None, C::Pointer) => msg_send_id![class, pointingHandCursor],
            (None, C::Text) => msg_send_id![class, IBeamCursor],
            (None, C::Grab) => msg_send_id![class, openHandCursor],
            (None, C::Grabbing) => msg_send_id![class, closedHandCursor],
            (None, C::NotAllowed) => msg_send_id![class, operationNotAllowedCursor],
            (None, C::ResizeColumn) => msg_send_id![class, resizeLeftRightCursor],
            (None, C::ResizeRow) => msg_send_id![class, resizeUpDownCursor],
            (None, C::Crosshair) => msg_send_id![class, crosshairCursor],
            (None, _) => msg_send_id![class, arrowCursor],
        };
        if let Some(ns) = ns { let _: () = msg_send![&*ns, set]; }
    }
}

/// The window server ignores `[NSCursor set]` from an app that isn't active, which an overlay's
/// app usually isn't. WindowServer's per-connection `SetsCursorInBackground` property lifts
/// that; it's private, so it's opt-in (`macos-background-cursor`), looked up at run time and
/// skipped if it's gone. Without the feature none of its names are in the binary.
#[cfg(feature = "macos-background-cursor")]
fn sets_cursor_in_background() {
    use std::ffi::{c_char, c_void};
    thread_local! { static DONE: Cell<bool> = const { Cell::new(false) }; }
    if DONE.with(|done| done.replace(true)) { return; }
    unsafe extern "C" { fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void; }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" { static kCFBooleanTrue: *const c_void; }
    const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
    // SAFETY: both symbols have these C signatures; the key and value outlive the call.
    unsafe {
        let (main, set) = (dlsym(RTLD_DEFAULT, c"_CGSDefaultConnection".as_ptr()), dlsym(RTLD_DEFAULT, c"CGSSetConnectionProperty".as_ptr()));
        if main.is_null() || set.is_null() { return; }
        let main: extern "C" fn() -> i32 = std::mem::transmute(main);
        let set: extern "C" fn(i32, i32, *const c_void, *const c_void) -> i32 = std::mem::transmute(set);
        let key = objc2_foundation::NSString::from_str("SetsCursorInBackground");
        let connection = main();
        set(connection, connection, (&*key as *const objc2_foundation::NSString).cast(), kCFBooleanTrue);
    }
}

/// A transparent cursor (`None`), or one of the system's WebKit cursors as winit loads them.
fn image_cursor(name: Option<&str>) -> Option<Retained<NSObject>> {
    use objc2::runtime::AnyClass;
    use objc2_foundation::NSString;
    // A 16x16 transparent GIF.
    const TRANSPARENT: &[u8] = &[
        0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x10, 0x00, 0x10, 0x00, 0xf0, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00,
        0x00, 0x00, 0x10, 0x00, 0x10, 0x00, 0x00, 0x02, 0x0e, 0x84, 0x8f, 0xa9, 0xcb, 0xed, 0x0f,
        0xa3, 0x9c, 0xb4, 0xda, 0x8b, 0xb3, 0x3e, 0x05, 0x00, 0x3b,
    ];
    let (image_class, cursor_class) = (AnyClass::get("NSImage")?, AnyClass::get("NSCursor")?);
    // SAFETY: AppKit image and cursor construction on the main thread; every object is retained.
    unsafe {
        let image: Option<Retained<NSObject>> = match name {
            None => {
                let data: Option<Retained<NSObject>> = msg_send_id![objc2::class!(NSData), dataWithBytes: TRANSPARENT.as_ptr().cast::<std::ffi::c_void>(), length: TRANSPARENT.len()];
                let image: objc2::rc::Allocated<NSObject> = msg_send_id![image_class, alloc];
                msg_send_id![image, initWithData: &*data?]
            }
            Some(name) => {
                let path = NSString::from_str(&format!("/System/Library/Frameworks/ApplicationServices.framework/Versions/A/Frameworks/HIServices.framework/Versions/A/Resources/cursors/{name}/cursor.pdf"));
                let image: objc2::rc::Allocated<NSObject> = msg_send_id![image_class, alloc];
                msg_send_id![image, initByReferencingFile: &*path]
            }
        };
        let image = image?;
        let size: NSSize = msg_send![&*image, size];
        let hot = if name.is_some() { NSPoint::new(size.width / 2.0, size.height / 2.0) } else { NSPoint::new(0.0, 0.0) };
        let cursor: objc2::rc::Allocated<NSObject> = msg_send_id![cursor_class, alloc];
        msg_send_id![cursor, initWithImage: &*image, hotSpot: hot]
    }
}

struct MouseMonitors { tokens: Vec<Retained<NSObject>> }

pub(crate) struct Prewarm(Rc<Cell<bool>>);
impl Drop for Prewarm { fn drop(&mut self) { self.0.set(false); } }
impl Drop for MouseMonitors {
    fn drop(&mut self) {
        for token in &self.tokens {
            // SAFETY: each token was returned by NSEvent and is removed exactly once on main.
            unsafe { let _: () = msg_send![NSEvent::class(), removeMonitor: &**token]; }
        }
    }
}

/// Regular (Dock icon and app menu) or accessory (menu-bar only). Called on the main thread.
pub(crate) fn set_dock_icon(visible: bool) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let policy = if visible { NSApplicationActivationPolicy::Regular } else { NSApplicationActivationPolicy::Accessory };
    let app = NSApplication::sharedApplication(mtm);
    if visible { crate::native_menu::ensure_default_menu(&app); }
    app.setActivationPolicy(policy);
}

/// winit's content view. Once a [`Panel`] adopts it, its window is the panel.
pub(crate) fn content_view(window: &winit::window::Window) -> Option<Retained<objc2_app_kit::NSView>> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else { return None };
    // SAFETY: winit's content view is alive as long as its window, which the caller holds.
    unsafe { Retained::retain(handle.ns_view.as_ptr().cast()) }
}

pub(crate) fn ns_window(window: &winit::window::Window) -> Option<Retained<NSWindow>> {
    content_view(window)?.window()
}

/// Whether none of `window` is on screen: covered, or its displays asleep. AppKit keeps the
/// state from the window server's notifications, so reading it costs one message send. wgpu
/// declines a drawable on the same check.
pub(crate) fn window_occluded(window: &winit::window::Window) -> bool {
    ns_window(window).is_some_and(|window| occlusion_hidden(&window))
}

fn occlusion_hidden(window: &NSWindow) -> bool {
    const VISIBLE: usize = 1 << 1;
    // SAFETY: a property read on the main thread.
    let state: usize = unsafe { msg_send![window, occlusionState] };
    state & VISIBLE == 0
}

pub(crate) fn configure_window(window: &winit::window::Window, options: &crate::MacWindowOptions) {
    crate::macos_appearance::install();
    if let Some(window) = ns_window(window) { configure_native(&window, options); }
}

fn configure_native(window: &NSWindow, options: &crate::MacWindowOptions) {
    if let Some(shadow) = options.shadow { window.setHasShadow(shadow); }
    if let Some(level) = options.level { window.setLevel(level); }
    // SAFETY: all callers are native creation paths on the AppKit main thread.
    unsafe {
        if let Some(bits) = options.collection_behavior {
            let _: () = msg_send![window, setCollectionBehavior: bits];
        }
        if let Some(exclude) = options.exclude_from_capture {
            let _: () = msg_send![window, setSharingType: if exclude { 0usize } else { 1usize }];
        }
    }
}

/// Orders a window front without making it key or activating the app.
pub(crate) fn show_without_focus(window: &winit::window::Window) {
    let Some(w) = ns_window(window) else { return };
    // SAFETY: an AppKit call on the main thread.
    unsafe { w.orderFrontRegardless() };
}

/// An overlay's non-activating panel. winit's content view (with the Metal layer wgpu draws
/// into) moves into it, so clicks reach Rune through winit while the app never activates and
/// the panel only becomes key on request. winit's own window stays hidden and gets its view back on drop.
pub(crate) struct Panel {
    bridge: Option<crate::macos_panel_bridge::Bridge>,
    monitors: RefCell<Option<MouseMonitors>>,
    pointer: Rc<PointerState>,
    options: OverlayOptions,
    follow_pointer: Cell<bool>,
    visible: Cell<bool>,
    pointer_poll: Cell<Option<Instant>>,
    warming: Rc<Cell<bool>>,
    /// A fade in or out is running: the panel reads as visible until it finishes, so a
    /// fading-out overlay can still draw its exit.
    revealing: Rc<Cell<bool>>,
    /// Bumped by every alpha change, so a finished fade-out orders out only if nothing came after.
    fade: Rc<Cell<u64>>,
    wake: Option<crate::Waker>,
    panel: objc2::rc::Retained<objc2_app_kit::NSPanel>,
    window: objc2::rc::Retained<objc2_app_kit::NSWindow>,
    view: objc2::rc::Retained<objc2_app_kit::NSView>,
}

impl Panel {
    pub fn adopt(window: &winit::window::Window, at: crate::overlay::Placement, interactive: bool, options: &OverlayOptions, wake: Option<crate::Waker>) -> Option<Panel> {
        Self::adopt_kind(window, at, interactive, options, wake, false)
    }

    pub fn adopt_window(window: &winit::window::Window, size: (f32, f32), native: &crate::MacWindowOptions) -> Option<Panel> {
        let options = OverlayOptions { input: OverlayInput::All, keys: KeyPolicy::OnDemand, prewarm: native.launcher, ..Default::default() };
        let panel = Self::adopt_kind(window, crate::windows::PlaceRequest::at(size), true, &options, None, true)?;
        configure_native(&panel.panel, native);
        Some(panel)
    }

    fn adopt_kind(window: &winit::window::Window, at: crate::overlay::Placement, interactive: bool, options: &OverlayOptions, wake: Option<crate::Waker>, ordinary: bool) -> Option<Panel> {
        use objc2_app_kit::{NSBackingStoreType, NSColor, NSWindowCollectionBehavior, NSWindowStyleMask};
        let mtm = MainThreadMarker::new()?;
        crate::macos_appearance::install();
        let view = content_view(window)?;
        let ns_window = view.window()?;
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(at.size.0 as f64, at.size.1 as f64));
        // SAFETY: AppKit calls on the main thread; the panel is retained by `Panel`.
        let warming = Rc::new(Cell::new(false));
        let revealing = Rc::new(Cell::new(false));
        let style = if ordinary { ns_window.styleMask() } else { NSWindowStyleMask::Borderless };
        let panel = unsafe {
            let panel: Retained<OverlayPanel> = msg_send_id![super(mtm.alloc().set_ivars(PanelFlags { keys: options.keys == KeyPolicy::OnDemand, warming: warming.clone(), revealing: revealing.clone(), constrain: ordinary })),
                initWithContentRect: rect,
                styleMask: style | NSWindowStyleMask::NonactivatingPanel,
                backing: NSBackingStoreType::NSBackingStoreBuffered,
                defer: false];
            Retained::cast::<NSPanel>(panel)
        };
        unsafe {
            panel.setReleasedWhenClosed(false);
            panel.setFloatingPanel(true);
            panel.setBecomesKeyOnlyIfNeeded(options.keys == KeyPolicy::None);
            panel.setHidesOnDeactivate(false);
            panel.setBackgroundColor(Some(&NSColor::clearColor()));
            panel.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::Stationary
                    | NSWindowCollectionBehavior::IgnoresCycle
                    | NSWindowCollectionBehavior::FullScreenAuxiliary,
            );
        }
        let mut native = crate::MacWindowOptions::default().shadow(false)
            .level(match options.level { OverlayLevel::Status => 25, OverlayLevel::AboveStatus => 26, OverlayLevel::AboveOverlays => 27 });
        if options.exclude_from_capture { native = native.exclude_from_capture(true); }
        configure_native(&panel, &native);
        if ordinary {
            panel.setTitle(&ns_window.title());
            panel.setLevel(3);
            panel.setHasShadow(ns_window.hasShadow());
            // SAFETY: both retained windows are owned on the AppKit main thread.
            unsafe {
                panel.setContentMinSize(ns_window.contentMinSize());
                panel.setContentMaxSize(ns_window.contentMaxSize());
            }
            panel.setFrame_display(ns_window.frame(), false);
        }
        panel.setOpaque(false);
        panel.setIgnoresMouseEvents(options.input.resolved(interactive) != OverlayInput::All);
        panel.setAcceptsMouseMovedEvents(true);
        if options.prewarm {
            unsafe { panel.setAlphaValue(0.0); }
            panel.setIgnoresMouseEvents(true);
        }
        // defer:NO creates the WindowServer identity while hidden. Never publish a sentinel
        // (-1/0) cast to CGWindowID as a successful native creation.
        if unsafe { panel.windowNumber() } <= 0 { return None; }
        ns_window.setContentView(None);
        panel.setContentView(Some(&view));
        let pointer = Rc::new(PointerState { input: Cell::new(options.input.resolved(interactive)), follow: Cell::new(options.follow_pointer), ..Default::default() });
        let bridge = if ordinary { crate::macos_panel_bridge::Bridge::new_window(&ns_window, &panel, &view) }
            else { crate::macos_panel_bridge::Bridge::new(&ns_window, &panel, &view, Some(pointer.clone())) };
        if bridge.is_none() {
            panel.setContentView(None);
            ns_window.setContentView(Some(&view));
            return None;
        }
        panel.setDelegate(unsafe { ns_window.delegate() }.as_deref());
        Some(Panel { bridge, monitors: RefCell::new(None), pointer, options: options.clone(), follow_pointer: Cell::new(options.follow_pointer), visible: Cell::new(false), pointer_poll: Cell::new(None), warming, revealing, fade: Rc::default(), wake, panel, window: ns_window, view })
    }

    /// On the primary screen (the one with the menu bar), in AppKit's bottom-left coordinates.
    pub fn place(&self, at: crate::overlay::Placement, options: &OverlayOptions) {
        let Some(rect) = self.rect(at, options) else { return };
        if self.panel.frame() != rect {
            self.window.setFrame_display(rect, false);
            self.panel.setFrame_display(rect, true);
            if self.visible.get() { self.pointer.observe(&self.panel, self.wake.as_ref()); }
        }
    }

    pub fn rect(&self, at: crate::overlay::Placement, options: &OverlayOptions) -> Option<NSRect> {
        overlay_rect_on(at, options, self.panel.screen().map(|s| screen_id(&s)))
    }

    /// Reattaches winit's view if something outside Rune took it from the panel, here or in
    /// `sendEvent:`. Clicks recorded while it was gone were never delivered, so native capture
    /// starts over, and a press the runtime still holds is released where the pointer was.
    fn keep_view(&self) {
        // SAFETY: plain readbacks on the main thread; nothing is retained.
        let (content, window): (*const NSView, *const NSWindow) = unsafe {
            (msg_send![&*self.panel, contentView], msg_send![&*self.view, window])
        };
        let attached = std::ptr::eq(content, &*self.view) && std::ptr::eq(window.cast::<NSPanel>(), &*self.panel);
        if !attached && crate::macos_panel_bridge::reattach(&self.panel) { eprintln!("rune: panel lost its view; reattached"); }
        if crate::macos_panel_bridge::take_reattached(&self.panel) { self.pointer.view_lost(); }
    }

    /// Whether none of the panel is on screen: covered, or its displays asleep. Reads as
    /// visible during a prewarm acquisition.
    pub fn occluded(&self) -> bool {
        occlusion_hidden(&self.panel)
    }

    /// Reveals the panel, fading its alpha in over `fade` (instantly when zero).
    pub fn show(&self, fade: Duration) {
        self.keep_view();
        self.visible.set(true);
        self.refresh_monitors();
        self.pointer.observe(&self.panel, self.wake.as_ref());
        if !self.options.prewarm && !self.panel.isVisible() {
            // SAFETY: AppKit calls on the main thread.
            unsafe {
                if !fade.is_zero() { self.panel.setAlphaValue(0.0); }
                self.panel.orderFrontRegardless();
            }
        }
        self.fade_to(1.0, fade, false);
    }

    /// Animates the panel's alpha to `alpha` over `duration`, replacing any fade in flight,
    /// then orders it out if `order_out` and nothing changed it since.
    fn fade_to(&self, alpha: f64, duration: Duration, order_out: bool) {
        use objc2::runtime::{AnyClass, AnyObject};
        let generation = self.fade.get() + 1;
        self.fade.set(generation);
        let fading = !duration.is_zero();
        self.revealing.set(fading);
        // SAFETY: AppKit calls on the main thread; the completion handler runs there too.
        unsafe {
            if let Some(context) = AnyClass::get("NSAnimationContext") {
                let _: () = msg_send![context, beginGrouping];
                let current: *mut AnyObject = msg_send![context, currentContext];
                let _: () = msg_send![current, setDuration: duration.as_secs_f64()];
                if fading {
                    let (panel, fade, flag) = (self.panel.clone(), self.fade.clone(), self.revealing.clone());
                    let done = block2::RcBlock::new(move || if fade.get() == generation {
                        flag.set(false);
                        if order_out { panel.orderOut(None) }
                    });
                    let _: () = msg_send![current, setCompletionHandler: &*done];
                }
                let animator: *mut AnyObject = msg_send![&*self.panel, animator];
                let _: () = msg_send![animator, setAlphaValue: alpha];
                let _: () = msg_send![context, endGrouping];
            }
            if duration.is_zero() {
                self.panel.setAlphaValue(alpha);
                if order_out { self.panel.orderOut(None); }
            }
        }
    }

    /// The panel's AppKit window level (`NSWindow.Level`), changeable while shown.
    pub fn set_level(&self, level: isize) { self.panel.setLevel(level); }

    pub fn prewarm(&self) -> Prewarm {
        debug_assert!(self.options.prewarm);
        unsafe { self.panel.setAlphaValue(0.0); }
        self.panel.setIgnoresMouseEvents(true);
        if !self.panel.isVisible() { unsafe { self.panel.orderFrontRegardless() }; }
        self.warming.set(true);
        Prewarm(self.warming.clone())
    }

    /// The moved view, for a new wgpu surface (winit's window handle would look for the view
    /// in winit's window, where it no longer is).
    pub fn raw_handles(&self) -> (winit::raw_window_handle::RawDisplayHandle, winit::raw_window_handle::RawWindowHandle) {
        use winit::raw_window_handle::{AppKitDisplayHandle, AppKitWindowHandle, RawDisplayHandle, RawWindowHandle};
        let view = std::ptr::NonNull::from(&*self.view).cast();
        (RawDisplayHandle::AppKit(AppKitDisplayHandle::new()), RawWindowHandle::AppKit(AppKitWindowHandle::new(view)))
    }

    /// Hides the panel, fading its alpha out over `fade` (instantly when zero). Input stops at once.
    pub fn hide(&self, fade: Duration) {
        self.visible.set(false);
        self.revealing.set(false);
        self.pointer_poll.set(None);
        self.monitors.borrow_mut().take();
        self.pointer.captured.set(false);
        self.pointer.pending.set(None);
        self.pointer.last.set(None);
        self.pointer.inside.set(false);
        self.pointer.clicks.borrow_mut().clear();
        self.pointer.release_since.set(None);
        self.pointer.recovered_release.set(None);
        self.set_cursor(None);
        if self.options.prewarm {
            self.fade_to(0.0, fade, false);
            unsafe { self.panel.resignKeyWindow(); }
            self.panel.setIgnoresMouseEvents(true);
        } else if fade.is_zero() {
            self.fade.set(self.fade.get() + 1);
            self.panel.orderOut(None);
        } else {
            unsafe { self.panel.resignKeyWindow(); }
            self.fade_to(0.0, fade, true);
        }
    }

    pub fn window_number(&self) -> u32 { unsafe { self.panel.windowNumber() as u32 } }
    pub fn capture_exclusion(&self) -> OverlaySupport {
        if self.options.exclude_from_capture { OverlaySupport::Requested } else { OverlaySupport::Unknown }
    }
    pub fn global_pointer(&self) -> OverlaySupport {
        if (self.follow_pointer.get() || self.pointer.input.get() == OverlayInput::Regions)
            && self.monitors.borrow().as_ref().is_none_or(|m| m.tokens.len() != 2) { OverlaySupport::Failed }
        else { OverlaySupport::Supported }
    }
    pub fn request_keys(&self) {
        if self.options.keys == KeyPolicy::OnDemand {
            self.panel.makeFirstResponder(Some(&self.view));
            unsafe { self.panel.makeKeyWindow(); }
        }
    }
    pub fn is_key(&self) -> bool { self.panel.isKeyWindow() }
    pub fn take_pointer(&self) -> Option<(f32, f32)> { self.pointer.pending.take() }
    pub fn pointer_captured(&self) -> bool { self.pointer.captured.get() }
    pub fn take_recovered_release(&self) -> Option<(f32, f32)> { self.pointer.recovered_release.take() }
    pub fn take_click(&self, down: bool) -> Option<(f32, f32)> {
        let mut clicks = self.pointer.clicks.borrow_mut();
        if clicks.front().is_some_and(|(pressed, _, _)| *pressed == down) {
            clicks.pop_front().map(|(_, x, y)| (x, y))
        } else { None }
    }
    pub fn pointer_released(&self) {
        self.pointer.released();
        self.refresh_monitors();
        if self.visible.get() { self.pointer.observe(&self.panel, self.wake.as_ref()); }
    }
    pub fn pointer_deadline(&self) -> Option<Instant> { self.pointer_poll.get() }
    /// Samples the pointer if its poll is due; true if it did.
    fn poll_pointer(&self, now: Instant) -> bool {
        if self.pointer_poll.get().is_some_and(|at| at <= now) {
            self.pointer_poll.set(Some(now + Duration::from_secs_f64(1.0 / 120.0)));
            self.pointer.trace(&self.panel, "poll", None);
            self.pointer.observe(&self.panel, None);
            return true;
        }
        false
    }
    /// Once per event-loop turn: keeps the view and samples the pointer, through the timed
    /// poll when it is due and `poll` allows it.
    pub fn observe_pointer(&self, now: Instant, poll: bool) {
        self.keep_view();
        if self.visible.get() && !(poll && self.poll_pointer(now)) { self.pointer.observe(&self.panel, None); }
    }
    pub fn set_input(&self, input: OverlayInput) {
        self.pointer.input.set(input);
        self.refresh_monitors();
        if self.visible.get() { self.pointer.observe(&self.panel, self.wake.as_ref()); }
    }
    /// AppKit applies cursor rects (winit's cursor) only in the key window, and an overlay is
    /// rarely key. While the pointer is over its hit area (`Some`), set the cursor directly and
    /// again on every mouse event; set the arrow once when it leaves (`None`).
    pub fn set_cursor(&self, cursor: Option<crate::el::Cursor>) {
        let was = self.pointer.cursor.replace(cursor);
        match cursor {
            Some(cursor) => set_ns_cursor(cursor),
            None if was.is_some() => set_ns_cursor(crate::el::Cursor::Default),
            None => {}
        }
        if was.is_some() != cursor.is_some() { self.refresh_monitors(); }
    }
    pub fn set_follow_pointer(&self, follow: bool) { self.follow_pointer.set(follow); self.pointer.follow.set(follow); self.refresh_monitors(); }
    fn refresh_monitors(&self) {
        if !self.visible.get() { return; }
        if self.pointer.input.get() == OverlayInput::Regions {
            if self.pointer_poll.get().is_none() { self.pointer_poll.set(Some(Instant::now())); }
        } else { self.pointer_poll.set(None); }
        if self.needs_monitors() { self.start_monitors(); }
        else { self.monitors.borrow_mut().take(); }
    }
    pub fn set_regions(&self, regions: &[[f32; 4]]) {
        if self.pointer.regions.borrow().as_slice() == regions { return; }
        *self.pointer.regions.borrow_mut() = regions.to_vec();
        self.pointer.trace(&self.panel, "regions", None);
        if self.visible.get() { self.pointer.observe(&self.panel, None); }
    }

    fn needs_monitors(&self) -> bool {
        self.follow_pointer.get() || self.pointer.input.get() == OverlayInput::Regions || self.pointer.captured.get()
            || self.pointer.cursor.get().is_some()
    }

    fn start_monitors(&self) {
        if self.monitors.borrow().is_some() || !self.needs_monitors() { return; }
        // Monitors wake the loop; position/button state is sampled there. A local down
        // owns its drag until winit has delivered Up, including outside the hit region.
        let movement = (1usize << 5) | (1 << 6) | (1 << 7) | (1 << 27);
        let mask = movement | (1 << 1) | (1 << 2);
        let (pointer, panel, wake) = (self.pointer.clone(), self.panel.clone(), self.wake.clone());
        let global = block2::RcBlock::new(move |event: *mut NSEvent| {
            pointer.trace(&panel, "global", unsafe { event.as_ref() });
            pointer.reapply_cursor(&panel);
            if let Some(wake) = &wake { wake(); }
        });
        let (pointer, panel, wake) = (self.pointer.clone(), self.panel.clone(), self.wake.clone());
        let local = block2::RcBlock::new(move |event: *mut NSEvent| -> *mut NSEvent {
            pointer.trace(&panel, "local", unsafe { event.as_ref() });
            pointer.reapply_cursor(&panel);
            unsafe {
                let kind: usize = msg_send![event, type];
                let window: *mut NSWindow = msg_send![event, window];
                if (kind == 1 || kind == 2) && window == (&*panel as *const NSPanel).cast_mut().cast() {
                    let point: NSPoint = msg_send![event, locationInWindow];
                    let frame = panel.frame();
                    let (x, y) = (point.x as f32, (frame.size.height - point.y) as f32);
                    pointer.record_click(kind == 1, x, y);
                }
            }
            if let Some(wake) = &wake { wake(); }
            event
        });
        let mut tokens = Vec::new();
        unsafe {
            let token: Option<Retained<NSObject>> = msg_send_id![NSEvent::class(), addGlobalMonitorForEventsMatchingMask: mask, handler: &*global];
            tokens.extend(token);
            let token: Option<Retained<NSObject>> = msg_send_id![NSEvent::class(), addLocalMonitorForEventsMatchingMask: mask, handler: &*local];
            tokens.extend(token);
        }
        *self.monitors.borrow_mut() = Some(MouseMonitors { tokens });
        self.pointer.observe(&self.panel, self.wake.as_ref());
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        self.monitors.get_mut().take();
        self.panel.setDelegate(None);
        self.bridge.take();
        self.panel.orderOut(None);
        self.panel.setContentView(None);
        self.window.setContentView(Some(&self.view));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlay_handlerless_hit_releases_native_ownership_in_every_input_mode() {
        fn view(_: &(), _: &mut crate::Cx<()>) -> crate::El {
            crate::el().child(crate::el().absolute(10.0, 10.0).size(20.0, 20.0).hit())
        }
        for mode in [OverlayInput::Regions, OverlayInput::None] {
            let mut rt = crate::Runtime::new((), view, (100.0, 100.0), crate::Env::default());
            rt.native_overlay = true;
            rt.frame(0.0);
            let pointer = PointerState { input: Cell::new(OverlayInput::Regions), ..Default::default() };
            pointer.regions.borrow_mut().extend_from_slice(rt.hit_regions());
            pointer.record_click(true, 20.0, 20.0);
            pointer.clicks.borrow_mut().pop_front(); // winit dispatches Down
            rt.pointer_down(20.0, 20.0);
            assert!(!rt.pointer_captured(), "a handlerless hit owns only the native press");
            assert!(pointer.captured.get());
            pointer.record_click(false, 50.0, 50.0);
            pointer.input.set(mode);
            pointer.clicks.borrow_mut().pop_front(); // winit dispatches Up
            pointer.released(); // acknowledged even when Runtime input is disabled
            assert!(!pointer.captured.get(), "native release cannot depend on Runtime capture or mode");
            assert!(!pointer.hit(50.0, 50.0));
        }
    }

    #[test]
    fn overlay_lost_up_recovers_once_after_grace_using_release_position() {
        let pointer = PointerState { input: Cell::new(OverlayInput::Regions), ..Default::default() };
        pointer.regions.borrow_mut().push([10.0, 10.0, 30.0, 30.0]);
        pointer.record_click(true, 20.0, 20.0);
        let now = Instant::now();
        pointer.observe_release(1, now, (40.0, 40.0));
        pointer.observe_release(0, now, (50.0, 50.0));
        pointer.observe_release(0, now + Duration::from_millis(99), (60.0, 60.0));
        assert!(pointer.captured.get());
        pointer.observe_release(0, now + Duration::from_millis(100), (70.0, 70.0));
        assert!(!pointer.captured.get());
        assert!(pointer.clicks.borrow().is_empty());
        assert_eq!(pointer.recovered_release.take(), Some((50.0, 50.0)));
        pointer.observe_release(0, now + Duration::from_secs(1), (80.0, 80.0));
        assert_eq!(pointer.recovered_release.take(), None);
    }
    #[test]
    fn overlay_queued_up_and_next_down_keep_their_ownership_until_dispatched() {
        let pointer = PointerState { input: Cell::new(OverlayInput::Regions), ..Default::default() };
        pointer.regions.borrow_mut().push([10.0, 10.0, 30.0, 30.0]);
        pointer.record_click(true, 20.0, 20.0);
        pointer.record_click(false, 100.0, 100.0);
        pointer.observe_release(0, Instant::now(), (100.0, 100.0));
        pointer.observe_release(0, Instant::now() + Duration::from_secs(1), (100.0, 100.0));
        assert!(pointer.captured.get());
        assert_eq!(pointer.recovered_release.get(), None);
        pointer.clicks.borrow_mut().pop_front(); // Down dispatched
        pointer.clicks.borrow_mut().pop_front(); // Up dispatched
        pointer.record_click(true, 20.0, 20.0); // next native Down queued
        pointer.released();
        assert!(pointer.captured.get());
        pointer.clicks.borrow_mut().clear();
        pointer.released();
        assert!(!pointer.captured.get());
    }
    #[test]
    fn overlay_undelivered_click_releases_capture_after_stall() {
        // The local monitor saw a click that never reached winit, so neither entry dispatches.
        let pointer = PointerState { input: Cell::new(OverlayInput::Regions), ..Default::default() };
        pointer.regions.borrow_mut().push([10.0, 10.0, 30.0, 30.0]);
        pointer.record_click(true, 20.0, 20.0);
        pointer.record_click(false, 20.0, 20.0);
        let now = Instant::now();
        pointer.observe_release(0, now, (20.0, 20.0));
        pointer.observe_release(0, now + Duration::from_millis(1999), (20.0, 20.0));
        assert!(pointer.captured.get(), "a briefly queued Up still belongs to winit");
        pointer.observe_release(0, now + Duration::from_secs(2), (20.0, 20.0));
        assert!(!pointer.captured.get());
        assert!(pointer.clicks.borrow().is_empty());
        assert!(!pointer.hit(50.0, 50.0), "outside the regions passes through again");
    }
    #[test]
    fn overlay_lost_view_releases_capture_where_the_pointer_was() {
        let pointer = PointerState { input: Cell::new(OverlayInput::Regions), ..Default::default() };
        pointer.regions.borrow_mut().push([10.0, 10.0, 30.0, 30.0]);
        pointer.last.set(Some((25.0, 15.0)));
        pointer.record_click(true, 20.0, 20.0);
        pointer.view_lost();
        assert!(!pointer.captured.get());
        assert!(pointer.clicks.borrow().is_empty());
        assert_eq!(pointer.recovered_release.take(), Some((25.0, 15.0)), "the runtime's press is released");
    }
    #[test]
    fn overlay_rejected_down_cannot_poison_later_click_and_owned_up_survives_exit() {
        let pointer = PointerState { input: Cell::new(OverlayInput::Regions), ..Default::default() };
        pointer.regions.borrow_mut().push([10.0, 10.0, 30.0, 30.0]);
        pointer.record_click(true, 1.0, 1.0);
        pointer.record_click(true, 20.0, 20.0);
        pointer.record_click(false, 100.0, 100.0);
        assert_eq!(*pointer.clicks.borrow(), VecDeque::from([(true, 20.0, 20.0), (false, 100.0, 100.0)]));
        // Physical Up must not turn off native hit testing before winit dispatches it.
        assert!(pointer.hit(100.0, 100.0));
        pointer.captured.set(false);
        assert!(!pointer.hit(100.0, 100.0));
    }
    #[test]
    fn overlay_appkit_top_row_belongs_to_secondary_display() {
        let secondary = NSRect::new(NSPoint::new(-1920.0, 0.0), NSSize::new(1920.0, 1080.0));
        assert!(contains_appkit_rect(secondary, NSPoint::new(-100.0, 1080.0)));
        assert!(!contains_appkit_rect(secondary, NSPoint::new(-100.0, 0.0)));
        assert!(!contains_appkit_rect(secondary, NSPoint::new(0.0, 500.0)));
        let below = NSRect::new(NSPoint::new(-1920.0, -1080.0), NSSize::new(1920.0, 1080.0));
        assert!(contains_appkit_rect(below, NSPoint::new(-100.0, 0.0)));
    }
}

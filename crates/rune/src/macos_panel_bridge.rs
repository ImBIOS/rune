//! winit 0.30 keeps its original NSWindow in the view/delegate. Forward the handful of
//! readbacks that must describe the adopted panel.
//!
//! The forwards are added once to winit's own classes rather than to a per-window subclass:
//! winit observes `effectiveAppearance`, so its window's isa is KVO's `NSKVONotifying_`
//! class, and KVO's `-class`, `-dealloc` and observer bookkeeping read state stored on that
//! exact class. A subclass of it makes `-class` return nil. Objects Rune hasn't bridged, and
//! calls off the main thread, fall through to AppKit.

use std::{cell::RefCell, collections::HashMap, ffi::{c_char, c_void}, sync::{Mutex, OnceLock}};
use objc2::{msg_send, sel, runtime::{AnyClass, AnyObject, Bool, Sel}};
use objc2_app_kit::{NSPanel, NSView, NSWindow};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::rc::Rc;

unsafe extern "C" {
    fn object_getClass(object: *const AnyObject) -> *const AnyClass;
    fn class_getSuperclass(class: *const AnyClass) -> *const AnyClass;
    fn class_getInstanceMethod(class: *const AnyClass, selector: Sel) -> *const c_void;
    fn method_getTypeEncoding(method: *const c_void) -> *const c_char;
    fn class_addMethod(class: *const AnyClass, selector: Sel, imp: *const c_void, types: *const c_char) -> bool;
    fn dladdr(address: *const c_void, info: *mut DlInfo) -> i32;
}

#[repr(C)]
struct DlInfo { fname: *const c_char, fbase: *mut c_void, sname: *const c_char, saddr: *mut c_void }

#[derive(Clone, Copy)]
struct Source { view: usize, panel: usize, ordinary: bool }

thread_local! {
    static WINDOWS: RefCell<HashMap<usize, Source>> = RefCell::default();
    static VIEWS: RefCell<HashMap<usize, Rc<crate::macos::PointerState>>> = RefCell::default();
}
/// (class, its superclass) for every class Rune added methods to.
static INSTALLED: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

fn source(window: &AnyObject) -> Option<Source> {
    WINDOWS.try_with(|windows| windows.borrow().get(&(window as *const _ as usize)).copied()).ok().flatten()
}
fn panel(window: &AnyObject, ordinary_only: bool) -> Option<*mut AnyObject> {
    source(window).filter(|s| s.ordinary || !ordinary_only).map(|s| s.panel as *mut AnyObject)
}
/// The superclass of the class Rune installed into, which is what an unbridged object (or a
/// KVO subclass of it) would have reached without Rune.
fn base(object: &AnyObject) -> &'static AnyClass {
    let installed = INSTALLED.lock().unwrap_or_else(|e| e.into_inner());
    let mut class = unsafe { object_getClass(object) };
    while !class.is_null() {
        if let Some((_, parent)) = installed.iter().find(|(c, _)| *c == class as usize) {
            return unsafe { &*(*parent as *const AnyClass) };
        }
        class = unsafe { class_getSuperclass(class) };
    }
    unreachable!("Rune's forwards only run on classes it installed them into")
}

macro_rules! forward {
    ($name:ident, $ordinary_only:expr, ($($arg:ident: $ty:ty),*) -> $ret:ty, $($message:tt)+) => {
        extern "C" fn $name(window: &AnyObject, _: Sel $(, $arg: $ty)*) -> $ret {
            unsafe { match panel(window, $ordinary_only) {
                Some(panel) => msg_send![panel, $($message)+],
                None => msg_send![super(window, base(window)), $($message)+],
            } }
        }
    };
}
forward!(visible, false, () -> Bool, isVisible);
forward!(occlusion, false, () -> usize, occlusionState);
forward!(scale, false, () -> f64, backingScaleFactor);
forward!(screen, false, () -> *mut AnyObject, screen);
// winit's `set_cursor` invalidates the view's cursor rects in its own (hidden) window.
forward!(invalidate_cursor_rects, false, (view: *mut AnyObject) -> (), invalidateCursorRectsForView: view);
// Ordinary panel hosts retain winit's geometry/appearance API. Only their source window
// gets these forwards; overlay placement continues to manage both native frames itself.
forward!(frame, true, () -> NSRect, frame);
forward!(key, true, () -> Bool, isKeyWindow);
forward!(set_frame, true, (rect: NSRect, display: Bool) -> (), setFrame: rect, display: display);
forward!(set_origin, true, (point: NSPoint) -> (), setFrameOrigin: point);
forward!(set_size, true, (size: NSSize) -> (), setContentSize: size);
forward!(set_title, true, (title: *mut AnyObject) -> (), setTitle: title);
forward!(set_level, true, (level: isize) -> (), setLevel: level);
forward!(set_hit, true, (ignore: Bool) -> (), setIgnoresMouseEvents: ignore);

/// `-contentView`. AppKit restyles a window by removing `[self contentView]` from its
/// superview (`setStyleMask:`, `setContentView:`, toolbars), and other frameworks restyle the
/// first window in `NSApp.windows`, which is the hidden source (ReplayKit does when a
/// ScreenCaptureKit stream starts). Handing them the view that now lives in the panel would
/// pull it out of the panel. So only callers in this image, where winit is linked, get the
/// moved view; everyone else sees the source's real, empty content.
#[unsafe(naked)]
extern "C" fn content_view(_window: &AnyObject, _selector: Sel) -> *mut AnyObject {
    // objc_msgSend tail-calls the IMP, so the return address still points into the sender.
    #[cfg(target_arch = "aarch64")]
    core::arch::naked_asm!("mov x2, x30", "b {}", sym content_view_from);
    #[cfg(target_arch = "x86_64")]
    core::arch::naked_asm!("mov rdx, [rsp]", "jmp {}", sym content_view_from);
}
extern "C" fn content_view_from(window: &AnyObject, _: Sel, caller: usize) -> *mut AnyObject {
    if let Some(source) = source(window) && in_this_image(caller) { return source.view as *mut AnyObject; }
    unsafe { msg_send![super(window, base(window)), contentView] }
}
fn image(address: usize) -> usize {
    let mut info = DlInfo { fname: std::ptr::null(), fbase: std::ptr::null_mut(), sname: std::ptr::null(), saddr: std::ptr::null_mut() };
    if unsafe { dladdr(address as *const c_void, &mut info) } == 0 { 0 } else { info.fbase as usize }
}
fn in_this_image(address: usize) -> bool {
    static OWN: OnceLock<usize> = OnceLock::new();
    let own = *OWN.get_or_init(|| image(content_view_from as *const () as usize));
    own != 0 && image(address) == own
}

extern "C" fn hit_test(view: &AnyObject, _: Sel, point: NSPoint) -> *mut AnyObject {
    let state = VIEWS.try_with(|views| views.borrow().get(&(view as *const _ as usize)).cloned()).ok().flatten();
    unsafe {
        if let Some(state) = state {
            let parent: *mut AnyObject = msg_send![view, superview];
            let local: NSPoint = msg_send![view, convertPoint: point, fromView: parent];
            let bounds: NSRect = msg_send![view, bounds];
            let flipped: Bool = msg_send![view, isFlipped];
            let y = if flipped.as_bool() { local.y - bounds.origin.y } else { bounds.origin.y + bounds.size.height - local.y };
            if !state.hit((local.x - bounds.origin.x) as f32, y as f32) { return std::ptr::null_mut(); }
        }
        msg_send![super(view, base(view)), hitTest: point]
    }
}

/// Adds `methods` to `class` once. Refuses AppKit's own classes, whose instances Rune doesn't
/// own, and any class that already implements one of them, since the fallback is its superclass.
fn install(class: &AnyClass, appkit: &AnyClass, methods: &[(Sel, *const c_void)]) -> bool {
    let mut installed = INSTALLED.lock().unwrap_or_else(|e| e.into_inner());
    let key = class as *const AnyClass as usize;
    if installed.iter().any(|(c, _)| *c == key) { return true; }
    if std::ptr::eq(class, appkit) { return false; }
    unsafe {
        let parent = class_getSuperclass(class);
        if parent.is_null() { return false; }
        for (selector, _) in methods {
            let method = class_getInstanceMethod(class, *selector);
            if method.is_null() || method != class_getInstanceMethod(parent, *selector) { return false; }
        }
        // Recorded first: a forward that did get added always finds its fallback.
        installed.push((key, parent as usize));
        methods.iter().all(|(selector, implementation)| {
            let types = method_getTypeEncoding(class_getInstanceMethod(class, *selector));
            class_addMethod(class, *selector, *implementation, types)
        })
    }
}

/// Returns `view` to `panel` if anything took it, and reports whether it had to.
pub(crate) fn reattach(panel: &NSPanel) -> bool {
    let key = panel as *const NSPanel as usize;
    let Some((source, entry)) = WINDOWS.with(|w| w.borrow().iter().find(|(_, s)| s.panel == key).map(|(k, s)| (*k, *s))) else { return false };
    let view = entry.view as *mut AnyObject;
    unsafe {
        let content: *mut AnyObject = msg_send![panel, contentView];
        if content == view { return false; }
        let source = source as *mut AnyObject;
        // A foreign setContentView: may have installed the view in the source itself.
        let real: *mut AnyObject = msg_send![super(&*source, base(&*source)), contentView];
        if real == view { let _: () = msg_send![source, setContentView: std::ptr::null_mut::<AnyObject>()]; }
        let _: () = msg_send![panel, setContentView: view];
        let key_window: Bool = msg_send![panel, isKeyWindow];
        if key_window.as_bool() { let _: Bool = msg_send![panel, makeFirstResponder: view]; }
    }
    true
}

pub(crate) struct Bridge { source: usize, view: Option<usize> }
impl Bridge {
    /// Main-thread only. The owner retains all three objects until after this bridge drops.
    pub fn new(source: &NSWindow, panel: &NSPanel, view: &NSView, filter: Option<Rc<crate::macos::PointerState>>) -> Option<Self> {
        Self::new_kind(source, panel, view, filter, false)
    }
    pub fn new_window(source: &NSWindow, panel: &NSPanel, view: &NSView) -> Option<Self> {
        Self::new_kind(source, panel, view, None, true)
    }
    fn new_kind(source: &NSWindow, panel: &NSPanel, view: &NSView, filter: Option<Rc<crate::macos::PointerState>>, ordinary: bool) -> Option<Self> {
        use objc2::ClassType;
        // `-class` is the class winit declared, below any KVO subclass.
        let class: &AnyClass = unsafe { msg_send![source, class] };
        let forwards = [
            (sel!(contentView), content_view as *const c_void),
            (sel!(isVisible), visible as *const c_void),
            (sel!(occlusionState), occlusion as *const c_void),
            (sel!(backingScaleFactor), scale as *const c_void),
            (sel!(screen), screen as *const c_void),
            (sel!(invalidateCursorRectsForView:), invalidate_cursor_rects as *const c_void),
            (sel!(frame), frame as *const c_void),
            (sel!(isKeyWindow), key as *const c_void),
            (sel!(setFrame:display:), set_frame as *const c_void),
            (sel!(setFrameOrigin:), set_origin as *const c_void),
            (sel!(setContentSize:), set_size as *const c_void),
            (sel!(setTitle:), set_title as *const c_void),
            (sel!(setLevel:), set_level as *const c_void),
            (sel!(setIgnoresMouseEvents:), set_hit as *const c_void),
        ];
        if !install(class, NSWindow::class(), &forwards) { return None; }
        if filter.is_some() {
            let view_class: &AnyClass = unsafe { msg_send![view, class] };
            if !install(view_class, NSView::class(), &[(sel!(hitTest:), hit_test as *const c_void)]) { return None; }
        }
        let source = source as *const NSWindow as usize;
        WINDOWS.with(|w| w.borrow_mut().insert(source, Source { view: view as *const NSView as usize, panel: panel as *const NSPanel as usize, ordinary }));
        let view = filter.map(|filter| {
            let view = view as *const NSView as usize;
            VIEWS.with(|v| v.borrow_mut().insert(view, filter));
            view
        });
        Some(Self { source, view })
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        WINDOWS.with(|w| w.borrow_mut().remove(&self.source));
        if let Some(view) = self.view { VIEWS.with(|v| v.borrow_mut().remove(&view)); }
    }
}

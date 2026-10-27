//! AppKit cursors set directly, for overlays that are rarely the key window.

#[cfg(feature = "macos-background-cursor")]
use std::cell::Cell;

use objc2::rc::Retained;
use objc2::{msg_send, msg_send_id};
use objc2_foundation::{NSObject, NSPoint, NSSize};

/// `[NSCursor set]` with winit's shape for each cursor. Main thread.
pub(crate) fn set_ns_cursor(cursor: crate::el::Cursor) {
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

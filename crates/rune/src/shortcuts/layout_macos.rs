//! Translate only display labels. The input source is borrowed for one call and never changed.
use std::ffi::c_void;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> *const c_void;
    fn TISGetInputSourceProperty(source: *const c_void, key: *const c_void) -> *const c_void;
    static kTISPropertyUnicodeKeyLayoutData: *const c_void;
    fn LMGetKbdType() -> u8;
    fn UCKeyTranslate(layout: *const c_void, key: u16, action: u16, modifiers: u32,
        keyboard_type: u32, options: u32, dead: *mut u32, capacity: u32,
        length: *mut u32, chars: *mut u16) -> i32;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
    fn CFRelease(value: *const c_void);
}
unsafe extern "C" { fn pthread_main_np() -> i32; }

struct Source(*const c_void);
impl Drop for Source {
    fn drop(&mut self) { unsafe { CFRelease(self.0) }; }
}

pub(super) fn label(key: u32) -> Option<String> {
    // AppKit also calls TIS. A private mutex cannot serialize with it; stay on the
    // UI thread. Background/headless callers get the documented US fallback.
    if unsafe { pthread_main_np() } == 0 { return None; }
    // TIS returns a retained source; its layout data remains alive until Source drops.
    unsafe {
        let source = TISCopyCurrentKeyboardLayoutInputSource();
        if source.is_null() { return None; }
        let source = Source(source);
        let data = TISGetInputSourceProperty(source.0, kTISPropertyUnicodeKeyLayoutData);
        if data.is_null() { return None; }
        let layout = CFDataGetBytePtr(data);
        if layout.is_null() { return None; }
        translate(layout.cast(), key)
    }
}

/// The layout must point to live TIS Unicode layout data, on the main thread.
pub(super) unsafe fn translate(layout: *const c_void, key: u32) -> Option<String> {
    let mut chars = [0u16; 16];
    let mut length = 0;
    let mut dead = 0;
    // kUCKeyActionDisplay, no modifiers, kUCKeyTranslateNoDeadKeysMask.
    let status = unsafe { UCKeyTranslate(layout, key as u16, 3, 0, LMGetKbdType() as u32,
        1, &mut dead, chars.len() as u32, &mut length, chars.as_mut_ptr()) };
    if status != 0 || length == 0 || length as usize > chars.len() { return None; }
    let label = String::from_utf16(&chars[..length as usize]).ok()?;
    (!label.chars().any(char::is_control)).then_some(label)
}

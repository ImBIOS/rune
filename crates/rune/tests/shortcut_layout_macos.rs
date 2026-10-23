//! Main-thread native test: read Dvorak's data without selecting it or changing user settings.
#[cfg(target_os = "macos")]
#[path = "../src/shortcuts/layout_macos.rs"]
mod layout_macos;

#[cfg(target_os = "macos")]
fn main() {
    use std::ffi::c_void;
    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn TISCreateInputSourceList(filter: *const c_void, all: u8) -> *const c_void;
        fn TISGetInputSourceProperty(source: *const c_void, key: *const c_void) -> *const c_void;
        static kTISPropertyInputSourceID: *const c_void;
        static kTISPropertyUnicodeKeyLayoutData: *const c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFArrayGetCount(array: *const c_void) -> isize;
        fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
        fn CFStringGetCString(string: *const c_void, buffer: *mut i8, size: isize, encoding: u32) -> u8;
        fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
        fn CFRelease(value: *const c_void);
    }
    let shortcut: rune::Shortcut = "super+q".parse().unwrap();
    let current = layout_macos::label(12).expect("current keyboard layout has Unicode data");
    assert_eq!(shortcut.keycaps().last(), Some(&current));
    unsafe {
        let list = TISCreateInputSourceList(std::ptr::null(), 1);
        assert!(!list.is_null());
        let mut found = false;
        for index in 0..CFArrayGetCount(list) {
            let source = CFArrayGetValueAtIndex(list, index);
            let id = TISGetInputSourceProperty(source, kTISPropertyInputSourceID);
            let mut bytes = [0i8; 256];
            if id.is_null() || CFStringGetCString(id, bytes.as_mut_ptr(), 256, 0x08000100) == 0 { continue; }
            if std::ffi::CStr::from_ptr(bytes.as_ptr()).to_bytes() != b"com.apple.keylayout.Dvorak" { continue; }
            let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
            assert!(!data.is_null());
            let layout = CFDataGetBytePtr(data);
            assert!(!layout.is_null());
            assert_eq!(layout_macos::translate(layout.cast(), 12).as_deref(), Some("'"));
            assert_eq!(layout_macos::translate(layout.cast(), 13).as_deref(), Some(","));
            found = true;
            break;
        }
        CFRelease(list);
        assert!(found, "Dvorak layout must be installed for this native test");
    }
    assert_eq!(shortcut.physical_key(), rune::PhysicalKeyCode::KeyQ);
    assert_eq!(shortcut.to_string(), "super+q");
    println!("PASS: current layout Q={current:?}; Dvorak physical Q=' and W=,; stored binding super+q");
}

#[cfg(not(target_os = "macos"))]
fn main() {}

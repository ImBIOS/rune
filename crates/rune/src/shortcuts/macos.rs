//! Carbon hotkeys do not require Accessibility or Input Monitoring permission.
use super::{Backend, Shortcut, ShortcutError, ShortcutId, Sink, native_status, parse::*};
use std::{cell::RefCell, collections::HashMap, ffi::c_void, ptr};

type Ref = *mut c_void;
#[repr(C, packed(2))]
#[derive(Clone, Copy)]
struct HotKeyId { signature: u32, id: u32 }
#[repr(C, packed(2))]
struct EventType { class: u32, kind: u32 }
const SIGNATURE: u32 = u32::from_be_bytes(*b"rune");
const KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
const EXISTS: i64 = -9878;
const NOT_HANDLED: i32 = -9874;
// Without this flag Carbon permits another process to register the same combination.
const EXCLUSIVE: u32 = 1;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> Ref;
    fn InstallEventHandler(target: Ref, handler: unsafe extern "C" fn(Ref, Ref, Ref) -> i32, count: usize, types: *const EventType, data: Ref, out: *mut Ref) -> i32;
    fn RemoveEventHandler(handler: Ref) -> i32;
    fn RegisterEventHotKey(code: u32, mods: u32, id: HotKeyId, target: Ref, options: u32, out: *mut Ref) -> i32;
    fn UnregisterEventHotKey(hotkey: Ref) -> i32;
    fn GetEventParameter(event: Ref, name: u32, ty: u32, actual: *mut u32, size: usize, actual_size: *mut usize, data: Ref) -> i32;
}

struct Context { sink: Sink, keys: RefCell<HashMap<ShortcutId, Ref>> }
pub(super) struct Native { handler: Ref, context: Box<Context> }

unsafe extern "C" fn activated(_: Ref, event: Ref, data: Ref) -> i32 {
    // SAFETY: Carbon only calls this while the installed handler and its boxed context live.
    // The application event target delivers callbacks on the registration/main thread.
    unsafe {
        let mut id = HotKeyId { signature: 0, id: 0 };
        if GetEventParameter(event, u32::from_be_bytes(*b"----"), u32::from_be_bytes(*b"hkid"), ptr::null_mut(),
            std::mem::size_of::<HotKeyId>(), ptr::null_mut(), (&mut id as *mut HotKeyId).cast()) != 0 || id.signature != SIGNATURE {
            return NOT_HANDLED;
        }
        let context = &*data.cast::<Context>();
        if !context.keys.borrow().contains_key(&ShortcutId(id.id)) { return NOT_HANDLED; }
        // Never unwind across Carbon. Application handlers themselves run later, in Rune.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (context.sink)(ShortcutId(id.id), None)));
        0
    }
}

impl Native {
    pub(super) fn new(sink: Sink) -> Result<Self, ShortcutError> {
        if objc2_foundation::MainThreadMarker::new().is_none() { return Err(ShortcutError::Backend("Carbon requires the main thread".into())); }
        let mut context = Box::new(Context { sink, keys: RefCell::default() });
        let mut handler = ptr::null_mut();
        // SAFETY: Context's address remains stable until after RemoveEventHandler in Drop.
        native_status(unsafe { InstallEventHandler(GetApplicationEventTarget(), activated, 1,
            &EventType { class: KEYBOARD, kind: 5 }, (&mut *context as *mut Context).cast(), &mut handler) }.into(), EXISTS)?;
        Ok(Self { handler, context })
    }
}

impl Backend for Native {
    fn register(&mut self, id: ShortcutId, shortcut: &Shortcut) -> Result<(), ShortcutError> {
        let code = keycode(&shortcut.key).ok_or_else(|| ShortcutError::InvalidAccelerator(shortcut.to_string()))?;
        let mut mods = 0;
        for (bit, carbon) in [(SUPER, 1 << 8), (SHIFT, 1 << 9), (ALT, 1 << 11), (CTRL, 1 << 12)] {
            if shortcut.mods & bit != 0 { mods |= carbon; }
        }
        let mut hotkey = ptr::null_mut();
        // SAFETY: On the main thread; pointers are valid output slots and target is Carbon-owned.
        native_status(unsafe { RegisterEventHotKey(code, mods, HotKeyId { signature: SIGNATURE, id: id.0 },
            GetApplicationEventTarget(), EXCLUSIVE, &mut hotkey) }.into(), EXISTS)?;
        self.context.keys.borrow_mut().insert(id, hotkey);
        Ok(())
    }
    fn unregister(&mut self, id: ShortcutId) -> Result<(), ShortcutError> {
        let hotkey = *self.context.keys.borrow().get(&id).ok_or(ShortcutError::UnknownId)?;
        // SAFETY: This registration is owned by this backend and has not been released.
        native_status(unsafe { UnregisterEventHotKey(hotkey) }.into(), EXISTS)?;
        self.context.keys.borrow_mut().remove(&id);
        Ok(())
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        // SAFETY: Registrations and handler are owned here, on the app thread. Removing the
        // handler before freeing Context prevents callbacks with a dangling user-data pointer.
        unsafe {
            for (_, hotkey) in self.context.keys.get_mut().drain() { UnregisterEventHotKey(hotkey); }
            RemoveEventHandler(self.handler);
        }
    }
}

fn keycode(key: &str) -> Option<u32> {
    // Carbon virtual key codes (physical ANSI positions, matching winit KeyCode semantics).
    Some(match key {
        "a" => 0, "s" => 1, "d" => 2, "f" => 3, "h" => 4, "g" => 5, "z" => 6, "x" => 7, "c" => 8, "v" => 9,
        "b" => 11, "q" => 12, "w" => 13, "e" => 14, "r" => 15, "y" => 16, "t" => 17,
        "1" => 18, "2" => 19, "3" => 20, "4" => 21, "6" => 22, "5" => 23, "equal" => 24, "9" => 25,
        "7" => 26, "minus" => 27, "8" => 28, "0" => 29, "bracketright" => 30, "o" => 31, "u" => 32,
        "bracketleft" => 33, "i" => 34, "p" => 35, "enter" => 36, "l" => 37, "j" => 38, "quote" => 39,
        "k" => 40, "semicolon" => 41, "backslash" => 42, "comma" => 43, "slash" => 44, "n" => 45,
        "m" => 46, "period" => 47, "tab" => 48, "space" => 49, "backquote" => 50, "backspace" => 51, "escape" => 53,
        "f17" => 64, "f18" => 79, "f19" => 80, "f20" => 90, "f5" => 96, "f6" => 97, "f7" => 98, "f3" => 99,
        "f8" => 100, "f9" => 101, "f11" => 103, "f13" => 105, "f16" => 106, "f14" => 107, "f10" => 109,
        "f12" => 111, "f15" => 113, "insert" => 114, "home" => 115, "pageup" => 116, "delete" => 117,
        "f4" => 118, "end" => 119, "f2" => 120, "pagedown" => 121, "f1" => 122,
        "left" => 123, "right" => 124, "down" => 125, "up" => 126,
        _ => return None,
    })
}

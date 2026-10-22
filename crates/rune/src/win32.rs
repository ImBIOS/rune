//! Win32 pieces winit 0.30 doesn't expose, for overlays.

use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongPtrW, HWND_TOPMOST, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT,
};

fn hwnd(window: &winit::window::Window) -> Option<HWND> {
    let RawWindowHandle::Win32(h) = window.window_handle().ok()?.as_raw() else { return None };
    Some(h.hwnd.get() as HWND)
}

/// Topmost tool window that never activates and lets every click through.
pub(crate) fn prepare_overlay(window: &winit::window::Window) {
    let Some(hwnd) = hwnd(window) else { return };
    // SAFETY: style changes on a live window owned by this thread.
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let style = style | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_TRANSPARENT | WS_EX_LAYERED;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style as isize);
    }
}

/// Shows the overlay without activating it, above other topmost windows.
pub(crate) fn show_overlay(window: &winit::window::Window) {
    let Some(hwnd) = hwnd(window) else { return };
    // SAFETY: Win32 calls on a live window owned by this thread.
    unsafe {
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

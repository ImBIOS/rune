//! Win32 pieces winit 0.30 doesn't expose, for overlays.

use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongPtrW, HWND_TOPMOST, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SPI_GETWORKAREA, SetWindowLongPtrW, SetWindowPos, ShowWindow, SystemParametersInfoW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT,
};

fn hwnd(window: &winit::window::Window) -> Option<HWND> {
    let RawWindowHandle::Win32(h) = window.window_handle().ok()?.as_raw() else { return None };
    Some(h.hwnd.get() as HWND)
}

/// Topmost tool window that never activates; unless `interactive`, every click goes through.
pub(crate) fn prepare_overlay(window: &winit::window::Window, interactive: bool) {
    let Some(hwnd) = hwnd(window) else { return };
    let through = if interactive { 0 } else { WS_EX_TRANSPARENT | WS_EX_LAYERED };
    // SAFETY: style changes on a live window owned by this thread.
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let style = style | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | through;
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

/// The bottom of the primary display's work area (above the taskbar), in physical pixels.
pub(crate) fn work_area_bottom() -> Option<i32> {
    let mut area = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    // SAFETY: SPI_GETWORKAREA writes one RECT.
    (unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, (&raw mut area).cast(), 0) } != 0).then_some(area.bottom)
}

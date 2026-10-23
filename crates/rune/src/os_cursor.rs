//! The OS cursor a winit host last applied.
//!
//! `Cursor::Hidden` sets a transparent cursor image rather than winit's
//! `set_cursor_visible(false)`: on Windows that call also confines the pointer to the centre of
//! a focused window, and its `ShowCursor` count is process-wide. A cursor image is per-window
//! like any other shape, so the OS drops it when the pointer leaves the window or the window
//! goes away.

use winit::event_loop::ActiveEventLoop;
use winit::window::{CursorIcon, CustomCursor, Window};

use crate::el::Cursor;

pub(crate) struct OsCursor {
    applied: (bool, Cursor),
    transparent: CustomCursor,
}

impl OsCursor {
    pub(crate) fn new(event_loop: &ActiveEventLoop) -> Self {
        let source = CustomCursor::from_rgba(vec![0u8; 4], 1, 1, 0, 0).expect("a 1x1 cursor image is valid");
        OsCursor { applied: (false, Cursor::Default), transparent: event_loop.create_custom_cursor(source) }
    }

    /// Applies `want` while the host is `active` (the pointer is over its window or its hit
    /// area), and the arrow once when it stops being active. winit sets some cursors at once
    /// (Win32 `SetCursor`), so an inactive host must leave the cursor to whichever window has
    /// the pointer.
    pub(crate) fn apply(&mut self, window: &Window, want: Cursor, active: bool) {
        let Some(next) = next(&mut self.applied, want, active) else { return };
        match next {
            Cursor::Hidden => window.set_cursor(self.transparent.clone()),
            other => window.set_cursor(icon(other)),
        }
    }
}

/// The cursor to set, if the (active, cursor) pair changed and the host owns the cursor or
/// just gave it up.
fn next(applied: &mut (bool, Cursor), want: Cursor, active: bool) -> Option<Cursor> {
    let want = if active { want } else { Cursor::Default };
    let was = std::mem::replace(applied, (active, want));
    (was != (active, want) && (active || was.0)).then_some(want)
}

fn icon(c: Cursor) -> CursorIcon {
    match c {
        Cursor::Default | Cursor::Hidden => CursorIcon::Default,
        Cursor::Pointer => CursorIcon::Pointer,
        Cursor::Text => CursorIcon::Text,
        Cursor::Grab => CursorIcon::Grab,
        Cursor::Grabbing => CursorIcon::Grabbing,
        Cursor::NotAllowed => CursorIcon::NotAllowed,
        Cursor::Move => CursorIcon::Move,
        Cursor::ResizeColumn => CursorIcon::ColResize,
        Cursor::ResizeRow => CursorIcon::RowResize,
        Cursor::Crosshair => CursorIcon::Crosshair,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_sets_its_cursor_while_active_and_the_arrow_once_when_it_stops() {
        let mut applied = (false, Cursor::Default);
        assert_eq!(next(&mut applied, Cursor::Crosshair, false), None, "never while inactive");
        assert_eq!(next(&mut applied, Cursor::Crosshair, true), Some(Cursor::Crosshair));
        assert_eq!(next(&mut applied, Cursor::Hidden, true), Some(Cursor::Hidden));
        assert_eq!(next(&mut applied, Cursor::Hidden, true), None);
        assert_eq!(next(&mut applied, Cursor::Hidden, false), Some(Cursor::Default), "the arrow once on leaving");
        assert_eq!(next(&mut applied, Cursor::Hidden, false), None, "then another window's cursor stays");
        assert_eq!(next(&mut applied, Cursor::Pointer, false), None);
        assert_eq!(next(&mut applied, Cursor::Default, true), Some(Cursor::Default), "entering claims the cursor");
        assert_eq!(next(&mut applied, Cursor::Default, true), None);
    }
}

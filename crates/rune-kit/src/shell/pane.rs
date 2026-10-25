//! A resizable, collapsible pane's state, owned by the app so it can persist it.

use rune::KeyEvent;

/// A side pane's width and whether it shows: a sidebar's or an inspector's. The app owns it
/// (a field of its state), so it can save the width and restore it with [`PaneState::new`].
/// Widths are whole points within the pane's range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneState {
    width: f32,
    open: bool,
    default: f32,
    min: f32,
    max: f32,
    resizing: bool,
}

impl PaneState {
    /// Range of a sidebar unless set: 160 to 480 points.
    pub const MIN: f32 = 160.0;
    pub const MAX: f32 = 480.0;

    /// An open pane `width` points wide, which double-clicking its edge also returns to.
    pub fn new(width: f32) -> PaneState {
        let mut p = PaneState { width, open: true, default: width, min: Self::MIN, max: Self::MAX, resizing: false };
        p.clamp();
        p
    }
    /// Its width range. The width and the default clamp into it.
    pub fn range(mut self, min: f32, max: f32) -> Self {
        self.min = min.max(1.0);
        self.max = max.max(self.min);
        self.clamp();
        self
    }
    /// Starts collapsed (a saved state, or an inspector that opens on demand).
    pub fn collapsed(mut self) -> Self {
        self.open = false;
        self
    }
    /// A saved width, clamped and rounded (the width it starts with; the default stays).
    pub fn with_width(mut self, width: f32) -> Self {
        self.set_width(width);
        self
    }

    pub fn width(&self) -> f32 {
        self.width
    }
    pub fn is_open(&self) -> bool {
        self.open
    }
    pub fn min_width(&self) -> f32 {
        self.min
    }
    pub fn max_width(&self) -> f32 {
        self.max
    }
    pub fn default_width(&self) -> f32 {
        self.default
    }
    /// Whether its edge is being dragged.
    pub fn is_resizing(&self) -> bool {
        self.resizing
    }
    /// Points it takes in the layout: its width when open, else 0.
    pub fn shown_width(&self) -> f32 {
        if self.open { self.width } else { 0.0 }
    }

    /// Clamps to the range and rounds to a whole point. Doesn't open the pane.
    pub fn set_width(&mut self, width: f32) {
        if width.is_finite() {
            self.width = width.round().clamp(self.min, self.max);
        }
    }
    pub fn set_open(&mut self, open: bool) {
        self.open = open;
    }
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
    /// Back to the width it was made with (double-clicking its edge).
    pub fn reset_width(&mut self) {
        self.width = self.default;
    }
    /// Where dragging its edge to `width` leaves it: clamped, or collapsed (keeping its last
    /// width) once dragged below half its minimum, as AppKit's split views do. Dragging back
    /// out opens it again.
    pub fn drag_to(&mut self, width: f32) {
        if width < self.min * 0.5 {
            self.open = false;
        } else {
            self.open = true;
            self.set_width(width);
        }
    }

    /// A drag on its edge began or ended. The grip stays while a drag that collapsed the pane
    /// goes on, so the drag can open it again.
    pub(super) fn set_resizing(&mut self, on: bool) {
        self.resizing = on;
    }

    fn clamp(&mut self) {
        self.width = self.width.round().clamp(self.min, self.max);
        self.default = self.default.round().clamp(self.min, self.max);
    }
}

impl Default for PaneState {
    /// A 248 point sidebar, open.
    fn default() -> Self {
        PaneState::new(248.0)
    }
}

/// Whether `ev` is the sidebar's show/hide shortcut: ⇧⌘B (Ctrl+Shift+B off macOS), and on macOS
/// also ⌃⌘S, AppKit's Toggle Sidebar. Call it from the app's key handler (`AppSpec::on_key`)
/// and toggle the sidebar's [`PaneState`] when it matches.
pub fn is_sidebar_toggle(ev: &KeyEvent) -> bool {
    let m = ev.mods;
    let shift_b = ev.is_command('b') && m.shift && !m.alt && !(cfg!(target_os = "macos") && m.ctrl);
    let ctrl_s = cfg!(target_os = "macos") && ev.is_command('s') && m.ctrl && !m.shift && !m.alt;
    shift_b || ctrl_s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_clamp_round_and_collapse_past_half_the_minimum() {
        let mut p = PaneState::new(276.0).range(136.0, 640.0);
        p.set_width(1000.0);
        assert_eq!(p.width(), 640.0);
        p.set_width(100.4);
        assert_eq!(p.width(), 136.0);
        p.set_width(300.6);
        assert_eq!(p.width(), 301.0);
        p.drag_to(80.0);
        assert!(p.is_open() && p.width() == 136.0, "above half the minimum it clamps");
        p.drag_to(60.0);
        assert!(!p.is_open() && p.width() == 136.0, "below half it collapses and keeps its width");
        assert_eq!(p.shown_width(), 0.0);
        p.drag_to(200.0);
        assert!(p.is_open() && p.width() == 200.0);
        p.reset_width();
        assert_eq!(p.width(), 276.0);
        assert_eq!(PaneState::new(20.0).width(), PaneState::MIN);
        p.set_width(f32::NAN);
        assert_eq!(p.width(), 276.0);
    }
}

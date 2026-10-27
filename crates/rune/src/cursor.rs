//! The pointer's shape over an element.

/// The pointer's shape over an element (CSS `cursor`). Elements without one inherit their
/// nearest ancestor's; text fields default to [`Cursor::Text`], everything else to the arrow.
///
/// A macOS overlay is rarely the key window, so it sets its cursor directly while the pointer
/// is over its hit area. macOS honours that only while the overlay's app is frontmost, unless
/// `rune` is built with the `macos-background-cursor` feature. That feature uses a private
/// WindowServer property (`SetsCursorInBackground`), so App Store review may reject the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Cursor {
    #[default]
    Default,
    Pointer,
    Text,
    Grab,
    Grabbing,
    NotAllowed,
    Move,
    ResizeColumn,
    ResizeRow,
    /// A precise-selection cross (CSS `crosshair`), for picking a point or dragging out a rect.
    Crosshair,
    /// No OS cursor (CSS `none`), for views that draw their own. It shows again as soon as the
    /// pointer leaves the element or the window, the element goes away, or the host loses the
    /// pointer: a window losing focus, an overlay hiding.
    Hidden,
}

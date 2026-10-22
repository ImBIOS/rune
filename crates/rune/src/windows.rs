//! Window descriptions shared by the native shell and deterministic headless selection.
use crate::{App, AppSpec};

/// A display in desktop physical-pixel coordinates (origins may be negative).
/// Headless runs provide one deterministic virtual display.
#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    pub name: Option<String>,
    pub position: (i32, i32),
    pub size: (u32, u32),
    pub scale: f64,
    pub primary: bool,
}

#[derive(Clone, Debug)]
pub struct WindowOptions {
    pub transparent: bool,
    pub borderless: bool,
    pub click_through: bool,
    pub always_on_top: bool,
    pub visible: bool,
    /// Physical desktop coordinates, not points. Use a display's position unchanged.
    pub position: Option<(i32, i32)>,
    /// Physical content size; overrides AppSpec's logical size when set.
    pub physical_size: Option<(u32, u32)>,
    /// Logical minimum; None inherits AppSpec's minimum, Some((0, 0)) removes the floor.
    pub min_size: Option<(f32, f32)>,
}

impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            transparent: false, borderless: false, click_through: false,
            always_on_top: false, visible: true, position: None, physical_size: None,
            min_size: None,
        }
    }
}

impl WindowOptions {
    /// One transparent, borderless, topmost, click-through overlay covering this display.
    pub fn overlay(display: &Display) -> Self {
        Self {
            transparent: true, borderless: true, click_through: true, always_on_top: true,
            visible: true, position: Some(display.position), physical_size: Some(display.size),
            min_size: Some((0.0, 0.0)),
        }
    }
}

/// One independent retained tree and app state. Share Live handles or application services
/// between states; events, focus, animation clocks and redraws remain window-local.
pub struct WindowSpec<S: 'static> {
    pub key: String,
    pub app: App<S>,
    pub options: WindowOptions,
    /// Runs on the event thread after native creation. Not called by headless tooling.
    pub on_created: Option<Box<dyn FnOnce(crate::window::WindowHandle)>>,
}

impl<S> WindowSpec<S> {
    pub fn new(key: impl Into<String>, app: AppSpec<S>) -> Self {
        Self::from_app(key, App::new(app))
    }
    /// Preserve AppHandle, windowless, keep-running and GPU-release behavior for this window.
    /// Closing a default window removes only it; quitting any app exits the whole event loop.
    /// The outer run_windows command line controls tooling; App::args and the process-level
    /// App::accessory launch policy are not used here. AppHandle::set_dock_icon remains global.
    pub fn from_app(key: impl Into<String>, app: App<S>) -> Self {
        Self { key: key.into(), app, options: WindowOptions::default(), on_created: None }
    }
    pub fn options(mut self, options: WindowOptions) -> Self { self.options = options; self }
    pub fn state(mut self, state: S) -> Self { self.app = self.app.init(move || state); self }
    pub fn on_created(mut self, callback: impl FnOnce(crate::window::WindowHandle) + 'static) -> Self {
        self.on_created = Some(Box::new(callback));
        self
    }
}

pub(crate) fn validate<S>(windows: &[WindowSpec<S>]) -> Result<(), String> {
    let mut keys = std::collections::HashSet::new();
    for window in windows {
        if window.key.is_empty() || !keys.insert(&window.key) { return Err(format!("empty or duplicate window key: {:?}", window.key)); }
    }
    Ok(())
}

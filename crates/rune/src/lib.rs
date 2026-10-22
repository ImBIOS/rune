//! Rune, first slice: keyed element tree from plain view functions, Taffy layout, analytic
//! spring tracks, an instanced wgpu uber-quad renderer with Parley text, pointer and keyboard
//! input, scroll viewports, growth stacks, streaming reveals, themes as data, and headless agent
//! tooling.

#[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
mod activation;
mod app;
#[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
mod layer_shell;
mod overlay;
#[cfg(target_os = "windows")]
mod win32;
#[cfg(target_os = "macos")]
mod macos;
pub mod color;
pub mod edit;
pub mod el;
mod key_diagnostics;
pub mod growth;
pub mod live;
mod pixels;
pub mod gpu_springs;
pub mod render;
mod surface_alpha;
pub mod runtime;
pub mod shape;
mod virtual_list;
mod flight;
pub mod file_dialog;
pub mod text;
pub mod theme;
pub mod tok;
pub mod tooling;
mod window;
pub mod windows;
pub use window::WindowHandle;
pub use windows::{Display, RunOptions, WindowOptions, WindowSpec};
pub use winit::raw_window_handle;

pub use app::{App, AppHandle};
pub use overlay::{Availability, OverlayHandle, OverlaySpec, ScreenEdge};
pub use color::{Color, oklch, oklcha};
pub use edit::{Ime, Key, KeyEvent, Mods, TextEdit};
pub use el::{
    Align, Anchor, Cursor, Drag, El, GrowthSpec, Outline, Phase, PointerPress, ScrollSpec, Shadow, Shell, Side, Tx, canvas, col, edit_access, el, icon, rich,
    row, spacer, text, text_input,
};
pub use growth::{GROWTH, StreamEdge, edge};
pub use live::Live;
pub use pixels::Pixels;
pub use file_dialog::{FileDialogError, FileDialogKind, FileDialogOptions, FileDialogs, ScriptedFileDialog};
pub use rune_motion::{
    Anim, AnimExt, Clock, Curve, Edge, LayoutAnim, NodeId, Phase as PresencePhase, Presented, Spring, SpringPair,
    Transition, Travel,
};
pub use runtime::{Cx, Env, Runtime, Waker};
pub use shape::{LineCap, Path, Shape};
pub use text::{Icon, Span};
pub use theme::Theme;
pub use tok::{Family, TextStyle};
pub use tooling::{AppSpec, extra_args, run, run_windows, run_windows_with, set_app_id};

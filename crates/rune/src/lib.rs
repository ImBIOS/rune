//! Rune, first slice: keyed element tree from plain view functions, Taffy layout, analytic
//! spring tracks, an instanced wgpu uber-quad renderer with Parley text, pointer and keyboard
//! input, scroll viewports, growth stacks, streaming reveals, themes as data, and headless agent
//! tooling.

#[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
mod activation;
mod app;
#[cfg(target_os = "macos")]
mod macos;
pub mod color;
pub mod edit;
pub mod el;
pub mod growth;
pub mod live;
pub mod render;
pub mod runtime;
mod virtual_list;
mod flight;
pub mod text;
pub mod theme;
pub mod tok;
pub mod tooling;
mod window;

pub use app::{App, AppHandle};
pub use color::{Color, oklch, oklcha};
pub use edit::{Ime, Key, KeyEvent, Mods, TextEdit};
pub use el::{
    Align, Anchor, Cursor, Drag, El, GrowthSpec, Outline, Phase, ScrollSpec, Shadow, Shell, Side, Tx, col, edit_access, el, icon, rich,
    row, spacer, text, text_input,
};
pub use growth::{GROWTH, StreamEdge, edge};
pub use live::Live;
pub use rune_motion::{
    Anim, AnimExt, Clock, Curve, Edge, LayoutAnim, NodeId, Phase as PresencePhase, Presented, Spring, SpringPair,
    Transition, Travel,
};
pub use runtime::{Cx, Env, Runtime, Waker};
pub use text::{Icon, Span};
pub use theme::Theme;
pub use tok::{Family, TextStyle};
pub use tooling::{AppSpec, extra_args, run, set_app_id};

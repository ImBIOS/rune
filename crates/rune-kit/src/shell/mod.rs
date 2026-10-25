//! A desktop app's shell: [`SplitView`] (sidebar, main pane, inspector, title bar band),
//! [`Toolbar`], [`Sidebar`] with [`SidebarSection`], [`SidebarHeader`] and [`SidebarRow`], and
//! the app-owned [`PaneState`] behind each resizable pane.
//!
//! The module uses only rune's and rune-kit's public API.

mod pane;
mod sidebar;
mod split;
mod toolbar;

pub use pane::{PaneState, is_sidebar_toggle};
pub use sidebar::{INDENT, ROW_HEIGHT, Sidebar, SidebarHeader, SidebarRow, SidebarSection};
pub use split::SplitView;
pub use toolbar::{TOOLBAR_HEIGHT, Toolbar};

use rune::Icon;

/// The sidebar toggle's glyph: a window with its leading pane drawn in.
pub static SIDEBAR: Icon = Icon {
    name: "kit-sidebar",
    view: 16.0,
    body: r#"<rect x="2" y="3" width="12" height="10" rx="2" stroke="currentColor" stroke-width="1" fill="none"/><path d="M6 3.5V12.5" stroke="currentColor" stroke-width="1"/>"#,
};
/// The inspector toggle's glyph: the same window with its trailing pane drawn in.
pub static INSPECTOR: Icon = Icon {
    name: "kit-inspector",
    view: 16.0,
    body: r#"<rect x="2" y="3" width="12" height="10" rx="2" stroke="currentColor" stroke-width="1" fill="none"/><path d="M10 3.5V12.5" stroke="currentColor" stroke-width="1"/>"#,
};

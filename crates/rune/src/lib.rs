//! Rune, first slice: keyed element tree from plain view functions, Taffy layout, analytic
//! spring tracks, an instanced wgpu uber-quad renderer with Parley text, pointer and keyboard
//! input, scroll viewports, growth stacks, streaming reveals, themes as data, and headless agent
//! tooling.

#[cfg(all(target_arch = "wasm32", any(feature = "hot", feature = "dev", feature = "native-file-dialog")))]
compile_error!("web does not support Rune's hot, dev, or native-file-dialog features; disable them for wasm32");

#[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
mod activation;
mod app;
pub mod time;
#[cfg(target_arch = "wasm32")]
pub mod web;
#[cfg(all(feature = "dev", debug_assertions))]
mod dev_clock;
#[cfg(all(feature = "dev", debug_assertions))]
pub mod dev;

/// Live dev value with a default at the call site. Release builds expand to the default alone.
#[cfg(all(feature = "dev", debug_assertions))]
#[macro_export]
macro_rules! tune {
    ($key:expr, $default:expr) => { $crate::dev::tune($key, $default, None) };
    ($key:expr, $default:expr, $min:expr, $max:expr, $step:expr) => {
        $crate::dev::tune($key, $default, Some([$min, $max, $step]))
    };
}
#[cfg(not(all(feature = "dev", debug_assertions)))]
#[macro_export]
macro_rules! tune {
    ($key:expr, $default:expr $(, $min:expr, $max:expr, $step:expr)?) => { $default };
}

/// The logical clock a scene fn (`AppSpec::scenes`) builds its state against. `--scene` starts
/// at time zero; the dev panel's state picker passes the running clock (`poll`'s `now`) and the
/// last frame's animation dt, scaled by the panel's speed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct SceneClock {
    pub now: f64,
    pub dt: f64,
}

/// Readable from scene fns, `AppSpec::init` and `AppSpec::setup`; zero anywhere else.
pub fn scene_clock() -> SceneClock {
    #[cfg(all(feature = "dev", debug_assertions))]
    return dev::scene_clock();
    #[cfg(not(all(feature = "dev", debug_assertions)))]
    SceneClock::default()
}

mod shortcuts;
pub use shortcuts::{PhysicalKeyCode, Shortcut, ShortcutError, ShortcutId, modifier_keycaps};
mod physical_input;
pub use physical_input::PhysicalInput;
mod native_menu;
pub use native_menu::{MenuError, MenuItem};
mod popup_menu;
pub use popup_menu::{PopupAnchor, PopupItem, PopupMenu, PopupMenuError, popup_menu_supported};
#[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
mod layer_shell;
#[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
mod x11_overlay;
#[cfg(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))]
mod x11_pointer;
mod overlay;
mod overlay_options;
mod os_cursor;
#[cfg(any(test, target_os = "windows", all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))))]
mod pointer_observer;
#[cfg(target_os = "windows")]
mod win32;
#[cfg(target_os = "windows")]
mod win32_pointer;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub mod macos_panel_bridge;
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub mod macos_appearance;
#[cfg(target_os = "macos")]
mod display_link;
#[cfg(target_os = "macos")]
mod macos_help;
#[cfg(target_os = "macos")]
mod fn_key;
pub mod color;
pub mod compose;
pub mod edit;
pub mod el;
mod key_diagnostics;
pub mod growth;
pub mod live;
pub mod memo;
mod pixels;
pub mod gpu_springs;
pub mod image;
#[cfg(feature = "hot")]
pub mod hot;
pub mod render;
mod record;
mod headless_run;
mod keycaps;
mod surface_alpha;
mod surface_stats;
pub use surface_stats::{Pacing, SurfaceCounts, SurfaceStats};
pub mod native_surface;
mod system_material;
mod video;
mod recorder;
pub mod runtime;
pub mod shape;
mod virtual_list;
mod virtual_rows;
mod virtual_desktop;
mod desktop;
mod flight;
mod frame_profile;
#[cfg(feature = "frame-profile")]
#[doc(hidden)]
pub use frame_profile::{PHASES, take_frame_phases, thread_cpu_ms};
pub mod file_dialog;
pub mod text;
pub mod theme;
pub mod tok;
pub mod tooling;
mod window;
mod drop_pointer;
mod window_frame;
mod frame_clock;
mod present_time;
mod secure_input;
pub mod clipboard;
mod preferences;
mod accent;
pub use accent::{SystemAccent, system_accent};
pub use clipboard::{ClipboardWrite, SecretCopy, SecretState};
pub mod drag_out;
pub use drag_out::{DragImage, DragItem, DragOperation, DragOutcome};
pub mod windows;
pub use window::WindowHandle;
pub use windows::{AnyWindowSpec, ConfiguredWindowOptions, Display, MacWindowOptions, RunOptions, WindowOptions, WindowSpec};
pub use winit::raw_window_handle;

pub use app::{App, AppHandle};
pub use overlay::{Availability, OverlayHandle, OverlaySpec, ScreenEdge};
pub use overlay_options::{ConfiguredOverlay, DisplayId, DisplayTarget, NativeDisplayId, KeyPolicy, OverlayCapabilities, OverlayCreated, OverlayInput, OverlayLevel, OverlayOptions, OverlayPlacement, OverlaySupport};
pub use color::{Color, oklch, oklcha};
pub use compose::{Compositor, Frame};
pub use edit::{Ime, Key, KeyEvent, KeySide, Modifier, Mods, TextEdit};
pub use el::{
    Align, Anchor, Cursor, Cycle, Drag, El, GrowthSpec, Outline, Phase, PointerPress, RadialRadius, ScrollSpec, Shadow, Shell, Side, Tx, auto_grow, canvas, col, edit_access, el, icon, rich,
    row, spacer, text, text_input,
};
pub use growth::{GROWTH, StreamEdge, edge};
pub use live::{Live, LiveText, Tween};
pub use memo::memo;
pub use pixels::Pixels;
pub use file_dialog::{FileDialogError, FileDialogKind, FileDialogOptions, FileDialogs, ScriptedFileDialog};
pub use image::{Image, ImageCache, ImageError, ImageId};
pub use rune_motion::{
    Anim, AnimExt, Beat, Blend, Clock, CubicBezier, Curve, Decay, Easing, Edge, Effect, LayoutAnim, LoopKind, LoopSpec, NodeId,
    Phase as PresencePhase, Presented, Reveal, Sequence, Spring, SpringPair, StepPosition, Transition, Travel, Vec2, Velocity,
};
pub use runtime::{Cx, Env, Runtime, TitlebarAction, Waker, commit_generation};
pub use shape::{LineCap, Path, Shape};
pub use native_surface::{NativeSurface, SurfaceFit};
pub use system_material::{MAX_NATIVE_MATERIALS, SystemMaterial};
pub use video::{MediaRequest, MediaSource, Player, PlayerState, Video};
pub use recorder::{FakeMicrophone, Level, MicPermission, RecordFormat, Recorder, RecorderState, Recording};
pub use text::{Icon, Link, LinkPaint, Span, TextCase};
pub use theme::Theme;
pub use tok::{Family, OpticalSize, TextStyle};
pub use tooling::{AppSpec, Chrome, extra_args, run, run_windows, run_windows_with, run_windows_with_args, run_with, set_app_id,
    try_run_windows_with, try_run_windows_with_args};

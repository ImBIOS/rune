//! The retained tree: view → keyed nodes → Taffy layout at the target state → `rune-motion` Scene.
//! Animation samples tracks at frame time; it never re-runs view functions.
//!
//! Beside Scene's tracks the runtime owns four render-side behaviours that never touch app code:
//! scroll viewports (offset, momentum, follow-the-end), growth stacks (row heights on a capped
//! spring, positions derived), streaming word reveals, and text-field carets. Each frame:
//! queued input → (commit if state changed) → Scene → growth → scroll → reveals → build.

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;

use parley::Layout;
use parley::editing::Cursor;
use parley::layout::Affinity;
use rune_motion::{
    Anim, Corners, LayoutAnim, Mount, NodeId, NodeSpec, Phase as PresencePhase, Prop, PropClass, Rect as MRect, Scene,
    Spring, Transition, prop, snap::grid,
};
use taffy::prelude::*;
use taffy::TraversePartialTree;

use crate::color::{Color, over, srgb_encode};
use crate::edit::{EditOutcome, Ime, KeyEvent};
use crate::el::{Align, Drag, DragEndFn, DragFn, DragOutFn, El, GrowthSpec, InputSpec, Phase, PointerEvent, PointerFn, PressFn, ScrollSpec, Shell, TextSpec, Tx, Visual};
use crate::growth::{GrowTrack, SUBSTEP};
// Half-open, like overlays, NSPointInRect and PtInRect: a shared edge belongs to one side.
use crate::overlay_options::contains;
use crate::flight::{TextFlight, transformed};
use crate::render::{Instance, KIND_DASHED, KIND_EFFECT, KIND_GLYPH, KIND_GRADIENT, KIND_IMAGE, KIND_INSET, KIND_RADIAL, KIND_RECT, KIND_SEGMENT, KIND_SHADOW, KIND_TRIANGLE, NO_CLIP};
use crate::shape::{LineCap, Op, Shape};
use crate::text::{Deco, PaintedDeco, GlyphQuad, Ink, TextSystem, layout_height};
use crate::tok::motion;

#[inline(always)]
fn handler_state<S: 'static>(state: &mut S) -> &mut dyn Any {
    #[cfg(feature = "hot")]
    if std::any::TypeId::of::<S>() == std::any::TypeId::of::<crate::hot::HostState>() {
        return (state as &mut dyn Any).downcast_mut::<crate::hot::HostState>().unwrap().value.as_mut();
    }
    state
}

/// Calls an app's `poll` or `on_key`. With `rune/hot` it goes through subsecond's jump table,
/// so a patched body runs instead of the pointer captured at startup, and a panicking patch
/// falls back to that pointer.
macro_rules! app_call {
    ($f:expr, $($arg:expr),* $(,)?) => {{
        #[cfg(feature = "hot")]
        let called = crate::hot::patched_or_original!("callback", $f, $($arg),*);
        #[cfg(not(feature = "hot"))]
        let called = ($f)($($arg),*);
        called
    }};
}

#[inline(always)]
pub(crate) fn handler_state_ref<S: 'static>(state: &S) -> &dyn Any {
    #[cfg(feature = "hot")]
    if std::any::TypeId::of::<S>() == std::any::TypeId::of::<crate::hot::HostState>() {
        return (state as &dyn Any).downcast_ref::<crate::hot::HostState>().unwrap().value.as_ref();
    }
    state
}

#[cfg(debug_assertions)]
#[path = "layout_diagnostics.rs"]
mod layout_diagnostics;

#[path = "focus.rs"]
mod focus;
#[path = "physical_dispatch.rs"]
mod physical_dispatch;
#[cfg_attr(not(target_os = "macos"), allow(unused_imports))]
pub(crate) use physical_dispatch::Unhandled;
#[path = "modifier_keys.rs"]
mod modifier_keys;
#[path = "popup_dispatch.rs"]
mod popup_dispatch;
#[path = "file_dialog_dispatch.rs"]
mod file_dialog_dispatch;
#[path = "drop_dispatch.rs"]
mod drop_dispatch;
#[path = "scroll_bottom.rs"]
mod scroll_bottom;
#[path = "caret.rs"]
mod caret;
#[path = "layout_read.rs"]
mod layout_read;
#[path = "links.rs"]
mod links;
#[path = "selectable.rs"]
mod selectable;
#[path = "help.rs"]
mod help;
#[path = "scroll_edge.rs"]
mod scroll_edge;
#[path = "scroll_x.rs"]
mod scroll_x;
#[path = "scroll_offset.rs"]
mod scroll_offset;
#[path = "sticky.rs"]
mod sticky;
#[path = "gesture_dispatch.rs"]
mod gesture_dispatch;
pub(crate) use scroll_x::{SHIFT_TURNS_WHEEL, wheel_delta};
pub(crate) use scroll_offset::ScrollRequest;
#[cfg(test)]
#[path = "truncate_tests.rs"]
mod truncate_tests;
#[cfg(test)]
#[path = "line_clamp_tests.rs"]
mod line_clamp_tests;
#[cfg(test)]
#[path = "aspect_fit_tests.rs"]
mod aspect_fit_tests;
#[cfg(test)]
#[path = "drop_tests.rs"]
mod drop_tests;
#[cfg(test)]
#[path = "on_key_tests.rs"]
mod on_key_tests;
#[cfg(test)]
#[path = "live_only_tests.rs"]
mod live_only_tests;
#[cfg(test)]
#[path = "anchored_grow_tests.rs"]
mod anchored_grow_tests;

/// Seconds after a commit before debug builds check a `Tx::live_only` event (`skip_view`).
#[cfg(debug_assertions)]
const LIVE_ONLY_CHECK: f64 = 0.25;

const P_FILL: Prop<Color> = Prop::custom(64, PropClass::Color);
const P_BORDER: Prop<Color> = Prop::custom(65, PropClass::Color);
const P_TEXT: Prop<Color> = Prop::custom(66, PropClass::Color);
const P_HOVER_C: Prop<Color> = Prop::custom(67, PropClass::Color);
const P_PRESS_C: Prop<Color> = Prop::custom(68, PropClass::Color);
const P_SHADOWS: [Prop<Color>; 4] = [
    Prop::custom(69, PropClass::Color),
    Prop::custom(70, PropClass::Color),
    Prop::custom(71, PropClass::Color),
    Prop::custom(72, PropClass::Color),
];
const P_HOVER: Prop<f32> = Prop::custom(73, PropClass::Opacity);
const P_PRESS: Prop<f32> = Prop::custom(74, PropClass::Opacity);
const P_GRADIENT: [Prop<Color>; 2] = [Prop::custom(75, PropClass::Color), Prop::custom(76, PropClass::Color)];
const P_OUTLINE: Prop<Color> = Prop::custom(77, PropClass::Color);
const P_BORDER_BOTTOM: Prop<Color> = Prop::custom(78, PropClass::Color);
/// `El::press_scale`'s presented factor, multiplied into the element's own scale.
const P_PRESS_SCALE: Prop<f32> = Prop::custom(79, PropClass::Scale);
const P_RADIAL: [Prop<Color>; 8] = [
    Prop::custom(80, PropClass::Color),
    Prop::custom(81, PropClass::Color),
    Prop::custom(82, PropClass::Color),
    Prop::custom(83, PropClass::Color),
    Prop::custom(84, PropClass::Color),
    Prop::custom(85, PropClass::Color),
    Prop::custom(86, PropClass::Color),
    Prop::custom(87, PropClass::Color),
];
/// A property's name in traces.
fn prop_name(id: u16) -> &'static str {
    const BUILTIN: [&str; 12] = ["opacity", "frame", "offset", "scale", "rotation", "blur", "radius", "fill", "mask", "scroll", "presence", "travel"];
    match id {
        0..12 => BUILTIN[id as usize],
        64 => "fill-color",
        65 => "border-color",
        66 => "text-color",
        67 => "hover-color",
        68 => "press-color",
        69..=72 => "shadow-color",
        73 => "hover",
        74 => "press",
        75 | 76 => "gradient-color",
        77 => "outline-color",
        78 => "border-bottom-color",
        79 => "press-scale",
        80..=87 => "radial-color",
        88 => "icon-turn",
        89 => "scale",
        90..=93 => "shadow",
        _ => "custom",
    }
}

/// `El::icon_rotation`'s presented turn, in radians.
const P_ICON_TURN: Prop<f32> = Prop::custom(88, PropClass::Rotation);
/// `El::scale`'s presented factor. `prop::SCALE` belongs to `live_scale`; they multiply.
const P_SCALE: Prop<f32> = Prop::custom(89, PropClass::Scale);
/// The first four shadows' geometry: x, y, blur and spread in a rect's lanes.
const P_SHADOW_GEO: [Prop<rune_motion::Rect>; 4] = [
    Prop::custom(90, PropClass::Shape),
    Prop::custom(91, PropClass::Shape),
    Prop::custom(92, PropClass::Shape),
    Prop::custom(93, PropClass::Shape),
];
/// Whether a shadow colour or geometry spring, or an icon's turn, is moving node `id`.
fn paint_pose_springing(store: &rune_motion::AnimationStore, id: NodeId) -> bool {
    store.is_animating(id, P_ICON_TURN)
        || P_SHADOWS.iter().any(|&p| store.is_animating(id, p))
        || P_SHADOW_GEO.iter().any(|&p| store.is_animating(id, p))
}
/// Seconds a changed `El::scale` holds still before its text rasterizes at that size.
const STATIC_SCALE_SETTLE: f64 = 0.1;
/// How far (points, per axis) a click may travel and still pair into a double click.
const CLICK_SLOP: f32 = 4.0;

/// Scroll momentum decay (iOS normal: 0.998 per ms).
const SCROLL_DECAY: f32 = 2.002;

#[cfg(any(test, target_arch = "wasm32"))]
#[path = "web_runtime.rs"]
pub(crate) mod web_runtime;

#[derive(Clone, Copy, Debug)]
pub struct Env {
    pub system_dark: bool,
    pub scale: f32,
}
impl Env {
    pub const fn new(system_dark: bool, scale: f32) -> Self {
        Self { system_dark, scale }
    }
}
impl Default for Env {
    fn default() -> Self { Self::new(true, 1.0) }
}

/// Wakes the shell from another thread (new data arrived); a no-op headless.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// The most frames in a row an index-built list keeps the host drawing to fill its overscan.
/// Filling takes four at most (half a viewport per side each); this bounds a list whose
/// heights keep changing.
const VIRTUAL_FILL_FRAMES: u32 = 12;

thread_local! {
    static COMMITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many commits (runs of a view that go on to the screen) have started on this thread,
/// across all its windows; 0 before the first. A cache of what views draw can keep whatever
/// the last commit or two used and evict the rest.
pub fn commit_generation() -> u64 {
    COMMITS.get()
}

/// Passed to view functions: the environment, plus runtime settings a view may declare.
pub struct Cx<S> {
    env: Env,
    titlebar_inset: f32,
    system_accent: crate::SystemAccent,
    system_reduced_motion: bool,
    reduced_motion: bool,
    time_scale: f32,
    caret: Option<(String, [f32; 4])>,
    caret_read: std::cell::Cell<bool>,
    layout: layout_read::LayoutSeen,
    layout_reads: std::cell::RefCell<Vec<String>>,
    _s: PhantomData<fn(&S)>,
}

impl<S> Cx<S> {
    #[cfg(feature = "hot")]
    pub(crate) fn retype<T>(&self) -> Cx<T> {
        Cx {
            env: self.env, titlebar_inset: self.titlebar_inset, system_accent: self.system_accent,
            system_reduced_motion: self.system_reduced_motion, reduced_motion: self.reduced_motion, time_scale: self.time_scale,
            caret: self.caret.clone(), caret_read: self.caret_read.clone(),
            layout: self.layout.clone(), layout_reads: self.layout_reads.clone(), _s: PhantomData,
        }
    }

    #[cfg(feature = "hot")]
    pub(crate) fn settings_from<T>(&mut self, other: Cx<T>) {
        self.reduced_motion = other.reduced_motion;
        self.time_scale = other.time_scale;
        self.caret_read = other.caret_read;
        self.layout_reads = other.layout_reads;
    }

    pub fn env(&self) -> Env {
        self.env
    }
    pub fn system_dark(&self) -> bool {
        self.env.system_dark
    }
    /// Points from the window's leading edge to the trailing edge of the macOS traffic lights
    /// drawn over the content, for a top bar's leading inset: 0 when nothing is drawn there (full
    /// screen, a titled window whose content starts below its titlebar, other platforms). The
    /// view re-runs when it changes, as entering and leaving full screen do. Headless runs report
    /// [`Chrome::titlebar_inset`](crate::Chrome::titlebar_inset). A `memo` closure that reads
    /// it needs it in its version.
    pub fn titlebar_inset(&self) -> f32 {
        self.titlebar_inset
    }
    /// The user's accent colour (System Settings › Appearance on macOS) for this window's
    /// appearance, [`SystemAccent::DEFAULT`](crate::SystemAccent::DEFAULT)'s blue elsewhere and
    /// headless. The view re-runs, memos included, when the user picks another accent.
    pub fn accent(&self) -> Color {
        self.system_accent.accent(self.env.system_dark)
    }
    /// The keyboard focus ring colour for this window's appearance: AppKit's
    /// `keyboardFocusIndicatorColor`, the accent at 50% elsewhere. Elements with no ring of
    /// their own draw their default focus ring in it.
    pub fn focus_color(&self) -> Color {
        self.system_accent.focus(self.env.system_dark)
    }
    /// Browser safe-area insets in points, in top/right/bottom/left order. Zero on native.
    /// Apply these inside a full-canvas root so backgrounds can extend to the screen edges.
    pub fn safe_area(&self) -> [f32; 4] {
        #[cfg(target_arch = "wasm32")]
        { crate::web::safe_area() }
        #[cfg(not(target_arch = "wasm32"))]
        { [0.0; 4] }
    }
    /// The host's OS accessibility preference, independent of this commit's override.
    pub fn system_reduced_motion(&self) -> bool {
        self.system_reduced_motion
    }
    /// Whether this commit reduces motion: the host preference, or the view's override from
    /// [`Self::reduced_motion`]. Pass it to builders that pick a different motion under it.
    pub fn is_reduced_motion(&self) -> bool {
        self.reduced_motion
    }
    /// Override the host preference for this commit: travel jumps, opacity/colour briefly fade.
    pub fn reduced_motion(&mut self, on: bool) {
        self.reduced_motion = on;
    }
    /// Scale the window's animation clock (slow motion below 1, paused at 0). To slow or freeze
    /// only part of the window, use [`El::time_scale`](crate::El::time_scale), which multiplies
    /// with this.
    pub fn time_scale(&mut self, scale: f32) {
        self.time_scale = scale.clamp(0.0, 8.0);
    }
    /// The focused text field's caret line in window points `[x, y, w, h]`, as last drawn (after
    /// scrolling, at the cursor inside an IME composition), when that field is keyed `key` or sits
    /// inside an element keyed `key`. A view that reads it re-runs when the caret moves, so a
    /// popup placed from it lands the frame after each edit.
    pub fn caret(&self, key: &str) -> Option<[f32; 4]> {
        self.caret_read.set(true);
        let rect = caret_of(&self.caret, key);
        crate::memo::read_caret(key, rect);
        rect
    }
    /// The laid-out rect `[x, y, w, h]` in window points, after scrolling, of the element keyed
    /// `key` (or named by a key path such as `"msg-42/body"`, as `Tx::fly_text` takes), or
    /// `None` when there is none. It is where layout puts the element, not a frame of its spring.
    ///
    /// A view that reads it re-runs when that rect moves or resizes past half a point: in the
    /// same commit when the change comes from layout, the next frame when scrolling moved it.
    /// Reading an element the view itself resizes (its own size, say) settles: the commit
    /// re-runs the view at most twice more while the value moves, then keeps the last layout,
    /// and layout reads alone can't keep causing commits. A view whose output never reaches a
    /// fixed point gets a stale value rather than oscillating.
    pub fn layout(&self, key: &str) -> Option<[f32; 4]> {
        let mut reads = self.layout_reads.borrow_mut();
        if !reads.iter().any(|k| k == key) { reads.push(key.to_owned()); }
        let rect = self.layout.get(key).copied().flatten();
        crate::memo::read_layout(key, rect);
        rect
    }
}

/// [`Cx::caret`] for the caret `caret` (the focused field's path and rect).
pub(crate) fn caret_of(caret: &Option<(String, [f32; 4])>, key: &str) -> Option<[f32; 4]> {
    let (path, rect) = caret.as_ref()?;
    path.split('/').any(|seg| seg == key).then_some(*rect)
}

/// A node's place under a growth stack's row. The row's height carries vertical motion there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RowLayout {
    /// Not inside a growth row.
    Free,
    /// Inside one: layout snaps.
    Snaps,
    /// Inside one under `El::animate_layout`: x and width spring, y and height snap.
    Horizontal,
}

pub(crate) struct Node {
    on_paste: Option<crate::el::PasteFn>,
    on_window_focus: Option<crate::el::WindowFocusFn>,
    focus: crate::el::FocusSpec,
    modal: Option<crate::el::ModalSpec>,
    looping: Option<rune_motion::LoopSpec>,
    pub id: NodeId,
    pub path: String,
    pub key: Option<String>,
    pub trace_name: Option<&'static str>,
    pub trace_namespace: Option<(&'static str, &'static str)>,
    pub role: &'static str,
    pub label: Option<String>,
    pub vis: Visual,
    live: Vec<crate::live::Bound>,
    pub text: Option<TextSpec>,
    pub text_layout: Option<Rc<Layout<Ink>>>,
    /// Where `text_layout` cut the text for `El::truncate` and its kin.
    pub(crate) elided: Option<Elision>,
    /// The run a cell measures and draws from (see `El::advance_in`).
    run_layout: Option<Rc<crate::text::RunLayout>>,
    live_text_seen: Option<u128>,
    on_press: Option<PressFn>,
    on_double_press: Option<PressFn>,
    on_secondary_press: Option<PointerFn>,
    on_hover: Option<PressFn>,
    on_hover_change: Option<crate::el::WindowFocusFn>,
    hover_delay: [f32; 2],
    on_pointer_move: Option<PointerFn>,
    gestures: Option<Box<crate::canvas_input::CanvasInput>>,
    above: Option<crate::el::Anchor>,
    on_drag: Option<DragFn>,
    drag_out: Option<DragOutFn>,
    drag_threshold: Option<f32>,
    on_drag_start: Option<PressFn>,
    on_drag_end: Option<DragEndFn>,
    drop: Option<Box<crate::el::DropSpec>>,
    motion: Spring,
    opacity_motion: Option<rune_motion::Anim>,
    lead: Option<(Spring, Spring)>,
    width_leads: bool,
    transition: Option<Transition>,
    /// `El::matched` id, with the element's own (not inherited) layout spring.
    matched: Option<(Rc<str>, Option<Spring>)>,
    scroll: Option<ScrollSpec>,
    scroll_axes: crate::el::ScrollAxes,
    on_at_bottom: Option<crate::el::AtBottomFn>,
    on_visible_rows: Option<crate::el::VisibleRowsFn>,
    on_layout: Option<crate::el::LayoutFn>,
    on_screen_layout: Option<crate::el::LayoutFn>,
    on_drawn_layout: Option<crate::el::DrawnLayoutFn>,
    /// `El::sticky_top`, `El::sticky_bottom` and `El::stuck_shadow`.
    sticky: Option<Box<crate::el::Sticky>>,
    scroll_edges: crate::el::ScrollEdges,
    growth: Option<GrowthSpec>,
    streaming: bool,
    input: Option<Box<InputSpec>>,
    cursor: Option<crate::el::Cursor>,
    window_drag: bool,
    /// `El::selection_group`.
    selection_group: bool,
    a11y: crate::el::A11y,
    /// Inside an `El::inert` subtree (this element or an ancestor).
    pub(crate) inert: bool,
    press_scale: Option<f32>,
    /// `El::scale`: a factor on the element's own scale, springing on `P_SCALE`.
    static_scale: f32,
    /// `P_SCALE` has a track: paint reads the sprung factor.
    scale_track: bool,
    /// `El::time_scale`: this element's own factor; the scene multiplies ancestors' in.
    time_scale: f32,
    /// The image ratio this picture's layout took, when it sizes one side from its pixels.
    pixel_aspect: Option<f32>,
    /// `El::aspect_fit`: the ratio and the element's own style, which the fitted size replaces.
    aspect_fit: Option<Box<(f32, Style)>>,
    material: Option<crate::SystemMaterial>,
    /// Where it sits relative to a growth row, which carries its contents' motion.
    in_row: RowLayout,
    virtual_mount: bool,
    pub parent: Option<usize>,
    taffy: taffy::NodeId,
    /// Layout target in root space: x0, y0, x1, y1 in points.
    pub target: [f32; 4],
    /// Layout target in parent space.
    pub local: MRect,
    /// Padding plus border (left, top, right, bottom): text draws inside it.
    inset: [f32; 4],
    /// `El::help`.
    help: Option<String>,
    /// `El::help_when_truncated`.
    help_cut: bool,
    #[cfg(debug_assertions)]
    scroll_source: Option<&'static std::panic::Location<'static>>,
}

impl Node {
    /// Whether `selector` names this node, as `Cx::layout` reads it: its key, its full path, or
    /// a key path such as `msg-42/body` (see `flight::selects`).
    pub(crate) fn named(&self, selector: &str) -> bool {
        self.path == selector || crate::flight::selects(selector, &self.path, self.key.as_deref())
    }
}

enum Action {
    WindowFocus(bool),
    Physical(crate::PhysicalInput, Option<physical_dispatch::Fallback>),
    FocusControl(String),
    /// The pointer entered a `focus_highlight` row (`Some`) or left every such row.
    HoverFocus(Option<String>),
    Dismiss(String),
    PointerMove(String, PointerEvent),
    Hover(String),
    HoverChange(String, bool),
    AtBottom(String, bool),
    VisibleRows(String, std::ops::Range<usize>),
    Layout(String, [f32; 4]),
    ScreenLayout(String, [f32; 4]),
    /// `El::on_drawn_layout`; true when only a live value, scroll or animation moved it.
    DrawnLayout(String, [f32; 4], bool),
    Press(String, crate::PointerButton),
    DoublePress(String),
    Link(String, usize),
    SecondaryPress(String, PointerEvent),
    Drag(String, Drag, crate::PointerButton),
    DragStart(String),
    DragEnd(String, crate::DragOutcome),
    /// A file drag from another app moved, left, or dropped (`drop_dispatch`).
    Files(drop_dispatch::FileEvent),
    Key { ev: KeyEvent, repeat: bool },
    KeyUp(KeyEvent),
    /// A wheel delta at window point (x, y) for the `on_wheel` handlers at the pointer,
    /// innermost first (`gesture_dispatch.rs`).
    Wheel(u64, Vec<String>, crate::WheelEvent),
    Gesture(Vec<String>, gesture_dispatch::Gesture),
    Ime(Ime),
    Paste(crate::ClipboardContent, Option<Option<String>>),
    /// A `Tx::read_clipboard` callback's id and what the clipboard held.
    ClipboardRead(u64, crate::ClipboardContent),
    /// A `Tx::open_file` or `Tx::save_file` callback's id and the dialog's answer.
    FileDialog(u64, crate::file_dialog::FileDialogResult),
    Focus(String, f32, f32),
    Blur,
}

/// What kept a frame animating, as bits in traces (`quality.why`).
pub(crate) struct AnimSource;

impl AnimSource {
    pub const TRACKS: u16 = 1;
    pub const GROWTH: u16 = 2;
    pub const SCROLL: u16 = 4;
    pub const REVEAL: u16 = 8;
    pub const LOOP: u16 = 16;
    pub const SCALE_SETTLE: u16 = 32;
    pub const VIDEO: u16 = 64;
    pub const TEXT_FLIGHT: u16 = 128;
    pub const LIVE: u16 = 256;
    pub const VIRTUAL_FILL: u16 = 512;
    pub const NAMES: [&'static str; 10] =
        ["tracks", "growth", "scroll", "reveal", "loop", "scale-settle", "video", "text-flight", "live", "virtual-fill"];

    pub fn names(bits: u16) -> Vec<&'static str> {
        Self::NAMES.iter().enumerate().filter(|(i, _)| bits & (1 << i) != 0).map(|(_, n)| *n).collect()
    }
}

/// What tools see of one element.
#[derive(Clone, Debug)]
pub struct ElementInfo {
    pub trace_name: Option<&'static str>,
    pub trace_namespace: Option<(&'static str, &'static str)>,
    pub loop_phase: Option<f64>,
    pub focusable: bool,
    pub focused: bool,
    pub focus_visible: bool,
    pub inert: bool,
    /// Playing its exit transition, or inside an element that is. Lookups by key treat it
    /// as gone; it stays listed so tools can watch the exit.
    pub exiting: bool,
    pub path: String,
    pub key: Option<String>,
    pub role: &'static str,
    pub label: Option<String>,
    /// What a text element shows (none for a secure field). An element of role `"text"` drawn
    /// from several texts, such as rune-kit's `RollingText`, gives its label.
    pub text: Option<String>,
    pub rect: [f32; 4],
    pub target: [f32; 4],
    pub opacity: f32,
    /// Effective uniform scale, including ancestors and the presence transition.
    pub scale: f32,
    /// Additive blur culling margin in logical points, including scaled ancestor sigmas.
    /// Three times this value bounds filter support; it is not the equivalent Gaussian sigma.
    pub blur: f32,
    pub radius: f32,
    pub fill: Color,
    pub text_color: Option<Color>,
    pub hover: f32,
    pub press: f32,
    pub moving: bool,
    pub parent: Option<usize>,
    /// The part of `rect` its ancestors' clips leave visible.
    pub visible: [f32; 4],
    /// Words of this text are still fading in.
    pub revealing: bool,
    /// Inside a growth row whose height is moving: its visible part is being revealed.
    pub in_growing_row: bool,
    /// This node is riding user-driven wheel/trackpad scrolling or its momentum.
    pub scrolling: bool,
    /// Invisible estimated extent of a virtual list, not a drawable surface.
    pub virtual_extent: bool,
    /// Property-specific intentional cuts for 1:1 live bindings (never smoothed bindings).
    pub live_intent: u32,
    /// On the focused text field: its caret line in window points (see [`Cx::caret`]).
    pub caret: Option<[f32; 4]>,
    /// Dragging it starts an OS drag ([`El::drag_out`]).
    pub drag_out: bool,
    /// Its tooltip and accessibility help ([`El::help`]).
    pub help: Option<String>,
    /// The part of this [`El::selectable`] text that is selected, as copying would take it.
    pub selected: Option<String>,
    /// Its outline depth ([`El::level`]).
    pub level: Option<u32>,
    /// Whether it is open ([`El::expanded`]); `None` when it can't open.
    pub expanded: Option<bool>,
    /// A selected item of a list, tree, table or group ([`El::focus_selected`]).
    pub item_selected: bool,
    /// How far [`El::sticky_top`], [`El::sticky_bottom`] or [`El::sticky_left`] moved it from
    /// where scrolling put it this frame, in points: down for a stuck header, up for a stuck
    /// footer, right for a stuck left element, 0 when it isn't stuck. `rect` already includes it.
    pub stuck: f32,
}

/// An element [`Runtime::element_at`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picked {
    path: String,
    key: Option<String>,
}

/// Options for [`Runtime::element_at_with`] and its siblings. Fields are private, so options can
/// be added without breaking callers; `Pick::default()` is [`Runtime::element_at`]'s behaviour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pick {
    transparent: bool,
}

impl Pick {
    pub fn new() -> Self { Self::default() }

    /// Also find elements whose own or inherited opacity is 0, as a design canvas does to
    /// select an invisible layer. Exiting elements are skipped either way, and so are hidden
    /// or unmounted ones.
    pub fn transparent(mut self, on: bool) -> Self {
        self.transparent = on;
        self
    }
}

impl Picked {
    /// Its path in the element tree (as [`ElementInfo::path`]).
    pub fn path(&self) -> &str { &self.path }
    /// Its key ([`El::key`]), if it has one.
    pub fn key(&self) -> Option<&str> { self.key.as_deref() }
}

/// What an element presents beyond its rect and opacity, as of the last frame
/// ([`Runtime::presented_pose`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Pose {
    rotation: f32,
    scale: f32,
    shadows: Vec<crate::el::Shadow>,
    moving: bool,
}

impl Pose {
    /// The element's own presented `El::rotate`, in radians clockwise. Ancestors' turns are not
    /// included.
    pub fn rotation(&self) -> f32 { self.rotation }
    /// Effective uniform scale, ancestors included (as [`ElementInfo::scale`]).
    pub fn scale(&self) -> f32 { self.scale }
    /// The element's shadows (`El::box_shadow`) at their current sprung geometry and colour.
    pub fn shadows(&self) -> &[crate::el::Shadow] { &self.shadows }
    /// A spring or decorative loop still moves its rect, scale, rotation, icon turn or shadows,
    /// or an ancestor's. An active repeating loop does not settle.
    pub fn moving(&self) -> bool { self.moving }
}

#[derive(Clone, Copy)]
struct Inh {
    opacity: f32,
    paint_opacity: f32,
    clip: [f32; 4],
    fade: [f32; 4],
    fade_origin: [f32; 4],
    /// Paint clips are consumed by isolation; logical clips remain for hit tests/inspection.
    paint_clip: [f32; 4],
    paint_fade: [f32; 4],
    paint_fade_origin: [f32; 4],
    rounded: Option<([f32; 4], [f32; 4])>,
    /// Ancestors' scales, composed: a point `x` (points, with `dx`/`dy` applied) draws at
    /// `anchor_to + (x - anchor) * scale`. The anchors differ when scales nest.
    scale: f32,
    anchor: [f32; 2],
    anchor_to: [f32; 2],
    blur: f32,
    /// Vertical offset in points from scroll viewports and growth rows above.
    dy: f32,
    /// Horizontal offset in points: a matched flight centring its scaled children.
    dx: f32,
    /// The part of `dx`/`dy` from flights' covers above. A flight nested in another travels its
    /// own root-space path, so it drops this.
    cover: [f32; 2],
    /// The clip, fade and fade origin outside the nearest flight above, for a flight nested in it.
    outer: Option<[[f32; 4]; 3]>,
    mark: u32,
    /// The nearest hover group's hover amount (0–1), for `group_*` styles.
    group: f32,
    /// How `scale` is changing, which decides how text rasterizes under it.
    scale_motion: ScaleMotion,
    /// Inside an active `El::cache` scope: do not cull children against the window viewport.
    in_cache: bool,
    /// Inside an elided or off-screen `El::cache` scope: skip primitive emission.
    skip_paint: bool,
    /// Ancestors' `El::rotate` layers, composed, as an index into `turn_scratch` plus one
    /// (0: none): where this subtree's device pixels land. An index keeps `Inh` small.
    turn: u32,
}

/// A 2×3 affine in device pixels, `[a, b, c, d, tx, ty]`: `(x, y)` lands at
/// `(a·x + b·y + tx, c·x + d·y + ty)`. The composed `El::rotate` layers above a node.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Turn {
    mat: [f32; 6],
    rect: [f32; 4],
    angle: f32,
    pivot: [f32; 2],
    outer_clip: [f32; 4],
}

/// The composed turn for a node under `Inh::turn` index `inherited` turning `own` radians about
/// `pivot`: pushed onto `turns` when it turns itself. Off the paint loop's common path.
#[cold]
#[inline(never)]
fn compose_turn(
    turns: &mut Vec<Turn>,
    inherited: u32,
    own: Option<f32>,
    (rect, pivot, outer_clip): ([f32; 4], [f32; 2], [f32; 4]),
) -> (Option<Turn>, u32) {
    let above = inherited.checked_sub(1).map(|k| turns[k as usize]);
    match own {
        None => (above, inherited),
        Some(a) => {
            let own = Turn::about(a, pivot, rect, outer_clip);
            let t = above.map_or(own, |t| t.after(own));
            turns.push(t);
            (Some(t), turns.len() as u32)
        }
    }
}

/// Open the layer a turned node draws into, upright, at `rect` (x, y, w, h in device pixels).
/// The layer takes the clips from above, so the node's own clips start fresh inside it; a
/// rectangular cut through the inherited rounded mask gets an outer scope of its own.
#[cold]
#[inline(never)]
fn open_turned_layer(instances: &mut Vec<Instance>, layers: &mut Vec<(usize, usize, usize)>, (i, surfaces): (usize, usize),
    rect: [f32; 4], (angle, pivot): (f32, [f32; 2]),
    (clip, fade, fade_origin, rounded): (&mut [f32; 4], &mut [f32; 4], &mut [f32; 4], &mut Option<([f32; 4], [f32; 4])>))
{
    let intersect = |a: [f32; 4], b: [f32; 4]| [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])];
    if let Some((mask, _)) = *rounded && intersect(*clip, mask) != mask {
        let mut marker = Instance::begin_layer(rect, *clip, Default::default());
        marker.fade = crate::render::pack_fade(*fade, *fade_origin, *clip);
        layers.push((i, instances.len(), surfaces));
        instances.push(marker);
        (*clip, *fade, *fade_origin) = (NO_CLIP, [0.0; 4], NO_CLIP);
    }
    if let Some((mask, _)) = *rounded { *clip = intersect(*clip, mask); }
    let mut marker = Instance::begin_layer(rect, *clip, Default::default()).turned(angle, pivot);
    marker.fade = crate::render::pack_fade(*fade, *fade_origin, *clip);
    if let Some((_, radii)) = *rounded { marker.round_clip_corners(radii); }
    layers.push((i, instances.len(), surfaces));
    instances.push(marker);
    (*clip, *fade, *fade_origin, *rounded) = (NO_CLIP, [0.0; 4], NO_CLIP, None);
}

fn screen_clip_of(parent: Option<usize>, inh: &[Inh], turns: &[Turn]) -> [f32; 4] {
    let Some(pi) = parent else { return NO_CLIP };
    let Some(h) = inh.get(pi) else { return NO_CLIP };
    if let Some(&t) = h.turn.checked_sub(1).and_then(|k| turns.get(k as usize)) {
        let ic = if h.clip == NO_CLIP { NO_CLIP } else { t.bbox(h.clip) };
        let oc = t.outer_clip;
        [oc[0].max(ic[0]), oc[1].max(ic[1]), oc[2].min(ic[2]), oc[3].min(ic[3])]
    } else {
        h.clip
    }
}

impl Turn {
    /// `angle` radians clockwise about `p`.
    fn about(angle: f32, p: [f32; 2], rect: [f32; 4], outer_clip: [f32; 4]) -> Turn {
        let (sn, c) = angle.sin_cos();
        Turn {
            mat: [c, -sn, sn, c, p[0] - c * p[0] + sn * p[1], p[1] - sn * p[0] - c * p[1]],
            rect,
            angle,
            pivot: p,
            outer_clip,
        }
    }

    /// `inner`, then `self`.
    fn after(self, inner: Turn) -> Turn {
        let [a, b, c, d, x, y] = self.mat;
        let [e, f, g, h, u, v] = inner.mat;
        let ic = if inner.outer_clip == NO_CLIP { NO_CLIP } else { self.bbox(inner.outer_clip) };
        let oc = self.outer_clip;
        Turn {
            mat: [a * e + b * g, a * f + b * h, c * e + d * g, c * f + d * h, a * u + b * v + x, c * u + d * v + y],
            outer_clip: [oc[0].max(ic[0]), oc[1].max(ic[1]), oc[2].min(ic[2]), oc[3].min(ic[3])],
            ..inner
        }
    }

    /// The upright bounding box of edges `r` once turned.
    fn bbox(self, r: [f32; 4]) -> [f32; 4] {
        let [a, b, c, d, x, y] = self.mat;
        let mut out = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
        for (px, py) in [(r[0], r[1]), (r[2], r[1]), (r[0], r[3]), (r[2], r[3])] {
            let (qx, qy) = (a * px + b * py + x, c * px + d * py + y);
            out = [out[0].min(qx), out[1].min(qy), out[2].max(qx), out[3].max(qy)];
        }
        out
    }

    /// Unturns a screen-space point `q` back into the layer's upright device pixels.
    fn unturn(self, [qx, qy]: [f32; 2]) -> [f32; 2] {
        let [a, b, c, d, tx, ty] = self.mat;
        let (dx, dy) = (qx - tx, qy - ty);
        [a * dx + c * dy, b * dx + d * dy]
    }
}

#[derive(Clone, Debug)]
struct NodeCache {
    signature: u64,
    raster_scale: f32,
    seen_scale: f32,
    scale_changed_at: f64,
    env_scale: f32,
    was_live: bool,
    rel_bounds: [f32; 4],
    glyphs: usize,
    commit_gen: u64,
    paint_state_hash: u64,
}

#[derive(Clone, Copy)]
struct ActiveCache {
    node_index: usize,
    node_id: NodeId,
    instance_start: usize,
    origin: [f32; 2],
    size: [f32; 2],
    cur_scale: f32,
    raster_scale: f32,
    seen_scale: f32,
    scale_changed_at: f64,
    env_scale: f32,
    /// Everything outside commits that paints the subtree: hover, press, links, an outside
    /// hover group and the window-wide state in `build`'s paint hash.
    paint_hash: u64,
    glyph_start: usize,
    cached_glyphs: usize,
    elided: bool,
}

/// How a subtree's scale is changing. Text under a still scale rasterizes at its scaled size.
/// Under a live zoom gesture it steps through logarithmic $2^{1/4}$ buckets and lands on the
/// exact resting scale once still; under a short spring it samples its rest-size glyphs.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
enum ScaleMotion {
    #[default]
    Still,
    /// Bound to a live scale that is actively changing: rasterize at $2^{1/4}$ octave buckets.
    Live,
    Moving,
}

impl ScaleMotion {
    /// The scale text under `sc` rasterizes at. It draws at `sc`, filtered, when they differ.
    fn raster(self, sc: f32) -> f32 {
        let k = match self {
            ScaleMotion::Still => sc,
            ScaleMotion::Live => 2.0f32.powf((sc.max(1e-4).log2() * 4.0).round() * 0.25),
            ScaleMotion::Moving => 1.0,
        };
        // A tiny or huge raster isn't worth its atlas space; sample the rest size instead.
        if (1.0 / 16.0..=4.0).contains(&k) { k } else { 1.0 }
    }
}

impl Default for Inh {
    fn default() -> Self {
        Inh { opacity: 1.0, paint_opacity: 1.0, clip: NO_CLIP, fade: [0.0; 4], fade_origin: NO_CLIP,
            paint_clip: NO_CLIP, paint_fade: [0.0; 4], paint_fade_origin: NO_CLIP, rounded: None,
            scale: 1.0, anchor: [0.0; 2], anchor_to: [0.0; 2], blur: 0.0, dy: 0.0, dx: 0.0, cover: [0.0; 2], outer: None, mark: 0, group: 0.0,
            scale_motion: ScaleMotion::Still, in_cache: false, skip_paint: false, turn: 0 }
    }
}

#[derive(Clone, Copy, Debug)]
struct ScrollState {
    offset: f64,
    v: f32,
    pinned: bool,
    max: f64,
    direct: bool,
    scrolling: bool,
    /// Velocity of a glide to the bottom (`Tx::scroll_to_bottom`) or to `glide_to`
    /// (`Tx::scroll_to`) while one runs.
    glide: Option<f32>,
    /// Where a glide goes, when not to the bottom.
    glide_to: Option<f64>,
    /// At-bottom state last sent to `El::on_at_bottom`.
    at_bottom: Option<bool>,
    /// Whether the last frame drew the content's bottom on the viewport's (see `stuck_offset`).
    held: bool,
    /// Room freed under a pinned growth stack, held under its last row (see `step_slack`).
    slack: scroll_bottom::Slack,
    /// The horizontal axis of `El::scroll_x` and `El::scroll_xy`; all zero on a vertical one.
    x: AxisScroll,
}

/// A scroll viewport's horizontal offset, momentum velocity and limit (points).
#[derive(Clone, Copy, Debug, Default)]
struct AxisScroll {
    offset: f64,
    v: f32,
    max: f64,
    /// Velocity of a glide to `aim` (`Tx::scroll_to`) while one runs.
    glide: Option<f32>,
    aim: f64,
}

#[derive(Clone, Debug, Default)]
struct RevealState {
    len: usize,
    /// (word start byte, start time) for words still fading.
    words: Vec<(u32, f64)>,
    fade: f32,
}

#[derive(Clone, Debug, Default)]
struct Stack {
    /// Row ids in presented order, including rows still closing after removal.
    order: Vec<NodeId>,
    /// Removed rows: when their space starts closing.
    closing: HashMap<NodeId, f64>,
    /// Last known target top (container-relative) per row, for rows that left.
    tops: HashMap<NodeId, f32>,
    pad_top: f32,
    pad_bottom: f32,
    gap: f32,
    width: f32,
    /// Measure at the landing width immediately; defer height targets until it is stable.
    width_hold_until: f64,
    width_shrink_until: f64,
    pending_heights: HashMap<NodeId, f32>,
    /// How far its rows grew (positive steps only) this frame.
    grown: f32,
    /// Whether any row in the stack still has height left to grow.
    pub(crate) growing: bool,
    /// Whether any row in the stack is still animating.
    pub(crate) moving: bool,
}

struct LayoutEntry {
    id: taffy::NodeId,
    measure: Option<u64>,
    used: bool,
}

pub struct Runtime<S: 'static> {
    pub state: S,
    pub(crate) on_focus: Option<fn(&mut S, bool, &mut Tx)>,
    view: fn(&S, &mut Cx<S>) -> El,
    #[cfg(feature = "hot")]
    pub(crate) hot: Option<crate::hot::Session>,
    pub text: TextSystem,
    /// RGBA images elements show (`El::pixels`), uploaded by the renderer.
    pub images: crate::pixels::ImageAtlas,
    pub scene: Scene,
    pub env: Env,
    /// Viewport in points.
    pub viewport: (f32, f32),
    /// Transparent native surfaces/headless exports clear to transparent black.
    pub transparent: bool,
    /// Opt-in gamma-space (sRGB) blending for translucent fills, shadows and blurs.
    pub(crate) srgb_blend: bool,
    pub(crate) native_overlay: bool,
    /// The window or overlay surface's frame counts, published by its host.
    pub(crate) surface_stats: Option<std::sync::Arc<std::sync::Mutex<crate::SurfaceStats>>>,
    /// `App::dev_stats`.
    pub(crate) app_stats: Option<fn() -> String>,
    /// Animation time (seconds, scaled by the time scale).
    pub t: f64,
    pub reduced_motion: bool,
    system_reduced_motion: bool,
    titlebar_inset: f32,
    system_accent: crate::SystemAccent,
    pub time_scale: f32,
    /// Called at the start of every frame with the clock; return true when state changed (new data).
    pub poll: Option<fn(&mut S, f64, &mut Tx) -> bool>,
    /// Seconds since the first frame, unscaled (virtual when headless).
    pub clock: f64,
    #[cfg(all(feature = "dev", debug_assertions))]
    pub(crate) dev_clock: Option<crate::dev_clock::DevClock>,
    #[cfg(all(feature = "dev", debug_assertions))]
    pub(crate) dev: Option<crate::dev::Session<S>>,
    wake_deadlines: Vec<f64>,
    /// Windowed runs read the clock from the wall instead of summing frame steps.
    pub wall_start: Option<crate::time::Instant>,
    /// Called for keys unhandled by the focused control; return true to consume it.
    pub on_key: Option<fn(&mut S, &KeyEvent, &mut Tx) -> bool>,
    pub(crate) nodes: Vec<Node>,
    layout: TaffyTree<usize>,
    layout_entries: HashMap<String, LayoutEntry>,
    node_index: HashMap<NodeId, usize>,
    path_ids: HashMap<String, NodeId>,
    pub(crate) memo: crate::memo::MemoCache,
    next_id: u64,
    commits: u64,
    dirty: bool,
    relayout: bool,
    /// Every viewport change since the last commit came from the window's own resize
    /// animation (`step_viewport`), not the user or the system.
    viewport_stepped: bool,
    settle: bool,
    queue: Vec<Action>,
    pointer: Option<(f32, f32)>,
    capture: Option<(String, Drag)>,
    /// The button holding `capture` (`El::pointer_buttons`).
    capture_button: crate::PointerButton,
    secondary_capture: Option<String>,
    capture_inside: bool,
    /// The pointer left while a press held it; the leave lands on release.
    left_in_capture: bool,
    drag_data: f32,
    pub instances: Vec<Instance>,
    /// Per instance, the `El::mark` of its nearest marked node (0 for none). Filled only while
    /// `track_marks` is set (a compositor is attached).
    pub marks: Vec<u32>,
    pub track_marks: bool,
    pub surfaces: Vec<crate::native_surface::SurfaceDraw>,
    /// `El::system_material` elements built this frame, in paint order.
    pub(crate) materials: Vec<crate::system_material::MaterialDraw>,
    /// The shell shows native views for the first `MAX_NATIVE_MATERIALS` of `materials`; their
    /// elements skip their fill and cut their shadows out of their interior.
    pub(crate) native_materials: bool,
    hits: Vec<(NodeId, [f32; 4])>,
    overlay_hits: Vec<[f32; 4]>,
    glyph_scratch: Vec<GlyphQuad>,
    deco_scratch: Vec<PaintedDeco>,
    inh_scratch: Vec<Inh>,
    /// The composed turns `Inh::turn` points into, rebuilt each paint.
    turn_scratch: Vec<Turn>,
    /// For nodes inside a turned layer: `(unturned_rect_pt, unturned_clip_px)`. Empty unless a
    /// node turns.
    unturned_drawn: Vec<([f32; 4], [f32; 4])>,
    /// Only nonempty when retained exit nodes or matched flights make storage order differ
    /// from paint order.
    layer_paint_order: Vec<usize>,
    /// The last commit's `El::matched` ids, one present element each (the last in tree order).
    matched: Vec<(Rc<str>, NodeId)>,
    /// Elements flying from a matched one, with their scene target. They paint last.
    flying: Vec<(NodeId, MRect)>,
    /// `inh_scratch` and `row_dy` index `nodes`: no commit since the last build.
    built: bool,
    #[cfg(debug_assertions)]
    matched_warned: HashSet<Rc<str>>,
    layer_scratch: Vec<(usize, usize, usize)>,
    /// Sideways scroll of focused single-line fields, in points, keyed by node path.
    input_scroll: HashMap<String, f32>,
    dissolve_scratch: Vec<bool>,
    has_dissolve: bool,
    cache_live_scratch: Vec<bool>,
    cache_check_scratch: Vec<bool>,
    cache_paint_scratch: Vec<u64>,
    cache_inside_scratch: Vec<bool>,
    node_caches: HashMap<NodeId, NodeCache>,
    has_cache: bool,
    commit_gen: u64,
    last_dt: f64,
    /// Some picture sizes a side from its image's ratio (`pixel_aspect`).
    has_pixel_aspect: bool,
    has_drawn_layout: bool,
    has_key_up: bool,
    /// Some element is `El::sticky_top` or `El::sticky_bottom` (see `sticky.rs`).
    has_sticky: bool,
    /// Each node's sticky shift this frame (points), while `has_sticky`.
    stuck: Vec<f32>,
    /// Per node, what its children stick within: the nearest vertical viewport's top and
    /// bottom, then the node's own content box top and bottom (unscaled points), while
    /// `has_sticky`.
    sticky_frame: Vec<[f32; 8]>,
    /// `Tx::keep_on_screen` across the commit underway.
    keeps: Vec<sticky::Keep>,
    /// `Tx::scroll_into_view` requests that follow their target while growth/layout springs run.
    scroll_reveals: Vec<sticky::Reveal>,
    /// Some element is `El::raise`d: paint order isn't tree order.
    has_raise: bool,
    /// The focus where the held non-IME key(s) went down, so a release after focus moves
    /// still reaches that element first.
    key_focus: Option<String>,
    keys_held: u32,
    ime_keys_down: u32,
    ime_eat_key_up: bool,
    /// Pictures whose image changed shape since the last commit, which lays them out at once.
    aspect_snap: Vec<NodeId>,
    /// Drawn rect per node (points, root space), for tools and hit tests.
    drawn: Vec<[f32; 4]>,
    /// The scale each node's text drew at (its own and its ancestors'), for text hit tests.
    drawn_scale: Vec<f32>,
    /// Per node: it drew inside its clip and the window in the last build.
    seen: Vec<bool>,
    scrolls: HashMap<NodeId, ScrollState>,
    /// The axis the current wheel or trackpad gesture scrolls (`scroll_x.rs`).
    wheel_lock: scroll_x::WheelLock,
    #[cfg(any(test, target_arch = "wasm32"))]
    web_scroll: Option<web_runtime::TouchScroll>,
    grows: HashMap<NodeId, GrowTrack>,
    stacks: HashMap<NodeId, Stack>,
    pub(crate) virtuals: HashMap<String, crate::virtual_list::VirtualList>,
    /// The memo pass the current commit's first view build was. Memos none of its builds used
    /// go once it applies.
    memo_since: u64,
    /// Frames in a row an index-built list has asked for to fill its overscan.
    virtual_fill: u32,
    /// True when the commit in flight is for scroll/layout only (`self.state` wasn't mutated).
    state_unchanged: bool,
    pub(crate) virtual_culled: HashSet<String>,
    /// Fresh view accessors for focused text fields omitted by virtualization.
    virtual_inputs: HashMap<String, Box<InputSpec>>,
    reveals: HashMap<NodeId, RevealState>,
    flights: Vec<TextFlight>,
    /// Per node index this frame: presented row top dy, row height override, and bottom feather mask.
    row_dy: Vec<f32>,
    row_h: Vec<f32>,
    row_mask: Vec<f32>,
    grow_acc: f32,
    caught_up: f32,
    slack_dt: f32,
    focused: Option<String>,
    keyboard_focus: bool,
    /// The pointer focused a `focus_highlight` row, so its highlight shows without a key.
    hover_focus: bool,
    /// Type-select's typed prefix and when its last key arrived (`now_clock`).
    typeahead: (String, f64),
    /// The activation key (Enter, Escape, Space) held down and the focus it was first pressed
    /// on. Its repeats are dropped once focus has moved, so a held key that closed something
    /// can't act again on whatever took focus next.
    held: Option<physical_dispatch::HeldKey>,
    /// Modifier keys down, for `Key::Modifier` (off unless the app opts in).
    modifier_keys: modifier_keys::ModifierKeys,
    /// Tab visits every control; off, only text fields and lists (macOS Full Keyboard
    /// Access off). Native hosts sample the system setting; headless runtimes default on.
    full_keyboard_access: bool,
    /// `App::tab_visits_controls`: Tab visits every control whatever `full_keyboard_access` says.
    tab_visits_controls: bool,
    modal_stack: Vec<(String, Option<String>)>,
    /// Focus when the running commit began, before a new view's focus request applied: what a
    /// modal that opens in this commit returns focus to.
    focus_before_commit: Option<String>,
    /// Focus before the current gesture/commit blurred it, so a scrim press that blurs on
    /// pointer-down and closes its overlay on pointer-up still honours `focus_return`.
    last_focused: Option<String>,
    /// Focused text fields that unmounted without being blurred: while another field holds focus,
    /// remounting one with its old `focused = true` flag does not steal focus back.
    unmounted_focused: Vec<std::sync::Weak<crate::edit::History>>,
    modal_index: Option<usize>,
    modal_members: Vec<bool>,
    loops: HashMap<NodeId, rune_motion::LoopTrack>,
    loops_active: bool,
    /// `El::scale` per node that has one or had one: (factor, animation time it last changed,
    /// whether it has ever changed). See [`ScaleMotion`].
    static_scales: HashMap<NodeId, (f32, f64, bool)>,
    /// A changed `El::scale` hasn't held still long enough for its text to rasterize.
    static_scale_settling: bool,
    /// Some node ran on a scaled clock (`El::time_scale`) at the last commit.
    time_scaled: bool,
    /// Shared host deadline: unscaled wake time and corresponding animation time.
    loop_wake: Option<(f64, f64)>,
    surface_visible: bool,
    hovered: Option<String>,
    /// Elements with `on_hover_change` that the pointer is inside.
    hover_within: Vec<String>,
    /// Elements with `hover_delays` whose `on_hover_change` last heard `true`.
    hover_shown: Vec<String>,
    /// Delayed hover changes waiting out their delay: path, inside, due on the clock.
    hover_pending: Vec<(String, bool, f64)>,
    /// A press stopped the waits to report an enter; the pointer's next move restarts them.
    hover_paused: bool,
    /// Files dragged over the window from another app (`drop_dispatch`).
    file_drag: drop_dispatch::FileDrag,
    pub ime_area: Option<[f32; 4]>,
    /// The focused field's caret (path, window rect) the next view sees through `Cx::caret`.
    caret: Option<(String, [f32; 4])>,
    /// The last view read the caret, so it re-runs when the caret moves.
    caret_watch: bool,
    /// Keys the last view read with `Cx::layout`, and what each resolved to.
    layout_seen: layout_read::LayoutSeen,
    /// Rect last sent to each element's `El::on_layout`.
    layout_fired: HashMap<NodeId, [f32; 4]>,
    /// Rect last sent to each element's `El::on_screen_layout`.
    screen_fired: HashMap<NodeId, [f32; 4]>,
    /// (Drawn rect, laid-out rect) last sent to each element's `El::on_drawn_layout`.
    drawn_fired: HashMap<NodeId, ([f32; 4], [f32; 4])>,
    /// The window's content top-left in global screen points, as its host last saw it.
    window_origin: (f32, f32),
    /// Commits in a row caused only by layout feedback (see `layout_read`).
    layout_echoes: u32,
    /// An edit ran this frame: keep the caret in its scroll viewport after the commit.
    caret_follow: bool,
    links: links::LinkState,
    /// The selection in `El::selectable` text.
    text_sel: selectable::TextSelection,
    /// Modifier keys held now, as the window last reported them (`Tx::mods`).
    held_mods: crate::Mods,
    /// `El::help` tooltips.
    help: help::HelpState,
    /// The shell cancels native composition before enabling the new text focus.
    pub(crate) ime_reset: bool,
    /// Text the focused field asked to copy (the shell writes it to the clipboard).
    pub copy_request: Option<crate::ClipboardWrite>,
    // Outer None: no read; inner None: app-level paste rather than a focused editor.
    pub(crate) paste_request: Option<Option<String>>,
    /// `Tx::read_clipboard` reads for the shell to start, by id.
    pub(crate) read_requests: Vec<u64>,
    /// Their callbacks, until the clipboard answers.
    clipboard_reads: Vec<(u64, crate::el::ReadFn)>,
    /// The plain text and pill ranges of this runtime's last copy that held pills, for web
    /// clipboard pastes where the OS worker does not own the pasteboard.
    pill_copy: Option<(zeroize::Zeroizing<String>, Vec<(std::ops::Range<usize>, String)>)>,
    next_clipboard_read: u64,
    pub(crate) clear_secret_request: bool,
    /// `Runtime::use_clipboard`: a test clipboard that replaces the OS one.
    pub(crate) clipboard: Option<crate::clipboard::MemoryClipboard>,
    /// A handler or `update` asked to close the window or quit (the shell takes it).
    pub shell_request: Option<Shell>,
    /// A handler or `update` asked to minimize the window (`Tx::minimize`); the shell takes it.
    pub(crate) minimize_request: bool,
    /// Pop-up menus asked for (`Tx::popup_menu`) and not yet answered, oldest first.
    pub(crate) popups: std::collections::VecDeque<crate::popup_menu::Request>,
    /// The first of `popups` is showing.
    pub(crate) popup_open: bool,
    /// The file dialog asked for (`Tx::open_file`, `Tx::save_file`) and not yet answered.
    pub(crate) file_dialog: Option<crate::file_dialog::Pending>,
    /// File dialog callbacks, by id, until their answer runs.
    file_dialog_answers: Vec<(u64, crate::file_dialog::AnswerFn)>,
    next_file_dialog: u64,
    /// A frame change for the window (`Tx::resize_window`), with its spring resolved.
    pub(crate) window_frame: Option<crate::window_frame::FrameRequest>,
    /// The window's height follows the root's natural height (`App::fit_content_height`).
    pub(crate) fit_height: bool,
    /// The content height last asked of the window.
    fit_asked: Option<f32>,
    /// A fitting window at rest asked to move: this frame shows the commit's motion at its
    /// start, as the window can't move until the next frame.
    fit_hold: bool,
    /// The last frame held its motion for the window (`fit_hold`). The host reads it.
    pub(crate) fit_held: bool,
    /// The host's window frame isn't moving or about to: set by the host before each frame.
    pub(crate) window_resting: bool,
    /// What `update` asked of the next commit.
    pending: Tx,
    pub animating: bool,
    /// Why the last frame was animating, as [`AnimSource`] bits, for traces.
    pub(crate) anim_sources: u16,
    /// This frame's commit landed every track (`Tx::settle`): a deliberate cut.
    pub settled: bool,
    /// Events since the last frame, for traces.
    pub events: Vec<String>,
    /// Tooling opts in before any callbacks. Absent in ordinary app runs.
    pub(crate) trace_channels: Option<HashMap<&'static str, (&'static str, f32)>>,
    /// Counters for perf gates.
    pub stats: FrameStats,
    live_wake: Arc<crate::live::Wake>,
    live_clock: Arc<()>,
    window_focused: Option<bool>,
    /// `keys_lost` queued its `FocusLost` since the window last had focus.
    keys_lost: bool,
    live_subscriptions: Vec<Arc<crate::live::Subscription>>,
    live_dirty: bool,
    /// Events whose handler said `Tx::live_only`, and so didn't commit.
    live_only_skips: u64,
    /// The clock at the last commit.
    last_commit_clock: f64,
    /// Debug builds check `Tx::live_only` events (`skip_view`).
    #[cfg(debug_assertions)]
    live_only_check: bool,
    /// The live-only event this frame's commit checks: its handler, and what the view had built.
    #[cfg(debug_assertions)]
    live_check: Option<(String, u64)>,
    /// Live-only events checked (and so committed) so far.
    #[cfg(debug_assertions)]
    live_checks: u64,
    /// Stale-view warnings printed for `Tx::live_only` handlers, each once.
    #[cfg(debug_assertions)]
    pub(crate) live_only_warnings: Vec<String>,
    /// When the frame being built will present; videos pull the picture for it.
    pub(crate) frame_target: Option<crate::time::Instant>,
    /// A drawn video is playing or seeking: keep drawing frames.
    videos_active: bool,
    /// Drawn videos' state generations; a change re-runs the view.
    video_signature: u64,
    /// Opt-in tooling checks. None keeps release commits free of key traversal/allocation.
    pub(crate) key_warnings: Option<Vec<String>>,
    /// Seconds until the next frame while only [`Cycle`](crate::el::Cycle)s move on screen
    /// (the shell sleeps until then instead of drawing every display frame).
    pub ambient: Option<f32>,
    /// Where (window points) the current press started.
    press_at: (f32, f32),
    /// The last press on a window drag region: when (clock seconds) and where.
    last_drag_press: Option<(f64, f32, f32)>,
    /// The first of a possible double press: the element, its down time and point.
    last_press: Option<(String, f64, f32, f32)>,
    /// The press now held is the second of a double press.
    double_armed: bool,
    press_down_t: f64,
    /// The held press has travelled past the click slop: it isn't a click of a double press.
    press_moved: bool,
    /// A scroll under the held press cancelled it ([`Runtime::scroll_cancels_press`]).
    press_cancelled: bool,
    titlebar_action: Option<TitlebarAction>,
    /// Two presses closer than this (seconds) are a double press: the system setting on macOS
    /// and Windows, 0.5 elsewhere and headless.
    pub double_click_interval: f64,
    /// How long (seconds) the pointer rests on an `El::help` element before its tooltip shows:
    /// the system's hover time on Windows, 0.5 elsewhere and headless. macOS windows use
    /// AppKit's own tooltips.
    pub help_delay: f64,
    /// The host starts OS drags, so `drag_out` elements start one (headless tools act as one).
    os_drag: bool,
    /// The held press is on a `drag_out` element that hasn't crossed the threshold yet.
    drag_out_armed: bool,
    /// The armed element's drag threshold (points).
    drag_out_threshold: f32,
    drag_out_request: Option<crate::drag_out::DragOutStart>,
    /// How OS drags ended, filled by the host's session callbacks between frames.
    drag_ends: crate::drag_out::DragEnds,
    /// Drag-outs whose `on_drag_start` ran and whose `on_drag_end` hasn't yet.
    drags_open: usize,
}

/// What a press on a window drag region asks the shell to do, synchronously in that press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitlebarAction {
    /// Move the window with the pointer (the system tracks the drag).
    Drag,
    /// The titlebar double-click action (zoom, minimise or nothing, per system setting).
    DoubleClick,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FrameStats {
    pub commits: u64,
    pub layout_nodes: usize,
    pub glyphs: usize,
    pub shaped: u32,
}

fn ease_out(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    1.0 - (1.0 - x) * (1.0 - x) * (1.0 - x)
}

fn word_starts(s: &str, from: usize, out: &mut Vec<u32>) {
    let mut prev_space = true;
    for (i, c) in s[from..].char_indices() {
        let space = c.is_whitespace();
        if !space && prev_space {
            out.push((from + i) as u32);
        }
        prev_space = space;
    }
}

impl<S: 'static> Runtime<S> {
    pub fn new(state: S, view: fn(&S, &mut Cx<S>) -> El, viewport: (f32, f32), env: Env) -> Self {
        Self::with_text(state, view, viewport, env, TextSystem::default())
    }

    pub fn with_text(state: S, view: fn(&S, &mut Cx<S>) -> El, viewport: (f32, f32), env: Env, text: TextSystem) -> Self {
        let mut scene = Scene::new();
        scene.set_pixel_scale(env.scale);
        crate::recorder::headless_reset();
        crate::video::headless_reset();
        Runtime {
            state,
            view,
            #[cfg(feature = "hot")]
            hot: None,
            text,
            images: Default::default(),
            scene,
            env,
            viewport,
            transparent: false,
            srgb_blend: false,
            native_overlay: false,
            surface_stats: None,
            app_stats: None,
            t: 0.0,
            reduced_motion: false,
            system_reduced_motion: false,
            titlebar_inset: 0.0,
            system_accent: crate::SystemAccent::DEFAULT,
            time_scale: 1.0,
            poll: None,
            clock: 0.0,
            #[cfg(all(feature = "dev", debug_assertions))]
            dev_clock: None,
            #[cfg(all(feature = "dev", debug_assertions))]
            dev: None,
            wake_deadlines: Vec::new(),
            wall_start: None,
            on_key: None,
            nodes: Vec::new(),
            layout: {
                let mut tree = TaffyTree::new();
                tree.disable_rounding();
                tree
            },
            layout_entries: HashMap::new(),
            node_index: HashMap::new(),
            path_ids: HashMap::new(),
            memo: Default::default(),
            next_id: 1,
            commits: 0,
            dirty: true,
            relayout: false,
            viewport_stepped: false,
            settle: false,
            queue: Vec::new(),
            pointer: None,
            capture: None,
            capture_button: crate::PointerButton::Primary,
            secondary_capture: None,
            capture_inside: false,
            left_in_capture: false,
            drag_data: 0.0,
            instances: Vec::new(),
            marks: Vec::new(),
            track_marks: false,
            surfaces: Vec::new(),
            materials: Vec::new(),
            native_materials: false,
            hits: Vec::new(),
            overlay_hits: Vec::new(),
            glyph_scratch: Vec::new(),
            deco_scratch: Vec::new(),
            inh_scratch: Vec::new(),
            turn_scratch: Vec::new(),
            unturned_drawn: Vec::new(),
            layer_paint_order: Vec::new(),
            matched: Vec::new(),
            flying: Vec::new(),
            built: false,
            #[cfg(debug_assertions)]
            matched_warned: HashSet::new(),
            layer_scratch: Vec::new(),
            input_scroll: HashMap::new(),
            dissolve_scratch: Vec::new(),
            has_dissolve: false,
            cache_live_scratch: Vec::new(),
            cache_check_scratch: Vec::new(),
            cache_paint_scratch: Vec::new(),
            cache_inside_scratch: Vec::new(),
            node_caches: HashMap::new(),
            has_cache: false,
            commit_gen: 0,
            last_dt: 0.0,
            has_pixel_aspect: false,
            has_drawn_layout: false,
            has_key_up: false,
            has_sticky: false,
            stuck: Vec::new(),
            sticky_frame: Vec::new(),
            keeps: Vec::new(),
            scroll_reveals: Vec::new(),
            has_raise: false,
            key_focus: None,
            keys_held: 0,
            ime_keys_down: 0,
            ime_eat_key_up: false,
            aspect_snap: Vec::new(),
            drawn: Vec::new(),
            drawn_scale: Vec::new(),
            seen: Vec::new(),
            scrolls: HashMap::new(),
            wheel_lock: Default::default(),
            #[cfg(any(test, target_arch = "wasm32"))]
            web_scroll: None,
            grows: HashMap::new(),
            stacks: HashMap::new(),
            virtuals: HashMap::new(),
            virtual_culled: HashSet::new(),
            memo_since: 0,
            virtual_fill: 0,
            state_unchanged: false,
            virtual_inputs: HashMap::new(),
            reveals: HashMap::new(),
            flights: Vec::new(),
            row_dy: Vec::new(),
            row_h: Vec::new(),
            row_mask: Vec::new(),
            grow_acc: 0.0,
            caught_up: 0.0,
            slack_dt: 0.0,
            focused: None,
            keyboard_focus: false,
            hover_focus: false,
            typeahead: (String::new(), f64::NEG_INFINITY),
            held: None,
            modifier_keys: Default::default(),
            full_keyboard_access: true,
            tab_visits_controls: false,
            modal_stack: Vec::new(),
            focus_before_commit: None,
            last_focused: None,
            unmounted_focused: Vec::new(),
            modal_index: None,
            modal_members: Vec::new(),
            loops: HashMap::new(),
            loops_active: false,
            static_scales: HashMap::new(),
            static_scale_settling: false,
            time_scaled: false,
            loop_wake: None,
            surface_visible: true,
            hovered: None,
            hover_within: Vec::new(),
            hover_shown: Vec::new(),
            hover_pending: Vec::new(),
            hover_paused: false,
            file_drag: Default::default(),
            ime_area: None,
            caret: None,
            caret_watch: false,
            layout_seen: HashMap::new(),
            layout_fired: HashMap::new(),
            screen_fired: HashMap::new(),
            drawn_fired: HashMap::new(),
            window_origin: (0.0, 0.0),
            layout_echoes: 0,
            caret_follow: false,
            links: Default::default(),
            text_sel: Default::default(),
            held_mods: Default::default(),
            help: Default::default(),
            help_delay: 0.5,
            ime_reset: false,
            copy_request: None,
            paste_request: None,
            read_requests: Vec::new(),
            clipboard_reads: Vec::new(),
            pill_copy: None,
            next_clipboard_read: 0,
            clear_secret_request: false,
            clipboard: None,
            shell_request: None,
            minimize_request: false,
            popups: Default::default(),
            file_dialog: None,
            file_dialog_answers: Vec::new(),
            next_file_dialog: 0,
            window_frame: None,
            fit_height: false,
            fit_asked: None,
            fit_hold: false,
            fit_held: false,
            window_resting: false,
            popup_open: false,
            pending: Tx::default(),
            animating: false,
            anim_sources: 0,
            settled: false,
            events: Vec::new(),
            trace_channels: None,
            stats: FrameStats::default(),
            live_wake: crate::live::Wake::new(Arc::new(|| {})),
            live_clock: Arc::new(()),
            frame_target: None,
            videos_active: false,
            video_signature: 0,
            window_focused: None,
            keys_lost: false,
            on_focus: None,
            live_subscriptions: Vec::new(),
            live_dirty: false,
            live_only_skips: 0,
            last_commit_clock: f64::NEG_INFINITY,
            #[cfg(debug_assertions)]
            live_only_check: true,
            #[cfg(debug_assertions)]
            live_check: None,
            #[cfg(debug_assertions)]
            live_checks: 0,
            #[cfg(debug_assertions)]
            live_only_warnings: Vec::new(),
            key_warnings: None,
            ambient: None,
            press_at: (0.0, 0.0),
            last_drag_press: None,
            last_press: None,
            double_armed: false,
            press_down_t: 0.0,
            press_moved: false,
            press_cancelled: false,
            titlebar_action: None,
            double_click_interval: 0.5,
            os_drag: true,
            drag_out_armed: false,
            drag_out_threshold: crate::drag_out::DRAG_THRESHOLD,
            drag_out_request: None,
            drag_ends: Default::default(),
            drags_open: 0,
        }
    }

    /// A drag-out the pointer move just delivered asked for. The shell must start the OS
    /// session before returning from that move event.
    pub(crate) fn take_drag_out(&mut self) -> Option<crate::drag_out::DragOutStart> {
        self.drag_out_request.take()
    }

    /// Where the host reports how a drag-out ended; delivered to `on_drag_end` next frame.
    pub(crate) fn drag_end_sink(&self) -> crate::drag_out::DragEnds { self.drag_ends.clone() }
    pub(crate) fn drag_ends_pending(&self) -> bool { !self.drag_ends.borrow().is_empty() }

    /// Whether this host can start OS drags. Without, `drag_out` elements act as if they had
    /// none.
    pub(crate) fn set_os_drag(&mut self, supported: bool) { self.os_drag = supported; }

    fn drags_out(&self, n: &Node) -> bool { self.os_drag && n.drag_out.is_some() }

    /// A window drag or titlebar double-click requested by the press just delivered. The shell
    /// must act on it before returning from that press event.
    pub fn take_titlebar_action(&mut self) -> Option<TitlebarAction> {
        self.titlebar_action.take()
    }

    /// Clock seconds now: the wall clock in a window (input arrives between frames), the
    /// virtual clock headless.
    fn now_clock(&self) -> f64 {
        self.wall_start.map_or(self.clock, |t| t.elapsed().as_secs_f64())
    }

    pub fn set_viewport(&mut self, viewport: (f32, f32), scale: f32) {
        // Someone resized a window that fits its content: it goes back to the content's height.
        if self.fit_height && self.window_resting && (self.fit_asked.unwrap_or(self.viewport.1) - viewport.1).abs() > 0.5 {
            self.fit_asked = None;
            self.relayout = true;
            self.viewport_stepped = false;
        }
        if self.change_viewport(viewport, scale) { self.viewport_stepped = false; }
    }

    /// The viewport one frame of the window's own resize animation moved it to. Unlike a
    /// user's resize it doesn't land running motion: content at rest follows the window and
    /// springs keep going.
    pub(crate) fn step_viewport(&mut self, viewport: (f32, f32), scale: f32) {
        let fresh = !self.relayout;
        if self.change_viewport(viewport, scale) && fresh { self.viewport_stepped = true; }
    }

    /// Whether the change needs a layout.
    fn change_viewport(&mut self, viewport: (f32, f32), scale: f32) -> bool {
        // A window that fits its content lays out the same at any height: its own resize
        // mustn't cut short the layout springs it is following.
        if self.fit_height && viewport.0 == self.viewport.0 && scale == self.env.scale {
            self.viewport = viewport;
            return false;
        }
        let changed = viewport != self.viewport || scale != self.env.scale;
        if changed {
            if scale != self.env.scale { self.flights.clear(); }
            self.viewport = viewport;
            self.env.scale = scale;
            self.scene.set_pixel_scale(scale);
            self.relayout = true;
        }
        changed
    }

    /// Install before the first frame. Live updates use this separate coalesced wake lane.
    pub fn set_live_waker(&mut self, waker: Waker) {
        for sub in &self.live_subscriptions { sub.active.store(false, std::sync::atomic::Ordering::Release); }
        self.live_subscriptions.clear();
        self.live_wake = crate::live::Wake::new(waker);
        self.live_wake.visible(self.surface_visible);
        self.subscribe_live();
    }

    pub fn set_live_visible(&mut self, visible: bool) {
        self.live_wake.visible(visible);
        self.surface_visible = visible;
        if !visible { self.loop_wake = None; }
    }
    pub(crate) fn live_visible(&self) -> bool { self.surface_visible }
    pub fn live_pending(&self) -> bool {
        self.surface_visible && (self.live_dirty || self.live_wake.pending.load(std::sync::atomic::Ordering::Acquire))
    }

    fn subscribe_live(&mut self) {
        use std::sync::atomic::Ordering;
        for node in &mut self.nodes {
            for bound in &mut node.live { bound.attach(&self.live_clock, self.clock); }
        }
        for sub in &self.live_subscriptions { sub.active.store(false, Ordering::Release); }
        // Pixels only wake: each frame's build uploads their current image, so unlike bound
        // values they leave nothing to sample on a later visible frame.
        let mut sampled = false;
        for node in &self.nodes {
            // An element removed outright leaves the scene now but `nodes` only at the next
            // prune. Exiting elements and their children are still drawn, so their text and
            // pixels keep waking until the exit ends (commit already dropped their bindings).
            if !self.scene.contains(node.id) { continue; }
            let text = node.text.as_ref().and_then(|t| t.live.as_ref()).map(|t| &t.source);
            sampled |= text.is_some() || !node.live.is_empty();
            let pixels = node.vis.pixels.as_ref().map(|p| &p.source);
            for source in node.live.iter().map(|b| &b.source).chain(text).chain(pixels) {
                if let Some(sub) = self.live_subscriptions.iter().find(|s| s.source_id == source.id()) {
                    sub.active.store(true, Ordering::Release);
                } else {
                    self.live_subscriptions.push(source.subscribe(&self.live_wake));
                }
            }
        }
        self.live_subscriptions.retain(|s| s.active.load(Ordering::Acquire));
        self.live_dirty = sampled;
    }

    /// Clear before this frame reads anything a source publishes (a picture's size for layout,
    /// a bound value): a concurrent publication either lands in this frame or keeps a wake for
    /// the next. Never clear after reading (lost wake race). Hidden hosts keep the wake for show.
    fn take_live_wake(&self) -> bool {
        self.surface_visible && self.live_wake.pending.swap(false, std::sync::atomic::Ordering::AcqRel)
    }

    fn latch_live(&mut self, pending: bool) -> bool {
        if !self.surface_visible || self.live_subscriptions.is_empty() { return false; }
        if !pending && !self.live_dirty { return false; }
        self.live_dirty = false;
        for node in &mut self.nodes {
            if let Some(spec) = &mut node.text
                && let Some(live) = &spec.live
                && let Some(text) = live.read(&mut node.live_text_seen)
                && spec.source_text() != &*text
            {
                spec.content = text.to_string();
                spec.apply_case();
                node.text_layout = Some(self.text.layout_live(&spec.content, spec.style, self.env.scale));
            }
            for bound in &mut node.live {
                let initial = bound.seen.is_none();
                let (bits, active) = bound.read(&self.live_clock, self.clock);
                self.live_dirty |= active;
                let Some(bits) = bits else { continue };
                let spring = if initial { None } else { bound.spring };
                use crate::live::{LiveValue, Property};
                fn apply<T: rune_motion::Animatable>(scene: &mut Scene, id: NodeId, prop: Prop<T>, value: T, spring: Option<Spring>) {
                    if let Some(spring) = spring { scene.animate(id, prop, value, spring); }
                    else { scene.set(id, prop, value); }
                }
                match bound.property {
                    Property::Opacity => apply(&mut self.scene, node.id, prop::OPACITY, f32::decode(bits).clamp(0.0, 1.0), spring),
                    Property::Blur => apply(&mut self.scene, node.id, prop::BLUR, f32::decode(bits).max(0.0), spring),
                    Property::Scale => apply(&mut self.scene, node.id, prop::SCALE, f32::decode(bits).max(0.0), spring),
                    Property::Offset => {
                        let [x, y] = <[f32; 2]>::decode(bits);
                        apply(&mut self.scene, node.id, prop::OFFSET, rune_motion::Vec2::new(x, y), spring);
                    }
                    Property::Fill => {
                        // The renderer uses the exact authored colour after its track settles.
                        node.vis.fill = Color::decode(bits);
                        apply(&mut self.scene, node.id, P_FILL, node.vis.fill, spring);
                    }
                    Property::Rect => {
                        let [x, y, w, h] = <[f32; 4]>::decode(bits);
                        self.scene.set_frame(node.id, MRect::new(x, y, w.max(0.0), h.max(0.0)),
                            spring.map_or(LayoutAnim::INSTANT, LayoutAnim::new));
                    }
                }
            }
        }
        self.live_dirty
    }

    pub fn set_system_dark(&mut self, dark: bool) {
        if dark != self.env.system_dark {
            self.env.system_dark = dark;
            self.dirty = true;
        }
    }

    /// What [`Cx::titlebar_inset`] reports. Native macOS windows set it from AppKit's buttons on
    /// every resize, full screen included; a change re-runs the view.
    pub fn set_titlebar_inset(&mut self, points: f32) {
        if points != self.titlebar_inset {
            self.titlebar_inset = points;
            self.dirty = true;
        }
    }

    /// What [`Cx::accent`] and [`Cx::focus_color`] report, and the colour of default focus
    /// rings. Native windows set it from [`system_accent`](crate::system_accent) and again when
    /// the user picks another accent; a change re-runs the view. Headless runs keep
    /// [`SystemAccent::DEFAULT`](crate::SystemAccent::DEFAULT) unless a test sets one.
    pub(crate) fn system_accent(&self) -> crate::SystemAccent {
        self.system_accent
    }

    pub fn set_system_accent(&mut self, accent: crate::SystemAccent) {
        if accent != self.system_accent {
            self.system_accent = accent;
            self.dirty = true;
        }
    }

    /// Tab visits every control when on, else only text fields and lists, like macOS with
    /// Keyboard navigation (Full Keyboard Access) off. Native windows follow the system setting.
    pub fn set_full_keyboard_access(&mut self, on: bool) {
        self.full_keyboard_access = on;
    }
    /// Tab visits every control whatever [`Runtime::set_full_keyboard_access`] says
    /// (`App::tab_visits_controls`).
    pub fn set_tab_visits_controls(&mut self, on: bool) {
        self.tab_visits_controls = on;
    }
    pub fn set_system_reduced_motion(&mut self, reduced: bool) {
        if self.system_reduced_motion != reduced {
            self.system_reduced_motion = reduced;
            self.dirty = true;
        }
    }
    /// Host accessibility preference; false by default in a headless runtime.
    pub fn system_reduced_motion(&self) -> bool {
        self.system_reduced_motion
    }

    /// Queue a native focus transition. Repeated notifications do not repeat the callback.
    /// Losing focus is also `keys_lost`.
    pub fn window_focus(&mut self, focused: bool) {
        // Every focus gain, even one a hide without blur left unreported.
        if focused { self.keys_lost = false; }
        if self.window_focused != Some(focused) {
            self.window_focused = Some(focused);
            if let Some(f) = self.on_focus { self.update(|s, tx| f(s, focused, tx)); }
            // Deliver the cancelled Up before FocusLost can commit a view that removes its
            // grip (or the focus callback clears the app's drag state).
            if !focused { self.cancel_pointer(); }
            self.queue.push(Action::WindowFocus(focused));
            if !focused {
                self.keys_lost();
                // Losing the window ends hover, rather than waiting out an exit grace.
                self.hover_pending.clear();
                for path in std::mem::take(&mut self.hover_shown) {
                    self.queue.push(Action::HoverChange(path, false));
                }
            }
        }
        if !focused { self.wheel_lost(); }
    }

    /// The window gets no more keys: it lost focus or hid, which may never report a blur. The
    /// focused physical handler hears `FocusLost` and held modifier keys are released, once
    /// until the window has focus again. A hidden host steps `hidden_timer_frame` while
    /// `keys_queued` to deliver them.
    pub(crate) fn keys_lost(&mut self) {
        if std::mem::replace(&mut self.keys_lost, true) { return; }
        self.physical_input(crate::PhysicalInput::FocusLost);
    }

    /// Keyboard input, a focus change, a file drag step or a file dialog answer is queued for the
    /// next frame.
    pub(crate) fn keys_queued(&self) -> bool {
        self.queue.iter().any(|a| matches!(a, Action::Physical(..) | Action::Key { .. } | Action::KeyUp(_) | Action::WindowFocus(_) | Action::Files(_) | Action::FileDialog(..)))
    }

    /// The last committed frame is out of date: state changed (an update, poll or handler), or
    /// one asked for a scroll or text flight that waits for the next frame.
    pub(crate) fn needs_commit(&self) -> bool {
        self.dirty || self.pending.frame_work() || self.patch_pending()
    }

    /// The last frame no longer shows the window: state, its size or queued input changed since.
    pub(crate) fn stale(&self) -> bool {
        self.needs_commit() || self.relayout || !self.queue.is_empty()
    }

    /// A live-edit patch arrived that the next frame hasn't rebuilt the view with yet.
    pub(crate) fn patch_pending(&self) -> bool {
        #[cfg(feature = "hot")]
        return self.hot.as_ref().is_some_and(crate::hot::Session::pending);
        #[cfg(not(feature = "hot"))]
        false
    }

    /// Marks app state changed outside a handler (the next frame re-runs the view).
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// Whether a compositor should apply its optional rune-fx motion blur.
    /// Always true when no dev panel is attached.
    pub fn motion_blur_enabled(&self) -> bool {
        #[cfg(all(feature = "dev", debug_assertions))]
        if let Some(dev) = &self.dev { return dev.blur; }
        true
    }

    #[cfg(all(feature = "dev", debug_assertions))]
    pub(crate) fn dev_velocity_overlay(&mut self) {
        for (i, node) in self.nodes.iter().enumerate() {
            let Some(v) = self.scene.store().velocity(node.id, prop::FRAME) else { continue };
            let (vx, vy) = (v.x + v.w * 0.5, v.y + v.h * 0.5);
            if vx.hypot(vy) < 1.0 { continue; }
            let [x, y, w, h] = self.drawn[i];
            let start = [x + w * 0.5, y + h * 0.5];
            let end = [start[0] + vx * 0.05, start[1] + vy * 0.05];
            let shape = Shape::new().line(start, end, 2.0, LineCap::Round, Color::WHITE);
            let scale = self.env.scale;
            push_shape(&mut self.instances, &shape, [0.0; 2], [scale; 2], scale, 0.8, self.srgb_blend, NO_CLIP, [0.0; 4]);
        }
        if self.track_marks { self.marks.resize(self.instances.len(), 0); }
    }

    /// Changes the state from outside any handler, for data another thread delivered
    /// (`AppHandle::update`). Runs now, so it works while no window shows; the next frame commits
    /// it with the motion `f` asked for.
    pub fn update(&mut self, f: impl FnOnce(&mut S, &mut Tx)) {
        self.update_if(|s, tx| { f(s, tx); true });
    }

    /// `update` for a change that may be no change: `f` returns whether it changed state the
    /// view reads, as `poll` does. On false the view doesn't re-run and no frame is asked for,
    /// while what `f` asked of its `Tx` still happens: clipboard, close and quit, menus and
    /// window frames reach the shell, a scroll or text flight still takes a frame, and a
    /// `wake_after` waits, like `update`'s, for the next frame or hidden poll to schedule it
    /// (a host that runs neither calls `schedule_pending_wakes`). Returns whether the last frame
    /// went out of date: `f` reported a change or asked for work only a frame does.
    ///
    /// Returning false after changing state the view reads leaves the screen stale until the
    /// next change.
    pub fn update_if(&mut self, f: impl FnOnce(&mut S, &mut Tx) -> bool) -> bool {
        let mut one = self.transaction();
        let changed = f(&mut self.state, &mut one);
        let stale = changed || one.frame_work() || one.window_frame.is_some() || one.srgb_blend.is_some_and(|b| b != self.srgb_blend)
            || (one.blur && self.focused.is_some());
        self.latch_requests(&mut one);
        merge(&mut self.pending, one);
        self.dirty |= changed;
        stale
    }

    /// Events whose handler called [`Tx::live_only`] and so didn't re-run the view, since the
    /// runtime started.
    pub fn live_only_events(&self) -> u64 {
        self.live_only_skips
    }

    /// Whether debug builds check `Tx::live_only` handlers (on by default): a live-only event
    /// a quarter second or more after the last commit still commits, and Rune warns when the
    /// view built something else. A test counting commits turns it off. Release builds never
    /// check.
    pub fn check_live_only(&mut self, on: bool) {
        #[cfg(debug_assertions)]
        { self.live_only_check = on; }
        let _ = on;
    }

    /// Takes a `Tx::minimize` that `what` can't carry out, saying so in debug builds.
    pub(crate) fn ignore_minimize(&mut self, what: &str) {
        if std::mem::take(&mut self.minimize_request) && cfg!(debug_assertions) {
            eprintln!("rune: minimize ignored: {what} can't minimize");
        }
    }

    /// Latches a Tx's shell and clipboard requests for the shell, so `update`, a hidden poll
    /// and `on_close` reach the clipboard without a frame, and applies clipboard requests to a
    /// memory clipboard at once. A secret clear drops an earlier secret copy not yet written.
    fn latch_requests(&mut self, tx: &mut Tx) {
        self.run_pick_reads(tx);
        self.shell_request = self.shell_request.max(tx.shell.take());
        self.minimize_request |= std::mem::take(&mut tx.minimize);
        if std::mem::take(&mut tx.clear_secret) {
            if self.copy_request.as_ref().is_some_and(crate::ClipboardWrite::is_secret) { self.copy_request = None; }
            self.clear_secret_request = true;
            self.pill_copy = None;
        }
        if let Some(copy) = tx.copy.take() {
            self.pill_copy = (!copy.is_secret())
                .then(|| copy.content().html())
                .flatten()
                .filter(|h| h.contains("<span data-pill="))
                .map(|h| {
                    let frag = crate::TextEdit::from_html(h);
                    (
                        zeroize::Zeroizing::new(copy.content().text().to_owned()),
                        frag.pills().map(|(r, id)| (r, id.to_owned())).collect::<Vec<_>>(),
                    )
                })
                .filter(|(_, pills)| !pills.is_empty());
            self.copy_request = Some(copy);
        }
        if std::mem::take(&mut tx.paste) { self.paste_request = Some(self.paste_target()); }
        for read in tx.clipboard_reads.drain(..).filter_map(|read| read.take()) {
            self.next_clipboard_read += 1;
            self.clipboard_reads.push((self.next_clipboard_read, read));
            self.read_requests.push(self.next_clipboard_read);
        }
        self.popups.extend(tx.popups.drain(..).filter_map(|call| call.take()));
        for request in tx.file_dialogs.drain(..).filter_map(|call| call.take()) {
            self.request_file_dialog(request);
        }
        if let Some(srgb) = tx.srgb_blend.take() { self.set_srgb_blend(srgb); }
        if std::mem::take(&mut tx.blur) {
            if self.focused.is_some() {
                self.set_focus(None, self.keyboard_focus);
                self.dirty = true;
            }
            // An explicit blur is not the temporary pointer-down blur of a scrim.
            self.last_focused = None;
        }
        if let Some(mut request) = tx.window_frame.take() {
            request.spring = (request.animate && !tx.instant && !self.reduced_motion).then(|| tx.motion.unwrap_or(motion::DEFAULT));
            self.window_frame = Some(self.window_frame.unwrap_or_default().merge(request));
        }
        crate::clipboard::flush_memory(self);
    }

    /// Asks the window for the content's new height, on this commit's spring. The first
    /// commit, a viewport change and `Tx::instant` jump.
    fn fit_window(&mut self, height: f32, tx: &Tx, first: bool) {
        let height = height.ceil_to(self.env.scale).max(1.0);
        if self.fit_asked.unwrap_or(self.viewport.1) == height { return; }
        self.fit_asked = Some(height);
        let jump = first || tx.instant || tx.relayout || self.reduced_motion;
        // Only a macOS window and headless runs move the frame on a spring.
        if !jump && self.window_resting && (cfg!(target_os = "macos") || self.wall_start.is_none()) { self.fit_hold = true; }
        let request = crate::window_frame::FrameRequest {
            size: Some((self.viewport.0, height)), animate: !jump,
            spring: (!jump).then(|| tx.motion.unwrap_or(motion::DEFAULT)), center: false,
        };
        self.window_frame = Some(self.window_frame.unwrap_or_default().merge(request));
    }

    /// The root's natural height at the current width, measured without committing, so a
    /// window that fits its content can open at that height.
    pub(crate) fn content_height(&mut self) -> f32 {
        let measured = self.measure(&Tx::default());
        let height = measured.nodes.first().map_or(self.viewport.1, |root| root.local.h);
        self.layout = measured.layout;
        height.ceil_to(self.env.scale).max(1.0)
    }

    /// `poll` alone, for a hidden window's wake: its timers and inbox advance while nothing
    /// lays out, animates or draws. Like `update`, its changes wait, dirty, for the next frame.
    /// Returns whether the last frame went out of date: `poll` reported a change or asked for a
    /// scroll or text flight, or a live-edit patch arrived. A `wake_after` alone doesn't count.
    pub(crate) fn poll_hidden(&mut self) -> bool {
        if let Some(clock) = self.wall_clock() { self.clock = clock; }
        // An `update`'s `wake_after` counts from now; no frame may come to schedule it.
        let wakes = std::mem::take(&mut self.pending.wakes);
        self.schedule_wakes(wakes);
        // Due timers are spent with or without a poll, as a frame spends them; one left due
        // would wake the host again every frame interval.
        let due = self.wake_deadlines.partition_point(|t| *t <= self.clock);
        self.wake_deadlines.drain(..due);
        let patched = self.patch_pending();
        let Some(poll) = self.poll else { return patched };
        let mut one = self.transaction();
        let changed = app_call!(poll, &mut self.state, self.clock, &mut one);
        self.dirty |= changed;
        self.schedule_wakes(one.wakes.drain(..));
        let asks_frame = one.srgb_blend.is_some_and(|b| b != self.srgb_blend) || (one.blur && self.focused.is_some());
        self.latch_requests(&mut one);
        let stale = changed || one.frame_work() || patched || asks_frame;
        merge(&mut self.pending, one);
        stale
    }

    /// Host time on the logical clock, unless a dev clock owns it.
    fn wall_clock(&self) -> Option<f64> {
        let wall_start = self.wall_start;
        #[cfg(all(feature = "dev", debug_assertions))]
        let wall_start = wall_start.filter(|_| self.dev_clock.is_none());
        wall_start.map(|t0| t0.elapsed().as_secs_f64())
    }

    /// Schedules the `wake_after`s that updates left for a frame, counting from now, when no
    /// frame or hidden poll comes to. The clock stays at the last poll, so the frame-interval
    /// floor on timers (`window::timer_deadline`) still counts from that poll.
    pub(crate) fn schedule_pending_wakes(&mut self) {
        let wakes = std::mem::take(&mut self.pending.wakes);
        self.schedule_wakes_from(self.wall_clock().unwrap_or(self.clock), wakes);
    }

    /// Sorted, de-duplicated deadlines on the logical clock.
    fn schedule_wakes(&mut self, wakes: impl IntoIterator<Item = std::time::Duration>) {
        self.schedule_wakes_from(self.clock, wakes);
    }

    fn schedule_wakes_from(&mut self, now: f64, wakes: impl IntoIterator<Item = std::time::Duration>) {
        for delay in wakes {
            let deadline = now + delay.as_secs_f64();
            if let Err(index) = self.wake_deadlines.binary_search_by(|t| t.total_cmp(&deadline)) {
                self.wake_deadlines.insert(index, deadline);
            }
        }
    }

    pub fn tracing(&self) -> bool { self.trace_channels.is_some() || self.scene.tracing() }

    fn transaction(&self) -> Tx {
        Tx { trace: self.tracing().then(crate::el::TraceCalls::default), mods: self.held_mods, ..Tx::default() }
    }

    /// A scalar from the actual draw path (e.g. `shell.width`). Values hold until resampled.
    /// Call before the tooling writer records the frame; scene traces sample in `frame`.
    pub fn trace_channel(&mut self, name: &'static str, kind: &'static str, value: f32) {
        if let Some(channels) = &mut self.trace_channels { channels.insert(name, (kind, value)); }
        if let Some(trace) = self.scene.trace_mut() { trace.channel(name, kind, value); }
    }

    /// Queues an app marker once for each attached writer. Formatting is skipped when off.
    pub fn trace_event(&mut self, event: impl std::fmt::Display) {
        if self.trace_channels.is_some() { self.events.push(event.to_string()); }
        if let Some(trace) = self.scene.trace_mut() { trace.event(event); }
    }

    /// Next requested wake on the unscaled app clock. None lets the shell sleep indefinitely.
    pub fn wake_deadline(&self) -> Option<f64> {
        let deadline = self.logical_wake_deadline()?;
        #[cfg(all(feature = "dev", debug_assertions))]
        if let Some(clock) = &self.dev_clock { return clock.deadline(deadline); }
        Some(deadline)
    }

    /// The native window or overlay's frame counts; `None` headless.
    pub fn surface_stats(&self) -> Option<crate::SurfaceStats> {
        self.surface_stats.as_deref().map(crate::surface_stats::read)
    }

    /// When `poll` last ran, on `wake_deadline`'s clock.
    pub(crate) fn polled_at(&self) -> f64 {
        #[cfg(all(feature = "dev", debug_assertions))]
        if let Some(clock) = &self.dev_clock { return clock.deadline(self.clock).unwrap_or(self.clock); }
        self.clock
    }

    fn logical_wake_deadline(&self) -> Option<f64> {
        self.wake_deadlines.first().copied().into_iter().chain(self.loop_wake.map(|w| w.0)).min_by(f64::total_cmp)
    }

    #[cfg(feature = "hot")]
    pub(crate) fn hot_scrolls(&self) -> Vec<(String, f64)> {
        self.nodes.iter().filter_map(|n| self.scrolls.get(&n.id).map(|s| (n.path.clone(), s.offset))).collect()
    }

    #[cfg(feature = "hot")]
    pub(crate) fn hot_motion(&self) -> Vec<(String, [f32; 4], [f32; 4])> {
        let store = self.scene.store();
        self.nodes.iter().filter_map(|n| {
            if !store.is_animating(n.id, prop::FRAME) { return None; }
            Some((n.path.clone(), store.sample(n.id, prop::FRAME)?.edges(), store.sample_velocity(n.id, prop::FRAME)?.edges()))
        }).collect()
    }

    /// Lands every track before the next frame: growth, reveals and follow-the-end (guarantee 8,
    /// for returning to a window or session).
    pub fn settle_all(&mut self) {
        self.settle = true;
        self.dirty = true;
    }

    /// Advances animation time by `dt` real seconds, runs queued handlers, re-runs the view only
    /// if state changed, steps `Scene`, growth, scroll and reveals, and builds this frame's
    /// instances.
    pub fn frame(&mut self, dt: f64) -> bool {
        self.frame_split(0.0, dt)
    }

    /// A hidden or windowless host's due timer. Under the dev panel, motion the timer starts
    /// keeps real time on hidden ticks, as it would on a shown window's redraws.
    pub(crate) fn hidden_timer_frame(&mut self) {
        #[cfg(all(feature = "dev", debug_assertions))]
        if self.dev.is_some() { return self.dev_hidden_frame(); }
        self.frame(0.0);
    }

    /// Builds what changed while a window that fits its content was hidden, before the host
    /// shows it, so the content's height (`window_frame`, a jump) is known before the window
    /// is ordered in and its first drawable taken. Lands every track, as showing does.
    /// Returns whether it built; the first frame then has nothing left to build.
    pub(crate) fn build_for_show(&mut self) -> bool {
        if !self.fit_height || self.commits == 0 || !self.stale() { return false; }
        self.settle_all();
        self.hidden_timer_frame();
        true
    }

    /// A hidden or covered host's due timer or wake. `poll` and due timers run now; the view
    /// and layout wait, dirty, for the frame that shows the host. Queued input and drag ends take a frame
    /// without drawing (`hidden_timer_frame`) so their handlers hear them now. Returns whether
    /// the last drawn frame went out of date.
    pub(crate) fn hidden_wake(&mut self) -> bool {
        if self.has_pending_input() || self.drag_ends_pending() {
            let commits = self.stats.commits;
            self.hidden_timer_frame();
            // File steps held for that frame's build land in another.
            if self.files_held() { self.hidden_timer_frame(); }
            return self.stats.commits != commits;
        }
        #[cfg(all(feature = "dev", debug_assertions))]
        if self.dev.is_some() { self.dev_hidden_frame(); return true; }
        self.poll_hidden()
    }

    /// One frame in one pass that steps `catch_up + dt` seconds. Motion already running advances
    /// `catch_up` before this frame's handlers and commit, so they retarget it from its pose at
    /// the frame's time. Then everything, including what the commit started, steps `dt`. A late
    /// frame or a sleeping host's gap belongs to old tracks, never to targets first observed
    /// on this frame.
    pub(crate) fn frame_split(&mut self, catch_up: f64, dt: f64) -> bool {
        #[cfg(all(feature = "dev", debug_assertions))]
        if let Some(mut dev) = self.dev.take() {
            let scope = dev.scope();
            let moving = crate::dev::scope(scope, || {
                // Bring existing tracks/timers to the command's timestamp first. Idle wall
                // time must never be charged to a transition or spring created by that command.
                let commands = dev.prepare();
                if commands { self.frame_clock(catch_up + dt, 0.0); }
                dev.before(self);
                if commands { self.frame_clock(0.0, 0.0) } else { self.frame_clock(catch_up, dt) };
                dev.after(self)
            });
            self.dev = Some(dev);
            return moving;
        }
        self.frame_clock(catch_up, dt)
    }

    fn frame_clock(&mut self, catch_up: f64, dt: f64) -> bool {
        // Like a native shell: the logical clock follows host time (timers, poll), while
        // animations step by the frame's own dt, so idle time never lands on a fresh commit.
        #[cfg(not(all(feature = "dev", debug_assertions)))]
        let elapsed = catch_up + dt;
        #[cfg(all(feature = "dev", debug_assertions))]
        let (elapsed, catch_up, dt) = match &mut self.dev_clock {
            Some(clock) => {
                let total = catch_up + dt;
                let (elapsed, animation) = clock.advance(self.wall_start.map(|t| t.elapsed().as_secs_f64()), total);
                // Scale and pause both parts alike; manual steps go to `dt`.
                let catch_up = if total > 0.0 { catch_up * (animation / total).min(1.0) } else { 0.0 };
                (elapsed, catch_up, animation - catch_up)
            }
            None => (catch_up + dt, catch_up, dt),
        };
        // A coarse headless step still polls at its timer deadlines, independent of FPS, as an
        // idle window wakes for them. Like a window's timer floor (`window::timer_deadline`),
        // a poll comes at most once a frame interval and takes every deadline due by then, so
        // wakes that polls keep asking for collapse instead of each adding a poll per frame.
        // Input queued for the requested frame stays at that frame's timestamp. The catch-up
        // is the earliest part of the step.
        let target = self.clock + elapsed;
        if self.wall_start.is_none() && self.timer_step(target).is_some() {
            let input = std::mem::take(&mut self.queue);
            let mut catch_up = catch_up;
            while let Some(at) = self.timer_step(target) {
                let step = at - self.clock;
                let old = step.min(catch_up);
                catch_up -= old;
                self.frame_step(step, old, step - old);
            }
            self.queue.extend(input);
            let step = target - self.clock;
            let old = step.min(catch_up);
            return self.frame_step(step, old, step - old);
        }
        self.frame_step(elapsed, catch_up, dt)
    }

    /// When a headless step toward `target` stops early for a timer: the next deadline, but no
    /// sooner than a frame interval after the last poll. None when the step's own poll at
    /// `target` takes everything due.
    fn timer_step(&self, target: f64) -> Option<f64> {
        let deadline = self.logical_wake_deadline()?;
        let at = deadline.max(self.clock + crate::window::TIMER_FRAME.as_secs_f64());
        (at < target).then_some(at)
    }

    /// `elapsed` advances the logical clock when no wall clock drives it; `catch_up` steps
    /// running motion before the commit and `dt` steps all motion after it.
    fn frame_step(&mut self, elapsed: f64, catch_up: f64, dt: f64) -> bool {
        let mut live_woke = self.take_live_wake();
        #[cfg(feature = "hot")]
        if self.hot.as_mut().is_some_and(crate::hot::Session::changed) {
            // Replace handler trait objects before queued input can call an old vtable.
            // A regular commit preserves keyed nodes, scrolls and spring velocities.
            self.memo.clear();
            self.commit(&Tx::default());
        }
        let mut tx = std::mem::take(&mut self.pending);
        let mut ran = false;
        let shaped0 = self.text.shaped;
        self.settled = false;
        self.clock = self.wall_clock().unwrap_or(self.clock + elapsed);
        if self.wall_start.is_none() {
            crate::video::headless_frame(self.clock);
            // Fake recorders publish this frame's level to bound meters, drawn this frame.
            crate::recorder::headless_frame(self.clock);
            live_woke |= self.take_live_wake();
        }
        if let Some((deadline, animation_time)) = self.loop_wake
            && !self.animating
        {
            // Native shells cap dt after idle. Advance the sleeping part of the
            // delay before new handlers retarget, without double-stepping manual time.
            let elapsed = animation_time - (deadline - self.clock).max(0.0) * self.time_scale as f64;
            self.t += (elapsed - self.t - (catch_up + dt) * self.time_scale as f64).max(0.0);
            self.scene.set_time(self.t);
            if self.clock >= deadline { self.loop_wake = None; }
        }
        // Bring old tracks, growth and scroll to this frame's time before a poll or update
        // retargets them. Presenting waits for `step_motion`, so the frame is sampled and
        // built once.
        if catch_up > 0.0 { self.catch_up_motion(catch_up); }
        let due = self.wake_deadlines.partition_point(|t| *t <= self.clock);
        self.wake_deadlines.drain(..due);
        self.fire_hover_delays();
        if let Some(poll) = self.poll {
            let mut one = self.transaction();
            // Scheduling does not imply a model change.
            if app_call!(poll, &mut self.state, self.clock, &mut one) {
                ran = true;
            }
            merge(&mut tx, one);
        }
        if self.commits == 0 && self.queue.iter().any(|a| matches!(a, Action::WindowFocus(_))) {
            self.commit(&Tx::default());
        }
        let mut ran_layout = false;
        self.report_visible_rows();
        // Hosts take a drag-out inside the move that asked for it; by now nothing will.
        self.drop_drag_out_request();
        let ended: Vec<_> = self.drag_ends.borrow_mut().drain(..).collect();
        let queue = order_drag_ends(std::mem::take(&mut self.queue), ended, &mut self.drags_open);
        let queue = self.hold_files(queue, ran || self.dirty);
        // Something besides a checked live-only event wants this frame's commit, so a change
        // it makes isn't the live-only handler's.
        #[cfg(debug_assertions)]
        let mut ran_other = ran;
        let mut pointer_hover = false;
        for action in queue {
            pointer_hover |= matches!(action, Action::Hover(_));
            // State may have requested focus on a newly mounted field. Resolve the
            // new tree before routing this frame's text, not after it has gone to
            // the previous field. The same boundary covers an earlier handler, and lets a
            // file drag find only what is mounted and reachable now (a covered window builds
            // nothing until this).
            if (ran || self.dirty || !tx.scroll_to.is_empty()) && matches!(&action, Action::Key { .. } | Action::KeyUp(_) | Action::Physical(..) | Action::Ime(_) | Action::Paste(..) | Action::Files(_)) {
                if ran || self.dirty {
                    for (source, target) in std::mem::take(&mut tx.text_flights) {
                        self.capture_text_flight(source, target);
                    }
                    if self.settle { tx.settle(); }
                    self.settled |= tx.settle;
                    #[cfg(debug_assertions)]
                    let check = self.live_check.take().filter(|_| !ran_other && !self.dirty && !ran_layout);
                    self.capture_keeps(&mut tx);
                    self.commit(&tx);
                    #[cfg(debug_assertions)]
                    { self.compare_live_check(check); ran_other = false; }
                    self.apply_keeps();
                    ran = false;
                }
                if !tx.scroll_to.is_empty() {
                    self.keeps.clear();
                    self.scroll_reveals.clear();
                    for (scope, key) in std::mem::take(&mut tx.scroll_to) {
                        self.reveal_row(scope.as_deref(), &key);
                    }
                }
            }
            if matches!(action, Action::Layout(..) | Action::ScreenLayout(..) | Action::DrawnLayout(_, _, false) | Action::VisibleRows(..)) {
                ran_layout |= self.run_action(action, &mut tx);
            } else {
                #[cfg(debug_assertions)]
                let checks = self.live_checks;
                let r = self.run_action(action, &mut tx);
                #[cfg(debug_assertions)]
                { ran_other |= r && checks == self.live_checks; }
                ran |= r;
            }
            self.run_pick_reads(&mut tx);
        }
        self.latch_requests(&mut tx);
        // A handler asked for a menu: the hover it takes ends in this commit, not a frame later.
        if self.release_pointer_for_popup() {
            for action in std::mem::take(&mut self.queue) {
                if matches!(action, Action::HoverChange(..)) { ran |= self.run_action(action, &mut tx); } else { self.queue.push(action); }
            }
            self.latch_requests(&mut tx);
        }
        self.release_files();
        self.schedule_wakes(tx.wakes.drain(..));
        for (source, target) in std::mem::take(&mut tx.text_flights) {
            self.capture_text_flight(source, target);
        }
        if self.caret_watch && self.caret_moved(None) { self.dirty = true; }
        let read_moved = !self.layout_seen.is_empty() && self.layout_feedback_open() && self.layout_moved(None, None);
        if self.has_pixel_aspect && self.pixel_aspect_changed() { self.relayout = true; }
        let besides = self.dirty || self.relayout || self.settle || self.virtual_window_changed();
        let other = ran || besides;
        self.count_layout_echo(ran_layout || read_moved, other);
        if other || ran_layout || read_moved {
            if self.relayout && !ran && !self.dirty {
                if self.viewport_stepped { tx.window_step = true } else { tx.relayout = true }
            }
            if self.settle {
                tx.instant = true;
                tx.settle = true;
            }
            self.settled = tx.settle;
            self.state_unchanged = !ran && !self.dirty && !ran_layout && !self.settle && self.commits > 0;
            #[cfg(debug_assertions)]
            let check = self.live_check.take().filter(|_| !ran_other && !besides && !ran_layout && !read_moved);
            self.capture_keeps(&mut tx);
            self.commit(&tx);
            #[cfg(debug_assertions)]
            self.compare_live_check(check);
            self.apply_keeps();
        }
        #[cfg(debug_assertions)]
        { self.live_check = None; }
        tx.keep_on_screen.clear();
        let mut scroll_to = std::mem::take(&mut tx.scroll_to);
        for (path, v) in &mut self.virtuals {
            if let Some(key) = v.reveal_selected.take() && !pointer_hover {
                // Use the keyed ancestor path so identical row keys in other lists do not
                // intercept the reveal, including when this row is still virtual.
                let scope = path.split('/').filter(|seg| !seg.starts_with('#')).collect::<Vec<_>>().join("/");
                let key = if scope.is_empty() { key } else { format!("{scope}/{key}") };
                scroll_to.push((None, key));
            }
        }
        let mut scroll_bottom = std::mem::take(&mut tx.scroll_bottom);
        let scroll_pages = std::mem::take(&mut tx.scroll_pages);
        let mut scroll_offsets = std::mem::take(&mut tx.scroll_offsets);
        if tx.instant {
            for (_, animated) in &mut scroll_bottom { *animated = false; }
            for r in &mut scroll_offsets { r.animated = false; }
        }
        let follow = std::mem::take(&mut self.caret_follow);
        let destination = !scroll_bottom.is_empty() || !scroll_pages.is_empty() || !scroll_offsets.is_empty();
        if !scroll_to.is_empty() || !scroll_bottom.is_empty() || !scroll_pages.is_empty() || !scroll_offsets.is_empty() || follow {
            self.keeps.clear();
            self.scroll_reveals.clear();
        }
        for (scope, key) in scroll_to { self.reveal_row(scope.as_deref(), &key); }
        if !scroll_bottom.is_empty() || !scroll_pages.is_empty() || !scroll_offsets.is_empty() || follow {
            self.step_scroll(0.0);
        }
        if follow { self.follow_caret(); }
        if !scroll_bottom.is_empty() { self.scroll_to_bottom(scroll_bottom); }
        if !scroll_pages.is_empty() { self.scroll_pages(scroll_pages); }
        if !scroll_offsets.is_empty() { self.scroll_to_offsets(scroll_offsets); }
        // Explicit destinations processed after a reveal win for its entire lifetime.
        if destination { self.scroll_reveals.clear(); }
        // Those scrolls moved the caret after the commit a view placed from it saw.
        if self.caret_watch && self.caret_moved(None) { self.commit(&Tx { instant: true, ..Tx::default() }); }
        self.update_interaction();
        #[cfg(test)]
        if let Some(publish) = tests::MID_FRAME.with(|hook| hook.borrow_mut().take()) { publish(); }
        let live_animating = self.latch_live(live_woke);
        if let Some(trace) = tx.trace.take() {
            for event in trace.events { self.trace_event(event); }
            for (name, kind, value) in trace.channels { self.trace_channel(name, kind, value); }
        }
        self.fit_held = std::mem::take(&mut self.fit_hold);
        let dt = if self.fit_held { 0.0 } else { dt };
        let animating = self.step_motion(dt) || live_animating;
        self.stats.shaped = self.text.shaped - shaped0;
        let filling = self.virtual_filling();
        if live_animating { self.anim_sources |= AnimSource::LIVE; }
        if self.time_scale <= 0.0 { self.anim_sources = 0; }
        if filling { self.anim_sources |= AnimSource::VIRTUAL_FILL; }
        self.animating = (animating && self.time_scale > 0.0) || filling;
        #[cfg(all(feature = "dev", debug_assertions))]
        if self.dev_clock.as_ref().is_some_and(|clock| clock.paused) { self.animating = false; }
        self.animating
    }

    fn catch_up_motion(&mut self, dt: f64) {
        let step = dt * self.time_scale as f64;
        self.t += step;
        self.scene.set_time(self.t);
        self.caught_up += step as f32;
        self.step_growth(step as f32);
        self.step_scroll(step as f32);
    }

    fn step_motion(&mut self, dt: f64) -> bool {
        let step = dt * self.time_scale as f64;
        self.slack_dt = std::mem::take(&mut self.caught_up) + step as f32;
        self.last_dt = step;
        self.t += step;
        self.scene.set_time(self.t);
        if self.scene.tracing() {
            for node in &self.nodes {
                self.scene.trace_name(node.id, node.trace_name);
                self.scene.trace_namespace(node.id, node.trace_namespace);
            }
        }
        for (id, track) in &self.loops {
            if let Some([x, y]) = track.offset(self.scene.store().node_time(*id), self.reduced_motion)
                && let Some(n) = self.node_index.get(id).map(|&i| &self.nodes[i])
            {
                let offset = rune_motion::Vec2::new(x * n.local.w, y * n.local.h);
                if self.scene.get(*id, prop::OFFSET) != Some(offset) {
                    self.scene.set(*id, prop::OFFSET, offset);
                }
            }
        }
        let out = self.scene.frame();
        let mut animating = out.animating;
        let mut sources = if out.animating { AnimSource::TRACKS } else { 0 };
        if !out.exited.is_empty() {
            self.prune_exited();
        }
        if !self.flying.is_empty() {
            let scene = &self.scene;
            let before = self.flying.len();
            // Screen travel ends once it lands and no ancestor still moves. An exiting flight
            // stays lifted until it's gone.
            self.flying.retain(|(id, _)| scene.store().has(*id, prop::TRAVEL)
                || matches!(scene.phase(*id), Some(PresencePhase::Exiting { .. })));
            if self.flying.len() != before { self.update_layer_paint_order(); }
        }
        let mut source = |on: bool, bit: u16| { if on { sources |= bit; } on };
        animating |= source(self.step_growth(step as f32), AnimSource::GROWTH);
        animating |= source(self.step_scroll(step as f32), AnimSource::SCROLL);
        for stack in self.stacks.values_mut() { stack.grown = 0.0; }
        animating |= source(self.reveals_active(), AnimSource::REVEAL);
        animating |= self.build();
        // Compensate drawn motion, leaving unrelated layout and growth springs alone.
        for _ in 0..3 {
            if !self.follow_keeps() { break; }
            animating |= self.build();
        }
        self.finish_keeps();
        self.report_at_bottom();
        animating |= self.report_visible_rows();
        self.report_layout();
        // A report waits for the next frame: draw one to deliver it.
        animating |= self.report_drawn();
        animating |= source(self.loops_active, AnimSource::LOOP);
        animating |= source(self.static_scale_settling, AnimSource::SCALE_SETTLE);
        animating |= source(self.videos_active, AnimSource::VIDEO);
        animating |= source(self.draw_text_flights(), AnimSource::TEXT_FLIGHT);
        self.images.end_frame();
        if self.track_marks {
            self.marks.resize(self.instances.len(), 0);
        }
        self.anim_sources = sources;
        animating
    }

    /// A picture sizing a side from its image changed shape (decoded, or a stream resized).
    /// The next commit lays those pictures out at once, whatever else it animates.
    fn pixel_aspect_changed(&mut self) -> bool {
        for n in &mut self.nodes {
            if let (Some(ratio), Some(pixels)) = (&mut n.pixel_aspect, &n.vis.pixels) {
                let now = pixel_ratio(pixels);
                if now != *ratio { self.aspect_snap.push(n.id); }
                *ratio = now;
            }
        }
        !self.aspect_snap.is_empty()
    }

    fn prune_exited(&mut self) {
        let old = std::mem::take(&mut self.nodes);
        self.has_dissolve = false;
        self.has_cache = false;
        let mut remap = vec![None; old.len()];
        self.node_index.clear();
        for (old_idx, mut n) in old.into_iter().enumerate() {
            if !self.scene.contains(n.id) {
                self.reveals.remove(&n.id);
                self.scrolls.remove(&n.id);
                self.loops.remove(&n.id);
                if !self.node_caches.is_empty() {
                    self.node_caches.remove(&n.id);
                    self.text.atlas.resident_caches.remove(&n.id.0);
                }
                continue;
            }
            n.parent = n.parent.and_then(|p| remap[p]);
            let idx = self.nodes.len();
            remap[old_idx] = Some(idx);
            self.node_index.insert(n.id, idx);
            self.has_dissolve |= n.vis.blend == crate::render::Blend::PlusLighter;
            self.has_cache |= n.vis.cache;
            self.nodes.push(n);
        }
        self.virtuals.retain(|path, _| self.nodes.iter().any(|n| n.path == *path));
        self.refresh_modal_index();
        self.flying.retain(|(id, _)| self.node_index.contains_key(id));
        self.has_raise = self.nodes.iter().any(|n| n.vis.raise);
        if !self.layer_paint_order.is_empty() || !self.flying.is_empty() || self.has_raise { self.update_layer_paint_order(); }
        // A removed element's live sources (a `Pixels` stream) stop waking this window.
        if !self.live_subscriptions.is_empty() {
            let dirty = self.live_dirty;
            self.subscribe_live();
            self.live_dirty &= dirty;
        }
    }

    /// Subtree (preorder) paint order, then each flying element's subtree on top. A sticky
    /// element paints after its normal siblings (and before overlay siblings), so it covers the
    /// rows it's stuck over.
    fn update_layer_paint_order(&mut self) {
        let n = self.nodes.len();
        let mut first = vec![n; n + 1];
        let mut next = vec![n; n];
        let rank = |i: usize| {
            let nd = &self.nodes[i];
            if nd.above.is_some() || nd.modal.is_some() { 2 }
            else if nd.sticky.as_ref().is_some_and(|s| s.edge.is_some()) { 1 }
            else { 0 }
        };
        if self.has_sticky {
            for r in (0..=2).rev() {
                for i in (0..n).rev().filter(|&i| rank(i) == r) {
                    let parent = self.nodes[i].parent.map_or(0, |p| p + 1);
                    next[i] = first[parent]; first[parent] = i;
                }
            }
        } else {
            for i in (0..n).rev() {
                let parent = self.nodes[i].parent.map_or(0, |p| p + 1);
                next[i] = first[parent]; first[parent] = i;
            }
        }
        let mut lifted: Vec<usize> = self.flying.iter().filter_map(|(id, _)| self.node_index.get(id).copied()).collect();
        if self.has_raise { lifted.extend((0..n).filter(|&i| self.nodes[i].vis.raise)); }
        lifted.sort_unstable();
        lifted.dedup();
        self.layer_paint_order.clear();
        for top in std::iter::once(None).chain(lifted.iter().map(|&r| Some(r))) {
            let mut i = top.unwrap_or(first[0]);
            while i < n {
                // A flying subtree paints in its own pass, not where the tree reaches it.
                if top == Some(i) || lifted.binary_search(&i).is_err() {
                    self.layer_paint_order.push(i);
                    if first[i + 1] < n { i = first[i + 1]; continue; }
                }
                loop {
                    if top == Some(i) { i = n; break; }
                    if next[i] < n { i = next[i]; break; }
                    match self.nodes[i].parent { Some(parent) => i = parent, None => { i = n; break; } }
                }
            }
        }
        if self.layer_paint_order.iter().copied().eq(0..n) { self.layer_paint_order.clear(); }
    }

    fn capture_text_flight(&mut self, source: String, target: String) {
        let Some((i, n)) = self.nodes.iter().enumerate().find(|(_, n)| crate::flight::selects(&source, &n.path, n.key.as_deref())) else { return };
        let (Some(spec), Some(layout), Some(r)) = (&n.text, &n.text_layout, self.drawn.get(i)) else { return };
        if spec.content.is_empty() { return; }
        let scale = self.env.scale;
        let (rect, origin) = text_extent(n, spec, layout, *r, scale);
        let clip = self.screen_clip_of_parent(n.parent).map(|v| v / scale);
        let clip = [rect.x.max(clip[0]).max(0.0), rect.y.max(clip[1]).max(0.0),
            (rect.x + rect.w).min(clip[2]).min(self.viewport.0), (rect.y + rect.h).min(clip[3]).min(self.viewport.1)];
        if rect.w <= 0.0 || clip[2] <= clip[0] || clip[3] <= clip[1] { return; }
        self.glyph_scratch.clear();
        self.text.glyphs_for_layout(layout, scale, [origin[0] * scale, origin[1] * scale], false, &mut self.glyph_scratch);
        let color = spec.color;
        let srgb = self.srgb_blend;
        // Colour glyphs live in the image atlas, which may move them before the flight ends.
        let glyphs = self.glyph_scratch.iter().filter(|q| q.color == 0 && q.rect[1] < clip[3] * scale && q.rect[1] + q.rect[3] > clip[1] * scale)
            .map(|q| glyph(q, premul(color, 1.0, srgb), glyph_gamma(color, scale, srgb), clip.map(|v| v * scale), [0.0; 4])).collect();
        self.flights.push(TextFlight::new(source, target, rect, clip, glyphs, scale, self.t));
    }

    /// All destination rectangles below come from this frame's completed growth/scroll build.
    /// The overlay only writes draw instances, never layout or app state.
    fn draw_text_flights(&mut self) -> bool {
        let s = self.env.scale;
        let srgb = self.srgb_blend;
        let now = self.t;
        let reduced = self.reduced_motion;
        let viewport = [0.0, 0.0, self.viewport.0, self.viewport.1];
        let nodes = &self.nodes;
        let scene = &self.scene;
        self.flights.retain(|f| nodes.iter().any(|n| f.lands(&n.path, n.key.as_deref())
            && !matches!(scene.phase(n.id), Some(PresencePhase::Exiting { .. }))));
        let mut done = Vec::new();
        for (fi, f) in self.flights.iter_mut().enumerate() {
            let Some((i, n)) = nodes.iter().enumerate().find(|(_, n)| f.lands(&n.path, n.key.as_deref())) else { continue };
            let (Some(spec), Some(layout)) = (&n.text, &n.text_layout) else { done.push(fi); continue };
            // Both ends are measured by their laid-out text, so a destination box wider than its
            // text (a body stretched across a column) doesn't scale the flight.
            let (target, origin) = text_extent(n, spec, layout, self.drawn[i], s);
            if f.measure(target, now, reduced) { done.push(fi); }
            // Land sub-quantum size residuals before snapping both edges. Otherwise a settled
            // height can alternate by one quantum while its still-moving top crosses the grid.
            let quantum = 1.0 / (64.0 * s);
            if (f.presented.w - target.w).abs() < quantum { f.presented.w = target.w; }
            if (f.presented.h - target.h).abs() < quantum { f.presented.h = target.h; }
            let edges = f.presented.edges().map(|v| grid(v, s));
            f.presented = MRect::new(edges[0], edges[1], edges[2] - edges[0], edges[3] - edges[1]);
            let p = f.progress(now, reduced);
            let (source_alpha, target_alpha) = f.alphas(target, now, reduced);
            let actual_clip = screen_clip_of(n.parent, &self.inh_scratch, &self.turn_scratch).map(|v| v / s);
            let actual_clip = [actual_clip[0].max(0.0), actual_clip[1].max(0.0), actual_clip[2].min(viewport[2]), actual_clip[3].min(viewport[3])];
            let current = f.presented;
            let zoom = current.w / f.source.w;
            let start_clip = [current.x + (f.source_clip[0] - f.source.x) * zoom,
                current.y + (f.source_clip[1] - f.source.y) * zoom,
                current.x + (f.source_clip[2] - f.source.x) * zoom,
                current.y + (f.source_clip[3] - f.source.y) * zoom];
            // A travelling overlay is above the composer and the growing bubble's mask. Using
            // that destination mask here can hide the text before it has reached the bubble.
            // The normal ancestor clip takes over only once the flight has landed.
            let end_clip = viewport;
            let clip = std::array::from_fn(|j| start_clip[j] + (end_clip[j] - start_clip[j]) * p);
            f.clip = if reduced { actual_clip } else { clip };
            f.source_alpha = source_alpha;
            f.target_alpha = target_alpha;
            let final_fade = n.parent.and_then(|p| self.inh_scratch.get(p)).map_or([0.0; 4], |h| h.fade.map(|v| v / s));
            let fade = [0.0; 4];
            if source_alpha > 0.0 {
                let source_rect = if reduced { f.source } else { current };
                let source_clip = if reduced { f.source_clip } else { clip };
                for q in &f.glyphs {
                    self.instances.push(transformed(*q, f.source, source_rect, f.scale, source_alpha, source_clip, fade));
                }
            }
            self.glyph_scratch.clear();
            self.text.glyphs_for_layout(layout, s, [origin[0] * s, origin[1] * s], false, &mut self.glyph_scratch);
            for q in &self.glyph_scratch {
                let Some(q) = glyph_or_image(q, &self.text, &mut self.images, premul(spec.color, 1.0, srgb), glyph_gamma(spec.color, s, srgb), NO_CLIP, [0.0; 4]) else { continue };
                self.instances.push(transformed(q, target, current, s, target_alpha,
                    if reduced { actual_clip } else { clip }, if reduced { final_fade } else { fade }));
            }
        }
        for i in done.into_iter().rev() { self.flights.remove(i); }
        !self.flights.is_empty()
    }

    fn run_action(&mut self, action: Action, tx: &mut Tx) -> bool {
        let mut one = self.transaction();
        // Debug builds name the element too, for the stale-view warning.
        #[cfg(debug_assertions)]
        let live_label = live_only_handler(&action).map(|h| match action_path(&action) {
            Some(path) => format!("{h} on {path:?}"),
            None => h.to_owned(),
        });
        #[cfg(not(debug_assertions))]
        let live_label = live_only_handler(&action);
        let ran = match action {
            Action::WindowFocus(focused) => {
                if !focused { self.held = None; }
                let Some(f) = self.nodes.first().and_then(|n| n.on_window_focus.clone()) else { return false };
                f(handler_state(&mut self.state), focused, &mut one);
                true
            }
            Action::Physical(event, fallback) => {
                // The window lost key focus (an internal focus move also notifies handlers).
                if matches!(event, crate::PhysicalInput::FocusLost) { self.held = None; }
                let ran = self.dispatch_physical(event, fallback, tx);
                let live_only = std::mem::take(&mut tx.live_only);
                return if ran && live_only { self.skip_view("a key handler") } else { ran };
            }
            Action::FocusControl(path) => {
                self.set_focus(Some(path), false);
                true
            }
            Action::HoverFocus(path) => match path {
                Some(path) => {
                    self.set_focus(Some(path), false);
                    self.hover_focus = true;
                    one.instant = true;
                    true
                }
                None => std::mem::take(&mut self.hover_focus),
            },
            Action::Dismiss(path) => {
                let Some(f) = self.node(&path).and_then(|n| n.modal.as_ref().map(|m| m.dismiss.clone())) else { return false };
                self.set_focus(None, self.keyboard_focus);
                f(handler_state(&mut self.state), &mut one);
                true
            }
            Action::PointerMove(path, point) => {
                let Some(f) = self.node(&path).and_then(|n| n.on_pointer_move.clone()) else { return false };
                f(handler_state(&mut self.state), point, &mut one);
                true
            }
            Action::AtBottom(path, at) => {
                let Some(f) = self.node(&path).and_then(|n| n.on_at_bottom.clone()) else { return false };
                f(handler_state(&mut self.state), at);
                true
            }
            Action::VisibleRows(path, range) => {
                let Some(f) = self.node(&path).and_then(|n| n.on_visible_rows.clone()) else { return false };
                f(handler_state(&mut self.state), range)
            }
            Action::Layout(path, rect) => {
                let Some(f) = self.node(&path).and_then(|n| n.on_layout.clone()) else { return false };
                f(handler_state(&mut self.state), rect);
                true
            }
            Action::ScreenLayout(path, rect) => {
                let Some(f) = self.node(&path).and_then(|n| n.on_screen_layout.clone()) else { return false };
                f(handler_state(&mut self.state), rect);
                true
            }
            Action::DrawnLayout(path, rect, _) => {
                let Some(f) = self.node(&path).and_then(|n| n.on_drawn_layout.clone()) else { return false };
                f(handler_state(&mut self.state), rect, &mut one);
                true
            }
            Action::Hover(path) => {
                let Some(f) = self.node(&path).and_then(|n| n.on_hover.clone()) else { return false };
                f(handler_state(&mut self.state), &mut one);
                true
            }
            Action::HoverChange(path, inside) => {
                let Some(f) = self.node(&path).and_then(|n| n.on_hover_change.clone()) else { return false };
                f(handler_state(&mut self.state), inside, &mut one);
                true
            }
            Action::Press(path, button) => {
                let Some(f) = self.node(&path).and_then(|n| n.on_press.clone()) else { return false };
                self.events.push(format!("press {path}"));
                one.button = button;
                f(handler_state(&mut self.state), &mut one);
                true
            }
            Action::DoublePress(path) => {
                let Some(f) = self.node(&path).and_then(|n| n.on_double_press.clone()) else { return false };
                self.events.push(format!("double press {path}"));
                f(handler_state(&mut self.state), &mut one);
                true
            }
            Action::Link(path, k) => self.press_link(&path, k, &mut one),
            Action::SecondaryPress(path, point) => {
                let Some(n) = self.node(&path).filter(|n| !self.is_exiting(n.id)) else { return false };
                let Some(f) = n.on_secondary_press.clone() else { return false };
                self.events.push(format!("secondary press {path}"));
                f(handler_state(&mut self.state), point, &mut one);
                true
            }
            Action::Drag(path, mut d, button) => {
                let Some(f) = self.node(&path).and_then(|n| n.on_drag.clone()) else { return false };
                if d.phase == Phase::Move {
                    one.instant = true;
                }
                one.button = button;
                d.data = if d.phase == Phase::Down { 0.0 } else { self.drag_data };
                f(handler_state(&mut self.state), &mut d, &mut one);
                self.drag_data = d.data;
                if d.phase != Phase::Move {
                    self.events.push(format!("drag {:?} {path}", d.phase));
                }
                true
            }
            Action::DragStart(path) => {
                let Some(f) = self.node(&path).and_then(|n| n.on_drag_start.clone()) else { return false };
                f(handler_state(&mut self.state), &mut one);
                true
            }
            Action::DragEnd(path, outcome) => {
                self.events.push(format!("drag-end {outcome:?} {path}"));
                let Some(f) = self.node(&path).and_then(|n| n.on_drag_end.clone()) else { return false };
                f(handler_state(&mut self.state), outcome, &mut one);
                true
            }
            Action::Files(event) => self.run_files(event, &mut one),
            Action::Key { ev, repeat } => {
                if self.held_gate(None, Some(&ev.key), repeat) { return false; }
                if ! matches!(ev.key, crate::Key::Modifier { .. }) { self.note_key_down(repeat); }
                self.route_key(&ev, repeat, &mut one)
            }
            Action::KeyUp(ev) => self.route_key_up(&ev, &mut one),
            Action::Wheel(gesture, chain, ev) => self.run_wheel(gesture, chain, ev, &mut one),
            Action::Gesture(chain, gesture) => self.run_gesture(chain, gesture, &mut one),
            Action::Ime(ev) => {
                let active = self.composing() || matches!(&ev, Ime::Preedit(s, _) | Ime::Commit(s) if !s.is_empty());
                let ran = self.with_focused(|e| e.ime(&ev), &mut one);
                if ran && active { self.ime_eat_key_up = true; }
                ran
            }
            Action::Paste(content, target) => {
                if target.is_some_and(|target| target != self.paste_target()) { return false; }
                let hook = self.focused_input().and_then(|i| i.paste.clone());
                let own_pills = if cfg!(not(target_arch = "wasm32")) || self.clipboard.is_some() {
                    content.has_own_pills()
                } else {
                    self.pill_copy.as_ref().is_some_and(|(t, pills)| {
                        t.as_str() == content.text()
                            && content.html().is_some_and(|h| {
                                let frag = crate::TextEdit::from_html(h);
                                frag.pills().count() == pills.len()
                                    && frag.pills().zip(pills).all(|((r1, id1), (r2, id2))| r1 == *r2 && id1 == id2)
                            })
                    })
                };
                let into_field = |state: &mut dyn Any, access: &crate::el::EditFn, tx: &mut Tx| {
                    if hook.is_some_and(|hook| hook(state, &content, tx)) { return EditOutcome::default(); }
                    let edit = access(state);
                    // A rich field keeps the paste's formatting. The HTML was sanitised when it
                    // was read, and `from_html` keeps only its own safe subset anyway.
                    if let Some(html) = content.html() && edit.is_rich() && !edit.is_secure() {
                        return if own_pills { edit.paste_html_with_pills(html, content.text()) } else { edit.paste_html(html, content.text()) };
                    }
                    if !content.has_plain() { return EditOutcome::default(); }
                    edit.paste(content.text())
                };
                if self.with_focused_state(into_field, &mut one) {
                    self.events.push("paste field".into());
                    true
                } else if let Some(f) = self.app_paste_handler() && (f.call)(handler_state(&mut self.state), &content, &mut one) {
                    self.events.push("paste window".into());
                    true
                } else { false }
            }
            Action::ClipboardRead(id, content) => {
                let Some(i) = self.clipboard_reads.iter().position(|(read, _)| *read == id) else { return false };
                let (_, f) = self.clipboard_reads.remove(i);
                f(handler_state(&mut self.state), &content, &mut one);
                true
            }
            Action::FileDialog(id, result) => self.run_file_dialog(id, result, &mut one),
            Action::Focus(path, x, y) => {
                let Some(access) = self.node(&path).and_then(|n| n.input.as_ref()?.access.clone()) else { return false };
                let single = self.node(&path).and_then(|n| n.input.as_ref()).is_some_and(|i| i.single_line);
                let layout = self.node(&path).and_then(|n| n.text_layout.clone());
                let s = self.env.scale;
                let scroll = self.input_scroll.get(&path).copied().unwrap_or(0.0);
                self.set_focus(Some(path), false);
                let e = access(handler_state(&mut self.state));
                e.set_single_line(single);
                e.break_history_group();
                e.clear_pending();
                if let Some(l) = layout {
                    let x = x + scroll;
                    let h = (l.height() - 1e-3).max(0.0);
                    let ys = if single || x < 0.0 { (y * s).clamp(0.0, h) } else { (y * s).max(0.0) };
                    let i = e.model_index(crate::text::cursor_from_point(&l, x * s, ys).index());
                    // A click on a pill puts the caret at its nearer edge.
                    e.set_caret(i);
                }
                one.instant = true;
                true
            }
            Action::Blur => {
                if self.focused.is_none() { return false; }
                self.set_focus(None, self.keyboard_focus);
                true
            }
        };
        let live_only = std::mem::take(&mut one.live_only);
        if live_only && live_label.is_some() {
            // A handler that asks for no commit cannot change another handler's animation.
            one.instant = false;
            one.settle = false;
            one.motion = None;
            one.color_motion = None;
        }
        merge(tx, one);
        if ran && live_only && let Some(label) = live_label {
            return self.skip_view(label);
        }
        ran
    }

    /// An event whose handler called `Tx::live_only`: false, so it doesn't commit, while this
    /// frame still latches the live values it set. Debug builds check one such event a quarter
    /// second after the last commit by committing anyway (true) and comparing what it built.
    fn skip_view(&mut self, label: impl std::fmt::Display) -> bool {
        #[cfg(debug_assertions)]
        if self.live_only_check && self.clock - self.last_commit_clock >= LIVE_ONLY_CHECK {
            if self.live_check.is_none() { self.live_check = Some((label.to_string(), self.view_fingerprint())); }
            self.live_checks += 1;
            self.live_dirty = true;
            return true;
        }
        let _ = label;
        self.live_only_skips += 1;
        self.live_dirty = true;
        false
    }

    /// What the last commit built: each element's path, layout, colours and text. Live values
    /// are left out, since a commit rebuilds them from the view and the next latch sets them.
    #[cfg(debug_assertions)]
    fn view_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::hash::DefaultHasher::new();
        let floats = |h: &mut std::hash::DefaultHasher, xs: &[f32]| for x in xs { x.to_bits().hash(h) };
        for n in &self.nodes {
            // Retained exits can disappear between commits without a model change.
            if self.is_exiting(n.id) { continue; }
            n.path.hash(&mut h);
            floats(&mut h, &n.target);
            let v = &n.vis;
            if !n.live.iter().any(|b| b.property == crate::live::Property::Fill) {
                floats(&mut h, &[v.fill.r, v.fill.g, v.fill.b, v.fill.a]);
            }
            floats(&mut h, &[v.border.r, v.border.g, v.border.b, v.border.a, v.border_width, v.opacity]);
            floats(&mut h, &v.radius);
            if let Some(t) = &n.text {
                if t.live.is_none() { t.content.hash(&mut h); }
                floats(&mut h, &[t.color.r, t.color.g, t.color.b, t.color.a, t.style.size, t.style.weight]);
                for p in &t.pills {
                    (p.start, p.end, p.icon.map(|i| i.name)).hash(&mut h);
                    for c in [p.bg, p.fg].into_iter().flatten() {
                        floats(&mut h, &[c.r, c.g, c.b, c.a]);
                    }
                }
                for &(a, b, c, w) in &t.rings {
                    (a, b).hash(&mut h);
                    floats(&mut h, &[c.r, c.g, c.b, c.a, w]);
                }
            }
        }
        h.finish()
    }

    /// After the commit a live-only check forced: warn when the view built something else, as
    /// it does when the handler changed state the view reads.
    #[cfg(debug_assertions)]
    fn compare_live_check(&mut self, check: Option<(String, u64)>) {
        let Some((handler, before)) = check else { return };
        if self.view_fingerprint() == before { return; }
        let warning = format!(
            "rune: {handler} called Tx::live_only, but the view built something else after it. The handler may have changed \
             state the view reads, which stays stale until another event commits. Set only Live values in a \
             live-only handler; for a time-dependent view, disable this heuristic with check_live_only(false)."
        );
        if !self.live_only_warnings.contains(&warning) {
            eprintln!("{warning}");
            self.live_only_warnings.push(warning);
        }
    }

    /// Offer a logical key that already passed `held_gate` to the app, the focused
    /// input's handler, focus traversal and the focused editor, in that order.
    fn route_key(&mut self, ev: &KeyEvent, repeat: bool, one: &mut Tx) -> bool {
        let physical_key = one.physical_key;
        one.focused = self.focused_key();
        if matches!(ev.key, crate::Key::Modifier { .. }) { return !repeat && self.route_modifier(ev, one); }
        one.key_repeat = repeat;
        let shown = self.keyboard_focus || self.hover_focus;
        self.keyboard_focus = true;
        // With no focused element, app-owned navigation (e.g. a toolbar)
        // gets first refusal before Rune starts a new Tab traversal.
        let app_first = self.focused.is_none();
        // Selected text has the keyboard for copying, as AppKit's first responder does.
        if !repeat && self.selection_key(ev, one) { return true; }
        // Return and Escape that reach a field mid-composition belong to the input method: they
        // confirm or cancel it, and submit, blur and press nothing.
        if matches!(ev.key, crate::Key::Enter | crate::Key::Escape) && self.composing() { return true; }
        if self.element_key(ev, one) { return true; }
        one.physical_key = None;
        if self.default_button_key(ev, one) { return true; }
        one.physical_key = physical_key;
        if app_first && let Some(f) = self.on_key && Tx::took(app_call!(f, &mut self.state, ev, one), one) { return true; }
        if let Some(f) = self.focused_input().and_then(|input| input.key.clone())
            && Tx::took(f(handler_state(&mut self.state), ev, one), one)
        {
            return true;
        }
        one.physical_key = None;
        // Inside a list item or code block, Tab (and Shift+Tab in lists) edits the document by default.
        if ev.key == crate::edit::Key::Tab
            && !ev.mods.cmd
            && !ev.mods.alt
            && !ev.mods.ctrl
            && self.focused_input().is_some_and(|i| {
                !i.single_line
                    && !i.edit.is_secure()
                    && (i.edit.selection_in_list() || (!ev.mods.shift && i.edit.active_block() == crate::edit::Block::Code))
            })
        {
            let layout = self.focused_layout();
            if self.with_focused(|e| e.key(ev, layout.as_deref()), one) {
                return true;
            }
        }
        if self.focus_key(ev, shown, one) { return true; }
        let input_handles = match &ev.key {
            crate::edit::Key::Function(_) | crate::edit::Key::Tab | crate::edit::Key::PageUp | crate::edit::Key::PageDown => false,
            crate::edit::Key::Command(c) => {
                matches!(c, 'a' | 'c' | 'x' | 'z')
                    || (matches!(c, 'b' | 'i' | 'u') && self.focused_input().is_some_and(|i| i.edit.format_chord(ev).is_some()))
            }
            _ => !ev.is_command_chord() || ev.control_binding().is_some(),
        };
        if input_handles {
            let layout = self.focused_layout();
            // A rich field copies its selection's formatting too, read before a cut removes it.
            let mut html = None;
            let copies = matches!(ev.key, crate::edit::Key::Command('c' | 'x'));
            let handled = self.with_focused(|e| {
                if copies && e.is_rich() && !e.is_secure() {
                    html = Some(e.selected_html()).filter(|h| !h.is_empty());
                }
                e.key(ev, layout.as_deref())
            }, one);
            if let Some(html) = html
                && let Some(text) = one.copy.as_ref().filter(|w| !w.is_secret()).map(|w| w.content().text().to_owned())
            {
                one.copy_rich(crate::ClipboardContent::new(text).with_html(html));
            }
            if handled { return true; }
        }
        one.physical_key = physical_key;
        if !app_first && let Some(f) = self.on_key && Tx::took(app_call!(f, &mut self.state, ev, one), one) {
            self.events.push(format!("key {:?}", ev.key));
            return true;
        }
        one.physical_key = None;
        self.page_scroll(ev)
    }

    /// Page Up and Page Down scroll by most of a viewport, and Home and End to the top and
    /// bottom, as an NSScrollView does, when nothing else took the key. The scroll view is the
    /// nearest one around the focused element, else around the pointer, else the first in the
    /// tree, skipping any whose content fits (nothing to scroll): a chat's short composer passes
    /// the keys to the message list beside it. A view that overflows keeps them even at the edge
    /// a key moves toward, where it does nothing, as an NSTextView does; an app that wants
    /// another view paged there routes the key itself with `Tx::scroll_page`.
    fn page_scroll(&mut self, ev: &KeyEvent) -> bool {
        use crate::Key::*;
        if !matches!(ev.key, PageUp | PageDown | Home | End) || ev.mods.cmd || ev.mods.alt || ev.mods.ctrl { return false; }
        // A focused field in a virtual row that scrolled out has no node: page its list.
        let focused = self.focused.as_deref().and_then(|path| {
            std::iter::successors(Some(path), |p| p.rsplit_once('/').map(|(head, _)| head))
                .find_map(|p| self.nodes.iter().position(|n| n.path == p))
        });
        let around = |from: usize| std::iter::successors(Some(from), |&i| self.nodes[i].parent).find(|&i| self.pageable(i));
        let i = focused.and_then(around)
            .or_else(|| self.pointer.and_then(|(x, y)| self.scroll_at(x, y)).and_then(|id| self.node_index.get(&id).copied()).and_then(around))
            .or_else(|| (0..self.nodes.len()).find(|&i| self.pageable(i)));
        let Some(i) = i else { return false };
        if !self.page_to(i, |offset, max, page| match ev.key { PageUp => offset - page, PageDown => offset + page, Home => 0.0, _ => max }) { return false; }
        self.events.push(format!("page {:?}", ev.key));
        true
    }

    /// Whether node `i` is a live scroll view whose content overflows it.
    fn pageable(&self, i: usize) -> bool {
        self.nodes[i].scroll.is_some() && !self.subtree_exiting(i) && self.allowed(i)
            && self.scrolls.get(&self.nodes[i].id).is_some_and(|st| st.max > 0.5)
    }

    /// Moves scroll view `i` to `to(offset, max, page)`, clamped, a page being 90% of its
    /// height. The hand moved it: any fling or glide stops, and a `stick_bottom` view that
    /// lands on its end follows it again.
    fn page_to(&mut self, i: usize, to: impl FnOnce(f64, f64, f64) -> f64) -> bool {
        self.keeps.clear();
        self.scroll_reveals.clear();
        let (id, page, stick) = (self.nodes[i].id, f64::from(self.nodes[i].local.h * 0.9), self.nodes[i].scroll.is_some_and(|s| s.stick_bottom));
        let Some(st) = self.scrolls.get_mut(&id) else { return false };
        st.offset = to(st.offset, st.max, page).clamp(0.0, st.max);
        st.v = 0.0;
        st.glide = None;
        st.direct = true;
        st.pinned = stick && st.offset >= st.max - 0.5;
        true
    }

    fn focused_layout(&mut self) -> Option<Rc<Layout<Ink>>> {
        let path = self.focused.clone()?;
        let s = self.env.scale;
        let n = self.node(&path)?;
        let (mut spec, w) = (n.text.clone()?, n.target[2] - n.target[0] - n.inset[0] - n.inset[2]);
        let text = n.input.as_ref()?.edit.display().0;
        use zeroize::Zeroize;
        spec.content.zeroize();
        spec.content = text;
        Some(self.text.layout_for_spec(&spec, s, spec.wrap.then_some(w * s), line_alignment(&spec)))
    }

    fn with_focused(&mut self, f: impl FnOnce(&mut crate::edit::TextEdit) -> EditOutcome, tx: &mut Tx) -> bool {
        self.with_focused_state(|state, access, _| f(access(state)), tx)
    }

    /// [`Self::with_focused`] for an edit that needs the app state (an app's paste hook).
    fn with_focused_state(&mut self, f: impl FnOnce(&mut dyn Any, &crate::el::EditFn, &mut Tx) -> EditOutcome, tx: &mut Tx) -> bool {
        let Some(path) = self.focused.clone() else { return false };
        let Some((access, submit, change, animate_layout, single_line)) =
            self.focused_input().map(|i| (i.access.clone(), i.submit.clone(), i.change.clone(), i.animate_layout, i.single_line))
        else {
            return false;
        };
        let Some(access) = access else { return false };
        access(handler_state(&mut self.state)).set_single_line(single_line);
        let before = change.as_ref().map(|_| zeroize::Zeroizing::new(access(handler_state(&mut self.state)).text.clone()));
        let mut out = f(handler_state(&mut self.state), &access, tx);
        // Submit and Escape's blur act once per press; held keys still type, move and delete.
        if tx.key_repeat { out.submit = false; out.blur = false; }
        // Typing is the user's own hand: no animation.
        tx.instant = !animate_layout;
        self.caret_follow = true;
        if let Some(change) = change
            && before.as_ref().map(|s| s.as_str()) != Some(access(handler_state(&mut self.state)).text.as_str())
        { change(handler_state(&mut self.state), tx); }
        if let Some(c) = out.copy {
            tx.copy(c);
        }
        if out.blur {
            self.set_focus(None, self.keyboard_focus);
        }
        if out.submit
            && let Some(sub) = submit
        {
            self.events.push(format!("submit {path}"));
            tx.instant = false;
            sub(handler_state(&mut self.state), tx);
        } else if out.submit {
            // A field with nothing to submit passes Return to the default button, as in a sheet.
            self.press_default_button(tx);
        }
        true
    }

    /// Whether the focused field has an input method composition (preedit) open.
    fn composing(&mut self) -> bool {
        let Some(access) = self.focused_input().map(|i| i.access.clone()) else { return false };
        match access {
            Some(access) => access(handler_state(&mut self.state)).preedit.is_some(),
            None => self.focused_input().is_some_and(|i| i.edit.preedit.is_some()),
        }
    }

    fn node(&self, path: &str) -> Option<&Node> {
        self.nodes.iter().enumerate().find(|(i, n)| n.path == path && !self.subtree_exiting(*i)).map(|(_, n)| n)
    }

    /// `El::on_key` handlers on the focused element and its ancestors, innermost first. A
    /// focused text field's keys stay its own.
    fn element_key(&mut self, ev: &KeyEvent, tx: &mut Tx) -> bool {
        let Some(i) = self.focused_index() else { return false };
        if !self.can_focus(i) || self.focused_input().is_some() { return false; }
        let handlers: Vec<_> = std::iter::successors(Some(i), |&j| self.nodes[j].parent)
            .take_while(|&j| self.allowed(j) && !self.subtree_exiting(j))
            .filter_map(|j| self.nodes[j].focus.key.clone())
            .collect();
        handlers.into_iter().any(|f| Tx::took(f(handler_state(&mut self.state), ev, tx), tx))
    }

    fn focused_input(&self) -> Option<&InputSpec> {
        let path = self.focused.as_deref()?;
        self.node(path).and_then(|n| n.input.as_deref()).or_else(|| self.virtual_inputs.get(path).map(Box::as_ref))
    }

    /// What [`Tx::focused`] reports: the focused link's id, else the focused element's key,
    /// else its path's last segment (a field in a virtual row that scrolled out has no node).
    fn focused_key(&self) -> Option<String> {
        let path = self.focused.as_deref()?;
        if let Some(id) = self.focused_index().and_then(|i| self.focused_link_id(i)) { return Some(id); }
        Some(self.focused_index().and_then(|i| self.nodes[i].key.clone())
            .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(path).to_owned()))
    }

    /// The focused element's index. Unlike [`Runtime::node`], this finds it while it exits.
    fn focused_index(&self) -> Option<usize> {
        let path = self.focused.as_deref()?;
        self.nodes.iter().position(|n| n.path == path)
    }

    /// Node `i`'s edges as drawn this frame (points, x0 y0 x1 y1), or its layout box before
    /// it draws.
    fn drawn_edges(&self, i: usize) -> [f32; 4] {
        self.drawn.get(i).map_or(self.nodes[i].target, |d| [d[0], d[1], d[0] + d[2], d[1] + d[3]])
    }

    /// Window point (x, y) as node `i`'s handlers see it.
    fn pointer_event(&self, i: usize, x: f32, y: f32) -> PointerEvent {
        let [lx, ly] = self.to_local(i, x, y);
        PointerEvent { x: lx, y: ly, window_x: x, window_y: y }
    }

    /// Window point (x, y) in node `i`'s own layout points: from its top-left as drawn, divided
    /// by the scale it draws at, so it follows scrolling, `live_offset`, `live_scale`,
    /// `El::scale` and `El::rotate`. From its layout box before it draws.
    pub(crate) fn to_local(&self, i: usize, x: f32, y: f32) -> [f32; 2] {
        let Some(d) = self.drawn.get(i) else {
            let t = self.nodes[i].target;
            return [x - t[0], y - t[1]];
        };
        let k = self.drawn_scale.get(i).copied().unwrap_or(1.0).max(1e-4);
        if self.turn_scratch.is_empty() {
            return [(x - d[0]) / k, (y - d[1]) / k];
        }
        let (b, ux, uy) = self.unturned_box_and_point(i, x, y);
        [(ux - b[0]) / k, (uy - b[1]) / k]
    }

    /// Node `i`'s unturned border box (points, x0 y0 x1 y1) and window point (x, y) unturned into
    /// that layer's points.
    pub(super) fn unturned_box_and_point(&self, i: usize, x: f32, y: f32) -> ([f32; 4], f32, f32) {
        if let Some(t) = self.inh_scratch.get(i).and_then(|h| h.turn.checked_sub(1)).and_then(|k| self.turn_scratch.get(k as usize))
            && let Some(&(b, _)) = self.unturned_drawn.get(i)
        {
            let s = self.env.scale;
            let [ux, uy] = t.unturn([x * s, y * s]).map(|v| v / s);
            return (b, ux, uy);
        }
        (self.drawn_box(i), x, y)
    }

    #[inline]
    pub(super) fn hit_contains(&self, id: NodeId, r: [f32; 4], x: f32, y: f32) -> bool {
        if self.turn_scratch.is_empty() {
            return contains(r, x, y);
        }
        self.hit_contains_turned(id, r, x, y)
    }

    #[cold]
    #[inline(never)]
    fn hit_contains_turned(&self, id: NodeId, r: [f32; 4], x: f32, y: f32) -> bool {
        let Some(&i) = self.node_index.get(&id) else { return contains(r, x, y) };
        let Some(&t) = self.inh_scratch.get(i).and_then(|h| h.turn.checked_sub(1)).and_then(|k| self.turn_scratch.get(k as usize)) else {
            return contains(r, x, y);
        };
        let s = self.env.scale;
        if !contains(t.outer_clip.map(|v| v / s), x, y) {
            return false;
        }
        let [ux, uy] = t.unturn([x * s, y * s]).map(|v| v / s);
        contains(r, ux, uy)
    }

    pub(super) fn hit_screen_rect(&self, id: NodeId, r: [f32; 4]) -> [f32; 4] {
        if self.turn_scratch.is_empty() {
            return r;
        }
        let Some(&i) = self.node_index.get(&id) else { return r };
        let Some(&t) = self.inh_scratch.get(i).and_then(|h| h.turn.checked_sub(1)).and_then(|k| self.turn_scratch.get(k as usize)) else {
            return r;
        };
        let s = self.env.scale;
        let b = t.bbox(r.map(|v| v * s)).map(|v| v / s);
        let c = t.outer_clip.map(|v| v / s);
        [b[0].max(c[0]), b[1].max(c[1]), b[2].min(c[2]), b[3].min(c[3])]
    }

    fn node_contains(&self, i: usize, x: f32, y: f32) -> bool {
        if !self.turn_scratch.is_empty()
            && let Some(turn_k) = self.inh_scratch.get(i).map(|h| h.turn).filter(|&k| k != 0)
            && let Some(&t) = self.turn_scratch.get(turn_k as usize - 1)
            && let Some(&(b, _)) = self.unturned_drawn.get(i)
        {
            let s = self.env.scale;
            if !contains(t.outer_clip.map(|v| v / s), x, y) {
                return false;
            }
            let lc = self.nodes[i].parent
                .filter(|&p| self.inh_scratch.get(p).is_some_and(|h| h.turn == turn_k))
                .and_then(|p| self.unturned_drawn.get(p))
                .map_or(NO_CLIP, |u| u.1)
                .map(|v| v / s);
            let shown = [b[0].max(lc[0]), b[1].max(lc[1]), b[2].min(lc[2]), b[3].min(lc[3])];
            let [ux, uy] = t.unturn([x * s, y * s]).map(|v| v / s);
            return contains(shown, ux, uy);
        }
        let r = self.drawn_edges(i);
        let c = self.nodes[i].parent.and_then(|p| self.inh_scratch.get(p)).map_or(NO_CLIP, |h| h.clip).map(|v| v / self.env.scale);
        let shown = [r[0].max(c[0]), r[1].max(c[1]), r[2].min(c[2]), r[3].min(c[3])];
        contains(shown, x, y)
    }

    fn screen_clip_of_parent(&self, parent: Option<usize>) -> [f32; 4] {
        screen_clip_of(parent, &self.inh_scratch, &self.turn_scratch)
    }

    fn subtree_exiting(&self, mut i: usize) -> bool {
        loop {
            if self.is_exiting(self.nodes[i].id) { return true; }
            let Some(parent) = self.nodes[i].parent else { return false };
            i = parent;
        }
    }

    fn is_exiting(&self, id: NodeId) -> bool {
        matches!(self.scene.phase(id), Some(PresencePhase::Exiting { .. }))
    }

    /// Shared-element motion (`El::matched`): each of the last commit's matched elements that
    /// left the tree hands its geometry, radius, fill and opacity to the element with its id that
    /// just mounted. Costs nothing without matched elements; else O(matched), plus one pass over
    /// the old nodes when a pair matches.
    fn match_elements(&mut self, nodes: &[Node], arrivals: Vec<(Rc<str>, usize, bool)>, tx: &Tx, instant: bool) {
        let mut by_id: HashMap<Rc<str>, (usize, bool)> = HashMap::with_capacity(arrivals.len());
        for (key, i, arriving) in arrivals {
            if by_id.insert(key.clone(), (i, arriving)).is_some() { self.warn_duplicate_match(&key); }
        }
        let mut current: Vec<(Rc<str>, NodeId)> = by_id.iter().map(|(key, &(i, _))| (key.clone(), nodes[i].id)).collect();
        current.sort_unstable_by_key(|(key, _)| by_id[key].0);
        let old = std::mem::replace(&mut self.matched, current);
        // Under Reduce Motion the old one exits and the new one enters, each by its own
        // transition (a crossfade in place) and with the parents it leaves or enters with.
        if instant || tx.settle || self.reduced_motion { return; }
        let mut old_index: Option<HashMap<NodeId, usize>> = None;
        // Descendants first: matching an element unmounts its old subtree, matched ones included.
        for (key, from) in old.into_iter().rev() {
            let Some(&(i, true)) = by_id.get(&key) else { continue };
            let to = nodes[i].id;
            if from == to || self.node_index.contains_key(&from) || !self.scene.contains(from) || self.is_exiting(from) { continue; }
            let index = old_index.get_or_insert_with(|| self.nodes.iter().enumerate().map(|(k, n)| (n.id, k)).collect());
            let Some(&from_i) = index.get(&from) else { continue };
            // Scroll offsets live outside the scene: start where `from` was seen on screen.
            let mut offset = rune_motion::Vec2::ZERO;
            if self.built {
                // Flights drop the cover offsets of flights around them.
                let shift = |k: usize| self.inh_scratch.get(k).map_or([0.0; 2], |h| [h.dx - h.cover[0], h.dy - h.cover[1]]);
                let source = self.nodes[from_i].parent.map_or([0.0; 2], shift);
                let row = self.row_dy.get(from_i).copied().unwrap_or(0.0);
                let dest = std::iter::successors(nodes[i].parent, |&p| nodes[p].parent)
                    .find_map(|p| index.get(&nodes[p].id).copied()).map_or([0.0; 2], shift);
                offset = rune_motion::Vec2::new(source[0] - dest[0], source[1] + row - dest[1]);
            }
            let spring = tx.motion.or(nodes[i].matched.as_ref().and_then(|m| m.1)).unwrap_or(motion::REFLOW);
            let fill = self.color(from, P_FILL, self.nodes[from_i].vis.fill);
            let opacity = self.scene.get(from, prop::OPACITY);
            self.scene.match_geometry_offset(from, to, offset, LayoutAnim::new(spring).screen());
            let live = |p: crate::live::Property| nodes[i].live.iter().any(|b| b.property == p);
            if fill != nodes[i].vis.fill && !live(crate::live::Property::Fill) {
                self.scene.set(to, P_FILL, fill);
                self.scene.animate(to, P_FILL, nodes[i].vis.fill, spring);
            }
            if let Some(o) = opacity.filter(|&o| o != nodes[i].vis.opacity) && !live(crate::live::Property::Opacity) {
                self.scene.set(to, prop::OPACITY, o);
                self.scene.animate(to, prop::OPACITY, nodes[i].vis.opacity, spring);
            }
            self.flying.retain(|f| f.0 != to);
            self.flying.push((to, nodes[i].local));
        }
    }

    fn warn_duplicate_match(&mut self, key: &Rc<str>) {
        let message = || format!("duplicate matched id {key:?}: the last element in tree order is matched; give each element its own id");
        if let Some(warnings) = self.key_warnings.as_mut() { warnings.push(message()); }
        #[cfg(debug_assertions)]
        if self.matched_warned.insert(key.clone()) { eprintln!("rune: {}", message()); }
    }

    fn set_or_anim<T: rune_motion::Animatable>(&mut self, id: NodeId, p: Prop<T>, val: T, s: Spring, instant: bool) {
        if instant {
            self.scene.set(id, p, val);
        } else {
            self.scene.animate(id, p, val, s);
        }
    }

    /// Per node of the new tree `nodes`: neither it nor anything under it drew in the last
    /// build, and none of its swept path lands in sight in this layout (in the window, inside
    /// its clips). Empty when nothing qualifies.
    fn unseen_before_and_after(&self, nodes: &[Node]) -> Vec<bool> {
        if !self.built || self.seen.len() != self.nodes.len() { return Vec::new(); }
        let mut seen = self.seen.clone();
        mark_ancestors(&self.nodes, &mut seen);
        let before: HashSet<NodeId> = self.nodes.iter().zip(&seen).filter(|(_, s)| !**s).map(|(n, _)| n.id).collect();
        if before.is_empty() { return Vec::new(); }
        let old_drawn: HashMap<NodeId, [f32; 4]> = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, old)| old.target[2] > old.target[0] || old.target[3] > old.target[1])
            .map(|(old_i, old)| (old.id, self.drawn_edges(old_i)))
            .collect();
        // Room for shadows and blur, which draw past a node's rect.
        const MARGIN: f32 = 32.0;
        let window = [0.0, 0.0, self.viewport.0, self.viewport.1];
        // Per node: the swept clip and scroll-offset range its children are under, in points.
        let mut inner = vec![(window, [0.0f32; 2], [0.0f32; 2]); nodes.len()];
        let mut after = vec![false; nodes.len()];
        for (i, n) in nodes.iter().enumerate() {
            let (clip, shift_min, shift_max) = match n.parent {
                Some(p) if n.above.is_none() && p < i => inner[p],
                Some(_) if n.above.is_none() => { after[i] = true; (window, [0.0; 2], [0.0; 2]) }
                _ => (window, [0.0; 2], [0.0; 2]),
            };
            let t = n.target;
            let r = [t[0] - shift_max[0], t[1] - shift_max[1], t[2] - shift_min[0], t[3] - shift_min[1]];
            let swept = match old_drawn.get(&n.id) {
                Some(&b) => [b[0].min(r[0]), b[1].min(r[1]), b[2].max(r[2]), b[3].max(r[3])],
                None => r,
            };
            after[i] |= swept[2] >= clip[0] - MARGIN && swept[0] <= clip[2] + MARGIN
                && swept[3] >= clip[1] - MARGIN && swept[1] <= clip[3] + MARGIN;
            let clip = if n.vis.clip.is_some() || n.scroll.is_some() {
                [clip[0].max(swept[0]), clip[1].max(swept[1]), clip[2].min(swept[2]), clip[3].min(swept[3])]
            } else { clip };
            let (shift_min, shift_max) = match (self.scrolls.get(&n.id), n.scroll) {
                (Some(st), Some(sp)) => {
                    let content = (i + 1..nodes.len()).find(|&j| nodes[j].parent == Some(i));
                    let max_x = if n.scroll_axes.x() {
                        content.map_or(0.0, |c| (nodes[c].target[2] - n.target[2]).max(0.0))
                    } else { 0.0 };
                    let max_y = if n.scroll_axes.y() {
                        content.map_or(0.0, |c| (nodes[c].target[3] - n.target[3]).max(0.0))
                    } else { 0.0 };
                    let ox0 = st.x.offset as f32;
                    let ox1 = ox0.clamp(0.0, max_x);
                    let oy0 = st.offset as f32 + st.slack.height();
                    let oy1 = if st.pinned && sp.stick_bottom { max_y + st.slack.height() } else { oy0.clamp(0.0, max_y + st.slack.height()) };
                    (
                        [shift_min[0] + ox0.min(ox1), shift_min[1] + oy0.min(oy1)],
                        [shift_max[0] + ox0.max(ox1), shift_max[1] + oy0.max(oy1)],
                    )
                }
                _ => (shift_min, shift_max),
            };
            inner[i] = (clip, shift_min, shift_max);
        }
        mark_ancestors(nodes, &mut after);
        nodes.iter().zip(&after).map(|(n, a)| !a && before.contains(&n.id)).collect()
    }

    fn color(&self, id: NodeId, prop: Prop<Color>, rest: Color) -> Color {
        if self.scene.store().is_animating(id, prop) { self.scene.get(id, prop).unwrap_or(rest) } else { rest }
    }

    /// Shadow `k` of node `id` with its geometry where its spring has it.
    fn shadow_at(&self, id: NodeId, k: usize, sh: &crate::el::Shadow) -> crate::el::Shadow {
        match P_SHADOW_GEO.get(k) {
            Some(&prop) if self.scene.store().active_len() > 0 && self.scene.store().is_animating(id, prop) => match self.scene.get(id, prop) {
                Some(g) => crate::el::Shadow { x: g.x, y: g.y, blur: g.w.max(0.0), spread: g.h, ..*sh },
                None => *sh,
            },
            _ => *sh,
        }
    }

    // ---------------------------------------------------------------------------------------
    // Commit: view → nodes → layout → Scene retarget

    fn virtual_window_changed(&self) -> bool {
        self.virtuals.iter().any(|(path, v)| self.wants_window(path, v))
    }

    fn report_visible_rows(&mut self) -> bool {
        if self.virtuals.is_empty() { return false; }
        let mut queued = false;
        for i in 0..self.nodes.len() {
            if self.nodes[i].on_visible_rows.is_none() || self.subtree_exiting(i) { continue; }
            let path = &self.nodes[i].path;
            let Some(v) = self.virtuals.get_mut(path) else { continue };
            let (offset, pinned) = self.path_ids.get(&v.scroll_path)
                .and_then(|id| self.scrolls.get(id))
                .map_or((0.0, false), |s| (s.offset as f32, s.pinned));
            let range = v.visible_range(offset, pinned);
            if v.reported_visible.as_ref() != Some(&range) {
                v.reported_visible = Some(range.clone());
                let p = path.clone();
                self.queue.retain(|a| !matches!(a, Action::VisibleRows(q, _) if *q == p));
                self.queue.push(Action::VisibleRows(p, range));
                queued = true;
            }
        }
        queued
    }

    fn wants_window(&self, path: &str, v: &crate::virtual_list::VirtualList) -> bool {
        let Some(&i) = self.path_ids.get(path).and_then(|id| self.node_index.get(id)) else { return false };
        if self.subtree_exiting(i) { return false; }
        self.path_ids.get(&v.scroll_path).and_then(|id| self.scrolls.get(id))
            .is_some_and(|s| v.needs_window(s.offset as f32, s.pinned))
    }

    /// Whether an index-built list still mounts its overscan a little per frame, so the host
    /// should draw another. Each such frame commits more rows; a few frames fill it.
    fn virtual_filling(&mut self) -> bool {
        let filling = self.virtuals.iter().any(|(path, v)| v.indexed && self.wants_window(path, v));
        self.virtual_fill = if filling { self.virtual_fill + 1 } else { 0 };
        filling && self.virtual_fill <= VIRTUAL_FILL_FRAMES
    }
}

/// Measurement capabilities: text/layout caches and virtual geometry only. In particular this
/// type has no Scene, growth tracks, reveal tracks, draw instances, or mutable app state.
struct Measurement<'a, S> {
    state: &'a S,
    view: fn(&S, &mut Cx<S>) -> El,
    env: Env,
    viewport: (f32, f32),
    fit_height: bool,
    system_reduced_motion: bool,
    titlebar_inset: f32,
    system_accent: crate::SystemAccent,
    reduced_motion: bool,
    time_scale: f32,
    commits: u64,
    nodes: &'a [Node],
    text: &'a mut TextSystem,
    layout: &'a mut TaffyTree<usize>,
    layout_entries: &'a mut HashMap<String, LayoutEntry>,
    node_index: &'a mut HashMap<NodeId, usize>,
    path_ids: &'a mut HashMap<String, NodeId>,
    memo: &'a mut crate::memo::MemoCache,
    next_id: &'a mut u64,
    scrolls: &'a mut HashMap<NodeId, ScrollState>,
    virtuals: &'a mut HashMap<String, crate::virtual_list::VirtualList>,
    virtual_culled: &'a mut HashSet<String>,
    virtual_inputs: &'a mut HashMap<String, Box<InputSpec>>,
    stats: &'a mut FrameStats,
    key_warnings: &'a mut Option<Vec<String>>,
    /// The focused element's path, so an index-built row holding it is still built.
    focused: Option<&'a str>,
    /// True when `self.state` was not mutated since the previous commit.
    state_unchanged: bool,
    /// Whether this pass measures again because estimates left part of a viewport blank.
    retry: bool,
    caret: Option<(String, [f32; 4])>,
    caret_read: bool,
    layout_seen: layout_read::LayoutSeen,
    layout_reads: Vec<String>,
}

/// Owned result of completed measurement. Scene mutation consumes this once; retries never
/// return it. Keeping this separate from El makes unmeasured views invalid commit inputs.
struct Measured {
    layout: TaffyTree<usize>,
    nodes: Vec<Node>,
    reduced_motion: bool,
    time_scale: f32,
    caret_read: bool,
    layout_reads: Vec<String>,
}

fn collect_virtual_inputs(el: &El, parent: &str, index: usize, inert: bool, inputs: &mut HashMap<String, Box<InputSpec>>) {
    // Most offscreen rows hold no focused field; find that out before building any paths.
    if !crate::memo::has_focused_input(el) {
        return;
    }
    let inert = inert || el.inert;
    let input = el.input.as_ref().filter(|i| i.edit.focused && i.access.is_some());
    // A memo's shared children are skipped whole when none is a focused field.
    let skip = el.memo_children.as_ref().filter(|m| !m.focused_input).map_or(0, |m| m.children.len());
    if input.is_none() && el.children.is_empty() && skip == el.memo_children.as_ref().map_or(0, |m| m.children.len()) {
        return;
    }
    let segment = el.key.clone().unwrap_or_else(|| format!("#{index}"));
    let path = format!("{parent}/{segment}");
    if let Some(input) = input {
        let mut input = input.clone();
        if inert { input.edit.focused = false; }
        inputs.insert(path.clone(), input);
    }
    for (i, child) in el.all_children().enumerate().skip(skip) {
        collect_virtual_inputs(child, &path, i, inert, inputs);
    }
}

impl<S: 'static> Measurement<'_, S> {
    fn finish(mut self, tx: &Tx) -> Measured {
        let mut passes = 0;
        let (layout, nodes) = loop {
            passes += 1;
            self.retry = passes > 1;
            self.virtual_culled.clear();
            self.virtual_inputs.clear();
            // Degenerate zero-height runs fall back to a full measurement instead of
            // an unbounded retry loop. Normal transcripts need one pass.
            let (layout, nodes) = self.measure_view(tx, passes > 8);
            if !self.measure_virtuals(&nodes, tx.settle) { break (layout, nodes); }
            *self.layout = layout;
        };
        // Exiting virtual subtrees still use their origin and measured extent while
        // fading. Runtime prunes this metadata with the retained scene nodes.
        Measured { layout, nodes, reduced_motion: self.reduced_motion, time_scale: self.time_scale, caret_read: self.caret_read, layout_reads: self.layout_reads }
    }

    fn node_id(&mut self, path: &str) -> NodeId {
        if let Some(&id) = self.path_ids.get(path) { return id; }
        let id = NodeId(*self.next_id);
        *self.next_id += 1;
        self.path_ids.insert(path.to_owned(), id);
        id
    }

    /// `bound` is the most `el` can be tall (see [`crate::virtual_list::height_bound`]); `scroll`
    /// is the scroll viewport `el` is the content of, whether it sticks to the bottom, and its bound.
    #[allow(clippy::too_many_arguments)]
    fn window_tree(&mut self, el: &mut El, parent: &str, index: usize, scroll: Option<(&str, bool, f32)>, settle: bool, force_all: bool, bound: f32, inert: bool) {
        el.expand_memo();
        let inert = inert || el.inert;
        let bound = crate::virtual_list::height_bound(&el.style, bound);
        let path = node_path(parent, el.key.as_deref(), index);
        if let Some(estimate) = el.virtual_estimate {
            assert!(el.growth.is_some(), "`.virtualize(..)` at {path:?} needs `.growth(GrowthSpec)` on the same element");
            let (scroll_path, sticky, scroll_bound) = scroll.expect("virtual stack must be the direct child of a scroll viewport");
            let old_scroll = self.path_ids.get(scroll_path).and_then(|id| self.scrolls.get(id));
            let pinned = (settle && sticky) || old_scroll.map_or(sticky, |s| s.pinned);
            let mut offset = old_scroll.map_or(0.0, |s| s.offset as f32);
            // A new viewport has no height yet: its bound from the tree, else the window's.
            let first = if scroll_bound.is_finite() { scroll_bound } else { self.viewport.1 };
            let height = self.nodes.iter().find(|n| n.path == scroll_path).map_or(first, |n| n.local.h);
            let padding = |p: LengthPercentage| match p.expand() {
                taffy::style::ExpandedLengthPercentage::Length(x) => x,
                _ => panic!("virtual stack padding must be in points"),
            };
            let v = self.virtuals.entry(path.clone()).or_default();
            v.set_estimate(estimate);
            v.scroll_path = scroll_path.to_owned();
            // A retry keeps the viewport the last pass measured; `self.nodes` is the last commit's.
            if !self.retry { v.viewport = height; }
            v.pad_top = padding(el.style.padding.top);
            v.pad_bottom = padding(el.style.padding.bottom);
            v.set_gap(crate::virtual_list::gap_y(&el.style));
            let source = el.virtual_rows.take();
            let state = handler_state_ref(self.state);
            if source.is_some() {
                assert!(el.children.is_empty(), "`.virtual_rows(..)` at {path:?} builds its own rows; it takes no children");
            }
            v.indexed = source.is_some();
            let version = el.virtual_rows_version.filter(|_| source.is_some());
            let same = match &source {
                Some(rows) => rows.count == v.keys.len()
                    && (self.state_unchanged || (version.is_some() && version == v.keys_version) || (0..rows.count).all(|i| (rows.key)(state, i) == v.keys[i])),
                None => v.same_keys(el.children.iter().map(|c| c.key.as_deref())),
            };
            v.keys_version = version;
            // Most commits keep the same rows; only a change pays for sets of every key.
            let anchor = if same {
                if pinned { None } else { v.first_visible(offset, |_| true) }
            } else {
                let mut keys: Vec<_> = match &source {
                    Some(rows) => (0..rows.count).map(|i| (rows.key)(state, i).into_owned()).collect(),
                    None => el.children.iter().map(|c| c.key.clone().expect("virtual rows require stable keys")).collect(),
                };
                // Rows sharing a key would share a height and a node: rename the later ones, as
                // the view's siblings are, so the list still lays out.
                let renamed = crate::key_diagnostics::rename_duplicates(keys.iter_mut());
                if !renamed.is_empty() {
                    let duplicates: Vec<_> = renamed.iter().map(|(_, key)| {
                        let key = key.clone();
                        let message = format!("duplicate virtual row key {key:?} under {path:?}; give rows unique keys");
                        crate::key_diagnostics::Duplicate { key, message }
                    }).collect();
                    if source.is_none() {
                        for (i, _) in &renamed { el.children[*i].key = Some(keys[*i].clone()); }
                    }
                    match self.key_warnings.as_mut() {
                        Some(out) => out.extend(duplicates.into_iter().map(|d| d.message)),
                        None => crate::key_diagnostics::warn_once(&duplicates),
                    }
                }
                // Offset-driven reorder already keeps each row where it was drawn. Anchoring
                // to the formerly first row as it changes slots would scroll the entire list
                // a second time. Inserts/removals still preserve the reader's anchor.
                let reorder = keys.len() == v.keys.len() && keys.iter().all(|key| v.has(key))
                    && self.nodes.iter().any(|n| n.vis.offset != [0.0; 2]
                        && n.parent.is_some_and(|p| self.nodes[p].path == path));
                v.set_keys(keys, self.commits, (!pinned && !reorder).then_some(offset))
            };
            v.anchor = anchor;
            let delta = v.anchor_delta();
            offset = (offset + delta).max(0.0);
            if let Some(st) = self.path_ids.get(scroll_path).and_then(|id| self.scrolls.get_mut(id)) {
                if !pinned { st.offset = (st.offset + f64::from(delta)).max(0.0); }
            }
            // Rows new since the last commit spread over frames, unless this pass is a retry to
            // fill the viewport or a settle. A list's first showing is all new rows: it mounts
            // its viewport and a budget of overscan, and the rest fills over the next frames.
            let budget = v.indexed && !self.retry && !settle;
            let shown = std::mem::take(&mut v.shown);
            v.select(offset, pinned, budget.then_some(&shown));
            if force_all { v.range = 0..v.keys.len(); v.before = 0.0; v.after = 0.0; }
            let mounted: HashSet<&str> = v.keys[v.range.clone()].iter().map(String::as_str).collect();
            for key in shown.iter().filter(|k| !mounted.contains(k.as_str()) && v.has(k)) {
                self.virtual_culled.insert(format!("{path}/{key}"));
            }
            v.shown = shown;
            el.style.padding.top = length(v.pad_top + v.before);
            el.style.padding.bottom = length(v.pad_bottom + v.after);
            if let Some(rows) = source {
                let key_warnings = &mut *self.key_warnings;
                let mut build = |i: usize| {
                    let key = &v.keys[i];
                    let mut row = (rows.build)(state, i).key(key.clone());
                    // The view's key repair ran before these rows existed.
                    if crate::key_diagnostics::clean(&row) { return row; }
                    let duplicates = crate::key_diagnostics::repair_at(&mut row, &format!("{path}/{key}"));
                    match key_warnings.as_mut() {
                        Some(out) => out.extend(duplicates.into_iter().map(|d| d.message)),
                        None => crate::key_diagnostics::warn_once(&duplicates),
                    }
                    row
                };
                // A focused field's row is built, unmounted, so its typing still lands.
                let under = |f: &str| f.strip_prefix(path.as_str()).is_some_and(|rest| rest.starts_with('/'));
                let focused = v.focused_row.as_deref().filter(|_| self.focused.is_some_and(under))
                    .and_then(|key| v.index_of(key)).filter(|i| !v.range.contains(i));
                if let Some(i) = focused {
                    collect_virtual_inputs(&build(i), &path, i, inert, self.virtual_inputs);
                }
                el.children = v.range.clone().map(|i| {
                    let mut row = build(i);
                    row.virtual_mount = v.born(i) < self.commits;
                    row
                }).collect();
            } else {
                if !self.retry && el.focus.reveal_selected {
                    let selected = el.children.iter().find(|c| c.focus.selected).and_then(|c| c.key.clone());
                    if selected != v.selected_row {
                        v.reveal_selected = selected.clone();
                        v.selected_row = selected;
                    }
                }
                el.children = std::mem::take(&mut el.children).into_iter().enumerate().filter_map(|(i, mut child)| {
                    if !v.range.contains(&i) {
                        collect_virtual_inputs(&child, &path, i, inert, self.virtual_inputs);
                        return None;
                    }
                    child.virtual_mount = v.born(i) < self.commits;
                    Some(child)
                }).collect();
            }
        }
        let bounds = crate::virtual_list::child_bounds(&el.style, el.scroll.is_some(), bound, &el.children);
        let scroll = el.scroll.map(|s| (path.as_str(), s.stick_bottom, bound));
        for (i, child) in el.children.iter_mut().enumerate() {
            let bound = bounds.of(child);
            self.window_tree(child, &path, i, scroll, settle, force_all, bound, inert);
        }
    }

    /// Finishes all row measurements before scene/growth mutation. Newly measured estimates
    /// above the viewport adjust the scroll offset by exactly the same amount.
    fn measure_virtuals(&mut self, nodes: &[Node], settle: bool) -> bool {
        let mut retry = false;
        for (path, v) in self.virtuals.iter_mut() {
            let Some((ci, _)) = nodes.iter().enumerate().find(|(_, n)| n.path == *path) else { continue };
            for n in nodes.iter().filter(|n| n.parent == Some(ci)) {
                v.set_height(n.key.as_deref().unwrap(), n.local.h);
            }
            let Some(scroll) = nodes.iter().find(|n| n.path == v.scroll_path) else { continue };
            v.viewport = scroll.local.h;
            let st = self.scrolls.get_mut(&scroll.id);
            let sticky = scroll.scroll.unwrap().stick_bottom;
            let pinned = (settle && sticky) || st.as_ref().map_or(sticky, |s| s.pinned);
            let offset = if let Some(st) = st {
                if !pinned { st.offset = (st.offset + f64::from(v.anchor_delta())).max(0.0); }
                st.offset as f32
            } else { 0.0 };
            // Rows that shrank or went leave the offset past the end until scrolling clamps it;
            // what shows then is the end.
            let offset = offset.min(v.max_offset());
            let end = v.row_end(v.range.end);
            let mounted_height = (end - v.before).max(0.0);
            // Never present a blank caused by an estimate. Expand and measure again first.
            retry |= if pinned {
                v.range.start > 0 && mounted_height < v.viewport
            } else {
                (v.range.start > 0 && offset < v.pad_top + v.before)
                    || (v.range.end < v.keys.len() && offset + v.viewport > v.pad_top + end)
            };
        }
        retry
    }

    /// Checks the view's keys and builds the virtual windows, index-built rows included.
    fn build_tree(&mut self, mut root: El, tx: &Tx, force_all: bool) -> El {
        // Two siblings with one key would share a layout node, which Taffy panics on. Rename the
        // later ones so every build, debug or release, lays out; --tree still lists them.
        let duplicates = crate::key_diagnostics::repair(&mut root);
        match self.key_warnings.as_mut() {
            Some(warnings) => *warnings = duplicates.into_iter().map(|d| d.message).collect(),
            None => crate::key_diagnostics::warn_once(&duplicates),
        }
        // Index-built rows build here, so memos inside them share the view's cache. A window that
        // fits its content lays its root out at its natural height, which nothing bounds.
        let bound = if self.fit_height { f32::INFINITY } else { self.viewport.1 };
        self.window_tree(&mut root, "", 0, None, tx.settle, force_all, bound, false);
        root
    }

    fn measure_view(&mut self, tx: &Tx, force_all: bool) -> (TaffyTree<usize>, Vec<Node>) {
        let scale = self.env.scale;
        let fresh_cx = |this: &Self| Cx::<S> {
            env: this.env,
            titlebar_inset: this.titlebar_inset,
            system_accent: this.system_accent,
            system_reduced_motion: this.system_reduced_motion,
            reduced_motion: this.system_reduced_motion,
            time_scale: this.time_scale,
            caret: this.caret.clone(),
            caret_read: std::cell::Cell::new(false),
            layout: this.layout_seen.clone(),
            layout_reads: Default::default(),
            _s: PhantomData,
        };
        let mut cx = fresh_cx(self);
        #[cfg(feature = "hot")]
        let emptied = self.memo.emptied();
        let memo = crate::memo::enter(std::mem::take(self.memo), self.env, self.system_accent, self.system_reduced_motion, cx.caret.clone(), cx.layout.clone());
        let view_time = crate::frame_profile::Scope::new(crate::frame_profile::Phase::View);
        #[cfg(not(feature = "hot"))]
        let mut root = { let root = (self.view)(self.state, &mut cx); self.build_tree(root, tx, force_all) };
        // Index-built rows run the app's closures in `build_tree`, so a patch's panic there
        // falls back like one in the view.
        #[cfg(feature = "hot")]
        let (mut root, memo) = {
            let view = self.view;
            let state = self.state;
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let root = subsecond::HotFn::current(view).call((state, &mut cx));
                self.build_tree(root, tx, force_all)
            })) {
                Ok(root) => (root, memo),
                Err(_) => {
                    eprintln!("rune-hot: view panicked; rendering the original view");
                    // The failed build's memo scope, cache and context writes go; the startup
                    // view builds from scratch against the retained state.
                    drop(memo);
                    self.virtual_culled.clear();
                    self.virtual_inputs.clear();
                    cx = fresh_cx(self);
                    let memo = crate::memo::enter(emptied, self.env, self.system_accent, self.system_reduced_motion, cx.caret.clone(), cx.layout.clone());
                    let root = crate::hot::original_only(|| view(state, &mut cx));
                    (crate::hot::original_only(|| self.build_tree(root, tx, force_all)), memo)
                }
            }
        };
        // Both paths finish the memo here, after the rows are built.
        let (memo, replayed) = memo.finish();
        drop(view_time);
        *self.memo = memo;
        self.reduced_motion = cx.reduced_motion;
        self.time_scale = cx.time_scale;
        // Reads made by memos this build reused count as made again.
        self.caret_read = cx.caret_read.get() || replayed.caret;
        self.layout_reads = cx.layout_reads.take();
        for key in replayed.layout {
            if !self.layout_reads.contains(&key) { self.layout_reads.push(key); }
        }

        let mut taffy = std::mem::replace(self.layout, TaffyTree::with_capacity(0));
        for entry in self.layout_entries.values_mut() {
            entry.used = false;
        }
        let mut nodes = Vec::with_capacity(self.nodes.len().max(32));
        // Root constraints are part of the retained style, not a dirty write after flattening.
        // A window that fits its content lays the root out at its natural height.
        root.style.size = Size { width: length(self.viewport.0), height: if self.fit_height { auto() } else { length(self.viewport.1) } };
        let root_id = flatten(root, None, "", 0, motion::DEFAULT, RowLayout::Free, true, &mut taffy, &mut nodes, &mut self.layout_entries, scale);
        self.layout_entries.retain(|_, entry| {
            if !entry.used { taffy.remove(entry.id).unwrap(); }
            entry.used
        });
        {
            let _layout_time = crate::frame_profile::Scope::new(crate::frame_profile::Phase::Layout);
            let room = Size {
                width: AvailableSpace::Definite(self.viewport.0),
                height: if self.fit_height { AvailableSpace::MaxContent } else { AvailableSpace::Definite(self.viewport.1) },
            };
            compute_layout(&mut taffy, root_id, room, &nodes, &mut self.text, scale);
            // `aspect_fit` elements laid out filling their room; fix each at the largest box
            // at its ratio inside it and lay out again, so siblings and alignment follow.
            let mut refit = false;
            for n in &nodes {
                if let Some(fit) = &n.aspect_fit {
                    let style = fitted(fit.0, &fit.1, taffy.layout(n.taffy).unwrap().size);
                    taffy.set_style(n.taffy, style).unwrap();
                    refit = true;
                }
            }
            if refit { compute_layout(&mut taffy, root_id, room, &nodes, &mut self.text, scale); }
        }
        self.stats.layout_nodes = nodes.len();
        #[cfg(debug_assertions)]
        layout_diagnostics::collapsed_scrolls(&taffy, &nodes);

        self.node_index.clear();
        self.node_index.reserve(nodes.len());
        for i in 0..nodes.len() {
            let id = self.node_id(&nodes[i].path);
            nodes[i].id = id;
            self.node_index.insert(id, i);
            let l = taffy.layout(nodes[i].taffy).unwrap();
            nodes[i].local = MRect::new(l.location.x, l.location.y, l.size.width, l.size.height);
            let (px, py) = nodes[i].parent.map_or((0.0, 0.0), |p| (nodes[p].target[0], nodes[p].target[1]));
            let mut x = px + l.location.x;
            let mut y = py + l.location.y;
            if let Some(a) = &nodes[i].above
                && let Some(j) = nodes[..i].iter().position(|n| n.key.as_ref() == Some(&a.key))
            {
                // Anchors inside scroll viewports sit where they are scrolled (and stuck) to.
                let target = sticky::anchored_target(&nodes, self.scrolls, j, self.viewport);
                (x, y) = anchor_position(a, target, (l.size.width, l.size.height), self.viewport);
                nodes[i].local.x = x - px;
                nodes[i].local.y = y - py;
            }
            nodes[i].target = [x, y, x + l.size.width, y + l.size.height];
            let ins = [
                l.padding.left + l.border.left,
                l.padding.top + l.border.top,
                l.padding.right + l.border.right,
                l.padding.bottom + l.border.bottom,
            ];
            nodes[i].inset = ins;
            // A cell of a run draws from the run's layout (and keeps it cached) instead of its own.
            let run = nodes[i].text.as_ref().and_then(|t| t.advance_in.as_ref().map(|(run, at)| (self.text.run_layout(run, t.style, scale), *at)));
            if let Some((r, _)) = run.filter(|(r, at)| r.x(*at).is_some()) {
                nodes[i].text_layout = Some(r.layout.clone());
                nodes[i].run_layout = Some(r);
            } else if let Some(spec) = &nodes[i].text {
                let w = (l.size.width - ins[0] - ins[2]).max(0.0);
                let wrap = spec.wrap.then_some(w * scale);
                let mut layout = self.text.layout_for_spec(spec, scale, wrap, line_alignment(spec));
                let mut elided = None;
                if spec.truncate && layout.full_width() > w * scale + 0.5 {
                    let (cut, e) = truncated(&mut self.text, spec, scale, w * scale);
                    (layout, elided) = (cut, Some(e));
                } else if let Some((cut, e)) = clamped(&mut self.text, spec, scale, w * scale, &layout) {
                    (layout, elided) = (cut, Some(e));
                }
                nodes[i].text_layout = Some(layout);
                nodes[i].elided = elided;
            }
        }

        (taffy, nodes)
    }
}

impl<S: 'static> Runtime<S> {
    fn commit(&mut self, tx: &Tx) {
        let _commit_time = crate::frame_profile::Scope::new(crate::frame_profile::Phase::Commit);
        COMMITS.set(COMMITS.get() + 1);
        self.last_commit_clock = self.clock;
        self.memo_since = self.memo.next_pass();
        for r in &self.scroll_reveals {
            if r.at_end && let Some(&id) = self.path_ids.get(&r.viewport)
                && let Some(scroll) = self.scrolls.get_mut(&id)
            {
                scroll.pinned = true;
            }
        }
        let mut measured = self.measure(tx);
        self.focus_before_commit = self.focused.clone();
        // Settle layout reads first so a field that mounts once a rect is known is in `nodes`.
        measured = self.settle_layout_reads(measured, tx);
        if self.sync_requested_focus(&mut measured.nodes) {
            self.layout = measured.layout;
            measured = self.measure(tx);
            measured = self.settle_layout_reads(measured, tx);
        }
        if measured.caret_read && self.caret_moved(Some(&measured.nodes)) {
            self.layout = measured.layout;
            measured = self.measure(tx);
            measured = self.settle_layout_reads(measured, tx);
        }
        self.apply(measured, tx);
        self.memo.keep_since(self.memo_since);
        self.track_focused_rows();
        self.prune_text_selection();
    }

    /// Records which row of each virtual list holds focus, by key, so the row is still built
    /// after it scrolls out. Walks the node tree, since a key may itself contain `/`.
    fn track_focused_rows(&mut self) {
        if self.virtuals.is_empty() { return; }
        let focused = self.focused.as_deref().and_then(|f| self.nodes.iter().position(|n| n.path == f));
        for (path, v) in self.virtuals.iter_mut() {
            let Some(rest) = self.focused.as_deref().and_then(|f| f.strip_prefix(path.as_str())).and_then(|r| r.strip_prefix('/')) else {
                v.focused_row = None;
                continue;
            };
            if let Some(mut i) = focused {
                while let Some(p) = self.nodes[i].parent {
                    if self.nodes[p].path == *path {
                        v.focused_row = self.nodes[i].key.clone();
                        break;
                    }
                    i = p;
                }
            } else {
                let matches = |k: &str| rest == k || rest.strip_prefix(k).is_some_and(|t| t.starts_with('/'));
                if !v.focused_row.as_deref().is_some_and(matches) {
                    v.focused_row = v.keys.iter().find(|k| matches(k)).cloned();
                }
            }
        }
    }

    fn measure(&mut self, tx: &Tx) -> Measured {
        Measurement {
            state: &self.state,
            view: self.view,
            env: self.env,
            viewport: self.viewport,
            fit_height: self.fit_height,
            system_reduced_motion: self.system_reduced_motion,
            titlebar_inset: self.titlebar_inset,
            system_accent: self.system_accent,
            reduced_motion: self.reduced_motion,
            time_scale: self.time_scale,
            caret: self.caret.clone(),
            caret_read: false,
            layout_seen: self.layout_seen.clone(),
            layout_reads: Vec::new(),
            commits: self.commits,
            nodes: &self.nodes,
            text: &mut self.text,
            layout: &mut self.layout,
            layout_entries: &mut self.layout_entries,
            node_index: &mut self.node_index,
            path_ids: &mut self.path_ids,
            memo: &mut self.memo,
            next_id: &mut self.next_id,
            scrolls: &mut self.scrolls,
            virtuals: &mut self.virtuals,
            virtual_culled: &mut self.virtual_culled,
            virtual_inputs: &mut self.virtual_inputs,
            stats: &mut self.stats,
            key_warnings: &mut self.key_warnings,
            focused: self.focused.as_deref(),
            state_unchanged: std::mem::take(&mut self.state_unchanged),
            retry: false,
        }.finish(tx)
    }

    /// The mutation phase cannot accept an El or an unfinished measurement pass.
    fn apply(&mut self, measured: Measured, tx: &Tx) {
        let _apply_time = crate::frame_profile::Scope::new(crate::frame_profile::Phase::Apply);
        let Measured { layout: taffy, mut nodes, reduced_motion, time_scale, caret_read, .. } = measured;
        self.caret_watch = caret_read;
        if reduced_motion != self.reduced_motion {
            self.reduced_motion = reduced_motion;
            self.scene.set_reduced_motion(reduced_motion);
        }
        self.time_scale = time_scale;
        if tx.settle { self.flights.clear(); }
        let scale = self.env.scale;
        // Culling is not a model removal. Retire offscreen scene/growth state immediately;
        // re-entering historical rows mount settled and don't replay streaming words.
        if !self.virtual_culled.is_empty() {
            let mut culled = vec![false; self.nodes.len()];
            for (i, n) in self.nodes.iter().enumerate() {
                if self.virtual_culled.contains(&n.path) || n.parent.is_some_and(|p| culled[p]) {
                    culled[i] = true;
                    self.scene.unmount(n.id);
                    self.grows.remove(&n.id);
                }
            }
            for path in &self.virtual_culled {
                if let Some(id) = self.path_ids.get(path) {
                    self.scene.unmount(*id);
                    self.grows.remove(id);
                }
            }
        }
        for v in self.virtuals.values_mut() {
            v.shown = v.keys[v.range.clone()].iter().cloned().collect();
        }
        // A row scrolled into view mounts settled with its subtree. Content added later under a
        // row already on screen is a live change and keeps its own entrance.
        // Parents precede children, so a remounting row passes its flag down its whole subtree.
        for i in 0..nodes.len() {
            if let Some(p) = nodes[i].parent {
                nodes[i].virtual_mount |= nodes[p].virtual_mount && !self.scene.contains(nodes[p].id);
            }
        }
        let first = self.commits == 0;
        self.commits += 1;
        self.stats.commits = self.commits;
        // Concealing is a security boundary: old geometry must not disclose the revealed
        // line count through a height spring, displaced siblings, or a lingering scroll glide.
        let revealed: HashSet<_> = self.nodes.iter()
            .filter(|n| n.input.as_ref().is_some_and(|input| input.edit.is_secure() && input.edit.is_revealed()))
            .map(|n| n.id).collect();
        let mut concealed = vec![false; nodes.len()];
        for (i, n) in nodes.iter().enumerate() {
            if n.input.as_ref().is_some_and(|input| input.edit.is_hidden_multiline())
                && revealed.contains(&n.id)
            {
                concealed[i] = true;
            }
        }
        mark_ancestors(&nodes, &mut concealed);
        let instant = tx.instant || first || concealed.iter().any(|&hidden| hidden);
        let now = self.t;
        if self.fit_height && let Some(root) = nodes.first() { self.fit_window(root.local.h, tx, first); }

        // Growth stacks: rows of a stack that existed before grow; a new stack mounts settled.
        for c in 0..nodes.len() {
            let Some(_spec) = nodes[c].growth else { continue };
            let cid = nodes[c].id;
            // A reader away from the tail is anchored using measured row heights. Land those
            // heights with the anchor correction, unless `keep_on_screen` or `scroll_into_view`
            // is driving this viewport across the commit. A reveal left from an earlier commit
            // keeps only rows already growing on their spring: new rows land, or under a parent
            // whose size springs too, the two springs carry them past their slots.
            let revealing = self.virtuals.get(&nodes[c].path)
                .is_some_and(|v| self.scroll_reveals.iter().any(|r| r.viewport == v.scroll_path));
            let reading = self.virtuals.get(&nodes[c].path)
                .filter(|v| {
                    !self.keeps.iter().any(|k| k.viewport == v.scroll_path)
                        && !tx.scroll_to.iter().any(|(scope, key)| {
                            nodes.iter().enumerate().find(|(_, n)| n.named(key)
                                && n.parent.is_none_or(|p| in_scope(&nodes[p].path, scope.as_deref())))
                                .is_some_and(|(i, _)| {
                                    std::iter::successors(nodes[i].parent, |&p| nodes[p].parent)
                                        .find(|&p| nodes[p].scroll.is_some() && nodes[p].scroll_axes.y())
                                        .is_some_and(|p| nodes[p].path == v.scroll_path)
                                })
                        })
                })
                .and_then(|v| self.path_ids.get(&v.scroll_path))
                .and_then(|id| self.scrolls.get(id)).is_some_and(|s| !s.pinned);
            let rows: Vec<usize> = (c + 1..nodes.len()).filter(|&j| nodes[j].parent == Some(c)).collect();
            let existed = self.stacks.contains_key(&cid) && !tx.settle;
            let mut stack = self.stacks.remove(&cid).unwrap_or_default();
            let new_ids: Vec<NodeId> = rows.iter().map(|&j| nodes[j].id).collect();
            let new_set: HashSet<NodeId> = new_ids.iter().copied().collect();
            // Merge: keep leaving rows where they stood.
            let mut order = Vec::with_capacity(new_ids.len() + 4);
            let mut ni = 0;
            for &old in &stack.order {
                if new_set.contains(&old) {
                    while ni < new_ids.len() && new_ids[ni] != old {
                        if !order.contains(&new_ids[ni]) {
                            order.push(new_ids[ni]);
                        }
                        ni += 1;
                    }
                    if !order.contains(&old) {
                        order.push(old);
                    }
                    ni += 1;
                } else if self.grows.get(&old).is_some_and(|g| g.h > 0.01) && existed && !instant {
                    stack.closing.entry(old).or_insert(now + 0.15);
                    order.push(old);
                } else {
                    self.grows.remove(&old);
                    stack.closing.remove(&old);
                }
            }
            for &id in &new_ids[ni.min(new_ids.len())..] {
                if !order.contains(&id) {
                    order.push(id);
                }
            }
            let ctop = nodes[c].target[1];
            let width = nodes[c].local.w;
            let revealing_moving = revealing && rows.iter().any(|&j| {
                let id = nodes[j].id;
                self.grows.get(&id).is_some_and(|g| g.moving()) || stack.pending_heights.contains_key(&id)
            });
            if first || !existed || tx.settle || self.reduced_motion || (reading && !revealing_moving) {
                stack.width_hold_until = 0.0;
                stack.width_shrink_until = 0.0;
                stack.pending_heights.clear();
            } else if (width - stack.width).abs() > 0.25 {
                stack.width_hold_until = now + 0.12;
                stack.width_shrink_until = now + 0.4;
            }
            stack.width = width;
            stack.pending_heights.retain(|id, _| new_set.contains(id));
            stack.gap = crate::virtual_list::gap_y(taffy.style(nodes[c].taffy).unwrap());
            stack.pad_top = rows.first().map_or(0.0, |&j| nodes[j].target[1] - ctop);
            stack.pad_bottom = rows.last().map_or(nodes[c].target[3] - ctop, |&j| nodes[c].target[3] - nodes[j].target[3]);
            for &j in &rows {
                let id = nodes[j].id;
                let mut h = nodes[j].target[3] - nodes[j].target[1];
                stack.closing.remove(&id);
                stack.tops.insert(id, nodes[j].target[1] - ctop);
                let streaming = nodes[j].streaming;
                let g_moving = self.grows.get(&id).is_some_and(|g| g.moving()) || stack.pending_heights.contains_key(&id);
                let row_reading = reading && !(revealing && g_moving);
                if streaming && existed && !tx.settle && !first && !self.reduced_motion && !row_reading {
                    // Measure the final wrapped text before mutating the growth track. Targets
                    // retain full layout height; only the presented stack extent lags the fill.
                    let mut end = j + 1;
                    while end < nodes.len() && nodes[end].parent.is_some_and(|p| p >= j) { end += 1; }
                    if let Some(n) = nodes[j..end].iter().rev().find(|n| n.text.as_ref().is_some_and(|t| t.wrap && t.reveal >= 0.0))
                        && let Some(l) = &n.text_layout
                    {
                        let width = (n.target[2] - n.target[0] - n.inset[0] - n.inset[2]) * scale;
                        h = (h - crate::text::stream_height_lag(l, width, scale)).max(0.0);
                    }
                }
                let duration = tx.motion.map_or(0.0, |m| m.duration * 1.15);
                let g = self.grows.entry(id).or_insert_with(|| {
                    if existed && !instant && !nodes[j].virtual_mount { GrowTrack { streaming, duration, ..GrowTrack::settled(0.0) } } else { GrowTrack::settled(h) }
                });
                g.streaming = streaming;
                let snap_change = instant && !streaming && (h - g.target).abs() > 1e-4;
                if tx.settle || !existed || first || self.reduced_motion || snap_change || row_reading {
                    *g = GrowTrack { streaming, ..GrowTrack::settled(h) };
                    stack.pending_heights.remove(&id);
                } else {
                    // Only a width/scale rewrap may shrink an active streaming row, including
                    // when fresh text arrives in the same frame as the resize.
                    let t = if streaming && !self.relayout && now >= stack.width_shrink_until { h.max(g.h.min(g.target)) } else { h };
                    if now < stack.width_hold_until {
                        stack.pending_heights.insert(id, t);
                    } else {
                        stack.pending_heights.remove(&id);
                        if (t - g.target).abs() > 1e-4 { g.duration = duration; }
                        g.retarget(t);
                    }
                }
            }
            if let Some(&last) = rows.last()
                && !self.scene.contains(nodes[last].id)
                && let Some(s) = std::iter::successors(nodes[c].parent, |&p| nodes[p].parent)
                    .find(|&p| nodes[p].scroll.is_some() && nodes[p].scroll_axes.y())
                && let Some(r) = self.scroll_reveals.iter_mut().find(|r| r.viewport == nodes[s].path && r.at_end)
            {
                r.scope = None;
                r.key = nodes[last].path.clone();
            }
            stack.growing = !stack.pending_heights.is_empty()
                || order.iter().any(|r| self.grows.get(r).is_some_and(|g| g.target > g.h + 1.0 / 128.0));
            stack.moving = stack.growing
                || order.iter().any(|r| self.grows.get(r).is_some_and(|g| g.moving()));
            stack.order = order;
            self.stacks.insert(cid, stack);
        }

        // Put nodes on their subtree's clock before this commit animates them.
        let time_scaled = self.time_scaled || nodes.iter().any(|n| n.time_scale != 1.0);
        let mut time_scales = Vec::new();
        if time_scaled { self.sync_time_scales(&nodes, &mut time_scales, false); }

        let unseen = self.unseen_before_and_after(&nodes);
        // `El::matched` elements of this tree: (id, index, newly mounted).
        let mut arrivals: Vec<(Rc<str>, usize, bool)> = Vec::new();
        let mut mounted = vec![false; nodes.len()];
        for i in 0..nodes.len() {
            let id = nodes[i].id;
            let parent_id = nodes[i].parent.map(|p| nodes[p].id);
            let spring_l = tx.motion.unwrap_or(nodes[i].motion);
            let spring_c = tx.color_motion.unwrap_or(nodes[i].motion);
            // Text leads its background so theme crossfades never dip through zero contrast.
            let spring_tc = Spring::new((spring_c.duration * 0.5).max(0.1), spring_c.bounce);
            let row_like = nodes[i].in_row == RowLayout::Snaps || nodes[i].parent.is_some_and(|p| nodes[p].growth.is_some());
            let layout_instant = instant || row_like || tx.relayout || self.virtuals.contains_key(&nodes[i].path)
                || self.aspect_snap.contains(&id);
            let mut scene_rect = nodes[i].local;
            if let Some(v) = nodes[i].parent.and_then(|p| self.virtuals.get(&nodes[p].path)) {
                scene_rect.y -= v.before;
            }

            let was_present = self.scene.contains(id);
            let was_exiting = self.is_exiting(id);
            if let Some((key, _)) = &nodes[i].matched {
                arrivals.push((key.clone(), i, !was_present || was_exiting));
            }
            if !was_present || was_exiting {
                // Presence belongs to the transition. Layout tuning must not replace
                // the caller's enter/exit springs when this node mounts or revives.
                let tr = nodes[i].transition.unwrap_or_else(Transition::opacity);
                // Revive presence first, then let this transaction own the layout retarget.
                // Scene::insert otherwise starts its default .4 s reflow before our .25 s
                // menu curve; the equal target makes the intended retarget a no-op.
                let mount_rect = if was_exiting {
                    self.scene.store().target(id, prop::FRAME).unwrap_or(scene_rect)
                } else { scene_rect };
                let mut spec = NodeSpec::new(mount_rect)
                    .opacity(nodes[i].vis.opacity)
                    .radius(Corners(nodes[i].vis.radius))
                    .transition(tr);
                if let Some(pid) = parent_id {
                    spec = spec.parent(pid);
                }
                // Growth carries a row's entrance; an explicit transition adds to it. Under
                // `animate_layout`, contents new to a row already in place enter on their own.
                mounted[i] = !was_present;
                let rides_row = row_like || (nodes[i].in_row == RowLayout::Horizontal && nodes[i].parent.is_some_and(|p| mounted[p]));
                let quiet_row = rides_row
                    && nodes[i].transition.is_none_or(|t| t.insertion == rune_motion::Effect::IDENTITY)
                    && !was_exiting
                    && !self.reduced_motion;
                let mount = if instant || nodes[i].virtual_mount || quiet_row { Mount::Initial } else { Mount::Animated };
                self.scene.insert(id, spec, mount);
            }
            // How far a growth or virtual row's place jumps in this commit (drawn minus new).
            let mut row_jump = 0.0;
            if was_present {
                // As in SwiftUI, a node leaves with the transition of its last render.
                if !was_exiting {
                    self.scene.set_transition(id, nodes[i].transition.unwrap_or_else(Transition::opacity));
                }
                let stack_row = nodes[i].parent.is_some_and(|p| nodes[p].growth.is_some()) || self.virtuals.contains_key(&nodes[i].path)
                    || (nodes[i].in_row == RowLayout::Horizontal && !self.aspect_snap.contains(&id));
                // A window's own resize step moves content at rest with the window; motion
                // already running retargets on its spring.
                let follows_window = tx.window_step && !self.flying.iter().any(|f| f.0 == id)
                    && !self.scene.store().is_animating(id, prop::FRAME);
                // Out of sight before and after: nobody would see it move.
                let out_of_sight = unseen.get(i).copied().unwrap_or(false) && nodes[i].matched.is_none()
                    && !self.flying.iter().any(|f| f.0 == id);
                let layout_anim = if out_of_sight {
                    LayoutAnim::INSTANT
                } else if stack_row && !instant && !tx.relayout && !follows_window {
                    // Growth and virtual windows own a row's vertical place; its horizontal place
                    // still springs, or a re-centred column jumps against its moving parent. So
                    // do `animate_layout` contents inside the row.
                    if let Some(drawn) = self.scene.get(id, prop::FRAME) { row_jump = drawn.y - scene_rect.y; }
                    let mut la = LayoutAnim::axes(spring_l, Anim::INSTANT);
                    if tx.motion.is_none() && nodes[i].width_leads {
                        la = la.width_leads();
                    }
                    la
                } else if layout_instant || follows_window {
                    LayoutAnim::INSTANT
                } else {
                    let mut la = LayoutAnim::new(spring_l);
                    if tx.motion.is_none() {
                        if let Some((lead, trail)) = nodes[i].lead {
                            la = la.lead(lead, trail);
                        } else if nodes[i].width_leads {
                            la = la.width_leads();
                        }
                    }
                    la
                };
                let flying = if self.flying.is_empty() || layout_anim == LayoutAnim::INSTANT { None }
                    else { self.flying.iter().position(|f| f.0 == id) };
                if let Some(f) = flying {
                    // Keep a matched flight on its one screen path; retarget it only if its
                    // layout moved, so an unrelated commit doesn't re-time it.
                    if self.flying[f].1 != nodes[i].local {
                        self.flying[f].1 = nodes[i].local;
                        self.scene.set_frame(id, scene_rect, layout_anim.screen());
                    }
                } else if !nodes[i].live.iter().any(|b| b.property == crate::live::Property::Rect) {
                    self.scene.set_frame(id, scene_rect, layout_anim);
                }
                let quiet = instant || out_of_sight;
                self.set_or_anim(id, prop::RADIUS, Corners(nodes[i].vis.radius), spring_l, quiet);
                if nodes[i].live.iter().any(|b| b.property == crate::live::Property::Opacity) {
                    // The live source owns this property, including across unrelated commits.
                } else if !quiet && let Some(anim) = nodes[i].opacity_motion {
                    // Only a new target: an anim with its own velocity would restart each commit,
                    // and would run on an element that mounted where it rests.
                    let rest = self.scene.store().target(id, prop::OPACITY).or_else(|| self.scene.get(id, prop::OPACITY));
                    if rest != Some(nodes[i].vis.opacity) {
                        self.scene.animate(id, prop::OPACITY, nodes[i].vis.opacity, anim);
                    }
                } else {
                    self.set_or_anim(id, prop::OPACITY, nodes[i].vis.opacity, spring_l, quiet);
                }
            }
            let shift = nodes[i].vis.offset;
            if nodes[i].looping.is_none() && !nodes[i].live.iter().any(|b| b.property == crate::live::Property::Offset)
                && (shift != [0.0; 2] || self.scene.store().has(id, prop::OFFSET))
            {
                let to = rune_motion::Vec2::new(shift[0], shift[1]);
                if instant || !was_present || was_exiting || self.reduced_motion {
                    self.scene.set(id, prop::OFFSET, to);
                } else if self.scene.store().target(id, prop::OFFSET) != Some(to) {
                    if row_jump != 0.0 {
                        // The row starts from where it was drawn: a reordered row that drops its
                        // offset slides into its new place.
                        let now = self.scene.get(id, prop::OFFSET).unwrap_or(rune_motion::Vec2::ZERO);
                        self.scene.set(id, prop::OFFSET, rune_motion::Vec2::new(now.x, now.y + row_jump));
                    }
                    match nodes[i].vis.offset_motion {
                        Some(anim) => self.scene.animate(id, prop::OFFSET, to, anim),
                        None => self.scene.animate(id, prop::OFFSET, to, spring_l),
                    }
                }
            }
            let k = nodes[i].static_scale;
            nodes[i].scale_track = k != 1.0 || self.scene.store().has(id, P_SCALE);
            if nodes[i].scale_track {
                if instant || !was_present || was_exiting || self.reduced_motion {
                    self.scene.set(id, P_SCALE, k);
                } else if self.scene.store().target(id, P_SCALE) != Some(k) {
                    if !self.scene.store().has(id, P_SCALE) { self.scene.set(id, P_SCALE, 1.0); }
                    match nodes[i].vis.scale_motion.as_deref().copied() {
                        Some(anim) => self.scene.animate(id, P_SCALE, k, anim),
                        None => self.scene.animate(id, P_SCALE, k, spring_l),
                    }
                }
            }
            let turn = nodes[i].vis.rotate;
            if turn != 0.0 || self.scene.store().has(id, prop::ROTATION) {
                if instant || !was_present || was_exiting || self.reduced_motion {
                    self.scene.set(id, prop::ROTATION, turn);
                } else if self.scene.store().target(id, prop::ROTATION) != Some(turn) {
                    if !self.scene.store().has(id, prop::ROTATION) { self.scene.set(id, prop::ROTATION, 0.0); }
                    match nodes[i].vis.rotate_motion.as_deref().copied() {
                        Some(anim) => self.scene.animate(id, prop::ROTATION, turn, anim),
                        None => self.scene.animate(id, prop::ROTATION, turn, spring_l),
                    }
                }
            }
            let vis = &nodes[i].vis;
            if !nodes[i].live.iter().any(|b| b.property == crate::live::Property::Blur)
                && (vis.blur != 0.0 || self.scene.store().has(id, prop::BLUR))
            {
                if !instant && was_present && let Some(anim) = vis.blur_motion {
                    self.scene.animate(id, prop::BLUR, vis.blur, anim);
                } else { self.set_or_anim(id, prop::BLUR, vis.blur, spring_l, instant || !was_present); }
            }
            if !nodes[i].live.iter().any(|b| b.property == crate::live::Property::Fill) {
                self.set_or_anim(id, P_FILL, vis.fill, spring_c, instant);
            }
            if vis.border_width > 0.0 {
                let had_bottom = self.scene.store().has(id, P_BORDER_BOTTOM);
                if vis.border_bottom.is_some() && !had_bottom && let Some(c) = self.scene.get(id, P_BORDER) {
                    // A solid border's bottom springs into the gradient rather than jumping.
                    self.scene.set(id, P_BORDER_BOTTOM, c);
                }
                self.set_or_anim(id, P_BORDER, vis.border, spring_c, instant);
                if vis.border_bottom.is_some() || had_bottom {
                    self.set_or_anim(id, P_BORDER_BOTTOM, vis.border_bottom.unwrap_or(vis.border), spring_c, instant);
                }
            }
            if let Some(c) = nodes[i].text.as_ref().map(|t| t.color).or(nodes[i].vis.icon.map(|(_, c)| c)) {
                self.set_or_anim(id, P_TEXT, c, spring_tc, instant);
            }
            if nodes[i].vis.icon.is_some() && (nodes[i].vis.icon_turn != 0.0 || self.scene.store().has(id, P_ICON_TURN)) {
                if was_present && !self.scene.store().has(id, P_ICON_TURN) {
                    self.scene.set(id, P_ICON_TURN, 0.0);
                }
                self.set_or_anim(id, P_ICON_TURN, nodes[i].vis.icon_turn, spring_l, instant || !was_present);
            }
            if vis.hover.a > 0.0 || self.scene.store().has(id, P_HOVER_C) {
                self.set_or_anim(id, P_HOVER_C, vis.hover, spring_c, instant);
            }
            if vis.press.a > 0.0 || self.scene.store().has(id, P_PRESS_C) {
                self.set_or_anim(id, P_PRESS_C, vis.press, spring_c, instant);
            }
            for (k, sh) in vis.shadows.iter().take(P_SHADOWS.len()).enumerate() {
                self.set_or_anim(id, P_SHADOWS[k], sh.color, spring_c, instant);
                let geo = rune_motion::Rect { x: sh.x, y: sh.y, w: sh.blur, h: sh.spread };
                self.set_or_anim(id, P_SHADOW_GEO[k], geo, spring_l, instant);
            }
            for k in vis.shadows.len()..P_SHADOWS.len() {
                if self.scene.store().has(id, P_SHADOWS[k]) {
                    self.scene.store_mut().remove(id, P_SHADOWS[k]);
                    self.scene.store_mut().remove(id, P_SHADOW_GEO[k]);
                } else {
                    break;
                }
            }
            if let Some(g) = vis.gradient {
                self.set_or_anim(id, P_GRADIENT[0], g[0], spring_c, instant);
                self.set_or_anim(id, P_GRADIENT[1], g[1], spring_c, instant);
            }
            if let Some(rg) = &vis.radial {
                for (k, &(_, c)) in rg.stops.iter().take(P_RADIAL.len()).enumerate() {
                    self.set_or_anim(id, P_RADIAL[k], c, spring_c, instant);
                }
            }
            if let Some(ol) = vis.outline {
                self.set_or_anim(id, P_OUTLINE, ol.color, spring_c, instant);
            }
            if (vis.hover.a > 0.0 || vis.press.a > 0.0 || vis.hover_group) && !self.scene.store().has(id, P_HOVER) {
                self.scene.set(id, P_HOVER, 0.0);
                self.scene.set(id, P_PRESS, 0.0);
            }

            // Streaming word reveal.
            if let Some(spec) = &nodes[i].text
                && spec.reveal >= 0.0
            {
                let content = &spec.content;
                let now = if time_scaled { self.scene.store().node_time(id) } else { now };
                let fade = if self.reduced_motion { 0.15 } else { spec.reveal };
                let mounted_late = !was_present && !instant && !nodes[i].virtual_mount;
                let st = self.reveals.entry(id).or_insert_with(|| RevealState {
                    len: if mounted_late { 0 } else { content.len() },
                    words: Vec::new(),
                    fade,
                });
                st.fade = fade;
                if instant || (!was_present && nodes[i].virtual_mount) || content.len() < st.len || !content.is_char_boundary(st.len) {
                    st.len = content.len();
                    st.words.clear();
                } else if content.len() > st.len && fade > 0.0 {
                    let mut starts = Vec::new();
                    word_starts(content, st.len, &mut starts);
                    // A word continuing across the old end keeps its time.
                    let mut tn = st.words.last().map_or(now, |w| w.1.max(now));
                    let pending = st.words.iter().filter(|w| w.1 > now).count() + starts.len();
                    let stagger = if self.reduced_motion {
                        0.0
                    } else {
                        let k = (pending as f32 / 24.0).clamp(0.0, 1.0);
                        (0.020 + (0.0055 - 0.020) * k) as f64
                    };
                    for b in starts {
                        st.words.push((b, tn));
                        tn += stagger;
                    }
                    st.len = content.len();
                } else {
                    st.len = content.len();
                }
            }
        }

        if !arrivals.is_empty() || !self.matched.is_empty() {
            self.match_elements(&nodes, arrivals, tx, instant);
        }

        // Scroll viewports: a new sticky viewport opens at the end.
        for (i, n) in nodes.iter().enumerate() {
            if let Some(sp) = n.scroll {
                let e = self.scrolls.entry(n.id).or_insert(ScrollState {
                    offset: 0.0,
                    v: 0.0,
                    pinned: sp.stick_bottom,
                    max: 0.0,
                    direct: false,
                    scrolling: false,
                    glide: None,
                    glide_to: None,
                    at_bottom: None,
                    held: false,
                    slack: Default::default(),
                    x: AxisScroll::default(),
                });
                if concealed[i] {
                    e.offset = 0.0;
                    e.v = 0.0;
                    e.max = 0.0;
                    e.glide = None;
                    e.glide_to = None;
                    e.held = false;
                    e.scrolling = false;
                    e.slack = scroll_bottom::Slack::landed(self.viewport);
                    e.x = AxisScroll::default();
                }
                if tx.settle && sp.stick_bottom {
                    e.pinned = true;
                    e.v = 0.0;
                }
                // A window resize or a settle lands the slack: the rows go straight to the bottom.
                if tx.settle || e.slack.window != self.viewport { e.slack = scroll_bottom::Slack::landed(self.viewport); }
            }
        }

        // Removed nodes stay mounted in `Scene` until their exit transition settles at opacity 0.
        let present_len = nodes.len();
        let old_nodes = std::mem::take(&mut self.nodes);
        let old_ids: Vec<NodeId> = old_nodes.iter().map(|n| n.id).collect();
        for mut old in old_nodes {
            if let Some(&i) = self.node_index.get(&old.id) {
                let mut fill = None;
                for bound in &mut nodes[i].live {
                    if let Some(previous) = old.live.iter().find(|b| b.property == bound.property && Arc::ptr_eq(&b.source, &bound.source)) {
                        bound.seen = if tx.settle { None } else { previous.seen };
                        if bound.property == crate::live::Property::Fill { fill = Some(old.vis.fill); }
                    }
                }
                if let Some(fill) = fill { nodes[i].vis.fill = fill; }
                // Removing a binding restores the normal property, including transform defaults.
                for previous in &old.live {
                    if !nodes[i].live.iter().any(|b| b.property == previous.property) {
                        match previous.property {
                            crate::live::Property::Offset => self.scene.set(old.id, prop::OFFSET, rune_motion::Vec2::ZERO),
                            crate::live::Property::Scale => self.scene.set(old.id, prop::SCALE, 1.0),
                            _ => {}
                        }
                    }
                }
            }
            if self.node_index.contains_key(&old.id) || !self.scene.contains(old.id) {
                continue;
            }
            let old_parent_id = old.parent.and_then(|p| old_ids.get(p).copied());
            let parent_remained_present =
                old_parent_id.is_none_or(|pid| self.node_index.get(&pid).is_some_and(|&i| i < present_len));
            if parent_remained_present {
                if tx.settle || (instant && !self.is_exiting(old.id)) {
                    self.scene.unmount(old.id);
                } else {
                    self.scene.remove(old.id);
                }
            } else if !tx.settle && self.flying.iter().any(|f| f.0 == old.id) {
                // A flight paints outside its ancestors, so it can't fade with an exiting one.
                self.scene.remove(old.id);
            }
            old.parent = old_parent_id.and_then(|pid| self.node_index.get(&pid).copied());
            old.live.clear();
            // Exit visuals outlive their model. Never call an accessor captured by
            // a deleted row while normalizing a later focus change.
            if let Some(input) = &mut old.input {
                let was_revealed = input.edit.is_secure() && input.edit.is_revealed();
                input.access = None;
                input.key = None;
                input.change = None;
                input.edit.focused = false;
                input.edit.clear_preedit();
                input.edit.conceal();
                if was_revealed && let Some(spec) = &mut old.text {
                    use zeroize::Zeroize;
                    spec.content.zeroize();
                    spec.content = input.edit.display().0;
                    if input.edit.is_hidden_multiline() { spec.wrap = false; }
                    let scale = self.env.scale;
                    let w = (old.local.w - old.inset[0] - old.inset[2]).max(0.0);
                    let wrap = spec.wrap.then_some(w * scale);
                    old.text_layout = Some(self.text.layout_for_spec(spec, scale, wrap, line_alignment(spec)));
                    old.elided = None;
                }
            }
            self.node_index.insert(old.id, nodes.len());
            nodes.push(old);
        }

        if time_scaled {
            // Exits keep following their present ancestors' scale, so one under a frozen subtree
            // finishes once it resumes.
            self.sync_time_scales(&nodes, &mut time_scales, true);
            self.time_scaled = time_scales.iter().any(|&k| k != 1.0);
        }
        self.nodes = nodes;
        self.commit_gen = self.commit_gen.wrapping_add(1);
        self.has_dissolve = self.nodes.iter().any(|n| n.vis.blend == crate::render::Blend::PlusLighter);
        self.has_cache = self.nodes.iter().any(|n| n.vis.cache);
        if !self.has_cache {
            if !self.node_caches.is_empty() { self.node_caches.clear(); }
            if !self.text.atlas.resident_caches.is_empty() { self.text.atlas.resident_caches.clear(); }
        } else if !self.node_caches.is_empty() {
            self.node_caches.retain(|id, _| self.scene.contains(*id));
            self.text.atlas.resident_caches.retain(|id, _| self.scene.contains(NodeId(*id)));
        }
        self.has_pixel_aspect = self.nodes.iter().any(|n| n.pixel_aspect.is_some());
        self.has_drawn_layout = self.nodes.iter().any(|n| n.on_drawn_layout.is_some());
        self.has_key_up = self.nodes.iter().any(|n| n.focus.key_up.is_some());
        self.has_sticky = self.nodes.iter().any(|n| n.sticky.as_ref().is_some_and(|s| s.edge.is_some()));
        self.has_raise = self.nodes.iter().any(|n| n.vis.raise);
        // A flight that left the tree stays lifted and covered while it exits, so it doesn't pop.
        self.flying.retain(|(id, _)| self.node_index.contains_key(id));
        if self.nodes.len() > present_len || !self.flying.is_empty() || self.has_sticky || self.has_raise { self.update_layer_paint_order(); }
        else { self.layer_paint_order.clear(); }
        self.built = false;
        self.virtuals.retain(|path, _| self.nodes.iter().any(|n| n.path == *path));
        self.subscribe_live();
        self.refresh_modal_index();
        self.sync_modals();
        self.sync_follows();
        self.loops.retain(|id, _| self.scene.contains(*id));
        self.static_scales.retain(|id, _| self.scene.contains(*id));
        for n in &self.nodes {
            let t = if time_scaled { self.scene.store().node_time(n.id) } else { self.t };
            if let Some(spec) = n.looping {
                let track = self.loops.entry(n.id).and_modify(|l| { l.retime(spec, t); l.enabled(true, t); })
                    .or_insert_with(|| rune_motion::LoopTrack::new(spec, t));
                if tx.settle || self.reduced_motion { track.finish(); }
            } else if let Some(l) = self.loops.get_mut(&n.id) {
                l.enabled(false, t);
            }
        }
        self.layout = taffy;
        self.text.end_commit();
        self.dirty = false;
        self.relayout = false;
        self.aspect_snap.clear();
        self.viewport_stepped = false;
        self.settle = false;
    }

    /// Runs each of `nodes` from `scales.len()` on at its `El::time_scale` times its parent's,
    /// pushing the products onto `scales`. `exiting` nodes drop their own factor, which the app
    /// can no longer change, and follow their parent's. A node whose clock changes keeps its
    /// place: the store rebases its tracks, and its reveal words and loop shift by the same amount.
    fn sync_time_scales(&mut self, nodes: &[Node], scales: &mut Vec<f32>, exiting: bool) {
        for n in &nodes[scales.len()..] {
            let own = if exiting { 1.0 } else { n.time_scale };
            let k = own * n.parent.and_then(|p| scales.get(p).copied()).unwrap_or(1.0);
            scales.push(k);
            // Unmounted at once this commit: its clock went with it. (Present nodes not yet in
            // the scene get theirs now, before their first tracks.)
            if exiting && !self.scene.contains(n.id) { continue; }
            let store = self.scene.store_mut();
            let before = store.node_time(n.id);
            store.set_node_time_scale(n.id, k);
            let shift = store.node_time(n.id) - before;
            if shift != 0.0 {
                if let Some(st) = self.reveals.get_mut(&n.id) {
                    for w in &mut st.words { w.1 += shift; }
                }
                if let Some(l) = self.loops.get_mut(&n.id) { l.shift(shift); }
            }
        }
    }

    // ---------------------------------------------------------------------------------------
    // Growth, scroll and reveals: render-side tracks.

    fn step_growth(&mut self, dt: f32) -> bool {
        if self.stacks.is_empty() {
            return false;
        }
        self.grow_acc += dt;
        let steps = (self.grow_acc / SUBSTEP).floor() as usize;
        self.grow_acc -= steps as f32 * SUBSTEP;
        let now = self.t;
        let mut moving = false;
        for (cid, stack) in self.stacks.iter_mut() {
            let Some(&ci) = self.node_index.get(cid) else { continue };
            let Some(spec) = self.nodes[ci].growth else { continue };
            let n_rows = stack.order.len();
            let mut st_growing = !stack.pending_heights.is_empty();
            let mut st_moving = st_growing;
            for (k, id) in stack.order.iter().enumerate() {
                let Some(g) = self.grows.get_mut(id) else { continue };
                let h0 = g.h;
                if let Some(&at) = stack.closing.get(id)
                    && now >= at
                {
                    if self.reduced_motion {
                        *g = GrowTrack::settled(0.0);
                    } else {
                        g.retarget(0.0);
                    }
                }
                // A row above the tail (the last row) carries it.
                let carrying = k + 2 == n_rows;
                for substep in 0..steps.min(240) {
                    if !stack.pending_heights.is_empty() {
                        let at = now - f64::from(self.grow_acc) - f64::from((steps.min(240) - substep) as f32 * SUBSTEP);
                        if at >= stack.width_hold_until && let Some(height) = stack.pending_heights.remove(id) {
                            g.retarget(height);
                        }
                    }
                    g.step(spec.spring, &spec.edge, carrying);
                }
                stack.grown += (g.h - h0).max(0.0);
                st_growing |= g.target > g.h + 1.0 / 128.0;
                st_moving |= g.moving() || (g.h - h0).abs() > 1e-5;
            }
            stack.order.retain(|id| {
                let keep = !stack.closing.contains_key(id) || self.grows.get(id).is_some_and(|g| g.h > 0.01 || g.target > 0.0);
                if !keep {
                    self.grows.remove(id);
                }
                keep
            });
            stack.closing.retain(|id, _| stack.order.contains(id));
            stack.growing = st_growing;
            stack.moving = st_moving;
            // Keep the shared clock awake through the hold. No timer thread or remeasurement.
            moving |= st_moving;
        }
        moving
    }

    /// Presented content extent of node `c` if it is a growth stack (points).
    fn stack_extent(&self, c: usize) -> Option<f64> {
        let n = &self.nodes[c];
        n.growth?;
        let stack = self.stacks.get(&n.id)?;
        let mut count = 0usize;
        let rows: f64 = stack.order.iter().filter_map(|id| self.grows.get(id)).map(|g| { count += 1; f64::from(g.h) }).sum();
        let gaps = f64::from(stack.gap) * count.saturating_sub(1) as f64;
        Some(f64::from(stack.pad_top) + rows + gaps + f64::from(stack.pad_bottom))
    }

    /// How far scroll viewport `i`'s content reaches past its bottom, negative while it fits.
    fn scroll_overflow(&self, i: usize) -> f64 {
        let view = self.nodes[i].target;
        let content_h = self.scroll_content(i).map_or(0.0, |c| {
            let n = &self.nodes[c];
            let top = n.target[1] - view[1];
            f64::from(top) + self.stack_extent(c).unwrap_or(f64::from(n.target[3] - n.target[1]))
        });
        content_h - f64::from(view[3] - view[1])
    }

    fn step_scroll(&mut self, dt: f32) -> bool {
        let slack_dt = std::mem::take(&mut self.slack_dt);
        let mut moving = false;
        for i in 0..self.nodes.len() {
            let Some(sp) = self.nodes[i].scroll else { continue };
            if self.subtree_exiting(i) { continue; }
            let id = self.nodes[i].id;
            let axes = self.nodes[i].scroll_axes;
            if axes.x() { moving |= self.step_scroll_x(i, dt); }
            // A horizontal viewport doesn't move vertically, even with content taller than it.
            let overflow = if axes.y() { self.scroll_overflow(i) } else { 0.0 };
            #[cfg(target_arch = "wasm32")]
            if let Some(active) = self.web_step_scroll(id, overflow, dt, false) {
                moving |= active;
                continue;
            }
            let held = (!self.keeps.is_empty() || !self.scroll_reveals.is_empty()) && {
                let path = &self.nodes[i].path;
                (self.keeps.iter().any(|k| k.viewport == *path) || self.scroll_reveals.iter().any(|r| r.viewport == *path))
                    && self.scroll_content(i).and_then(|c| self.stacks.get(&self.nodes[c].id)).is_some_and(|s| s.growing)
            };
            let Some(st) = self.scrolls.get_mut(&id) else { continue };
            st.scrolling = st.direct || st.v.abs() > 1.0 || st.x.v.abs() > 1.0;
            st.direct = false;
            st.max = overflow.max(0.0);
            if st.pinned && sp.stick_bottom {
                st.offset = st.max;
                st.v = 0.0;
                st.glide = None;
            } else if st.glide.is_some() {
                moving |= scroll_bottom::glide(st, dt);
            } else if st.v.abs() > 1.0 {
                // Exact for exponential decay, so a long frame carries the same distance as
                // the short frames it stands for.
                let decay = (-SCROLL_DECAY * dt).exp();
                st.offset += f64::from(st.v * (1.0 - decay) / SCROLL_DECAY);
                st.v *= decay;
                if st.offset <= 0.0 || st.offset >= st.max {
                    st.v = 0.0;
                }
                moving = true;
            } else {
                st.v = 0.0;
            }
            st.offset = st.offset.clamp(0.0, st.max);
            if sp.stick_bottom && !held && st.offset >= st.max - 0.5 && st.v.abs() <= 1.0 {
                st.pinned = true;
            }
            if sp.stick_bottom && axes.y() && slack_dt > 0.0 { moving |= self.step_slack(i, slack_dt); }
        }
        moving
    }

    fn reveals_active(&mut self) -> bool {
        let store = self.scene.store();
        let mut any = false;
        for (id, st) in self.reveals.iter_mut() {
            let now = store.node_time(*id);
            let fade = st.fade as f64;
            // Words fully shown leave the list (only from the front, which keeps lookups valid).
            let done = st.words.iter().take_while(|w| now - w.1 >= fade).count();
            if done > 0 && done == st.words.len() {
                st.words.clear();
            }
            any |= !st.words.is_empty() && store.node_time_scale(*id) > 0.0;
        }
        any
    }

    fn reveal_alpha(st: &RevealState, byte: u32, now: f64) -> f32 {
        let i = st.words.partition_point(|w| w.0 <= byte);
        if i == 0 {
            return 1.0;
        }
        let t0 = st.words[i - 1].1;
        if st.fade <= 0.0 {
            return if now >= t0 { 1.0 } else { 0.0 };
        }
        ease_out(((now - t0) / st.fade as f64) as f32)
    }

    // ---------------------------------------------------------------------------------------
    // Input: queued, handled at the next frame.

    /// The single reachable editor in a textbox. Only resolve descendants on presses;
    /// hover hit testing and cursor updates use the container's role directly.
    fn textbox_input(&self, i: usize) -> Option<usize> {
        if self.nodes[i].input.is_some() { return self.can_focus(i).then_some(i); }
        if self.nodes[i].role != "textbox" { return None; }
        let mut inputs = (0..self.nodes.len()).filter(|&j| {
            self.nodes[j].input.is_some()
                && self.can_focus(j)
                && std::iter::successors(self.nodes[j].parent, |&p| self.nodes[p].parent).any(|p| p == i)
        });
        let first = inputs.next()?;
        inputs.next().is_none().then_some(first)
    }

    /// The topmost element under (x, y) that takes primary input, selectable text included.
    fn top_target(&self, x: f32, y: f32) -> Option<usize> {
        self.hits.iter().rev().find_map(|&(id, r)| {
            if !self.hit_contains(id, r, x, y) { return None; }
            let i = *self.node_index.get(&id)?;
            let n = &self.nodes[i];
            // A secondary-only child must not obscure primary input or background blur.
            (!self.subtree_exiting(i) && self.allowed(i) && (n.on_press.is_some() || n.on_double_press.is_some() || n.on_hover.is_some()
                || n.on_drag.is_some() || n.drag_out.is_some() || n.input.is_some() || n.role == "textbox"
                || (n.role == "button" && n.vis.hit) || n.window_drag || n.focus.enabled || n.modal.is_some()
                || self.is_selectable(i))).then_some(i)
        })
    }

    /// The element a press, hover or drag at (x, y) goes to. Selectable text passes them to the
    /// nearest ancestor that handles them (a clickable or draggable row), unless the text has
    /// its own handlers or the point is on one of its links; selecting goes on beside that
    /// (`selectable_at`).
    fn primary_target(&self, x: f32, y: f32) -> Option<usize> {
        let i = self.top_target(x, y)?;
        let n = &self.nodes[i];
        let own = n.on_press.is_some() || n.on_double_press.is_some() || n.on_hover.is_some() || n.on_drag.is_some() || n.drag_out.is_some();
        if !self.is_selectable(i) || own || self.link_at(i, x, y).is_some() { return Some(i); }
        let handles = |n: &Node| n.on_press.is_some() || n.on_double_press.is_some() || n.on_hover.is_some()
            || n.on_drag.is_some() || n.drag_out.is_some() || n.focus.enabled;
        let mut p = n.parent;
        while let Some(a) = p {
            if self.subtree_exiting(a) || !self.allowed(a) || self.nodes[a].modal.is_some() { break; }
            if handles(&self.nodes[a]) { return Some(a); }
            p = self.nodes[a].parent;
        }
        Some(i)
    }

    /// The selectable text under (x, y), if it's the topmost thing there that takes input.
    fn selectable_at(&self, x: f32, y: f32) -> Option<usize> {
        self.top_target(x, y).filter(|&i| self.is_selectable(i))
    }

    pub fn pointer_move(&mut self, x: f32, y: f32) { self.move_pointer(x, y, true); }

    /// `drag_move` false keeps a held drag from getting a `Phase::Move`: a release at the
    /// point the drag last saw has nothing new to report, and its instant transaction would
    /// cut short whatever the press started (a tapped slider's knob mid-spring).
    fn move_pointer(&mut self, x: f32, y: f32, drag_move: bool) {
        if self.pointer != Some((x, y)) { self.hover_paused = false; }
        self.pointer = Some((x, y));
        if self.in_viewport(x, y) { self.left_in_capture = false; }
        for &(id, rect) in &self.hits {
            if self.hit_contains(id, rect, x, y)
                && let Some(&i) = self.node_index.get(&id)
                && self.nodes[i].on_pointer_move.is_some()
            {
                let point = self.pointer_event(i, x, y);
                self.queue.push(Action::PointerMove(self.nodes[i].path.clone(), point));
            }
        }
        self.refresh_hover(x, y);
        let Some((path, d)) = self.capture.clone() else { return };
        if self.drag_out_armed && crate::drag_out::past_threshold(self.press_at, (x, y), self.drag_out_threshold) && self.start_drag_out(path.clone()) {
            return;
        }
        self.selection_drag(x, y);
        let Some(i) = self.nodes.iter().enumerate().position(|(i, n)| n.path == path && !self.subtree_exiting(i)) else { return };
        let (id, r, has_drag) = (self.nodes[i].id, self.nodes[i].target, self.nodes[i].on_drag.is_some() && !self.drags_out(&self.nodes[i]));
        self.press_moved |= (x - self.press_at.0).abs() > CLICK_SLOP || (y - self.press_at.1).abs() > CLICK_SLOP;
        // A hit rect is where the element was drawn (after scrolling); `target` is its
        // unscrolled layout rect, kept only for elements that were never drawn.
        let mut hits = self.hits.iter().filter(|(hit, _)| *hit == id).peekable();
        self.capture_inside = !self.press_cancelled && if self.native_overlay || hits.peek().is_some() {
            hits.any(|&(id, r)| self.hit_contains(id, r, x, y))
        } else { contains(r, x, y) };
        if has_drag && drag_move {
            let (dx, dy) = (x - self.press_at.0, y - self.press_at.1);
            let [lx, ly] = self.to_local(i, x, y);
            let d = Drag { phase: Phase::Move, x: lx, y: ly, dx, dy, ..d };
            self.queue.push(Action::Drag(path, d, self.capture_button));
        }
    }

    /// Overlay observation updates coordinates without presenting unchanged hover pixels.
    pub(crate) fn observe_pointer(&mut self, x: f32, y: f32) -> bool {
        if self.pointer == Some((x, y)) { return false; }
        let visual_changed = self.nodes.iter().enumerate().any(|(i, n)| {
            if n.vis.hover.a <= 0.0 && n.vis.press.a <= 0.0 && n.press_scale.is_none() { return false; }
            let inside = |p: Option<(f32, f32)>| p.is_some_and(|(x, y)| self.node_contains(i, x, y));
            inside(self.pointer) != inside(Some((x, y)))
        });
        // Reaching or leaving a help element starts or ends its tooltip's wait.
        let help_changed = self.help_target_changes(x, y);
        let (queued, capture_inside) = (self.queue.len(), self.capture_inside);
        self.pointer_move(x, y);
        visual_changed || help_changed || self.queue.len() != queued || self.capture_inside != capture_inside
    }

    pub(crate) fn pointer_captured(&self) -> bool { self.capture.is_some() || self.secondary_capture.is_some() }

    /// The host moved its window by (dx, dy) points under a still pointer. Keep the pointer and
    /// a held press's origin in window points, so a drag that moves its own window still reports
    /// pointer travel in `Drag::dx`/`dy`.
    pub(crate) fn window_moved(&mut self, dx: f32, dy: f32) {
        if let Some((x, y)) = self.pointer { self.pointer = Some((x - dx, y - dy)); }
        self.press_at = (self.press_at.0 - dx, self.press_at.1 - dy);
    }

    pub(crate) fn cancel_pointer(&mut self) {
        self.cancel_drag();
        self.drop_drag_out_request();
        self.drag_out_armed = false;
        self.text_sel.dragging = false;
        self.capture = None;
        self.secondary_capture = None;
        self.capture_inside = false;
        self.pointer_leave();
        self.drag_files_leave();
    }

    /// End a held drag without a drop: its handler hears `Phase::Up` with [`Drag::cancelled`]
    /// and the release that follows does nothing. False when no drag is held.
    fn cancel_drag(&mut self) -> bool {
        let Some((path, d)) = &self.capture else { return false };
        let Some(i) = self.nodes.iter().enumerate().position(|(i, n)| &n.path == path && !self.subtree_exiting(i)) else { return false };
        if self.nodes[i].on_drag.is_none() || self.drags_out(&self.nodes[i]) { return false; }
        let (x, y) = self.pointer.unwrap_or(self.press_at);
        let [lx, ly] = self.to_local(i, x, y);
        let (dx, dy) = (x - self.press_at.0, y - self.press_at.1);
        let (path, d) = (path.clone(), Drag { phase: Phase::Up, x: lx, y: ly, dx, dy, cancelled: true, ..*d });
        let button = std::mem::take(&mut self.capture_button);
        self.queue.push(Action::Drag(path, d, button));
        self.capture = None;
        self.capture_inside = false;
        self.double_armed = false;
        self.drag_out_armed = false;
        self.text_sel.dragging = false;
        true
    }

    /// The held press crossed the drag threshold on a `drag_out` element: ask the host for an
    /// OS drag and hand it the pointer, so nothing stays pressed or hovered after the drop.
    /// False when the element offers nothing this time and the press stays a press.
    fn start_drag_out(&mut self, path: String) -> bool {
        self.drag_out_armed = false;
        let Some(i) = self.nodes.iter().enumerate().position(|(i, n)| n.path == path && !self.subtree_exiting(i)) else { return false };
        let Some(f) = self.nodes[i].drag_out.clone() else { return false };
        let Some(item) = f(handler_state_ref(&self.state)).filter(|item| !item.is_empty()) else { return false };
        let n = &self.nodes[i];
        let r = self.drawn.get(i).copied().unwrap_or([n.target[0], n.target[1], n.target[2] - n.target[0], n.target[3] - n.target[1]]);
        let clip = self.screen_clip_of_parent(n.parent).map(|v| v / self.env.scale);
        let (x0, y0) = (r[0].max(clip[0]), r[1].max(clip[1]));
        let rect = [x0, y0, ((r[0] + r[2]).min(clip[2]) - x0).max(0.0), ((r[1] + r[3]).min(clip[3]) - y0).max(0.0)];
        self.events.push(format!("drag-out {path}"));
        self.drop_drag_out_request();
        self.queue.push(Action::DragStart(path.clone()));
        self.drag_out_request = Some(crate::drag_out::DragOutStart { path, item, rect, bounds: r, press: self.press_at });
        self.capture = None;
        self.capture_inside = false;
        self.pointer_leave();
        true
    }

    /// A request no host took (headless tools, a cancelled press) ends as `Cancelled`, queued
    /// behind its start, so `on_drag_start` is always followed by `on_drag_end`.
    fn drop_drag_out_request(&mut self) {
        if let Some(start) = self.drag_out_request.take() {
            self.queue.push(Action::DragEnd(start.path, crate::DragOutcome::Cancelled));
        }
    }

    fn in_viewport(&self, x: f32, y: f32) -> bool {
        contains([0.0, 0.0, self.viewport.0, self.viewport.1], x, y)
    }

    /// A release outside the window, or after the pointer left during the press, ends the hover.
    fn leave_after_release(&mut self) {
        if self.pointer_captured() { return; }
        if std::mem::take(&mut self.left_in_capture) || self.pointer.is_some_and(|(x, y)| !self.in_viewport(x, y)) {
            self.pointer_leave();
        }
    }

    fn refresh_hover(&mut self, x: f32, y: f32) {
        self.refresh_hover_within();
        let hovered = self.primary_target(x, y);
        if hovered.map(|i| self.nodes[i].path.as_str()) == self.hovered.as_deref() { return; }
        self.hovered = hovered.map(|i| self.nodes[i].path.clone());
        if let Some(path) = &self.hovered { self.queue.push(Action::Hover(path.clone())); }
        // A row never takes focus from a text field (a search field above its results).
        let row = hovered.and_then(|i| self.highlight_row(i)).filter(|_| self.focused_input().is_none());
        match row {
            Some(i) if self.focused.as_deref() != Some(&self.nodes[i].path) || !self.hover_focus =>
                self.queue.push(Action::HoverFocus(Some(self.nodes[i].path.clone()))),
            None if self.hover_focus => self.queue.push(Action::HoverFocus(None)),
            _ => {}
        }
    }

    pub fn pointer_down(&mut self, x: f32, y: f32) {
        if self.capture.is_some() && self.capture_button != crate::PointerButton::Primary {
            self.pointer = Some((x, y));
            return;
        }
        self.drop_drag_out_request();
        self.pointer = Some((x, y));
        self.keyboard_focus = false;
        self.dismiss_help();
        // A press cancels the waits to report an enter; the next move after it starts them again.
        self.hover_pending.retain(|p| !p.1);
        self.hover_paused = true;
        let target = self.primary_target(x, y);
        if let Some(i) = self.active_modal()
            && target.is_none_or(|t| !self.allowed(t))
        {
            if self.nodes[i].modal.as_ref().is_some_and(|m| m.outside) {
                self.queue.push(Action::Dismiss(self.nodes[i].path.clone()));
            }
            return;
        }
        self.selection_down(self.selectable_at(x, y), x, y);
        // Inside a draggable row, dragging drags the row; clicks still select words and lines.
        if target.is_some_and(|t| self.nodes[t].on_drag.is_some() || self.nodes[t].drag_out.is_some()) {
            self.text_sel.dragging = false;
        }
        let Some(ti) = target else {
            if self.focused.is_some() {
                self.queue.push(Action::Blur);
            }
            return;
        };
        let on_link = self.link_down(ti, x, y);
        // Any press breaks a pending pair unless it's the second press of it.
        let pending = self.last_press.take();
        let now = self.now_clock();
        let n = &self.nodes[ti];
        if n.window_drag && n.on_press.is_none() && n.on_double_press.is_none() && n.on_drag.is_none() && n.drag_out.is_none() && self.textbox_input(ti).is_none() {
            let double = self.last_drag_press.is_some_and(|(t, px, py)| {
                now - t <= self.double_click_interval && (x - px).abs() <= CLICK_SLOP && (y - py).abs() <= CLICK_SLOP
            });
            self.last_drag_press = if double { None } else { Some((now, x, y)) };
            self.titlebar_action = Some(if double { TitlebarAction::DoubleClick } else { TitlebarAction::Drag });
            if self.focused.is_some() {
                self.queue.push(Action::Blur);
            }
            return;
        }
        self.press_at = (x, y);
        self.double_armed = n.on_double_press.is_some() && pending.is_some_and(|(p, t, px, py)| {
            p == n.path && now - t <= self.double_click_interval && (x - px).abs() <= CLICK_SLOP && (y - py).abs() <= CLICK_SLOP
        });
        self.press_down_t = now;
        self.press_moved = false;
        self.press_cancelled = false;
        if let Some(ji) = self.textbox_input(ti) {
            // Its text drew scaled or rotated with its box: map the press back to the rest layout.
            let [lx, ly] = self.to_local(ji, x, y);
            let jn = &self.nodes[ji];
            self.queue.push(Action::Focus(jn.path.clone(), lx - jn.inset[0], ly - jn.inset[1]));
            return;
        }
        // Clicking a link leaves focus where it is (a composer keeps typing). Selectable text
        // off its links isn't a control: the press blurs, as a click on plain content does.
        // Inside a control that owns its keyboard (`El::on_key`), a click on any part of it
        // focuses it, as a click on a table view's row focuses the table.
        let keyed = (!n.focus.enabled && !self.is_selectable(ti)).then(|| std::iter::successors(n.parent, |&p| self.nodes[p].parent)
            .take_while(|&p| self.allowed(p))
            .find(|&p| self.nodes[p].focus.key.is_some() && self.can_focus(p))).flatten();
        let in_textbox = std::iter::successors(n.parent, |&p| self.nodes[p].parent).any(|p| self.textbox_input(p).is_some());
        let preserve_focus = std::iter::successors(Some(ti), |&p| self.nodes[p].parent)
            .take_while(|&p| self.allowed(p))
            .any(|p| self.nodes[p].focus.preserve_on_press);
        if !on_link && n.focus.enabled && !self.is_selectable(ti) {
            self.queue.push(Action::FocusControl(n.path.clone()));
        } else if !on_link && let Some(p) = keyed {
            self.queue.push(Action::FocusControl(self.nodes[p].path.clone()));
        } else if !on_link && !in_textbox && !preserve_focus && self.focused.is_some() {
            self.queue.push(Action::Blur);
        }
        let r = n.target;
        let [lx, ly] = self.to_local(ti, x, y);
        let d = Drag {
            phase: Phase::Down,
            x: lx,
            y: ly,
            w: r[2] - r[0],
            h: r[3] - r[1],
            start_x: lx,
            start_y: ly,
            dx: 0.0,
            dy: 0.0,
            data: 0.0,
            cancelled: false,
        };
        let path = n.path.clone();
        self.drag_out_armed = self.drags_out(n);
        self.drag_out_threshold = n.drag_threshold.unwrap_or(crate::drag_out::DRAG_THRESHOLD);
        if n.on_drag.is_some() && !self.drag_out_armed {
            self.queue.push(Action::Drag(path.clone(), d, crate::PointerButton::Primary));
        }
        self.capture = Some((path, d));
        self.capture_button = crate::PointerButton::Primary;
        self.capture_inside = true;
    }

    /// The nearest element with `on_secondary_press` at (x, y), else the selectable text there
    /// (its Copy menu): an app's context menu on a row wins over its text's.
    fn secondary_target(&self, x: f32, y: f32) -> Option<&str> {
        let id = self.hits.iter().rev().find(|&&(id, r)| self.hit_contains(id, r, x, y))?.0;
        let mut index = *self.node_index.get(&id)?;
        let mut text = None;
        loop {
            let n = &self.nodes[index];
            if self.subtree_exiting(index) || !self.allowed(index) { break; }
            if n.on_secondary_press.is_some() { return Some(&n.path); }
            if text.is_none() && self.is_selectable(index) { text = Some(n.path.as_str()); }
            let Some(p) = n.parent else { break };
            index = p;
        }
        text
    }

    /// Capture a secondary gesture without activating primary actions or text selection.
    pub fn secondary_down(&mut self, x: f32, y: f32) {
        self.pointer = Some((x, y));
        self.keyboard_focus = false;
        self.dismiss_help();
        self.secondary_capture = self.secondary_target(x, y).map(str::to_owned);
    }

    pub fn secondary_up(&mut self, x: f32, y: f32) {
        self.pointer = Some((x, y));
        if let Some(path) = self.secondary_capture.take()
            && self.secondary_target(x, y) == Some(path.as_str())
            && !self.selection_menu(&path, x, y)
        {
            let i = self.nodes.iter().position(|n| n.path == path);
            let point = i.map_or(PointerEvent { x, y, window_x: x, window_y: y }, |i| self.pointer_event(i, x, y));
            self.queue.push(Action::SecondaryPress(path, point));
        }
        self.leave_after_release();
    }

    pub fn pointer_up(&mut self, x: f32, y: f32) {
        if self.capture.is_some() && self.capture_button != crate::PointerButton::Primary {
            self.pointer = Some((x, y));
            return;
        }
        self.release_capture(x, y);
    }

    pub(super) fn release_capture(&mut self, x: f32, y: f32) {
        // Hosts release at the last position they saw, which a leave doesn't move.
        let left = self.left_in_capture;
        self.release(x, y);
        self.left_in_capture = left;
        self.leave_after_release();
    }

    fn release(&mut self, x: f32, y: f32) {
        // A release never starts a drag-out: the system session needs the button held.
        self.drag_out_armed = false;
        let moved = self.pointer != Some((x, y));
        self.move_pointer(x, y, moved);
        // A drag that selected text is a selection, not a press.
        let selected = self.selected_by_drag();
        self.text_sel.dragging = false;
        let Some((path, d)) = self.capture.take() else { return };
        let button = std::mem::take(&mut self.capture_button);
        self.refresh_hover_within();
        let double = std::mem::take(&mut self.double_armed);
        let Some(i) = self.nodes.iter().enumerate().position(|(i, n)| n.path == path && !self.subtree_exiting(i)) else { return };
        let n = &self.nodes[i];
        let (has_drag, has_press, has_double) = (n.on_drag.is_some() && !self.drags_out(n), n.on_press.is_some(), n.on_double_press.is_some());
        if has_drag {
            let (dx, dy) = (x - self.press_at.0, y - self.press_at.1);
            let [lx, ly] = self.to_local(i, x, y);
            self.queue.push(Action::Drag(path.clone(), Drag { phase: Phase::Up, x: lx, y: ly, dx, dy, ..d }, button));
        }
        self.link_up(&path, x, y);
        if has_press && self.capture_inside && !selected {
            self.queue.push(Action::Press(path.clone(), button));
        }
        // Only clicks pair: a press that dragged or was held past the interval is neither half.
        let click = !self.press_moved && self.now_clock() - self.press_down_t <= self.double_click_interval;
        if has_double && self.capture_inside && click && button == crate::PointerButton::Primary {
            if double {
                self.queue.push(Action::DoublePress(path));
            } else {
                self.last_press = Some((path, self.press_down_t, self.press_at.0, self.press_at.1));
            }
        }
        self.capture_inside = false;
    }

    pub fn pointer_leave(&mut self) {
        self.wheel_lost();
        if self.capture.is_none() && self.secondary_capture.is_none() {
            self.pointer = None;
            self.hovered = None;
            self.left_in_capture = false;
            self.refresh_hover_within();
        } else {
            self.left_in_capture = true;
        }
    }

    /// Queue `on_hover_change` for elements the pointer entered or left.
    fn refresh_hover_within(&mut self) {
        if self.hover_within.is_empty() && self.hover_shown.is_empty() && self.hover_pending.is_empty()
            && !self.hits.iter().any(|(id, _)| self.node_index.get(id).is_some_and(|&i| self.nodes[i].on_hover_change.is_some()))
        {
            return;
        }
        let mut inside: Vec<String> = Vec::new();
        if let Some((x, y)) = self.pointer {
            for &(id, r) in &self.hits {
                if let Some(n) = self.node_index.get(&id).map(|&i| &self.nodes[i])
                    && n.on_hover_change.is_some() && self.hit_contains(id, r, x, y) && !inside.contains(&n.path)
                {
                    inside.push(n.path.clone());
                }
            }
        }
        // A held press stays inside what it started in until it releases.
        if self.capture.is_some() {
            for path in &self.hover_within {
                if !inside.contains(path) && self.nodes.iter().enumerate().any(|(i, n)| n.path == *path && self.allowed(i)) {
                    inside.push(path.clone());
                }
            }
        }
        let delayed = |this: &Self, path: &str| this.node(path).map(|n| n.hover_delay).filter(|d| *d != [0.0; 2]);
        for path in &self.hover_within {
            if !inside.contains(path) && delayed(self, path).is_none() { self.queue.push(Action::HoverChange(path.clone(), false)); }
        }
        for path in &inside {
            if !self.hover_within.contains(path) && delayed(self, path).is_none() { self.queue.push(Action::HoverChange(path.clone(), true)); }
        }
        self.hover_within = inside;
        self.settle_hover_delays();
    }

    /// Starts or drops the waits of `hover_delays` elements whose inside changed.
    fn settle_hover_delays(&mut self) {
        let mut paths: Vec<String> = self.hover_shown.clone();
        paths.extend(self.hover_pending.iter().map(|p| p.0.clone()));
        paths.extend(self.hover_within.iter().filter(|p| self.node(p).is_some_and(|n| n.hover_delay != [0.0; 2])).cloned());
        paths.sort();
        paths.dedup();
        let now = self.wall_clock().unwrap_or(self.clock);
        for path in paths {
            let Some(delay) = self.node(&path).map(|n| n.hover_delay) else {
                self.hover_shown.retain(|p| *p != path);
                self.hover_pending.retain(|p| p.0 != path);
                continue;
            };
            let shown = self.hover_shown.contains(&path);
            // A held button, or a press until the pointer next moves, never starts the wait
            // to report an enter.
            let want = self.window_focused != Some(false) && self.hover_within.contains(&path)
                && (shown || self.capture.is_none() && !self.hover_paused);
            let pending = self.hover_pending.iter().position(|p| p.0 == path);
            if want == shown {
                if let Some(i) = pending { self.hover_pending.remove(i); }
                continue;
            }
            if pending.is_some_and(|i| self.hover_pending[i].1 == want) { continue; }
            if let Some(i) = pending { self.hover_pending.remove(i); }
            let d = delay[if want { 0 } else { 1 }] as f64;
            if d <= 0.0 {
                self.report_hover(path, want);
            } else {
                self.hover_pending.push((path, want, now + d));
                self.schedule_wakes_from(now, [std::time::Duration::from_secs_f64(d)]);
            }
        }
    }

    fn report_hover(&mut self, path: String, inside: bool) {
        self.hover_shown.retain(|p| *p != path);
        if inside { self.hover_shown.push(path.clone()); }
        self.queue.push(Action::HoverChange(path, inside));
    }

    /// Reports the delayed hover changes due by this frame.
    fn fire_hover_delays(&mut self) {
        if self.hover_pending.is_empty() { return; }
        let clock = self.clock + 1e-6;
        let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.hover_pending).into_iter().partition(|p| p.2 <= clock);
        self.hover_pending = rest;
        for (path, inside, _) in due { self.report_hover(path, inside); }
    }

    /// A press on an element is held (the hand drives whatever follows it).
    pub fn pressing(&self) -> bool {
        self.capture.is_some()
    }

    /// Tracks the last tick sampled, by element path and property name: what spent this
    /// frame animating. Tracks of nodes no longer in the tree report their node id.
    pub(crate) fn touched_tracks(&self) -> Vec<(String, &'static str, bool)> {
        let store = self.scene.store();
        let mut out: Vec<_> = store.touched_tracks().map(|(track, node, p)| {
            let path = self.node_index.get(&node).map_or_else(|| format!("node:{}", node.0), |&i| self.nodes[i].path.clone());
            (path, prop_name(p.0), store.is_active(track))
        }).collect();
        out.sort();
        out.dedup();
        out
    }
    pub fn pointer(&self) -> Option<(f32, f32)> {
        self.pointer
    }

    /// Native overlay regions from the most recent frame, in logical window points (x0,y0,x1,y1).
    pub fn hit_regions(&self) -> &[[f32; 4]] { &self.overlay_hits }

    /// The pointer shape the shell should show: the pressed element's while a gesture holds
    /// the pointer, else the one under it in the current frame, from the nearest ancestor that
    /// sets one. The arrow while the pointer is outside the window.
    pub fn cursor(&self) -> crate::el::Cursor {
        let Some((x, y)) = self.pointer.filter(|&(x, y)| self.in_viewport(x, y)) else { return crate::el::Cursor::Default };
        // An element on its way out has no cursor: `Cursor::Hidden` ends when it's removed.
        // Selectable text shows its I-beam even where its presses go to a row around it.
        let target = match &self.capture {
            Some(_) if self.text_sel.dragging => self.selection_holder(),
            Some((p, _)) => (0..self.nodes.len()).find(|&i| self.nodes[i].path == *p && !self.subtree_exiting(i) && self.allowed(i)),
            None => self.selectable_at(x, y).or_else(|| self.primary_target(x, y)),
        };
        let Some(mut i) = target else {
            return crate::el::Cursor::Default;
        };
        if self.nodes[i].cursor.is_none() && (self.nodes[i].input.is_some() || self.nodes[i].role == "textbox") {
            return crate::el::Cursor::Text;
        }
        if self.is_selectable(i) && !self.text_sel.dragging && self.link_at(i, x, y).is_some() {
            return crate::el::Cursor::Pointer;
        }
        loop {
            if let Some(c) = self.nodes[i].cursor {
                return c;
            }
            let Some(parent) = self.nodes[i].parent else { return crate::el::Cursor::Default };
            i = parent;
        }
    }

    pub fn key(&mut self, ev: KeyEvent) {
        self.dismiss_help();
        // Escape during a drag cancels it and goes no further.
        if ev.key == crate::Key::Escape && self.cancel_drag() { return; }
        self.queue.push(Action::Key { ev, repeat: false });
    }

    /// Queue an auto-repeat of a held key, as the OS sends after the first press. Handlers see
    /// `Tx::key_repeat` true for it.
    pub fn repeat_key(&mut self, ev: KeyEvent) {
        self.queue.push(Action::Key { ev, repeat: true });
    }

    pub fn ime(&mut self, ev: Ime) {
        self.queue.push(Action::Ime(ev));
    }

    /// End native composition on window deactivation without discarding text or field focus.
    pub fn cancel_composition(&mut self) {
        self.ime(Ime::Preedit(String::new(), None));
        self.ime_reset = true;
    }

    pub fn paste(&mut self, text: impl Into<String>) {
        self.queue.push(Action::Paste(crate::ClipboardContent::new(text), None));
    }
    /// [`Runtime::paste`] with several forms, as from another app's clipboard: its HTML is
    /// sanitised as a real paste's is. For tests and scripts.
    pub fn paste_content(&mut self, content: crate::ClipboardContent) {
        self.queue.push(Action::Paste(content.untrusted(), None));
    }

    fn app_paste_handler(&self) -> Option<crate::el::PasteFn> {
        self.nodes.get(self.active_modal().unwrap_or(0))?.on_paste.clone()
    }
    pub(crate) fn accepts_paste(&self) -> bool {
        self.paste_target().is_some() || self.app_paste_handler().is_some()
    }
    pub(crate) fn paste_target(&self) -> Option<String> {
        self.focused_input().filter(|i| i.access.is_some()).and(self.focused.clone())
    }
    /// What the paste target hears: every form, images too, for a field with
    /// `on_input_paste` or a root `on_paste_content` handler; text, HTML and RTF for a rich
    /// [`TextEdit`](crate::TextEdit); only the clipboard's text for anything else (including a
    /// plain field or a plain `on_paste`).
    pub(crate) fn paste_forms(&self, target: Option<&str>) -> crate::clipboard::Forms {
        use crate::clipboard::Forms;
        match target {
            Some(path) => match self.node(path).and_then(|n| n.input.as_deref()).or_else(|| self.virtual_inputs.get(path).map(Box::as_ref)) {
                Some(input) if input.paste.is_some() => Forms::All,
                Some(input) if input.edit.is_rich() && !input.edit.is_secure() => Forms::Rich,
                _ => Forms::Text,
            },
            None if self.app_paste_handler().is_some_and(|p| p.rich) => Forms::All,
            None => Forms::Text,
        }
    }
    pub(crate) fn paste_result(&mut self, target: Option<String>, content: crate::ClipboardContent) {
        self.queue.push(Action::Paste(content, Some(target)));
    }
    pub(crate) fn read_result(&mut self, id: u64, content: crate::ClipboardContent) {
        self.queue.push(Action::ClipboardRead(id, content));
    }

    /// Whether the focused editor requires secure native keyboard input.
    pub fn has_secure_focus(&self) -> bool {
        self.focused_input().is_some_and(|i| i.edit.is_secure())
    }

    pub fn has_focus(&self) -> bool {
        self.focused.is_some()
    }

    /// Scrolls so row `key` shows. A virtual row placed by estimates may measure differently
    /// once mounted, along with the rows around it, so this places it again from what the
    /// commit measured until it stays put.
    fn reveal_row(&mut self, scope: Option<&str>, key: &str) {
        if self.reveal_row_once(scope, key, true) {
            for _ in 0..8 {
                if !self.reveal_row_once(scope, key, false) { break; }
            }
        }
        self.reveal_row_x(scope, key);
    }

    /// One placement of [`Self::reveal_row`]: whether it committed. After the first, it moves
    /// only when the row would land somewhere else.
    fn reveal_row_once(&mut self, scope: Option<&str>, key: &str, first: bool) -> bool {
        let geometry = self.nodes.iter().enumerate().find_map(|(i, row)| {
            if !row.named(key) || self.subtree_exiting(i) || !row.parent.is_none_or(|p| in_scope(&self.nodes[p].path, scope)) {
                return None;
            }
            let s = self.viewport_of(i)?;
            let parent = &self.nodes[s];
            let (laid_top, laid_h) = (row.target[1] - parent.target[1], row.local.h);
            let (top, height) = std::iter::successors(Some(i), |&p| self.nodes[p].parent)
                .take_while(|&p| p != s)
                .find_map(|r| {
                    let c = self.nodes[r].parent?;
                    self.nodes[c].growth?;
                    let stack = self.stacks.get(&self.nodes[c].id)?;
                    let rid = self.nodes[r].id;
                    let mut count = 0usize;
                    let before_r: f32 = stack.order.iter()
                        .take_while(|&&id| id != rid)
                        .filter_map(|id| self.grows.get(id))
                        .map(|g| { count += 1; g.h })
                        .sum::<f32>() + stack.gap * count as f32;
                    let r_top = (self.nodes[c].target[1] - parent.target[1]) + stack.pad_top + before_r;
                    let r_h = self.grows.get(&rid).map_or(self.nodes[r].local.h, |g| g.h);
                    if i == r {
                        Some((r_top, r_h))
                    } else {
                        let in_row = row.target[1] - self.nodes[r].target[1];
                        Some((r_top + in_row, row.local.h))
                    }
                })
                .unwrap_or((laid_top, laid_h));
            let laid_end = self.scroll_content(s).map_or(0.0, |c| self.nodes[c].target[3] - parent.target[1]);
            Some((parent.path.clone(), f64::from(top), f64::from(height), f64::from(parent.local.h), f64::from(laid_top), f64::from(laid_h), f64::from(laid_end)))
        }).or_else(|| {
            self.nodes.iter().filter(|n| in_scope(&n.path, scope)).filter_map(|n| {
                let v = self.virtuals.get(&n.path)?;
                let index = v.index_of(key).or_else(|| {
                    key.rmatch_indices('/').find_map(|(at, _)| {
                        let (scope, row_key) = (&key[..at], &key[at + 1..]);
                        let mut keyed = n.path.rsplit('/').filter(|seg| !seg.starts_with('#'));
                        scope.rsplit('/').all(|seg| keyed.next() == Some(seg)).then(|| v.index_of(row_key))?
                    })
                })?;
                let row_key = &v.keys[index];
                let top = f64::from(v.pad_top + v.prefix(index));
                let height = f64::from(v.height(row_key));
                let end = f64::from(v.pad_top + v.prefix(v.keys.len()) + v.pad_bottom);
                Some((v.scroll_path.clone(), top, height, f64::from(v.viewport), top, height, end))
            }).next()
        });
        let Some((path, top, height, viewport, laid_top, laid_h, laid_end)) = geometry else { return false };
        // Stuck headers cover the viewport's top: land the row below them.
        let Some(cover) = self.stuck_cover(&path, key) else { return false };
        let (top, height) = (top - cover, height + cover);
        let (laid_top, laid_h) = (laid_top - cover, laid_h + cover);
        let margin = self.node(&path).map_or(0.0, |n| f64::from(n.scroll_edges.margin));
        let Some(&id) = self.path_ids.get(&path) else { return false };
        let Some(&s_idx) = self.node_index.get(&id) else { return false };
        let stick = self.nodes[s_idx].scroll.is_some_and(|s| s.stick_bottom);
        let max = self.scroll_overflow(s_idx).max(0.0);
        let Some(scroll) = self.scrolls.get_mut(&id) else { return false };
        scroll.max = max;
        if first {
            scroll.v = 0.0;
            scroll.glide = None;
            scroll.glide_to = None;
        }
        let offset = scroll_edge::reveal(scroll.offset, top, height, viewport, margin).clamp(0.0, scroll.max);
        let laid_max = (laid_end - viewport).max(0.0);
        let laid_to = scroll_edge::reveal(scroll.offset, laid_top, laid_h, viewport, margin).clamp(0.0, laid_max);
        let at_end = stick && (scroll.pinned || offset >= scroll.max - 0.5) && laid_to >= laid_max - 0.5;
        if first {
            self.scroll_reveals.retain(|r| r.viewport != path);
            self.scroll_reveals.push(sticky::Reveal {
                scope: scope.map(str::to_owned),
                key: key.to_owned(),
                viewport: path.clone(),
                at_end,
            });
        } else if let Some(r) = self.scroll_reveals.iter_mut().find(|r| r.viewport == path) {
            r.at_end |= at_end;
        }
        if (offset - scroll.offset).abs() < 0.5 {
            if first { scroll.pinned = false; }
            return false;
        }
        scroll.offset = offset;
        scroll.pinned = false;
        scroll.v = 0.0;
        scroll.glide = None;
        scroll.glide_to = None;
        scroll.direct = true;
        let window = self.virtual_window_changed();
        if window {
            self.commit(&Tx::default());
            // The commit measured rows that were estimates: the next placement clamps to the end
            // they make.
            if let Some(i) = self.nodes.iter().position(|n| n.id == id) {
                let max = self.scroll_overflow(i).max(0.0);
                if let Some(scroll) = self.scrolls.get_mut(&id) {
                    scroll.max = max;
                    scroll.pinned = false;
                }
            }
        }
        window
    }

    /// The innermost vertical scroll viewport at (x, y).
    fn scroll_at(&self, x: f32, y: f32) -> Option<NodeId> {
        self.scroll_at_axis(x, y, false)
    }

    /// The innermost scroll viewport at (x, y) that moves on the horizontal axis or the vertical.
    fn scroll_at_axis(&self, x: f32, y: f32, horizontal: bool) -> Option<NodeId> {
        self.nodes
            .iter()
            .enumerate()
            .rev()
            .filter(|(i, n)| n.scroll.is_some() && n.scroll_axes.has(horizontal) && !self.subtree_exiting(*i) && self.allowed(*i))
            .find(|(i, _)| self.node_contains(*i, x, y))
            .map(|(_, n)| n.id)
    }

    /// Wheel or trackpad delta in points (positive `dy` moves content down, like a browser's
    /// negative deltaY). Applied 1:1: the platform supplies its own momentum phases.
    pub fn wheel(&mut self, x: f32, y: f32, dy: f32) {
        self.keeps.clear();
        self.scroll_reveals.clear();
        self.dismiss_help();
        let Some(id) = self.scroll_at(x, y) else { return };
        let stick = self.node_index.get(&id).and_then(|&i| self.nodes[i].scroll).is_some_and(|s| s.stick_bottom);
        if let Some(st) = self.scrolls.get_mut(&id) {
            let before = st.offset;
            st.offset = (st.offset - f64::from(dy)).clamp(0.0, st.max);
            st.v = 0.0;
            st.glide = None;
            st.direct = true;
            st.pinned = stick && st.offset >= st.max - 0.5;
            self.events.push(format!("wheel {dy:.1}"));
            if st.offset != before { self.scroll_cancels_press(id); }
        }
    }

    /// Releases a scroll with `velocity` (points per second, positive moves content down);
    /// the runtime decays it (for platforms without OS momentum and headless scripts).
    pub fn fling(&mut self, x: f32, y: f32, velocity: f32) {
        self.keeps.clear();
        self.scroll_reveals.clear();
        self.dismiss_help();
        let Some(id) = self.scroll_at(x, y) else { return };
        if let Some(st) = self.scrolls.get_mut(&id) {
            st.v = -velocity;
            st.pinned = false;
            st.glide = None;
            self.events.push(format!("fling {velocity:.0}"));
            if velocity != 0.0 { self.scroll_cancels_press(id); }
        }
    }

    pub fn has_pending_input(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Scrolling viewport `id` under a held press inside it cancels the press, as a UIScrollView
    /// cancels its content's touches: the row lets go and the release doesn't press. A drag
    /// keeps its capture.
    fn scroll_cancels_press(&mut self, id: NodeId) {
        let Some((path, _)) = &self.capture else { return };
        let Some(i) = self.nodes.iter().position(|n| &n.path == path) else { return };
        let inside = std::iter::successors(self.nodes[i].parent, |&p| self.nodes[p].parent).any(|p| self.nodes[p].id == id);
        if inside && self.nodes[i].on_drag.is_none() {
            self.press_cancelled = true;
            self.capture_inside = false;
            self.double_armed = false;
        }
    }

    fn update_interaction(&mut self) -> bool {
        let mut changed = false;
        let captured = self.capture.as_ref().map(|(p, _)| p.as_str()).or(self.secondary_capture.as_deref());
        for i in 0..self.nodes.len() {
            let n = &self.nodes[i];
            let tracks_hover = n.vis.hover.a > 0.0 || n.vis.hover_group;
            if self.is_exiting(n.id) || (!tracks_hover && n.vis.press.a <= 0.0 && n.press_scale.is_none()) {
                continue;
            }
            let inside = self.pointer.is_some_and(|(x, y)| self.node_contains(i, x, y));
            // A group stays hovered while one of its descendants holds the press.
            let within = |p: &str| p == n.path || (p.len() > n.path.len() && p.starts_with(&n.path) && p.as_bytes()[n.path.len()] == b'/');
            let capture_inside = if self.secondary_capture.is_some() { inside } else { self.capture_inside };
            let held = captured == Some(n.path.as_str()) && capture_inside;
            let allowed = self.allowed(i);
            // A held press stays hovered while it's inside, though a press scale shrank the
            // drawn rect away from the pointer.
            let hovered = allowed && (inside || held) && tracks_hover && captured.is_none_or(within);
            let pressed = allowed && held && n.vis.press.a > 0.0;
            let press_scale = n.press_scale;
            let id = n.id;
            // Like a native sidebar or menu: the highlight lands with the pointer and
            // fades briefly as it leaves.
            let hover_anim = match n.vis.hover_motion {
                Some((enter, exit)) => if hovered { enter } else { exit },
                None if hovered => Anim::INSTANT,
                None => rune_motion::Curve::tween(0.1, rune_motion::Easing::EASE_OUT).into(),
            };
            changed |= self.scene.store().target(id, P_HOVER).unwrap_or(0.0) != hovered as u8 as f32;
            changed |= self.scene.store().target(id, P_PRESS).unwrap_or(0.0) != pressed as u8 as f32;
            self.scene.animate(id, P_HOVER, hovered as u8 as f32, hover_anim);
            self.scene.animate(id, P_PRESS, pressed as u8 as f32, if pressed { motion::INSTANT } else { motion::SNAPPY });
            if let Some(k) = press_scale {
                // Down follows the hand; the release springs back with a little life.
                let down = allowed && held && !self.reduced_motion;
                self.scene.animate(id, P_PRESS_SCALE, if down { k } else { 1.0 }, if down { motion::INSTANT } else { motion::POP });
            }
        }
        changed
    }

    // ---------------------------------------------------------------------------------------
    // Frame build: Scene Presented → instances

    fn build(&mut self) -> bool {
        let _paint_time = crate::frame_profile::Scope::new(crate::frame_profile::Phase::Paint);
        let s = self.env.scale;
        let snap_px = |v: f32| grid(v, s) * s;
        self.instances.clear();
        let mut pinned_pages = 0u32;
        for f in &self.flights {
            for q in &f.glyphs {
                if q.params[1] == KIND_GLYPH {
                    pinned_pages |= 1u32 << (q.params[2] as u32).min(31);
                }
            }
        }
        self.text.begin_frame_pinned(pinned_pages);
        self.marks.clear();
        self.surfaces.clear();
        self.materials.clear();
        self.hits.clear();
        self.clear_help_hits();
        self.clear_drop_hits();
        self.loops_active = false;
        self.static_scale_settling = false;
        self.videos_active = false;
        let now = crate::time::Instant::now();
        let video_clock = match self.wall_start {
            None => crate::video::Clock::Virtual(self.clock),
            Some(_) => crate::video::Clock::Display(self.frame_target.filter(|t| *t > now).unwrap_or(now)),
        };
        let mut video_signature = 0u64;
        self.loop_wake = None;
        self.overlay_hits.clear();
        let n_nodes = self.nodes.len();
        self.inh_scratch.clear();
        self.turn_scratch.clear();
        self.unturned_drawn.clear();
        self.inh_scratch.resize(n_nodes, Inh::default());
        self.drawn.clear();
        self.drawn.resize(n_nodes, [0.0; 4]);
        self.drawn_scale.clear();
        self.drawn_scale.resize(n_nodes, 1.0);
        self.seen.clear();
        self.seen.resize(n_nodes, false);
        self.stuck.clear();
            self.sticky_frame.clear();
        if self.has_sticky {
            self.stuck.resize(n_nodes, 0.0);
            self.sticky_frame.resize(n_nodes, sticky::UNBOUNDED);
        }
        let sel_ranges = self.selection_ranges();
        self.ime_area = None;
        let now = self.t;
        let vp = [0.0, 0.0, self.viewport.0 * s, self.viewport.1 * s];
        let mut glyph_count = 0usize;
        let mut ambient: Option<f32> = None;
        // Retain scope capacity across frames. Records preserve absolute instance indices
        // for images, video insertions, retained springs and compositor list ranges.
        let mut layers = std::mem::take(&mut self.layer_scratch);
        layers.clear();

        // One linear pass, including retained exits, Live opacity, presence and loops. Reuse
        // inherited scratch for hover groups so activation samples exactly the paint values.
        self.dissolve_scratch.clear();
        if self.has_dissolve { self.dissolve_scratch.resize(n_nodes, false); }
        for index in 0..if self.has_dissolve { n_nodes } else { 0 } {
            let i = self.layer_paint_order.get(index).copied().unwrap_or(index);
            let n = &self.nodes[i];
            let inherited_group = n.parent.map_or(0.0, |p| self.inh_scratch[p].group);
            self.inh_scratch[i].group = if n.vis.hover.a > 0.0 || n.vis.hover_group {
                self.scene.get(n.id, P_HOVER).unwrap_or(0.0).clamp(0.0, 1.0)
            } else { inherited_group };
            let Some(parent) = n.parent.filter(|&p| self.nodes[p].vis.blend == crate::render::Blend::PlusLighter) else { continue };
            let Some(p) = self.scene.presented(n.id) else { continue };
            let mut opacity = p.opacity;
            if let Some([rest, hov]) = n.vis.group_opacity { opacity *= rest + (hov - rest) * inherited_group; }
            if fades(n.looping) && let Some(track) = self.loops.get(&n.id) {
                opacity *= track.sample_at(self.scene.store().node_time(n.id), self.clock, self.reduced_motion);
            }
            self.dissolve_scratch[parent] |= opacity > 0.001 && opacity < 1.0;
        }

        self.cache_live_scratch.clear();
        self.cache_check_scratch.clear();
        self.cache_paint_scratch.clear();
        self.cache_inside_scratch.clear();
        let mut paint_state_hash = 0u64;
        if self.has_cache {
            use std::hash::{Hash, Hasher};
            let mut h = rune_motion::Fx::default();
            self.hovered.hash(&mut h);
            self.hover_within.hash(&mut h);
            self.focused.hash(&mut h);
            self.keyboard_focus.hash(&mut h);
            self.hover_focus.hash(&mut h);
            self.srgb_blend.hash(&mut h);
            h.write_u32(s.to_bits());
            sel_ranges.hash(&mut h);
            let mut scroll_mix = 0u64;
            for (&id, st) in &self.scrolls {
                let mut sh = rune_motion::Fx::default();
                sh.write_u64(id.0);
                sh.write_u64(st.offset.to_bits());
                sh.write_u64(st.x.offset.to_bits());
                sh.write_u32(st.held as u32);
                sh.write_u32(st.slack.height().to_bits());
                scroll_mix = scroll_mix.wrapping_add(sh.finish());
            }
            for (path, &off) in &self.input_scroll {
                let mut sh = rune_motion::Fx::default();
                path.hash(&mut sh);
                sh.write_u32(off.to_bits());
                scroll_mix = scroll_mix.wrapping_add(sh.finish());
            }
            h.write_u64(scroll_mix);
            paint_state_hash = h.finish();

            self.cache_live_scratch.resize(n_nodes, false);
            self.cache_check_scratch.resize(n_nodes, false);
            self.cache_paint_scratch.resize(n_nodes, 0);
            // Only cached subtrees read these, so skip everything outside them.
            self.cache_inside_scratch.resize(n_nodes, false);
            for i in 0..n_nodes {
                let n = &self.nodes[i];
                self.cache_inside_scratch[i] = n.vis.cache || n.parent.is_some_and(|p| self.cache_inside_scratch[p]);
            }
            let store = self.scene.store();
            for i in (0..n_nodes).rev() {
                if !self.cache_inside_scratch[i] { continue; }
                let n = &self.nodes[i];
                let live = store.is_node_animating(n.id)
                    // Sticky and anchored descendants move within a raster without motion tracks.
                    || n.sticky.as_ref().is_some_and(|s| s.edge.is_some())
                    || n.above.is_some()
                    || (!self.reduced_motion && self.loops.get(&n.id).is_some_and(|l| l.active(store.node_time(n.id))))
                    || self.grows.get(&n.id).is_some_and(|g| g.moving())
                    || self.scrolls.get(&n.id).is_some_and(|st| st.slack.height() > 0.0)
                    || self.reveals.get(&n.id).is_some_and(|r| !r.words.is_empty())
                    || self.subtree_exiting(i)
                    || (!self.flights.is_empty() && self.flights.iter().any(|f| f.departs(&n.path, n.key.as_deref()) || f.lands(&n.path, n.key.as_deref())))
                    || n.vis.effect.is_some()
                    // A publication can replace texels without moving the atlas slot, so
                    // identical image primitives do not imply identical raster contents.
                    || n.vis.pixels.is_some()
                    || n.vis.surface.is_some()
                    || (n.material.is_some() && self.native_materials)
                    // Never retain a raster of a secret, even when the field is unfocused.
                    || n.input.as_ref().is_some_and(|inp| inp.edit.focused || inp.edit.is_secure())
                    || n.text.as_ref().is_some_and(|t| t.advance_in.is_some());
                let check = n.text.as_ref().is_some_and(|t| t.live.is_some())
                    || n.live.iter().any(|b| !n.vis.cache || !matches!(b.property, crate::live::Property::Offset | crate::live::Property::Scale));
                self.cache_live_scratch[i] |= live;
                self.cache_check_scratch[i] |= check;
                // Hover, press and link paint land without a commit (an instant track or a
                // pointer move inside one text), so hash what this frame samples.
                let links = n.text.as_ref().is_some_and(|t| !t.links.is_empty());
                if n.vis.hover.a > 0.0 || n.vis.hover_group || n.vis.press.a > 0.0 || n.press_scale.is_some() || links {
                    let mut ph = rune_motion::Fx::default();
                    ph.write_u64(n.id.0);
                    for prop in [P_HOVER, P_PRESS, P_PRESS_SCALE] {
                        ph.write_u32(self.scene.get(n.id, prop).unwrap_or(0.0).to_bits());
                    }
                    if links { self.link_states(i).hash(&mut ph); }
                    self.cache_paint_scratch[i] = self.cache_paint_scratch[i].wrapping_add(ph.finish());
                }
                if let Some(p) = n.parent {
                    self.cache_paint_scratch[p] = self.cache_paint_scratch[p].wrapping_add(self.cache_paint_scratch[i]);
                    // A lifted descendant draws outside this raster; don't elide its inputs.
                    self.cache_live_scratch[p] |= self.cache_live_scratch[i] || n.vis.raise;
                    self.cache_check_scratch[p] |= self.cache_check_scratch[i] || !n.live.is_empty();
                }
            }
        }

        // Growth rows: presented top offset, height and bottom feather mask per node index.
        self.row_dy.clear();
        self.row_dy.resize(n_nodes, 0.0);
        self.row_h.clear();
        self.row_h.resize(n_nodes, -1.0);
        self.row_mask.clear();
        self.row_mask.resize(n_nodes, -1.0);
        for (cid, stack) in &self.stacks {
            let Some(&ci) = self.node_index.get(cid) else { continue };
            let Some(spec) = self.nodes[ci].growth else { continue };
            let origin = self.virtuals.get(&self.nodes[ci].path).map_or(0.0, |v| v.before);
            let mut y = stack.pad_top - origin;
            let mut first_row = true;
            for id in &stack.order {
                let Some(g) = self.grows.get(id) else { continue };
                if !first_row { y += stack.gap; }
                first_row = false;
                let top_rel = stack.tops.get(id).copied().unwrap_or(0.0) - origin;
                let cur = y;
                if let Some(&ri) = self.node_index.get(id) {
                    self.row_dy[ri] = cur - top_rel;
                    self.row_h[ri] = g.h;
                    let withheld = if g.streaming { (self.nodes[ri].local.h - g.target).max(0.0) } else { 0.0 };
                    let rewrap = stack.pending_heights.contains_key(id) && self.nodes[ri].local.h > g.h + 0.01;
                    if spec.edge.clip && (g.h < g.target - 0.01 || g.mask > 0.01 || withheld > 0.01 || rewrap) {
                        self.row_mask[ri] = g.mask.max(withheld.min(spec.edge.floor)).max(if rewrap { spec.edge.floor } else { 0.0 });
                    }
                }
                y = cur + g.h;
            }
            // The stack itself presents the rows' heights, which scrolling follows.
            self.row_h[ci] = y + stack.pad_bottom;
        }

        // Exiting descendants are appended to storage. Bring them back into their subtree
        // only when isolation requires it; the ordinary painter stream stays byte-identical.
        // Matched flights paint last (see `update_layer_paint_order`).
        let flying = !self.flying.is_empty();
        let tree_order = ((flying || self.has_sticky || self.has_raise) && !self.layer_paint_order.is_empty()) || self.layer_paint_order.windows(2).any(|pair| {
            let n = &self.nodes[pair[0]];
            self.nodes[pair[1]].parent == Some(pair[0]) && ((self.has_dissolve && self.dissolve_scratch[pair[0]]) || n.vis.cache || n.vis.clip_radius.is_some() || n.vis.group_opacity.is_some()
                || fades(n.looping)
                || self.scene.presented(n.id).is_some_and(|p| (p.opacity > 0.0 && p.opacity < 1.0) || p.blur > 0.0 || p.transform.rotate.abs() > 1e-6))
        });
        let default_ring = self.default_ring_owner();
        let mut active_cache: Option<ActiveCache> = None;
        for paint_index in 0..n_nodes {
            let i = if tree_order { self.layer_paint_order[paint_index] } else { paint_index };
            let node_id = self.nodes[i].id;
            let node_parent = self.nodes[i].parent;
            // A flight paints outside every ancestor's layer, above the whole window.
            let lift = flying && self.flying.iter().any(|f| f.0 == node_id);
            let raised = self.nodes[i].vis.raise && !lift;
            if lift || raised {
                while let Some((_, start, surface_start)) = layers.pop() {
                    if active_cache.is_some_and(|ac| ac.instance_start == start) {
                        let ac = active_cache.take().unwrap();
                        self.close_cached_layer(ac, start, surface_start, &mut glyph_count);
                    } else {
                        crate::render::end_layer(&mut self.instances, &mut self.surfaces, &mut self.marks, start, surface_start);
                    }
                }
            }
            while let Some(&(ancestor, start, surface_start)) = layers.last() {
                let mut parent = node_parent;
                while parent.is_some_and(|p| p != ancestor) { parent = self.nodes[parent.unwrap()].parent; }
                if parent == Some(ancestor) { break; }
                if active_cache.is_some_and(|ac| ac.instance_start == start) {
                    let ac = active_cache.take().unwrap();
                    self.close_cached_layer(ac, start, surface_start, &mut glyph_count);
                } else {
                    crate::render::end_layer(&mut self.instances, &mut self.surfaces, &mut self.marks, start, surface_start);
                }
                layers.pop();
            }
            let n = &self.nodes[i];
            let Some(p) = self.scene.presented(n.id) else { continue };
            let mut pinh = n.parent.map_or(Inh::default(), |pr| self.inh_scratch[pr]);
            pinh.dy += self.row_dy[i];
            if let Some(a) = &n.above
                && let Some(j) = self.nodes[..i].iter().position(|n| n.key.as_ref() == Some(&a.key))
            {
                let d = self.drawn[j];
                let target = if d != [0.0; 4] { [d[0], d[1], d[0] + d[2], d[1] + d[3]] }
                    else { sticky::anchored_target(&self.nodes, &self.scrolls, j, self.viewport) };
                let (ax, ay) = anchor_position(a, target, (p.rect.w, p.rect.h), self.viewport);
                // Placed from the presented size and anchor, so drop the scene's own layout
                // position (this node's `prop::FRAME` x/y and its ancestors' origins) while
                // keeping this node's own `prop::OFFSET` and transition offset: with the frame
                // position spring left in, an End-aligned box growing in width would start from
                // its old x and its end edge would swing out and back.
                let w = self.scene.world(n.id).unwrap_or(p.rect);
                pinh.dx = ax - w.x;
                pinh.dy = ay - w.y;
                pinh.scale = 1.0;
                pinh.anchor = [0.0; 2];
                pinh.anchor_to = [0.0; 2];
            }
            if self.has_sticky && let Some((edge, inset)) = n.sticky.as_ref().and_then(|s| Some((s.edge?, s.inset))) {
                // Worked out from this frame's offsets, which the viewport above already applied.
                // Exclude this node's own `prop::OFFSET` (`live_offset`) so it translates from the
                // stuck place rather than being clamped away by the sticky edge.
                let mut frame = n.parent.map_or(sticky::UNBOUNDED, |pr| self.sticky_frame[pr]);
                let mut ancestor = n.parent;
                let mut viewport = None;
                while let Some(a) = ancestor {
                    let nd = &self.nodes[a];
                    let horizontal = edge == crate::el::StickyEdge::Left;
                    if nd.scroll.is_some() && if horizontal { nd.scroll_axes.x() } else { nd.scroll_axes.y() } {
                        viewport = Some(a);
                        break;
                    }
                    // A section nested inside a growth row must stop at that row's presented
                    // bottom, not its descendants' final layout height. Only sticky nodes
                    // need this ancestor walk; ordinary painting keeps its existing fast path.
                    if !horizontal && self.row_h[a] >= 0.0 {
                        frame[2] = frame[2].max(self.sticky_frame[a][2]);
                        frame[3] = frame[3].min(self.sticky_frame[a][3]);
                    }
                    ancestor = nd.parent;
                }
                let own_off = self.scene.get(n.id, prop::OFFSET).unwrap_or(rune_motion::Vec2::ZERO);
                let (top, left) = (p.rect.y - own_off.y + pinh.dy, p.rect.x - own_off.x + pinh.dx);
                let h = if self.row_h[i] >= 0.0 { self.row_h[i] } else { p.rect.h };
                let shift = sticky::shift(edge, inset, [top, top + h, left, left + p.rect.w], frame);
                self.stuck[i] = shift;
                if edge == crate::el::StickyEdge::Left { pinh.dx += shift } else { pinh.dy += shift }
                // A sticky element stays crisp against the viewport edge it sticks to while rows
                // scrolling under its fill fade out.
                let slot = match edge {
                    crate::el::StickyEdge::Left => 0,
                    crate::el::StickyEdge::Top => 1,
                    crate::el::StickyEdge::Bottom => 3,
                };
                if let Some(viewport) = viewport.filter(|&v| self.nodes[v].scroll_edges.fade > 0.0) {
                    let (width, origin) = self.sticky_fade(n.parent.unwrap(), viewport, slot, false);
                    pinh.fade[slot] = width;
                    pinh.fade_origin[slot] = origin;
                    let (width, origin) = self.sticky_fade(n.parent.unwrap(), viewport, slot, true);
                    pinh.paint_fade[slot] = width;
                    pinh.paint_fade_origin[slot] = origin;
                }
            }
            if raised {
                // Out of its ancestors' layers: their opacity applies directly instead.
                pinh.paint_opacity = pinh.opacity;
                pinh.paint_clip = pinh.clip;
                pinh.paint_fade = pinh.fade;
                pinh.paint_fade_origin = pinh.fade_origin;
                pinh.in_cache = false;
                pinh.skip_paint = false;
                // Rounded clips and turns consumed by an ancestor's layer must be reapplied
                // outside that layer too. Reopen only the masks and turns, not opacity or blur.
                pinh.rounded = None;
                let mut chain = Vec::new();
                let mut parent = node_parent;
                while let Some(p) = parent {
                    chain.push(p);
                    parent = self.nodes[p].parent;
                }
                for &p in chain.iter().rev() {
                    let ancestor = &self.nodes[p];
                    let k = self.inh_scratch[p].turn;
                    if k != 0 && ancestor.parent.map_or(0, |pp| self.inh_scratch[pp].turn) != k {
                        let t = self.turn_scratch[k as usize - 1];
                        let start = self.instances.len();
                        let marker = Instance::begin_layer(t.rect, t.outer_clip, Default::default()).turned(t.angle, t.pivot);
                        self.instances.push(marker);
                        layers.push((p, start, self.surfaces.len()));
                    }
                    if let Some(radius) = ancestor.vis.clip_radius {
                        let [x, y, w, h] = self.unturned_drawn.get(p).filter(|_| k != 0)
                            .map_or_else(|| self.drawn[p].map(|v| v * s), |&(b, _)| [b[0] * s, b[1] * s, (b[2] - b[0]) * s, (b[3] - b[1]) * s]);
                        let radii = ancestor.vis.clip_corners.unwrap_or_else(|| {
                            if ancestor.vis.radius != [0.0; 4] {
                                self.scene.presented(ancestor.id).map_or(ancestor.vis.radius, |p| p.radius.0)
                            } else { [radius; 4] }
                        }).map(|radius| {
                            let radius = (radius.max(0.0) * self.inh_scratch[p].scale * s).min(w.min(h) * 0.5);
                            if ancestor.vis.continuous_corners { -radius } else { radius }
                        });
                        let start = self.instances.len();
                        let mut marker = Instance::begin_layer([x, y, w, h], [x, y, x + w, y + h], Default::default());
                        marker.round_clip_corners(radii);
                        self.instances.push(marker);
                        layers.push((p, start, self.surfaces.len()));
                    }
                }
            }
            if lift {
                // Keep the destination's clips (consumed ones included) and offsets. Drop its
                // ancestors' opacity, scale and blur: an entering parent's fade or scale would
                // otherwise hide the flight or jump it off the rect it was seen at. Inside another
                // flight, also drop that flight's cover offset and clip.
                let [clip, fade, fade_origin] = pinh.outer.unwrap_or_else(|| {
                    let clip = if pinh.turn != 0 { self.screen_clip_of_parent(n.parent) } else { pinh.clip };
                    [clip, pinh.fade, pinh.fade_origin]
                });
                pinh = Inh { opacity: 1.0, paint_opacity: 1.0, clip, fade, fade_origin, paint_clip: clip, paint_fade: fade,
                    paint_fade_origin: fade_origin, rounded: None, scale: 1.0, anchor: [0.0; 2], anchor_to: [0.0; 2], blur: 0.0,
                    dx: pinh.dx - pinh.cover[0], dy: pinh.dy - pinh.cover[1], cover: [0.0; 2], outer: None,
                    scale_motion: ScaleMotion::Still, in_cache: false, skip_paint: false, turn: 0, ..pinh };
            }
            let mark = if n.vis.mark != 0 { n.vis.mark } else { pinh.mark };
            let mut own = p.opacity;
            if let Some([rest, hov]) = n.vis.group_opacity {
                own *= rest + (hov - rest) * pinh.group;
            }
            let base_opacity = (own * pinh.opacity).clamp(0.0, 1.0);
            let group = if n.vis.hover.a > 0.0 || n.vis.hover_group {
                self.scene.get(n.id, P_HOVER).unwrap_or(0.0).clamp(0.0, 1.0)
            } else {
                pinh.group
            };
            // Loops and reveals run on the node's clock (`El::time_scale`).
            let lnow = self.scene.store().node_time(n.id);
            let loop_value = self.loops.get(&n.id).map(|l| l.sample_at(lnow, self.clock, self.reduced_motion));
            if fades(n.looping) && let Some(v) = loop_value {
                own *= v;
            }
            let effective_opacity = (own * pinh.opacity).clamp(0.0, 1.0);
            let next = if tree_order { self.layer_paint_order.get(paint_index + 1).copied() } else { (i + 1 < n_nodes).then_some(i + 1) };
            let dissolve = self.has_dissolve && self.dissolve_scratch[i];
            let additive_parent = self.has_dissolve && n.parent.is_some_and(|p| self.dissolve_scratch[p]);
            let mut isolate = effective_opacity > 0.001 && (p.blur > 0.0 || dissolve
                || additive_parent || (own > 0.001 && own < 1.0
                    && next.is_some_and(|j| self.nodes[j].parent == Some(i))));
            let (mut clip, mut fade) = (pinh.paint_clip, pinh.paint_fade);
            let mut fade_origin = pinh.paint_fade_origin;
            let mut rounded = pinh.rounded;
            let press_k = if n.press_scale.is_some() { self.scene.get(n.id, P_PRESS_SCALE).unwrap_or(1.0) } else { 1.0 };
            let sequence = self.loops.get(&n.id)
                .filter(|_| matches!(n.looping.map(|l| l.kind), Some(rune_motion::LoopKind::Sequence(_))));
            let static_k = if n.scale_track { self.scene.get(n.id, P_SCALE).unwrap_or(n.static_scale) } else { n.static_scale };
            let own_sc = p.transform.scale.x * press_k * static_k
                * sequence.and_then(|l| l.pose(lnow, self.reduced_motion)).map_or(1.0, |p| p.scale);
            // An `El::scale` or `El::live_scale` an app drives from a slider, wheel or pinch
            // counts as moving until it holds still, then rasterizes crisp at its resting scale.
            let has_live_scale = n.live.iter().any(|b| b.property == crate::live::Property::Scale);
            let tracked_scale = if has_live_scale { static_k * p.transform.scale.x } else { static_k };
            let scale_settling = if tracked_scale != 1.0 || has_live_scale || self.static_scales.contains_key(&n.id) {
                let t = self.t;
                let seen = self.static_scales.entry(n.id).or_insert((tracked_scale, f64::NEG_INFINITY, false));
                if self.settled {
                    *seen = (tracked_scale, f64::NEG_INFINITY, seen.2);
                } else if (seen.0 - tracked_scale).abs() > 1e-5 {
                    *seen = (tracked_scale, t, true);
                }
                let moving = t - seen.1 < STATIC_SCALE_SETTLE;
                if !has_live_scale {
                    self.static_scale_settling |= moving;
                }
                moving
            } else { false };
            let store = self.scene.store();
            let scale_animating = store.is_animating(n.id, prop::SCALE) || store.is_animating(n.id, prop::PRESENCE)
                || (n.press_scale.is_some() && store.is_animating(n.id, P_PRESS_SCALE))
                || (n.scale_track && store.is_animating(n.id, P_SCALE))
                || sequence.is_some_and(|l| !self.reduced_motion && l.active(lnow));
            let scale_motion = pinh.scale_motion.max(
                if !has_live_scale && (scale_animating || scale_settling) {
                    ScaleMotion::Moving
                } else if has_live_scale && (scale_settling || scale_animating) {
                    ScaleMotion::Live
                } else {
                    ScaleMotion::Still
                });
            // Scale about this node's centre where its ancestors' scales put it, so a scaled child
            // of a scaled parent stays in place.
            let sc = pinh.scale * own_sc;
            let pivot = match n.vis.origin {
                Some([ox, oy]) => [p.rect.x + ox * p.rect.w, p.rect.y + oy * p.rect.h],
                None => [p.anchor.x, p.anchor.y],
            };
            let centre = [pivot[0] + pinh.dx, pivot[1] + pinh.dy];
            let centre_to = [pinh.anchor_to[0] + (centre[0] - pinh.anchor[0]) * pinh.scale,
                pinh.anchor_to[1] + (centre[1] - pinh.anchor[1]) * pinh.scale];
            let (anchor, anchor_to) = if (own_sc - 1.0).abs() > 1e-5 { (centre, centre_to) } else { (pinh.anchor, pinh.anchor_to) };
            // A pressed element still hits at its resting size, so a press near its edge stays inside.
            let press_anchor = centre_to;
            let blur = pinh.blur + p.blur * sc;
            // An effect slot's bleed can reach into view from a rect outside it.
            let filter_margin = blur * s * 3.0 + n.vis.effect.map_or(0.0, |(bleed, _)| bleed * sc * s);

            let mut raw_edges = p.rect.edges();
            raw_edges[0] += pinh.dx;
            raw_edges[2] += pinh.dx;
            raw_edges[1] += pinh.dy;
            raw_edges[3] += pinh.dy;
            if self.row_h[i] >= 0.0 {
                raw_edges[3] = raw_edges[1] + self.row_h[i];
            }
            if self.has_sticky {
                let up = n.parent.map_or(sticky::UNBOUNDED, |pr| self.sticky_frame[pr]);
                // Content scrolls without end on a viewport's scrolling axes; non-scrolling axes
                // bound sticky children to this node's content box.
                let sc_y = n.scroll.is_some() && n.scroll_axes.y();
                let sc_x = n.scroll.is_some() && n.scroll_axes.x();
                let v = if sc_y {
                    [raw_edges[1], raw_edges[3], f32::NEG_INFINITY, f32::INFINITY]
                } else {
                    [up[0], up[1], raw_edges[1] + n.inset[1], raw_edges[3] - n.inset[3]]
                };
                let x = if sc_x {
                    [raw_edges[0], raw_edges[2], f32::NEG_INFINITY, f32::INFINITY]
                } else {
                    [up[4], up[5], raw_edges[0] + n.inset[0], raw_edges[2] - n.inset[2]]
                };
                self.sticky_frame[i] = [v[0], v[1], v[2], v[3], x[0], x[1], x[2], x[3]];
            }
            // A product of 1 (0.5 inside 2.0) can still move the box.
            let mut rect_pt = if (sc - 1.0).abs() > 1e-5 || anchor != anchor_to {
                [
                    anchor_to[0] + (raw_edges[0] - anchor[0]) * sc,
                    anchor_to[1] + (raw_edges[1] - anchor[1]) * sc,
                    anchor_to[0] + (raw_edges[2] - anchor[0]) * sc,
                    anchor_to[1] + (raw_edges[3] - anchor[1]) * sc,
                ]
            } else {
                raw_edges
            };
            // Land a sub-quantum size residual before snapping both edges. Otherwise a
            // translating/scaling box can alternate between neighbouring widths at rest.
            for (axis, target) in [n.local.w, n.local.h].into_iter().enumerate() {
                let target = grid(target, s);
                if ((rect_pt[axis + 2] - rect_pt[axis]) - target).abs() * s < 1.0 / 32.0 {
                    rect_pt[axis + 2] = rect_pt[axis] + target;
                }
            }
            // A matched flight scales its laid-out content uniformly to cover the moving rect,
            // centred: `k` and the centring offset in points.
            let cover = if lift && n.local.w > 0.0 && n.local.h > 0.0 {
                let (pw, ph) = (rect_pt[2] - rect_pt[0], rect_pt[3] - rect_pt[1]);
                let k = (pw / n.local.w).max(ph / n.local.h).max(1e-3);
                Some((k, [(pw - n.local.w * k) * 0.5, (ph - n.local.h * k) * 0.5]))
            } else { None };
            let r = rect_pt.map(snap_px);
            let (w, h) = ((r[2] - r[0]).max(0.0), (r[3] - r[1]).max(0.0));
            let intersect = |a: [f32; 4], b: [f32; 4]| [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])];
            // `El::rotate`: this subtree draws into a layer turned about its origin.
            // The layer takes the clips from above; the node's own clips apply inside, upright.
            let own_turn = p.transform.rotate;
            let own_turn = (own_turn != 0.0 && own_turn.is_finite() && own_turn.abs() > 1e-6 && !lift).then_some(own_turn);
            let turn_pivot = [centre_to[0] * s, centre_to[1] * s];
            let (node_turn, turn_index) = if pinh.turn == 0 && own_turn.is_none() {
                (None, 0)
            } else {
                let mut outer_clip = intersect(pinh.clip, clip);
                if let Some((mask, _)) = rounded { outer_clip = intersect(outer_clip, mask); }
                compose_turn(&mut self.turn_scratch, pinh.turn, own_turn, ([r[0], r[1], w, h], turn_pivot, outer_clip))
            };
            if let Some(angle) = own_turn {
                if !pinh.skip_paint {
                    open_turned_layer(&mut self.instances, &mut layers, (i, self.surfaces.len()), [r[0], r[1], w, h],
                        (angle, turn_pivot), (&mut clip, &mut fade, &mut fade_origin, &mut rounded));
                } else {
                    (clip, fade, fade_origin, rounded) = (NO_CLIP, [0.0; 4], NO_CLIP, None);
                }
                pinh.clip = NO_CLIP;
                pinh.fade = [0.0; 4];
                pinh.fade_origin = NO_CLIP;
            }
            // Where the node shows on screen, upright: the turned box's bounds.
            let mut screen_r = [r[0], r[1], r[0] + w, r[1] + h];
            if let Some(t) = node_turn { screen_r = t.bbox(screen_r); }
            self.drawn[i] = [screen_r[0] / s, screen_r[1] / s, (screen_r[2] - screen_r[0]) / s, (screen_r[3] - screen_r[1]) / s];
            if cover.is_none() { self.drawn_scale[i] = sc; }
            let radii = p.radius.0.map(|v| v * sc * s);

            if !p.mask.is_none() {
                let ins = p.mask.inset.0.map(|v| v * sc * s);
                let fea = p.mask.feather.0.map(|v| v * sc * s);
                clip = [
                    (r[0] + ins[0]).max(clip[0]),
                    (r[1] + ins[1]).max(clip[1]),
                    (r[2] - ins[2]).min(clip[2]),
                    (r[3] - ins[3]).min(clip[3]),
                ];
                inherit_fade(&mut fade, &mut fade_origin, [r[0] + ins[0], r[1] + ins[1], r[2] - ins[2], r[3] - ins[3]], fea);
            }
            let m = filter_margin + 2.0;
            let visible = match node_turn {
                None => {
                    let vis_clip = if pinh.in_cache { intersect(pinh.clip, clip) } else { clip };
                    screen_r[2] >= vis_clip[0].max(vp[0]) - m
                        && screen_r[0] <= vis_clip[2].min(vp[2]) + m
                        && screen_r[3] >= vis_clip[1].max(vp[1]) - m
                        && screen_r[1] <= vis_clip[3].min(vp[3]) + m
                }
                Some(t) => {
                    let lc = intersect(pinh.clip, clip);
                    let oc = t.outer_clip;
                    r[2] >= lc[0] - m && r[0] <= lc[2] + m && r[3] >= lc[1] - m && r[1] <= lc[3] + m
                        && screen_r[2] >= oc[0].max(vp[0]) - m
                        && screen_r[0] <= oc[2].min(vp[2]) + m
                        && screen_r[3] >= oc[1].max(vp[1]) - m
                        && screen_r[1] <= oc[3].min(vp[3]) + m
                }
            };
            self.seen[i] = visible;
            let text_tint = |c: Color| match n.vis.group_color {
                Some(g) if pinh.group > 0.0 => c.mix(g, pinh.group),
                _ => c,
            };

            // A slide may sweep wholly outside its clip; judge visibility at its resting place.
            let home = match n.looping.map(|l| l.kind) {
                Some(rune_motion::LoopKind::Slide { .. } | rune_motion::LoopKind::SlideEased { .. }) => {
                    let o = self.scene.get(n.id, prop::OFFSET).unwrap_or(rune_motion::Vec2::ZERO);
                    [r[0] - o.x * s, r[1] - o.y * s, r[2] - o.x * s, r[3] - o.y * s]
                }
                _ => r,
            };
            // A loop at its brightest under one 8-bit step can't be seen: let it sleep.
            let loop_vis = match node_turn {
                None => home[2] > clip[0].max(vp[0] - filter_margin) && home[0] < clip[2].min(vp[2] + filter_margin)
                    && home[3] > clip[1].max(vp[1] - filter_margin) && home[1] < clip[3].min(vp[3] + filter_margin),
                Some(t) => {
                    let lc = intersect(pinh.clip, clip);
                    let sh = t.bbox(home);
                    let oc = t.outer_clip;
                    home[2] > lc[0] - filter_margin && home[0] < lc[2] + filter_margin
                        && home[3] > lc[1] - filter_margin && home[1] < lc[3] + filter_margin
                        && sh[2] > oc[0].max(vp[0] - filter_margin) && sh[0] < oc[2].min(vp[2] + filter_margin)
                        && sh[3] > oc[1].max(vp[1] - filter_margin) && sh[1] < oc[3].min(vp[3] + filter_margin)
                }
            };
            if self.surface_visible && base_opacity >= 1.0 / 255.0 && !self.reduced_motion
                && loop_vis
                && let Some(track) = self.loops.get(&n.id)
            {
                let node_k = self.scene.store().node_time_scale(n.id) as f64;
                if let Some(dt) = track.frame_interval().filter(|_| track.active(lnow)) {
                    ambient = Some(ambient.map_or(dt as f32, |a| a.min(dt as f32)));
                    let wake = self.clock + dt;
                    if self.loop_wake.is_none_or(|w| wake < w.0) {
                        self.loop_wake = Some((wake, now + dt * self.time_scale as f64));
                    }
                } else if self.time_scale > 0.0 && node_k > 0.0 {
                    self.loops_active |= track.active(lnow);
                }
                if self.time_scale > 0.0 && node_k > 0.0 && let Some(start) = track.pending_start(lnow) {
                    let window_start = now + (start - lnow) / node_k;
                    let wake = self.clock + (window_start - now) / self.time_scale as f64;
                    if self.loop_wake.is_none_or(|w| wake < w.0) { self.loop_wake = Some((wake, window_start)); }
                }
            }
            if n.vis.cache && self.cache_live_scratch[i] {
                if let Some(entry) = self.node_caches.get_mut(&n.id) {
                    entry.was_live = true;
                }
            }
            let mut skip_paint = pinh.skip_paint;
            let mut in_cache = pinh.in_cache;
            let mut open_cache = false;
            // A scale changed by view commits may accompany arbitrary content edits. Draw
            // live until it rests instead of trusting a partial content hash or allocating a
            // new raster on each commit. Live-only transforms still composite the old raster.
            let mut committed_scale_moving = false;
            if n.vis.cache && let Some(entry) = self.node_caches.get_mut(&n.id)
                && entry.commit_gen != self.commit_gen
                && (sc * s - entry.raster_scale).abs() > 1e-4
            {
                if (sc * s - entry.seen_scale).abs() > 1e-4 {
                    entry.seen_scale = sc * s;
                    entry.scale_changed_at = now;
                }
                committed_scale_moving = now - entry.scale_changed_at < STATIC_SCALE_SETTLE;
                if committed_scale_moving {
                    entry.was_live = true;
                    self.static_scale_settling = true;
                }
            }
            if n.vis.cache && active_cache.is_none() && !lift && !skip_paint && !self.cache_live_scratch[i]
                && !committed_scale_moving && own_turn.is_none()
                && effective_opacity > 0.001 && w > 0.0 && h > 0.0
            {
                let eff_scale = sc * s;
                let est_bounds = if let Some(entry) = self.node_caches.get(&n.id) {
                    let k = eff_scale / entry.raster_scale.max(1e-4);
                    [
                        r[0] + entry.rel_bounds[0] * k,
                        r[1] + entry.rel_bounds[1] * k,
                        r[0] + entry.rel_bounds[2] * k,
                        r[1] + entry.rel_bounds[3] * k,
                    ]
                } else {
                    let sh = n.vis.shadows.iter().enumerate().fold(0.0f32, |m, (k, sh)| {
                        let sh = self.shadow_at(n.id, k, sh);
                        m.max((sh.x.abs() + sh.y.abs() + sh.spread.max(0.0) + sh.blur * 3.0) * eff_scale)
                    });
                    [r[0] - sh, r[1] - sh, r[2] + sh, r[3] + sh]
                };
                let cache_vis = match node_turn {
                    None => est_bounds[2] >= clip[0].max(vp[0]) - m
                        && est_bounds[0] <= clip[2].min(vp[2]) + m
                        && est_bounds[3] >= clip[1].max(vp[1]) - m
                        && est_bounds[1] <= clip[3].min(vp[3]) + m,
                    Some(t) => {
                        let lc = intersect(pinh.clip, clip);
                        let sb = t.bbox(est_bounds);
                        let oc = t.outer_clip;
                        est_bounds[2] >= lc[0] - m && est_bounds[0] <= lc[2] + m
                            && est_bounds[3] >= lc[1] - m && est_bounds[1] <= lc[3] + m
                            && sb[2] >= oc[0].max(vp[0]) - m && sb[0] <= oc[2].min(vp[2]) + m
                            && sb[3] >= oc[1].max(vp[1]) - m && sb[1] <= oc[3].min(vp[3]) + m
                    }
                };
                if !cache_vis {
                    skip_paint = true;
                } else {
                    let est_w = (est_bounds[2] - est_bounds[0]).ceil();
                    let est_h = (est_bounds[3] - est_bounds[1]).ceil();
                    if est_w <= 4096.0 && est_h <= 4096.0 {
                        open_cache = true;
                    }
                }
            }
            // A negative clip radius rounds with continuous corners (`render::corner_extent`).
            let mut own_round = n.vis.clip_corners.or_else(|| {
                n.vis.clip_radius.map(|radius| {
                    if n.vis.radius != [0.0; 4] {
                        p.radius.0
                    } else {
                        [radius; 4]
                    }
                })
            }).map(|radii| {
                let half = w.min(h) * 0.5;
                let radii = radii.map(|radius| {
                    let r = (radius.max(0.0) * sc * s).min(half);
                    if n.vis.continuous_corners && r > 0.0 { -r } else { r }
                });
                (r, radii)
            });
            let mut hit_clip = intersect(pinh.clip, clip);
            if let Some((rect, _)) = own_round { hit_clip = intersect(hit_clip, rect); }
            let (mut hit_fade, mut hit_origin) = (pinh.fade, pinh.fade_origin);
            inherit_fade(&mut hit_fade, &mut hit_origin, fade_origin, fade);
            // A rectangle wholly in either central strip misses all four corner squares.
            // Its hard edges already exclude everything the rounded mask would remove.
            for (idx, round) in [&mut rounded, &mut own_round].into_iter().enumerate() {
                if open_cache && idx == 1 { continue; }
                if let Some((mask, radii)) = *round {
                    let e = radii.map(crate::render::corner_extent);
                    let (left, right, top, bottom) = (e[0].max(e[3]), e[1].max(e[2]), e[0].max(e[1]), e[2].max(e[3]));
                    let cut = intersect(clip, mask);
                    if cut[2] <= cut[0] || cut[3] <= cut[1]
                        || (cut[0] >= mask[0] + left && cut[2] <= mask[2] - right)
                        || (cut[1] >= mask[1] + top && cut[3] <= mask[3] - bottom) {
                        clip = cut;
                        *round = None;
                    }
                }
            }
            // The packed ABI carries one clip rectangle. Preserve a rectangular cut through
            // a rounded corner on a separate outer scope, never recenter the rounded mask.
            if !skip_paint && let Some((mask, _)) = rounded.or(own_round)
                && intersect(clip, mask) != mask {
                let mut marker = Instance::begin_layer([r[0], r[1], w, h], clip, Default::default());
                marker.fade = crate::render::pack_fade(fade, fade_origin, clip);
                layers.push((i, self.instances.len(), self.surfaces.len()));
                self.instances.push(marker);
                clip = NO_CLIP; fade = [0.0; 4]; fade_origin = NO_CLIP;
            }
            // Two rounded rectangles cannot generally be represented by one rounded rectangle.
            // Consume the ancestor on an outer composite, retaining both original corner centres.
            if !skip_paint && rounded.is_some() && own_round.is_some() && rounded != own_round {
                let (mask, radii) = rounded.take().unwrap();
                let mut marker = Instance::begin_layer([r[0], r[1], w, h], mask, Default::default());
                marker.fade = crate::render::pack_fade(fade, fade_origin, mask);
                marker.round_clip_corners(radii);
                layers.push((i, self.instances.len(), self.surfaces.len()));
                self.instances.push(marker);
                clip = NO_CLIP; fade = [0.0; 4]; fade_origin = NO_CLIP;
            }
            if let Some(mask) = own_round { rounded = Some(mask); }
            if let Some((mask, _)) = rounded {
                clip = intersect(clip, mask);
            }
            // Live/truncated text adds a glyph-only rectangle below. Consume the inherited
            // rounded mask first so the text clip cannot move its corner centres.
            isolate |= open_cache || (rounded.is_some() && (n.text.as_ref().is_some_and(|t| t.truncate_fade || t.live.is_some())
                || n.input.as_ref().is_some_and(|i| i.single_line)));
            let o = if isolate { 1.0 } else { (own * pinh.paint_opacity).clamp(0.0, 1.0) };
            if !skip_paint && isolate {
                let start_idx = self.instances.len();
                layers.push((i, start_idx, self.surfaces.len()));
                let mut marker = Instance::begin_layer([r[0], r[1], w, h], clip,
                    crate::render::LayerSpec { opacity: own * pinh.paint_opacity,
                        blend: if dissolve { crate::render::Blend::PlusLighter } else { crate::render::Blend::SourceOver },
                        blur: p.blur * sc * s, clip_radius: 0.0 });
                marker.fade = crate::render::pack_fade(fade, fade_origin, clip);
                if let Some((_, radii)) = rounded { marker.round_clip_corners(radii); }
                self.instances.push(marker);
                // Clip the completed layer, never its inputs (including off-wall blur taps).
                clip = NO_CLIP; fade = [0.0; 4]; fade_origin = NO_CLIP; rounded = None;
                if open_cache {
                    in_cache = true;
                    let eff_scale = sc * s;
                    let paint_hash = {
                        use std::hash::Hasher;
                        let mut h = rune_motion::Fx::default();
                        h.write_u64(paint_state_hash);
                        h.write_u64(self.cache_paint_scratch[i]);
                        h.write_u32(pinh.group.to_bits());
                        h.finish()
                    };
                    let (raster_scale, seen_scale, scale_changed_at, elided, cached_glyphs) =
                        if let Some(entry) = self.node_caches.get(&n.id) {
                            let is_resident = self.text.atlas.resident_caches.get(&n.id.0) == Some(&entry.signature);
                            let env_same = (s - entry.env_scale).abs() <= 1e-4 && !self.settled;
                            let (seen_scale, scale_changed_at) = if !env_same {
                                (eff_scale, f64::NEG_INFINITY)
                            } else if (eff_scale - entry.seen_scale).abs() > 1e-4 {
                                (eff_scale, now)
                            } else {
                                (entry.seen_scale, entry.scale_changed_at)
                            };
                            let scale_diff = (eff_scale - entry.raster_scale).abs() > 1e-4;
                            let same_commit = self.commit_gen == entry.commit_gen;
                            let scale_settling = same_commit && scale_diff && env_same && is_resident && !entry.was_live
                                && (now - scale_changed_at < STATIC_SCALE_SETTLE || scale_motion == ScaleMotion::Moving);
                            self.static_scale_settling |= scale_settling;
                            // A commit can change any paint property. Never use a partial
                            // model hash to skip checking the actual emitted draw output.
                            let content_same = same_commit && (!scale_diff || scale_settling);
                            let can_elide = is_resident
                                && !entry.was_live
                                && content_same
                                && paint_hash == entry.paint_state_hash
                                && !self.cache_check_scratch[i];
                            let raster_scale = if can_elide && scale_settling { entry.raster_scale } else { eff_scale };
                            (raster_scale, seen_scale, scale_changed_at, can_elide, entry.glyphs)
                        } else {
                            (eff_scale, eff_scale, f64::NEG_INFINITY, false, 0)
                        };
                    if elided {
                        skip_paint = true;
                    }
                    active_cache = Some(ActiveCache {
                        node_index: i,
                        node_id: n.id,
                        instance_start: start_idx,
                        origin: [r[0], r[1]],
                        size: [w, h],
                        cur_scale: eff_scale,
                        raster_scale,
                        seen_scale,
                        scale_changed_at,
                        env_scale: s,
                        paint_hash,
                        glyph_start: glyph_count,
                        cached_glyphs,
                        elided,
                    });
                }
            }
            let primitive_start = self.instances.len();
            let surface_start = self.surfaces.len();
            // Include native surface geometry, which lives beside the primitive stream.
            if !skip_paint && n.vis.surface.is_some() {
                for &(_, at, _) in &layers {
                    let b = &mut self.instances[at].rect;
                    let hi = [(b[0] + b[2]).max(r[2].min(clip[2])), (b[1] + b[3]).max(r[3].min(clip[3]))];
                    b[0] = b[0].min(r[0].max(clip[0])); b[1] = b[1].min(r[1].max(clip[1]));
                    b[2] = hi[0] - b[0]; b[3] = hi[1] - b[1];
                }
            }

            let first_instance = self.instances.len();
            let mut faded_slot = None;
            // Native input readiness follows focus, not entrance opacity. An input
            // mounting at opacity zero still needs its IME area in this frame.
            let input_caret = n.input.as_ref().filter(|input| input.edit.focused && self.can_focus(i))
                .filter(|_| !self.flights.iter().any(|f| n.key.as_deref() == Some(&f.source_key)
                    && f.alphas(f.destination, now, self.reduced_motion).0 > 0.0))
                .and_then(|_| input_caret(n, r, sc, s, &mut self.input_scroll));
            if let Some((_, area)) = input_caret
                && n.input.as_ref().is_some_and(|input| !input.edit.is_secure()) { self.ime_area = Some(area); }
            let paint_visible = if in_cache {
                r[2] >= clip[0] - filter_margin - 2.0
                    && r[0] <= clip[2] + filter_margin + 2.0
                    && r[3] >= clip[1] - filter_margin - 2.0
                    && r[1] <= clip[3] + filter_margin + 2.0
            } else {
                visible
            };
            if !skip_paint && effective_opacity > 0.001 && o > 0.001 && paint_visible {
                // Disabled styling is distributed to each authored paint leaf at build time.
                // Keep this local to emission: it neither propagates to children nor opens scopes.
                let o = o * n.vis.paint_alpha;
                let srgb = self.srgb_blend;
                // Subtree blur runs once after rasterization, never on each primitive as well.
                let blur = 0.0;
                let k_px = sc * s;
                let native_material = match n.material {
                    // A dimmed-out element takes no native view: its fallback draws nothing.
                    Some(material) if self.materials.len() < crate::MAX_NATIVE_MATERIALS
                        && effective_opacity * n.vis.paint_alpha > 0.001 => {
                        let radius = crate::system_material::clamp_radius(radii, w, h);
                        let continuous = n.vis.continuous_corners;
                        self.materials.push(crate::system_material::MaterialDraw {
                            id: n.id, material, rect: [r[0], r[1], w, h], radius, continuous,
                            opacity: effective_opacity * n.vis.paint_alpha,
                        });
                        self.native_materials.then_some(if continuous { -radius } else { radius })
                    }
                    _ => None,
                };
                let stuck_k = if self.has_sticky { (self.stuck[i].abs() / sticky::SHADOW_RAMP).min(1.0) } else { 0.0 };
                let stuck_shadows: (f32, &[crate::el::Shadow]) = match &n.sticky {
                    Some(s) if stuck_k > 0.0 => (stuck_k, &s.shadows),
                    _ => (0.0, &[]),
                };
                for (k, sh) in n.vis.shadows.iter().chain(stuck_shadows.1).enumerate() {
                    let c = if k >= n.vis.shadows.len() { sh.color.alpha(sh.color.a * stuck_shadows.0) }
                        else if k < P_SHADOWS.len() { self.color(n.id, P_SHADOWS[k], sh.color) } else { sh.color };
                    if c.a <= 0.0 || sh.inset {
                        continue;
                    }
                    let sh = &if k < n.vis.shadows.len() { self.shadow_at(n.id, k, sh) } else { *sh };
                    let sp = sh.spread * k_px;
                    let rect = [r[0] + sh.x * k_px - sp, r[1] + sh.y * k_px - sp, w + 2.0 * sp, h + 2.0 * sp];
                    let sradii = radii.map(|v| if v > 0.0 { (v + sp).max(0.0) } else { 0.0 });
                    if sh.blur + blur <= 0.0 {
                        // A crisp shadow: a ring when it sits exactly around the box, else a solid
                        // offset copy (hidden under an opaque element, as in CSS).
                        let ring = sh.x == 0.0 && sh.y == 0.0;
                        if ring && sp <= 0.0 {
                            continue;
                        }
                        self.instances.push(Instance {
                            rect,
                            radii: sradii,
                            fill: if ring { [0.0; 4] } else { premul(c, o, srgb) },
                            border: if ring { premul(c, o, srgb) } else { [0.0; 4] },
                            params: [if ring { sp } else { 0.0 }, KIND_RECT, 0.0, 1.0],
                            uv: [0.0; 4],
                            clip,
                            fade: crate::render::pack_fade(fade, clip, clip),
                        });
                        continue;
                    }
                    // Over a native material, the shadow leaves the material's interior clear:
                    // `uv` holds the hole relative to the shadow's rect, `radii[3]` its radius.
                    let (radii, uv) = match native_material {
                        Some(hole) => ([sradii[0], sradii[1], sradii[2], hole], [r[0] - rect[0], r[1] - rect[1], w, h]),
                        None => (sradii, [0.0; 4]),
                    };
                    self.instances.push(Instance {
                        rect,
                        radii,
                        fill: premul(c, o, srgb),
                        border: [0.0; 4],
                        params: [0.0, KIND_SHADOW, ((sh.blur + blur) * s * 0.5).max(0.5), 1.0],
                        uv,
                        clip,
                        fade: crate::render::pack_fade(fade, clip, clip),
                    });
                }
                // Shadows keep circular corners; the element's own shapes take the sign, and
                // negative radii draw continuous corners (shader.wgsl `sd_rounded`).
                let radii = if n.vis.continuous_corners { radii.map(|v| -v) } else { radii };
                let mut overlay = [0.0f32; 4];
                if let Some(c) = n.focus.highlight && self.highlight_visible(i) {
                    overlay = over(premul(c, 1.0, srgb), overlay);
                }
                if n.vis.hover.a > 0.0 || self.scene.store().is_animating(n.id, P_HOVER_C) {
                    let hov_v = self.scene.get(n.id, P_HOVER).unwrap_or(0.0).clamp(0.0, 1.0);
                    if hov_v > 0.0 {
                        overlay = over(premul(self.color(n.id, P_HOVER_C, n.vis.hover), hov_v, srgb), overlay);
                    }
                }
                if n.vis.press.a > 0.0 || self.scene.store().is_animating(n.id, P_PRESS_C) {
                    let prs_v = self.scene.get(n.id, P_PRESS).unwrap_or(0.0).clamp(0.0, 1.0);
                    if prs_v > 0.0 {
                        overlay = over(premul(self.color(n.id, P_PRESS_C, n.vis.press), prs_v, srgb), overlay);
                    }
                }
                let base = match n.material {
                    _ if native_material.is_some() => [0.0; 4],
                    Some(_) => premul(self.color(n.id, P_FILL, crate::system_material::fallback_fill(n.vis.fill, self.env.system_dark)), 1.0, srgb),
                    None => premul(self.color(n.id, P_FILL, n.vis.fill), 1.0, srgb),
                };
                let layered = n.vis.gradient.is_some() || n.vis.radial.is_some() || n.vis.surface.is_some();
                let fill = if layered { base } else { over(overlay, base) }.map(|c| c * o);
                let bw = n.vis.border_width * sc * s;
                let border = if bw > 0.0 { premul(self.color(n.id, P_BORDER, n.vis.border), o, srgb) } else { [0.0; 4] };
                // A gradient border draws as KIND_GRADIENT with a width: `border` top, `uv` bottom.
                // After a switch back to `border`, it stays a gradient until both stops meet.
                let store = self.scene.store();
                let springing = store.has(n.id, P_BORDER_BOTTOM)
                    && (store.is_animating(n.id, P_BORDER_BOTTOM) || store.is_animating(n.id, P_BORDER));
                let border_bottom = (bw > 0.0 && (n.vis.border_bottom.is_some() || springing))
                    .then(|| premul(self.color(n.id, P_BORDER_BOTTOM, n.vis.border_bottom.unwrap_or(n.vis.border)), o, srgb))
                    .filter(|b| n.vis.border_bottom.is_some() || *b != border);
                let (border_kind, border_uv) = border_bottom.map_or((KIND_RECT, [0.0; 4]), |b| (KIND_GRADIENT, b));
                let border_a = border[3].max(border_uv[3]);
                // A dashed ring is its own instance over the fill, which then draws without a ring.
                let dashed = n.vis.border_dash.filter(|_| bw > 0.0 && border_a > 0.0005).map(|[dash, gap]| Instance {
                    rect: [r[0], r[1], w, h],
                    radii,
                    fill: border,
                    border: border_bottom.unwrap_or(border),
                    params: [bw, KIND_DASHED, blur * s, 1.0],
                    uv: [dash * k_px, gap * k_px, n.vis.border_dash_phase * k_px, 0.0],
                    clip,
                    fade: crate::render::pack_fade(fade, clip, clip),
                });
                let (bw, border, border_kind, border_uv, border_a) =
                    if n.vis.border_dash.is_some() { (0.0, [0.0; 4], KIND_RECT, [0.0; 4], 0.0) } else { (bw, border, border_kind, border_uv, border_a) };
                if layered {
                    // Fill, gradient and overlays first, then the border on top, as in CSS.
                    if fill[3] > 0.0005 {
                        self.instances.push(Instance {
                            rect: [r[0], r[1], w, h],
                            radii,
                            fill,
                            border: [0.0; 4],
                            params: [0.0, KIND_RECT, blur * s, 1.0],
                            uv: [0.0; 4],
                            clip,
                            fade: crate::render::pack_fade(fade, clip, clip),
                        });
                    }
                    let g = n.vis.gradient.unwrap_or_default();
                    let top = premul(self.color(n.id, P_GRADIENT[0], g[0]), o, srgb);
                    let bottom = premul(self.color(n.id, P_GRADIENT[1], g[1]), o, srgb);
                    if top[3] > 0.0005 || bottom[3] > 0.0005 {
                        self.instances.push(Instance {
                            rect: [r[0], r[1], w, h],
                            radii,
                            fill: top,
                            border: [0.0; 4],
                            params: [0.0, KIND_GRADIENT, 0.0, 1.0],
                            uv: bottom,
                            clip,
                            fade: crate::render::pack_fade(fade, clip, clip),
                        });
                    }
                    if let Some(rg) = &n.vis.radial {
                        let segs = rg.stops.len().saturating_sub(1);
                        let rmax = radii[0].max(radii[1]).max(radii[2]).max(radii[3]).min(w * 0.5).min(h * 0.5);
                        let pad = 1.0 + blur * s * 2.0;
                        for seg in 0..segs {
                            let c0 = rg.stops[seg].1;
                            let c0 = if seg < P_RADIAL.len() { self.color(n.id, P_RADIAL[seg], c0) } else { c0 };
                            let c1 = rg.stops[seg + 1].1;
                            let c1 = if seg + 1 < P_RADIAL.len() { self.color(n.id, P_RADIAL[seg + 1], c1) } else { c1 };
                            let c0 = premul(c0, o, srgb);
                            let c1 = premul(c1, o, srgb);
                            if c0[3] <= 0.0005 && c1[3] <= 0.0005 {
                                continue;
                            }
                            let t0_val = rg.stops[seg].0;
                            let t1_val = rg.stops[seg + 1].0;
                            let (seg_rect, seg_radii, uv) = match rg.geometry {
                                crate::el::GradientGeometry::Radial { center, radius } => {
                                    let cx = center[0] * w;
                                    let cy = center[1] * h;
                                    let rx = (radius[0] * k_px).max(1e-3);
                                    let ry = (radius[1] * k_px).max(1e-3);
                                    if seg + 1 < segs {
                                        let mut lx0 = (cx - t1_val * rx - pad).floor().max(0.0);
                                        let mut ly0 = (cy - t1_val * ry - pad).floor().max(0.0);
                                        let mut lx1 = (cx + t1_val * rx + pad).ceil().min(w);
                                        let mut ly1 = (cy + t1_val * ry + pad).ceil().min(h);
                                        if rmax > 0.0 {
                                            if lx0 < rmax && (ly0 < rmax || ly1 > h - rmax) { lx0 = 0.0; }
                                            if lx1 > w - rmax && (ly0 < rmax || ly1 > h - rmax) { lx1 = w; }
                                            if ly0 < rmax && (lx0 < rmax || lx1 > w - rmax) { ly0 = 0.0; }
                                            if ly1 > h - rmax && (lx0 < rmax || lx1 > w - rmax) { ly1 = h; }
                                            if lx0 == 0.0 && (ly0 == 0.0 || ly1 == h) { lx1 = lx1.max((2.0 * rmax).min(w)); }
                                            if lx1 == w && (ly0 == 0.0 || ly1 == h) { lx0 = lx0.min((w - 2.0 * rmax).max(0.0)); }
                                            if ly0 == 0.0 && (lx0 == 0.0 || lx1 == w) { ly1 = ly1.max((2.0 * rmax).min(h)); }
                                            if ly1 == h && (lx0 == 0.0 || lx1 == w) { ly0 = ly0.min((h - 2.0 * rmax).max(0.0)); }
                                        }
                                        if lx1 <= lx0 || ly1 <= ly0 {
                                            continue;
                                        }
                                        let br = [
                                            if lx0 == 0.0 && ly0 == 0.0 { radii[0] } else { 0.0 },
                                            if lx1 == w && ly0 == 0.0 { radii[1] } else { 0.0 },
                                            if lx1 == w && ly1 == h { radii[2] } else { 0.0 },
                                            if lx0 == 0.0 && ly1 == h { radii[3] } else { 0.0 },
                                        ];
                                        ([r[0] + lx0, r[1] + ly0, lx1 - lx0, ly1 - ly0], br, [cx - lx0, cy - ly0, rx, ry])
                                    } else {
                                        ([r[0], r[1], w, h], radii, [cx, cy, rx, ry])
                                    }
                                }
                                crate::el::GradientGeometry::Linear { angle } => {
                                    let (sin, cos) = angle.sin_cos();
                                    let (dx, dy) = (sin, -cos);
                                    let span = (dx.abs() * w + dy.abs() * h).max(1e-3);
                                    ([r[0], r[1], w, h], radii, [dx / span, dy / span, 0.0, 0.0])
                                }
                            };
                            let t0 = t0_val + if seg > 0 { 2.0 } else { 0.0 };
                            let t1 = t1_val + if seg + 1 < segs { 2.0 } else { 0.0 };
                            self.instances.push(Instance {
                                rect: seg_rect,
                                radii: seg_radii,
                                fill: c0,
                                border: c1,
                                params: [t0, KIND_RADIAL, blur * s, t1],
                                uv,
                                clip,
                                fade: crate::render::pack_fade(fade, clip, clip),
                            });
                        }
                    }
                    if let Some((surface, fit)) = &n.vis.surface {
                        if let Some(pulled) = surface.pull_video(video_clock, &self.live_wake) {
                            self.videos_active |= pulled.active;
                            video_signature = video_signature.wrapping_mul(0x100_0000_01b3).wrapping_add(pulled.generation ^ surface.key() as u64);
                        }
                        self.surfaces.push(crate::native_surface::SurfaceDraw {
                            surface: surface.clone(), fit: *fit, before_instance: self.instances.len() as u32,
                            rect: [r[0], r[1], w, h], radii, clip, fade: crate::render::pack_fade(fade, fade_origin, clip), opacity: o,
                        });
                    }
                    let ov = overlay.map(|c| c * o);
                    if ov[3] > 0.0005 || border_a > 0.0005 {
                        self.instances.push(Instance {
                            rect: [r[0], r[1], w, h],
                            radii,
                            fill: ov,
                            border,
                            params: [bw, border_kind, blur * s, 1.0],
                            uv: border_uv,
                            clip,
                            fade: crate::render::pack_fade(fade, clip, clip),
                        });
                    }
                } else if fill[3] > 0.0005 || border_a > 0.0005 {
                    self.instances.push(Instance {
                        rect: [r[0], r[1], w, h],
                        radii,
                        fill,
                        border,
                        params: [bw, border_kind, blur * s, 1.0],
                        uv: border_uv,
                        clip,
                        fade: crate::render::pack_fade(fade, clip, clip),
                    });
                }
                self.instances.extend(dashed);
                for (k, sh) in n.vis.shadows.iter().chain(stuck_shadows.1).enumerate() {
                    if !sh.inset {
                        continue;
                    }
                    let c = if k >= n.vis.shadows.len() { sh.color.alpha(sh.color.a * stuck_shadows.0) }
                        else if k < P_SHADOWS.len() { self.color(n.id, P_SHADOWS[k], sh.color) } else { sh.color };
                    if c.a <= 0.0 {
                        continue;
                    }
                    let sh = &if k < n.vis.shadows.len() { self.shadow_at(n.id, k, sh) } else { *sh };
                    self.instances.push(Instance {
                        rect: [r[0], r[1], w, h],
                        radii,
                        fill: premul(c, o, srgb),
                        border: [0.0; 4],
                        params: [sh.spread * k_px, KIND_INSET, if sh.blur > 0.0 { (sh.blur * s * 0.5).max(0.5) } else { 0.0 }, 1.0],
                        uv: [sh.x * k_px, sh.y * k_px, 0.0, 0.0],
                        clip,
                        fade: crate::render::pack_fade(fade, clip, clip),
                    });
                }
                if let Some((bleed, backdrop)) = n.vis.effect {
                    let slot = Instance {
                        rect: [r[0], r[1], w, h],
                        radii,
                        fill: [0.0, 0.0, 0.0, 1.0],
                        border: [0.0; 4],
                        params: [if backdrop { 1.0 } else { 0.0 }, KIND_EFFECT, bleed * k_px, k_px],
                        uv: [0.0; 4],
                        clip,
                        fade: crate::render::pack_fade(fade, clip, clip),
                    };
                    // A compositor paints whatever it likes at a slot, so a faded slot gets its
                    // own layer that applies the opacity, instead of a factor it could forget.
                    if o < 1.0 {
                        let mut marker = Instance::begin_layer(slot.rect, clip, crate::render::LayerSpec { opacity: o, ..Default::default() });
                        marker.fade = slot.fade;
                        self.instances.push(marker);
                        faded_slot = Some(self.instances.len());
                        self.instances.push(Instance { clip: NO_CLIP, ..slot });
                        self.instances.push(Instance::end_layer());
                    } else {
                        self.instances.push(slot);
                    }
                }
                let focus_ring = if default_ring == Some(i) { Some(self.default_focus_ring()) }
                    else { self.focus_ring_visible(i).then_some(n.focus.ring).flatten() };
                if let Some(ol) = focus_ring.or(n.vis.outline) {
                    let c = if focus_ring.is_some() { ol.color } else { self.color(n.id, P_OUTLINE, ol.color) };
                    let g = (ol.offset + ol.width) * k_px;
                    if c.a > 0.0 && ol.width > 0.0 {
                        self.instances.push(Instance {
                            rect: [r[0] - g, r[1] - g, w + 2.0 * g, h + 2.0 * g],
                            radii: radii.map(|v| if v != 0.0 { v + g.copysign(v) } else { 0.0 }),
                            fill: [0.0; 4],
                            border: premul(c, o, srgb),
                            params: [ol.width * k_px, KIND_RECT, 0.0, 1.0],
                            uv: [0.0; 4],
                            clip,
                            fade: crate::render::pack_fade(fade, clip, clip),
                        });
                    }
                }
                if let Some(shape) = &n.vis.shape {
                    let k = match shape.view_box {
                        Some([vw, vh]) => [w / vw, h / vh],
                        None => [k_px, k_px],
                    };
                    push_shape(&mut self.instances, shape, [r[0], r[1]], k, k_px, o, srgb, clip, fade);
                }
                if let Some(pixels) = &n.vis.pixels {
                    let (rect, crop) = crate::pixels::fit(n.vis.pixels_fit, [r[0], r[1], w, h], pixels.size());
                    let span = [crop[2] - crop[0], crop[3] - crop[1]];
                    // The mip covers the whole image's drawn size, not just its visible window.
                    if let Some(image) = self.images.place(pixels, [rect[2] / span[0], rect[3] / span[1]]) {
                        let [x, y, iw, ih] = image.rect.map(|v| v as f32);
                        // A letterboxed image rounds its own, smaller corners.
                        let half = rect[2].min(rect[3]) * 0.5;
                        self.instances.push(Instance {
                            rect,
                            radii: if n.vis.pixels_fit.is_some() { radii.map(|v| v.min(half)) } else { radii },
                            fill: [0.0, 0.0, 0.0, o],
                            border: [0.0; 4],
                            params: [image.page as f32, KIND_IMAGE, image.premultiplied as u8 as f32, 1.0],
                            // The shader samples from a whole-texel origin; the span may be fractional.
                            uv: [x + (crop[0] * iw).round(), y + (crop[1] * ih).round(), span[0] * iw, span[1] * ih],
                            clip,
                            fade: crate::render::pack_fade(fade, clip, clip),
                        });
                    }
                }
                if let Some((ic, c)) = n.vis.icon {
                    let color = text_tint(self.color(n.id, P_TEXT, c));
                    let looped = if matches!(n.looping.map(|l| l.kind), Some(rune_motion::LoopKind::Rotation)) { loop_value.unwrap_or(0.0) } else { 0.0 };
                    let turn = looped + if self.scene.store().has(n.id, P_ICON_TURN) {
                        self.scene.get(n.id, P_ICON_TURN).unwrap_or(n.vis.icon_turn)
                    } else { n.vis.icon_turn };
                    let tk = if cover.is_some() { 1.0 } else { sc };
                    let raster = if cover.is_some() { 1.0 } else { scale_motion.raster(sc) };
                    if cover.is_none() && scale_motion == ScaleMotion::Live {
                        self.static_scale_settling = true;
                    }
                    let zoom = tk / raster.max(1e-4);
                    let drawn_px = w.min(h).round().max(1.0);
                    let px = (w.min(h) / zoom.max(1e-4)).round().max(1.0) as u32;
                    if let Some(q) = self.text.icon_quad(ic, px, [r[0], r[1]]) {
                        let (uv, page) = glyph_uv_page(q.uv);
                        let span = if (px as f32 - drawn_px).abs() > 0.5 { px as f32 } else { 0.0 };
                        self.instances.push(Instance {
                            rect: [q.rect[0], q.rect[1], drawn_px, drawn_px],
                            radii: [0.0; 4],
                            fill: premul(color, o, srgb),
                            border: [0.0; 4],
                            params: [turn, KIND_GLYPH, page, 1.0],
                            uv: [uv[0], uv[1], span, span],
                            clip,
                            fade: crate::render::pack_fade(fade, clip, clip),
                        });
                    }
                } else if let (Some(spec), Some(layout)) = (&n.text, &n.text_layout) {
                    let color = text_tint(self.color(n.id, P_TEXT, spec.color));
                    // A matched flight places its own text in its scaled content box. Its glyphs keep
                    // their rest size so a label doesn't pop at the swap.
                    let (zk, zr) = match cover {
                        Some((k, c)) => (k, [r[0] + c[0] * s, r[1] + c[1] * s,
                            r[0] + (c[0] + n.local.w * k) * s, r[1] + (c[1] + n.local.h * k) * s]),
                        None => (sc, r),
                    };
                    // Everywhere else text scales with its box (`El::scale`, `live_scale`,
                    // `press_scale`, transitions): `tk` about the text's origin. It rasterizes at
                    // `raster` and draws filtered by the rest, `tk / raster`.
                    let tk = if cover.is_some() { 1.0 } else { sc };
                    let raster = if cover.is_some() { 1.0 } else { scale_motion.raster(sc) };
                    if cover.is_none() && scale_motion == ScaleMotion::Live {
                        self.static_scale_settling = true;
                    }
                    let ins = n.inset.map(|v| v * zk * s);
                    let (x0, x1) = (zr[0] + ins[0], zr[2] - ins[2]);
                    let mut origin = [text_x(spec, x0, x1, (layout.full_width() * tk).ceil()), zr[1] + ins[1]];
                    let mut clip = clip;
                    if let Some(inp) = n.input.as_ref().filter(|i| i.single_line) {
                        let scroll = single_line_scroll(&mut self.input_scroll, &n.path, inp, &spec.content, layout, s, (x1 - x0) / tk.max(1e-4)) * tk;
                        origin[0] -= scroll;
                        if layout.full_width() * tk > x1 - x0 {
                            clip = [clip[0].max(x0), clip[1], clip[2].min(x1), clip[3]];
                        }
                    }
                    // Paragraph block backgrounds, quote bars and list markers.
                    if spec.blocks.iter().any(|&b| b != crate::edit::Block::Paragraph) {
                        emit_block_decorations(
                            &mut self.instances,
                            &mut self.text,
                            &mut self.images,
                            &mut self.glyph_scratch,
                            spec,
                            layout,
                            origin,
                            (x0, x1),
                            s,
                            tk,
                            raster,
                            color,
                            o,
                            srgb,
                            clip,
                            fade,
                        );
                    }
                    // Text fields: selection under the text, placeholder, caret over it.
                    let mut caret = None;
                    if let Some(inp) = &n.input {
                        let e = &inp.edit;
                        let departing = self.flights.iter().any(|f| f.departs(&n.path, n.key.as_deref())
                            && f.alphas(f.destination, now, self.reduced_motion).0 > 0.0);
                        if e.text.is_empty() && e.preedit.is_none() && !inp.placeholder.is_empty() && !departing {
                            let pl_indent = spec.blocks.first().map_or(0.0, |b| (b.indent_pt(spec.style.size) * s).round() * tk);
                            let pl_origin = [origin[0] + pl_indent, origin[1]];
                            let pl_r_origin = if scale_motion == ScaleMotion::Still { pl_origin } else { [0.0, 0.0] };
                            let pl = self.text.layout_rich(&inp.placeholder, &[], spec.style, s, Some((w - pl_indent).max(1.0) / tk.max(1e-4)));
                            self.glyph_scratch.clear();
                            self.text.glyphs_for_layout_scaled(&pl, s, pl_r_origin, raster, false, &mut self.glyph_scratch);
                            let pc = premul(inp.placeholder_color, o, srgb);
                            for q in &self.glyph_scratch {
                                self.instances.extend(glyph_or_image(q, &self.text, &mut self.images, pc, glyph_gamma(inp.placeholder_color, s, srgb), clip, fade)
                                    .map(|g| zoom_glyph(g, pl_origin, pl_r_origin, tk / raster)));
                            }
                        }
                        if e.focused && !departing {
                            let (a, b) = e.range();
                            if a != b && e.preedit.is_none() {
                                let (da, db) = e.display_selection_range(a, b);
                                selectable::push_selection(&mut self.instances, layout, da, db,
                                    origin, tk, premul(inp.selection, o, srgb), s, clip, fade);
                            }
                            caret = input_caret.map(|(caret, _)| caret);
                            if let Some((_, _)) = &e.preedit
                                && let (_, Some((pa, pb))) = e.display()
                            {
                                for (bb, _) in crate::text::range_boxes(layout, pa, pb, false) {
                                    self.instances.push(rect_inst(
                                        [origin[0] + bb.x0 as f32 * tk, origin[1] + bb.y1 as f32 * tk - s, (bb.x1 - bb.x0) as f32 * tk, s],
                                        premul(color, o, srgb),
                                        0.0,
                                        clip,
                                        fade,
                                    ));
                                }
                            }
                        }
                    }
                    // Span backgrounds (inline code), strikethroughs, a selection over them, then glyphs.
                    let link_fx = self.link_fx(i);
                    let reveal = self.reveals.get(&n.id).filter(|st| !st.words.is_empty() && spec.advance_in.is_none());
                    self.deco_scratch.clear();
                    let mut pill_icons = Vec::new();
                    let pill_bg = spec.pill_colors.map_or(spec.rich_style.link.alpha(0.2), |(bg, _)| bg);
                    crate::text::pill_chips(layout, &spec.pills, spec.style.size, s, origin, pill_bg, &mut self.deco_scratch, &mut pill_icons);
                    match n.elided.filter(|_| !spec.spans.is_empty() || !spec.rings.is_empty()) {
                        Some(e) => self.text.decorations_with_rings(layout, &e.spans(&spec.spans), &e.rings(&spec.rings), s, origin, &mut self.deco_scratch),
                        None => self.text.decorations_with_rings(layout, &spec.spans, &spec.rings, s, origin, &mut self.deco_scratch),
                    }
                    if !spec.strikes.is_empty() {
                        self.text.strike_decorations(layout, &spec.spans, &spec.strikes, color, s, origin, &mut self.deco_scratch);
                    }
                    let drawn_len = n.elided.map_or(spec.content.len(), |e| e.drawn_len(spec.content.len()));
                    link_fx.decos(layout, drawn_len, s, origin, &mut self.deco_scratch);
                    if !spec.rings.is_empty() {
                        // Adjacent backgrounds overlap by bg_pad. Paint ringed chips after
                        // plain fills so a neighboring fill cannot erase an inset edge;
                        // keep underlines above both. No sorting on the ringless path.
                        self.deco_scratch.sort_by_key(|d| if d.radius == 0.0 { 2 } else { u8::from(d.ring.is_some()) });
                    }
                    if tk != 1.0 {
                        for d in &mut self.deco_scratch {
                            d.rect = scale_about(d.rect, origin, tk);
                            d.radius *= tk;
                            if let Some((_, rw)) = &mut d.ring {
                                *rw *= tk;
                            }
                        }
                    }
                    for d in &self.deco_scratch {
                        let alpha = if d.ring.is_some() {
                            reveal.map_or(1.0, |st| Self::reveal_alpha(st, d.byte, self.scene.store().node_time(n.id)))
                        } else { 1.0 };
                        let c = premul(d.color, o * alpha, srgb);
                        let mut inst = rect_inst(d.rect, c, d.radius, clip, fade);
                        if let Some((rc, rw)) = d.ring {
                            inst.border = premul(rc, o * alpha, srgb);
                            inst.params[0] = rw;
                        }
                        self.instances.push(inst);
                    }
                    let pill_fg = spec.pill_colors.and_then(|(_, fg)| fg).unwrap_or(color);
                    for (icon, px, at, fg) in pill_icons {
                        let pill_fg = fg.unwrap_or(pill_fg);
                        let at = if tk == 1.0 { at } else { std::array::from_fn(|k| origin[k] + (at[k] - origin[k]) * tk) };
                        if let Some(q) = self.text.icon_quad(icon, px, at) {
                            self.instances.push(glyph(&q, premul(pill_fg, o, srgb), glyph_gamma(pill_fg, s, srgb), clip, fade));
                        }
                    }
                    if spec.selectable && let Some((a, b)) = selectable::range_in(&sel_ranges, i) {
                        let (a, b) = n.elided.map_or((a, b), |e| e.range(a, b));
                        selectable::push_selection(&mut self.instances, layout, a, b, origin, tk, premul(spec.selection, o, srgb), s, clip, fade);
                    }
                    self.glyph_scratch.clear();
                    let r_origin = if scale_motion == ScaleMotion::Still { origin } else { [0.0, 0.0] };
                    // A cell of a longer run draws its slice of the run's glyphs, shaped in context.
                    let zoom = if let (Some(r), Some((_, at))) = (&n.run_layout, &spec.advance_in)
                        && let Some(x) = r.x(*at)
                    {
                        self.text.glyphs_in(r, *at, s, [r_origin[0] - x, r_origin[1]], &mut self.glyph_scratch);
                        tk
                    } else {
                        // Only the lines the clip shows, with two ems either side for ink past
                        // the line box; the per-glyph test below still decides what draws.
                        let band = if tk > 1e-4 {
                            let em = spec.spans.iter().filter_map(|sp| sp.size).fold(spec.style.size.max(spec.style.line_px()), f32::max);
                            let pad = 2.0 * em * s + 2.0 / raster.max(1e-4);
                            (clip[1] - origin[1]) / tk - pad..(clip[3] - origin[1]) / tk + pad
                        } else {
                            f32::NEG_INFINITY..f32::INFINITY
                        };
                        self.text.glyphs_in_band(layout, s, r_origin, raster, reveal.is_some() || !link_fx.is_empty(), band, &mut self.glyph_scratch);
                        tk / raster
                    };
                    let gamma = glyph_gamma(color, s, srgb);
                    let base = premul(color, o, srgb);
                    for q in &self.glyph_scratch {
                        if self.flights.iter().any(|f| f.lands(&n.path, n.key.as_deref())) { continue; }
                        let [x0, y0] = if zoom == 1.0 && origin == r_origin { [q.rect[0], q.rect[1]] } else {
                            std::array::from_fn(|k| origin[k] + (q.rect[k] - r_origin[k]) * zoom)
                        };
                        let (x1, y1) = (x0 + q.rect[2] * zoom, y0 + q.rect[3] * zoom);
                        // Wholly outside the clip, a glyph draws nothing: a long line in an
                        // `El::scroll_x` skips the glyphs scrolled out of sight.
                        if y0 > clip[3] || y1 < clip[1] || x0 > clip[2] || x1 < clip[0] {
                            continue;
                        }
                        let (mut f, mut gm) = (base, gamma);
                        if q.ink > 0
                            && let Some(c) = spec.spans.get(q.ink as usize - 1).and_then(|sp| sp.color)
                        {
                            f = premul(c, o, srgb);
                            gm = glyph_gamma(c, s, srgb);
                        }
                        if !link_fx.is_empty() && let Some(c) = link_fx.glyph_color(q.byte) {
                            f = premul(c, o, srgb);
                            gm = glyph_gamma(c, s, srgb);
                        }
                        if let Some(st) = reveal {
                            let a = Self::reveal_alpha(st, q.byte, self.scene.store().node_time(n.id));
                            if a <= 0.0 {
                                continue;
                            }
                            f = f.map(|v| v * a);
                        }
                        self.instances.extend(glyph_or_image(q, &self.text, &mut self.images, f, gm, clip, fade)
                            .map(|g| zoom_glyph(g, origin, r_origin, zoom)));
                        glyph_count += 1;
                    }
                    if let (Some(c), Some(inp)) = (caret, &n.input) {
                        self.instances.push(rect_inst(c, premul(inp.caret, o, srgb), 0.0, clip, fade));
                    }
                }
            }
            let packed_fade = crate::render::pack_fade(fade, fade_origin, clip);
            for instance in &mut self.instances[first_instance..] {
                instance.fade = packed_fade;
            }
            // Text-only clipping leaves the box's fill/border and descendants untouched.
            if let (Some(spec), Some(layout)) = (&n.text, &n.text_layout)
                && (spec.truncate_fade || spec.live.is_some())
            {
                let ins = n.inset.map(|v| v * sc * s);
                let bounds = [r[0] + ins[0], r[1] + ins[1], r[2] - ins[2], r[3] - ins[3]];
                let text_clip = [clip[0].max(bounds[0]), clip[1].max(bounds[1]), clip[2].min(bounds[2]), clip[3].min(bounds[3])];
                let mut widths = fade;
                let mut origins = fade_origin;
                // Layout width already includes display scale `s`; bounds also include the
                // element/ancestor transform `sc`. Compare both after that transform.
                if spec.truncate_fade && layout.full_width() * sc > bounds[2] - bounds[0] + 0.5 * sc {
                    let width = (spec.style.size * spec.fade_em * sc * s).min((bounds[2] - bounds[0]).max(0.0));
                    let own = if spec.align == Align::End { [width, 0.0, 0.0, 0.0] }
                        else { [0.0, 0.0, width, 0.0] };
                    inherit_fade(&mut widths, &mut origins, bounds, own);
                }
                for instance in &mut self.instances[first_instance..] {
                    if instance.params[1] == KIND_GLYPH {
                        instance.clip = text_clip;
                        instance.fade = crate::render::pack_fade(widths, origins, text_clip);
                    }
                }
            }
            if let Some((_, radii)) = rounded {
                for instance in &mut self.instances[primitive_start..] { instance.round_clip_corners(radii); }
                let packed = crate::render::pack_clip_radii(clip, radii);
                for surface in &mut self.surfaces[surface_start..] { surface.fade[3] = packed; }
            }
            // A faded effect slot's layer carries its clip; the slot and the layer's end don't.
            if let Some(at) = faded_slot {
                for instance in &mut self.instances[at..at + 2] { instance.fade = [0; 4]; }
            }
            if self.track_marks {
                self.marks.resize(self.instances.len(), mark);
            }
            // A scroll viewport clips its content with a hard edge unless it sets soft ones.
            if let Some(f) = n.vis.clip.or(n.scroll.map(|_| [0.0; 4])) {
                clip = [r[0].max(clip[0]), r[1].max(clip[1]), r[2].min(clip[2]), r[3].min(clip[3])];
                let (own, origin) = self.scroll_edge_fade(i, f, r, sc * s, p.rect.edges()[3]);
                inherit_fade(&mut fade, &mut fade_origin, origin, own);
            }
            let mask = self.row_mask[i];
            if mask >= 0.0 {
                clip[3] = clip[3].min(r[3]);
                inherit_fade(&mut fade, &mut fade_origin, r, [0.0, 0.0, 0.0, mask * s]);
            }
            let mut dy = pinh.dy;
            if n.scroll.is_some()
                && let Some(st) = self.scrolls.get(&n.id)
            {
                let origin = self.virtuals.values().find(|v| v.scroll_path == n.path).map_or(0.0, |v| f64::from(v.before));
                let offset = self.stuck_offset(i, p.rect.edges()[3]).unwrap_or(st.offset);
                let held = self.bottom_offset(i, p.rect.edges()[3]).is_some_and(|b| (offset - b).abs() < 0.5);
                dy -= grid((offset - origin) as f32 + st.slack.height(), s);
                if let Some(st) = self.scrolls.get_mut(&self.nodes[i].id) { st.held = held; }
            }
            let (mut scale, mut anchor, mut anchor_to, mut dx) = (sc, anchor, anchor_to, pinh.dx);
            if n.scroll_axes.x() && let Some(st) = self.scrolls.get(&n.id) {
                dx -= grid(st.x.offset as f32, s);
            }
            if open_cache && sc > 1e-4 {
                dx += (r[0] / s - rect_pt[0]) / sc;
                dy += (r[1] / s - rect_pt[1]) / sc;
            }
            let (mut cover_offset, mut outer) = (pinh.cover, pinh.outer);
            if let Some((k, c)) = cover {
                // Children ride this node's presented origin at their rest layout. Scale them by
                // `k` about the drawn origin, centred, and clip them to the moving rect.
                clip = intersect(clip, r);
                hit_clip = intersect(hit_clip, r);
                let plain = [dx, dy];
                dx = rect_pt[0] - p.rect.x + c[0] / k;
                dy += rect_pt[1] - p.rect.y - pinh.dy + c[1] / k;
                (scale, anchor, anchor_to) = (k, [rect_pt[0], rect_pt[1]], [rect_pt[0], rect_pt[1]]);
                cover_offset = [dx - plain[0], dy - plain[1]];
                outer = Some([pinh.clip, pinh.fade, pinh.fade_origin]);
            }
            inherit_fade(&mut hit_fade, &mut hit_origin, fade_origin, fade);
            let full_clip = intersect(hit_clip, clip);
            if node_turn.is_some() {
                if self.unturned_drawn.is_empty() {
                    self.unturned_drawn.resize(n_nodes, ([0.0; 4], NO_CLIP));
                }
                self.unturned_drawn[i] = (r.map(|v| v / s), full_clip);
            }
            self.inh_scratch[i] = Inh { opacity: effective_opacity, paint_opacity: o,
                clip: full_clip, fade: hit_fade, fade_origin: hit_origin,
                paint_clip: clip, paint_fade: fade, paint_fade_origin: fade_origin, rounded, scale, anchor, anchor_to, blur, dy, dx,
                cover: cover_offset, outer, mark, group,
                scale_motion: if cover.is_some() { ScaleMotion::Moving } else { scale_motion },
                in_cache, skip_paint, turn: turn_index };
            let hits_before = self.hits.len();
            if (n.vis.hit || n.on_pointer_move.is_some() || n.on_press.is_some() || n.on_double_press.is_some() || n.on_secondary_press.is_some() || n.on_hover.is_some() || n.on_hover_change.is_some() || n.on_drag.is_some() || n.drag_out.is_some() || n.input.is_some() || n.role == "textbox" || n.window_drag || n.focus.enabled || n.modal.is_some()
                || n.text.as_ref().is_some_and(|t| t.selectable) || n.gestures.as_ref().is_some_and(|g| g.takes_gestures()))
                && !self.subtree_exiting(i)
                && self.allowed(i) && effective_opacity > 0.001 && visible
            {
                let c = if n.parent.is_some() { pinh.clip } else { self.inh_scratch[0].clip };
                let rect_pt = if (press_k - 1.0).abs() > 1e-5 {
                    let [ax, ay] = press_anchor;
                    [ax + (rect_pt[0] - ax) / press_k, ay + (rect_pt[1] - ay) / press_k,
                        ax + (rect_pt[2] - ax) / press_k, ay + (rect_pt[3] - ay) / press_k]
                } else { rect_pt };
                let cr = [
                    rect_pt[0].max(c[0] / s),
                    rect_pt[1].max(c[1] / s),
                    rect_pt[2].min(c[2] / s),
                    rect_pt[3].min(c[3] / s),
                ];
                let rect = if n.parent.is_some() { cr } else { rect_pt };
                if !self.push_link_hits(i, rect_pt, rect) { self.hits.push((self.nodes[i].id, rect)); }
                let n = &self.nodes[i];
                if n.vis.hit && effective_opacity > 0.0 && rect[2] > rect[0] && rect[3] > rect[1] {
                    self.overlay_hits.push(self.hit_screen_rect(n.id, rect));
                }
            }
            if self.nodes[i].help.is_some() && (!self.nodes[i].help_cut || self.nodes[i].elided.is_some()) && !self.subtree_exiting(i) && self.allowed(i) && effective_opacity > 0.001 && visible {
                let c = if self.nodes[i].parent.is_some() { pinh.clip } else { NO_CLIP };
                let r = [rect_pt[0].max(c[0] / s), rect_pt[1].max(c[1] / s), rect_pt[2].min(c[2] / s), rect_pt[3].min(c[3] / s)];
                self.push_help_hit(i, r, hits_before);
            }
            if self.nodes[i].drop.is_some() && !self.subtree_exiting(i) && self.allowed(i) && effective_opacity > 0.001 && visible {
                let c = if self.nodes[i].parent.is_some() { pinh.clip } else { NO_CLIP };
                let r = [rect_pt[0].max(c[0] / s), rect_pt[1].max(c[1] / s), rect_pt[2].min(c[2] / s), rect_pt[3].min(c[3] / s)];
                self.push_drop_hit(i, r, hits_before);
            }
        }
        while let Some((_, start, surface_start)) = layers.pop() {
            if active_cache.is_some_and(|ac| ac.instance_start == start) {
                let ac = active_cache.take().unwrap();
                self.close_cached_layer(ac, start, surface_start, &mut glyph_count);
            } else {
                crate::render::end_layer(&mut self.instances, &mut self.surfaces, &mut self.marks, start, surface_start);
            }
        }
        self.update_help();
        self.layer_scratch = layers;
        self.built = true;
        if video_signature != self.video_signature {
            // A video loaded, ended or failed: views reading its state re-run next frame.
            self.video_signature = video_signature;
            self.dirty = true;
            self.videos_active = true;
        }
        if self.track_marks { self.marks.resize(self.instances.len(), 0); }
        self.stats.glyphs = glyph_count;
        self.ambient = ambient;
        // A moving chip can enter a stationary pointer. Re-evaluate against this frame's
        // geometry without inventing pointer-move or drag events on animation frames.
        let queued = self.queue.len();
        if let Some((x, y)) = self.pointer { self.refresh_hover(x, y); }
        // A live transform can change hover paint with no handler and no running spring.
        // Retarget from the new geometry and schedule the frame that samples that paint.
        let interaction = self.pointer.is_some() && self.update_interaction();
        self.queue.len() > queued || interaction
    }

    fn close_cached_layer(
        &mut self,
        ac: ActiveCache,
        start: usize,
        surface_start: usize,
        glyph_count: &mut usize,
    ) {
        use std::hash::Hasher;
        let layer_grow = (3.0 * crate::render::layer_blur::sigma(self.instances[start].params[2])).ceil();
        if ac.elided {
            let entry = self.node_caches.get_mut(&ac.node_id).expect("elided cache entry");
            entry.seen_scale = ac.seen_scale;
            entry.scale_changed_at = ac.scale_changed_at;
            let k = if (ac.cur_scale - entry.raster_scale).abs() <= 1e-4 {
                1.0
            } else {
                ac.cur_scale / entry.raster_scale.max(1e-4)
            };
            let b0 = (ac.origin[0] + entry.rel_bounds[0] * k).floor();
            let b1 = (ac.origin[1] + entry.rel_bounds[1] * k).floor();
            let b2 = (ac.origin[0] + entry.rel_bounds[2] * k).ceil();
            let b3 = (ac.origin[1] + entry.rel_bounds[3] * k).ceil();
            let sig = entry.signature;
            let marker = &mut self.instances[start];
            marker.rect = [b0, b1, (b2 - b0).max(0.0), (b3 - b1).max(0.0)];
            marker.params[3] = 1.0;
            marker.radii = crate::render::pack_u64_f32(ac.node_id.0);
            marker.uv = crate::render::pack_u64_f32(sig);
            self.instances.push(Instance::end_layer());
            if self.track_marks {
                let mark = self.marks.get(start).copied().unwrap_or(0);
                self.marks.resize(self.instances.len(), mark);
            }
            *glyph_count += ac.cached_glyphs;
            return;
        }

        if self.instances.len() == start + 1 && self.surfaces.len() == surface_start {
            self.instances.pop();
            if self.track_marks {
                self.marks.truncate(self.instances.len());
            }
            self.node_caches.remove(&ac.node_id);
            self.text.atlas.resident_caches.remove(&ac.node_id.0);
            return;
        }

        let mut bounds = [ac.origin[0], ac.origin[1], ac.origin[0] + ac.size[0], ac.origin[1] + ac.size[1]];
        let mut h = rune_motion::Fx::default();
        h.write_u64(ac.node_id.0);
        // Blur is baked into the target, unlike opacity and clipping on the composite.
        h.write_u32(self.instances[start].params[2].to_bits());
        h.write_u8(self.srgb_blend as u8);
        h.write_u32(ac.raster_scale.to_bits());
        h.write_u32(ac.size[0].to_bits());
        h.write_u32(ac.size[1].to_bits());
        if let Some(key) = self.nodes[ac.node_index].vis.cache_key {
            h.write_u8(1);
            h.write_u64(key);
        } else {
            h.write_u8(0);
        }
        let q = |v: f32| (v * 64.0).round() as i64;
        let mut turn_stack: Vec<Option<Turn>> = Vec::new();
        for inst in &self.instances[start + 1..] {
            if inst.params[1] == crate::render::KIND_LAYER_END {
                turn_stack.pop();
            } else {
                let active_turn = turn_stack.last().copied().flatten();
                let (mut c0, mut c1, mut c2, mut c3) = if inst.params[1] == crate::render::KIND_LAYER {
                    let next = if inst.border[1] != 0.0 {
                        let own = Turn::about(inst.border[1], [inst.border[2], inst.border[3]], inst.rect, NO_CLIP);
                        Some(active_turn.map_or(own, |t| t.after(own)))
                    } else {
                        active_turn
                    };
                    turn_stack.push(next);
                    let g = (3.0 * crate::render::layer_blur::sigma(inst.params[2])).ceil();
                    let mut r = [inst.rect[0] - g, inst.rect[1] - g, inst.rect[0] + inst.rect[2] + g, inst.rect[1] + inst.rect[3] + g];
                    if inst.border[1] != 0.0 {
                        r = Turn::about(inst.border[1], [inst.border[2], inst.border[3]], inst.rect, NO_CLIP).bbox(r);
                    }
                    (
                        r[0].max(inst.clip[0]),
                        r[1].max(inst.clip[1]),
                        r[2].min(inst.clip[2]),
                        r[3].min(inst.clip[3]),
                    )
                } else {
                    let mut rect = inst.rect;
                    if inst.params[1] == KIND_GLYPH && inst.params[0] != 0.0 {
                        let (sin, cos) = inst.params[0].sin_cos();
                        let width = cos.abs() * rect[2] + sin.abs() * rect[3];
                        let height = sin.abs() * rect[2] + cos.abs() * rect[3];
                        rect = [rect[0] + (rect[2] - width) * 0.5, rect[1] + (rect[3] - height) * 0.5, width, height];
                    }
                    let grow = match inst.params[1] {
                        KIND_GLYPH => 0.0,
                        KIND_SHADOW => inst.params[2] * 3.0 + 1.0,
                        KIND_EFFECT => inst.params[2] + 1.0,
                        KIND_RECT | KIND_GRADIENT | KIND_DASHED | KIND_RADIAL => 1.0 + inst.params[2] * 2.0,
                        _ => 1.0,
                    };
                    (
                        (rect[0] - grow).max(inst.clip[0]),
                        (rect[1] - grow).max(inst.clip[1]),
                        (rect[0] + rect[2] + grow).min(inst.clip[2]),
                        (rect[1] + rect[3] + grow).min(inst.clip[3]),
                    )
                };
                if c2 > c0 && c3 > c1 {
                    if let Some(t) = active_turn {
                        [c0, c1, c2, c3] = t.bbox([c0, c1, c2, c3]);
                    }
                    bounds[0] = bounds[0].min(c0);
                    bounds[1] = bounds[1].min(c1);
                    bounds[2] = bounds[2].max(c2);
                    bounds[3] = bounds[3].max(c3);
                }
            }
            h.write_i64(q(inst.rect[0] - ac.origin[0]));
            h.write_i64(q(inst.rect[1] - ac.origin[1]));
            h.write_i64(q(inst.rect[2]));
            h.write_i64(q(inst.rect[3]));
            for r in inst.radii { h.write_u32(r.to_bits()); }
            for f in inst.fill { h.write_u32(f.to_bits()); }
            for b in inst.border { h.write_u32(b.to_bits()); }
            for p in inst.params { h.write_u32(p.to_bits()); }
            for u in inst.uv { h.write_u32(u.to_bits()); }
            for k in 0..4 {
                let cv = inst.clip[k];
                if cv.abs() < 1e6 {
                    h.write_i64(q(cv - ac.origin[k & 1]));
                } else {
                    h.write_u32(cv.to_bits());
                }
            }
            for fd in inst.fade { h.write_u32(fd); }
        }
        let b0 = (bounds[0] - layer_grow).floor();
        let b1 = (bounds[1] - layer_grow).floor();
        let b2 = (bounds[2] + layer_grow).ceil();
        let b3 = (bounds[3] + layer_grow).ceil();
        let bw = (b2 - b0).max(0.0);
        let bh = (b3 - b1).max(0.0);
        if bw > 4096.0 || bh > 4096.0 {
            self.node_caches.remove(&ac.node_id);
            self.text.atlas.resident_caches.remove(&ac.node_id.0);
            crate::render::end_layer(&mut self.instances, &mut self.surfaces, &mut self.marks, start, surface_start);
            return;
        }
        let mut sig = h.finish();
        if sig == 0 { sig = 1; }
        let rel_bounds = [b0 - ac.origin[0], b1 - ac.origin[1], b2 - ac.origin[0], b3 - ac.origin[1]];
        let glyphs = *glyph_count - ac.glyph_start;
        let is_resident = self.text.atlas.resident_caches.get(&ac.node_id.0) == Some(&sig);
        self.node_caches.insert(ac.node_id, NodeCache {
            signature: sig,
            raster_scale: ac.raster_scale,
            seen_scale: ac.seen_scale,
            scale_changed_at: ac.scale_changed_at,
            env_scale: ac.env_scale,
            was_live: false,
            rel_bounds,
            glyphs,
            commit_gen: self.commit_gen,
            paint_state_hash: ac.paint_hash,
        });
        if is_resident && self.surfaces.len() == surface_start {
            self.instances.truncate(start + 1);
        }
        let marker = &mut self.instances[start];
        marker.rect = [b0, b1, bw, bh];
        marker.params[3] = 1.0;
        marker.radii = crate::render::pack_u64_f32(ac.node_id.0);
        marker.uv = crate::render::pack_u64_f32(sig);
        self.instances.push(Instance::end_layer());
        if self.track_marks {
            let mark = self.marks.get(start).copied().unwrap_or(0);
            self.marks.resize(self.instances.len(), mark);
        }
    }

    // ---------------------------------------------------------------------------------------
    // Inspection

    /// Elements that draw nothing and hold nothing: no paint of their own, no text, no field,
    /// no focus, no children. Hit and hover zones, spacers. A trace flags them: nobody can see
    /// them come, go or move.
    pub(crate) fn blank_leaves(&self) -> HashSet<&str> {
        let mut parents = vec![false; self.nodes.len()];
        for n in &self.nodes {
            if let Some(p) = n.parent { parents[p] = true; }
        }
        self.nodes.iter().enumerate().filter(|&(i, n)| {
            let v = &n.vis;
            !parents[i] && n.text.is_none() && n.input.is_none() && !n.focus.enabled && n.material.is_none()
                && v.fill.a == 0.0 && (v.border_width == 0.0 || v.border.a == 0.0 && v.border_bottom.is_none_or(|c| c.a == 0.0))
                && v.shadows.is_empty() && v.surface.is_none() && v.icon.is_none() && v.gradient.is_none() && v.radial.is_none()
                && v.outline.is_none() && v.shape.is_none() && v.pixels.is_none() && v.effect.is_none()
                && v.hover.a == 0.0 && v.press.a == 0.0
        }).map(|(_, n)| n.path.as_str()).collect()
    }

    /// Video elements' playback state by element path, for `--tree`.
    pub(crate) fn videos(&self) -> HashMap<String, crate::video::Snapshot> {
        self.nodes.iter()
            .filter_map(|n| Some((n.path.clone(), n.vis.surface.as_ref()?.0.video()?.snapshot())))
            .collect()
    }

    pub fn elements(&self) -> Vec<ElementInfo> {
        fn intent(n: &Node) -> u32 {
            n.live.iter().filter(|b| b.spring.is_none()).fold(0, |bits, b| bits | match b.property {
                crate::live::Property::Offset => 1,
                crate::live::Property::Scale | crate::live::Property::Rect => 1 | 2,
                crate::live::Property::Opacity => 4,
                crate::live::Property::Fill | crate::live::Property::Blur => 0,
            })
        }
        let store = self.scene.store();
        let animating = |id: NodeId| store.is_animating(id, prop::FRAME) || store.is_animating(id, prop::TRAVEL)
            || store.is_animating(id, prop::PRESENCE) || store.is_animating(id, prop::OFFSET) || store.is_animating(id, prop::SCALE) || store.is_animating(id, P_PRESS_SCALE)
            || store.is_animating(id, P_SCALE) || store.is_animating(id, prop::ROTATION) || paint_pose_springing(store, id)
            || (!self.reduced_motion && self.loops.get(&id).is_some_and(|l| l.active(store.node_time(id))));
        let sel_ranges = self.selection_ranges();
        let mut elements: Vec<_> = self.nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| {
                let p = self.scene.presented(n.id)?;
                let exiting = self.subtree_exiting(i);
                let flight = self.flights.iter().find(|f| f.lands(&n.path, n.key.as_deref()));
                let mut moving = flight.is_some() || animating(n.id) || self.grows.get(&n.id).is_some_and(|g| g.moving());
                let drawn = self.drawn.get(i).copied().unwrap_or([p.rect.x, p.rect.y, p.rect.w, p.rect.h]);
                // A flight carries the destination's laid-out text, which can be narrower than
                // its box. Report the box carried along with it, so the element lands on its own
                // frame instead of jumping out to it when the flight ends.
                let rect = flight.map_or(drawn, |f| {
                    let (d, c) = (f.destination, f.presented);
                    let (zx, zy) = (c.w / d.w.max(1e-3), c.h / d.h.max(1e-3));
                    [c.x + (drawn[0] - d.x) * zx, c.y + (drawn[1] - d.y) * zy, drawn[2] * zx, drawn[3] * zy]
                });
                let s = self.env.scale;
                let clip = flight.map_or_else(|| self.screen_clip_of_parent(n.parent),
                    |f| f.clip.map(|v| v * s));
                let (x0, y0) = (rect[0].max(clip[0] / s), rect[1].max(clip[1] / s));
                let (x1, y1) = ((rect[0] + rect[2]).min(clip[2] / s), (rect[1] + rect[3]).min(clip[3] / s));
                let visible = [x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0)];
                let revealing = flight.is_some() || self.reveals.get(&n.id).is_some_and(|r| !r.words.is_empty());
                let mut in_growing_row = false;
                let mut scrolling = false;
                let mut live_intent = intent(n);
                // A group_opacity swap under an instant hover group is a deliberate cut (Beta's
                // status/close swap), for it and what it contains: opacity and transition intent.
                let up = |from: Option<usize>| std::iter::successors(from, |&pi| self.nodes[pi].parent);
                if let Some(gi) = up(Some(i)).find(|&j| self.nodes[j].vis.group_opacity.is_some()) {
                    let group = up(self.nodes[gi].parent)
                        .map(|pi| &self.nodes[pi].vis)
                        .find(|v| v.hover.a > 0.0 || v.hover_group);
                    if group.and_then(|v| v.hover_motion).is_some_and(|(e, x)| [e, x].iter().any(|a| matches!(a.curve, rune_motion::Curve::Instant))) {
                        live_intent |= 4 | 16;
                    }
                }
                let step_up = |k: usize| {
                    if let Some(a) = &self.nodes[k].above
                        && let Some(j) = self.nodes[..k].iter().position(|n| n.key.as_ref() == Some(&a.key))
                    {
                        Some(j)
                    } else {
                        self.nodes[k].parent
                    }
                };
                let mut a = step_up(i);
                while let Some(pi) = a {
                    let parent = &self.nodes[pi];
                    moving |= animating(parent.id)
                        || self.stacks.get(&parent.id).is_some_and(|s| s.moving)
                        || self.scrolls.get(&parent.id).is_some_and(|s| s.slack.moving() || s.glide.is_some());
                    live_intent |= intent(parent);
                    scrolling |= self.scrolls.get(&parent.id).is_some_and(|s| s.scrolling);
                    in_growing_row |= self.grows.get(&parent.id).is_some_and(|g| g.moving());
                    a = step_up(pi);
                }
                Some(ElementInfo {
                    trace_name: n.trace_name,
                    trace_namespace: n.trace_namespace,
                    loop_phase: self.loops.get(&n.id).map(|l| {
                        if self.reduced_motion && !matches!(n.looping.map(|s| s.kind), Some(rune_motion::LoopKind::Sequence(_))) { 0.0 }
                        else { l.phase(self.scene.store().node_time(n.id)) }
                    }),
                    focusable: self.can_focus(i),
                    focused: self.focused.as_deref() == Some(&n.path),
                    focus_visible: self.keyboard_focus && self.focused.as_deref() == Some(&n.path),
                    inert: !self.allowed(i) || exiting,
                    exiting,
                    path: n.path.clone(),
                    key: n.key.clone(),
                    role: n.role,
                    label: n.label.clone(),
                    text: match &n.text {
                        Some(t) => (!n.input.as_ref().is_some_and(|i| i.edit.is_secure())).then(|| t.source_text().to_owned()),
                        // A text built from several (a rolling label's glyphs) reads as its label.
                        None => n.label.clone().filter(|_| n.role == "text"),
                    },
                    rect,
                    target: [n.target[0], n.target[1], n.target[2] - n.target[0], n.target[3] - n.target[1]],
                    opacity: flight.map_or_else(|| self.inh_scratch.get(i).map_or(p.opacity, |h| h.opacity), |f| f.target_alpha),
                    scale: if flight.is_some() { 1.0 } else { self.inh_scratch.get(i).map_or(p.transform.scale.x, |h| h.scale) },
                    blur: if flight.is_some() { 0.0 } else { self.inh_scratch.get(i).map_or(p.blur, |h| h.blur) },
                    radius: p.radius.0[0],
                    fill: self.color(n.id, P_FILL, n.vis.fill),
                    text_color: n.text.as_ref().map(|t| t.color).or(n.vis.icon.map(|(_, c)| c)).map(|c| self.color(n.id, P_TEXT, c)),
                    hover: if n.focus.highlight.is_some() && self.highlight_visible(i) { 1.0 }
                        else { self.scene.get(n.id, P_HOVER).unwrap_or(0.0) },
                    press: self.scene.get(n.id, P_PRESS).unwrap_or(0.0),
                    moving,
                    parent: n.parent,
                    visible,
                    revealing,
                    in_growing_row,
                    scrolling,
                    live_intent,
                    caret: self.caret_rect(&self.nodes, i),
                    drag_out: n.drag_out.is_some(),
                    help: n.help.as_ref().filter(|_| !n.help_cut || n.elided.is_some()).cloned(),
                    selected: n.text.as_ref().filter(|t| t.selectable).and_then(|t| {
                        let (a, b) = selectable::range_in(&sel_ranges, i)?;
                        t.copy_slice(a as u32, b as u32)
                    }),
                    level: n.a11y.level,
                    expanded: n.a11y.expanded,
                    item_selected: n.focus.selected,
                    stuck: self.stuck.get(i).copied().unwrap_or(0.0),
                    virtual_extent: self.virtuals.contains_key(&n.path) && n.vis.fill.a == 0.0
                        && n.vis.border_width == 0.0 && n.vis.shadows.is_empty() && n.text.is_none() && n.vis.icon.is_none(),
                })
            })
            .collect();
        for f in &self.flights {
            let r = if self.reduced_motion { f.source } else { f.presented };
            let c = if self.reduced_motion { f.source_clip } else { f.clip };
            let path = format!("flight-source/{}", f.target_key);
            let t = f.destination;
            elements.push(ElementInfo {
                trace_name: None,
                trace_namespace: None,
                loop_phase: None,
                focusable: false, focused: false, focus_visible: false, inert: true, exiting: false,
                key: Some(path.clone()), path, role: "text", label: None, text: None,
                rect: [r.x, r.y, r.w, r.h], target: [t.x, t.y, t.w, t.h],
                opacity: f.source_alpha, scale: 1.0, blur: 0.0, radius: 0.0, fill: Color::TRANSPARENT, text_color: None,
                hover: 0.0, press: 0.0, moving: true, parent: None,
                visible: [r.x.max(c[0]), r.y.max(c[1]), (r.edges()[2].min(c[2]) - r.x.max(c[0])).max(0.0),
                    (r.edges()[3].min(c[3]) - r.y.max(c[1])).max(0.0)],
                revealing: true, in_growing_row: false, scrolling: false, virtual_extent: false, live_intent: 0, caret: None, drag_out: false,
                help: None, selected: None, level: None, expanded: None, item_selected: false, stuck: 0.0,
            });
        }
        self.link_elements(&mut elements);
        elements
    }

    /// Centre of the first element whose key is `key`, or whose path ends with `key` when it
    /// contains a `/` (e.g. `slider/knob`), in points. Failing that, the first line box of the
    /// link (`El::link`) whose id is `key`. Exiting elements and their children are gone here;
    /// [`Runtime::presented`] still reads them.
    pub fn find(&self, key: &str) -> Option<(f32, f32)> {
        let suffix = format!("/{key}");
        let matches = |n: &Node| if key.contains('/') { n.path == key || n.path.ends_with(&suffix) } else { n.key.as_deref() == Some(key) };
        let i = (0..self.nodes.len()).find(|&i| matches(&self.nodes[i]) && !self.subtree_exiting(i));
        let Some(i) = i else { return self.find_link(key) };
        let r = self.drawn.get(i).copied()?;
        Some((r[0] + r[2] * 0.5, r[1] + r[3] * 0.5))
    }

    /// Drawn rect `[x, y, w, h]` in points and effective opacity (ancestors included) of the
    /// element keyed `key`, exiting or not, as of the last frame.
    pub fn presented(&self, key: &str) -> Option<([f32; 4], f32)> {
        let i = self.nodes.iter().position(|n| n.key.as_deref() == Some(key))?;
        Some((*self.drawn.get(i)?, self.inh_scratch.get(i)?.opacity))
    }

    /// The topmost element drawn at window point (`x`, `y`) in points, as of the last frame: the
    /// point is tested against each element's drawn box the way a press is, through `El::rotate`,
    /// `El::scale`, `El::origin`, offsets and scrolling, and inside its ancestors' clips
    /// (including a turned ancestor's clip), in paint order (so `El::raise`, sticky and flying
    /// elements count). Unlike a press, it also finds elements with no handlers, and it ignores
    /// `El::inert` and modals, so a press can land on a different element than this returns.
    /// Elements that are exiting or fully transparent are skipped (see [`Runtime::element_at_with`] to include transparent ones). `El::hit`, which marks native
    /// overlay input, makes no difference, and overlay panels outside the window are not
    /// considered. An element is not cut by its own clip, only by its ancestors'. This is box
    /// geometry, not pixel/alpha picking: a box with no fill remains pickable (as an invisible
    /// input area does), and rounded corners and soft clip fades do not trim its hit box.
    /// Running handlers can use [`Tx::element_at`] without holding a `Runtime`.
    pub fn element_at(&self, x: f32, y: f32) -> Option<Picked> {
        self.element_at_with(x, y, Pick::default())
    }

    /// [`Runtime::element_at`] with options: [`Pick::transparent`] also finds fully
    /// transparent elements (exiting ones stay skipped).
    pub fn element_at_with(&self, x: f32, y: f32, pick: Pick) -> Option<Picked> {
        self.element_at_within(None, x, y, pick)
    }

    fn run_pick_reads(&mut self, tx: &mut Tx) {
        for read in std::mem::take(&mut tx.pick_reads) {
            let picked = self.element_at_within(read.within.as_deref(), read.x, read.y, read.pick);
            if let Some(f) = read.take() {
                f(&mut self.state, picked, tx);
                self.dirty = true;
            }
        }
        // A callback may ask again, but cannot keep this dispatch running indefinitely.
        self.pending.pick_reads.append(&mut tx.pick_reads);
    }

    /// [`Runtime::element_at`] restricted to an element and its descendants, ignoring other
    /// subtrees. `within` accepts a key or key path as [`Cx::layout`] does.
    pub fn element_at_in(&self, within: &str, x: f32, y: f32) -> Option<Picked> {
        self.element_at_in_with(within, x, y, Pick::default())
    }

    /// [`Runtime::element_at_in`] with options, as [`Runtime::element_at_with`].
    pub fn element_at_in_with(&self, within: &str, x: f32, y: f32, pick: Pick) -> Option<Picked> {
        self.element_at_within(Some(within), x, y, pick)
    }

    fn element_at_within(&self, within: Option<&str>, x: f32, y: f32, pick: Pick) -> Option<Picked> {
        let scope = match within {
            Some(key) => Some(self.nodes.iter().position(|n| crate::flight::selects(key, &n.path, n.key.as_deref()))?),
            None => None,
        };
        let n = self.nodes.len();
        let at = |k: usize| if self.layer_paint_order.is_empty() { k } else { self.layer_paint_order[k] };
        let count = if self.layer_paint_order.is_empty() { n } else { self.layer_paint_order.len() };
        (0..count).rev().map(at).find(|&i| {
            scope.is_none_or(|scope| std::iter::successors(Some(i), |&p| self.nodes[p].parent).any(|p| p == scope))
                && self.pickable(i, x, y, pick)
        }).map(|i| Picked {
            path: self.nodes[i].path.clone(),
            key: self.nodes[i].key.clone(),
        })
    }

    /// Whether window point (`x`, `y`), in points, is inside the drawn, clipped and turned box
    /// of the element keyed `key`, whatever is drawn above it and whether or not it has handlers.
    /// False when there is no such element, it is exiting or fully transparent. Same geometry
    /// as [`Runtime::element_at`].
    pub fn contains(&self, key: &str, x: f32, y: f32) -> bool {
        self.contains_with(key, x, y, Pick::default())
    }

    /// [`Runtime::contains`] with options, as [`Runtime::element_at_with`].
    pub fn contains_with(&self, key: &str, x: f32, y: f32, pick: Pick) -> bool {
        self.nodes.iter().position(|n| crate::flight::selects(key, &n.path, n.key.as_deref())).is_some_and(|i| self.pickable(i, x, y, pick))
    }

    fn pickable(&self, i: usize, x: f32, y: f32, pick: Pick) -> bool {
        self.drawn.get(i).is_some()
            && self.inh_scratch.get(i).is_some_and(|h| pick.transparent || h.opacity > 0.0)
            && !self.subtree_exiting(i)
            && self.node_contains(i, x, y)
    }

    /// The presented rotation, scale and shadows of the element keyed `key`, exiting or not, as
    /// of the last frame, and whether any spring still moves them. It reads the springs on
    /// demand: nothing runs per frame for it. This is a driver/headless read: running view
    /// functions and handlers do not hold a `Runtime`.
    pub fn presented_pose(&self, key: &str) -> Option<Pose> {
        let i = self.nodes.iter().position(|n| crate::flight::selects(key, &n.path, n.key.as_deref()))?;
        let n = &self.nodes[i];
        let store = self.scene.store();
        let shadows = n.vis.shadows.iter().enumerate().map(|(k, sh)| {
            let sh = self.shadow_at(n.id, k, sh);
            match P_SHADOWS.get(k) {
                Some(&p) => crate::el::Shadow { color: self.color(n.id, p, sh.color), ..sh },
                None => sh,
            }
        }).collect();
        let step = |p: usize| {
            self.nodes[p].above.as_ref()
                .and_then(|a| self.nodes[..p].iter().position(|n| n.key.as_ref() == Some(&a.key)))
                .or(self.nodes[p].parent)
        };
        let moving = self.rect_springing(i)
            || std::iter::successors(Some(i), |&p| step(p)).any(|p| {
                let id = self.nodes[p].id;
                paint_pose_springing(store, id)
                    || (!self.reduced_motion && self.loops.get(&id).is_some_and(|l| l.active(store.node_time(id))))
            });
        Some(Pose {
            rotation: self.scene.get(n.id, prop::ROTATION).unwrap_or(n.vis.rotate),
            scale: self.inh_scratch.get(i)?.scale,
            shadows,
            moving,
        })
    }

    /// Whether the root's own fill is dark, when it has an opaque one (headless `--keys` caps
    /// follow it, so they flip when the app changes theme itself).
    pub(crate) fn backdrop_dark(&self) -> Option<bool> {
        if self.transparent { return None; }
        let n = self.nodes.first()?;
        let c = self.color(n.id, P_FILL, n.vis.fill);
        (c.a > 0.5).then(|| 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b < 0.25)
    }

    /// Enable or disable gamma-space (sRGB) blending for translucent fills, shadows, blurs,
    /// images and video, as `App::srgb_blend` does for a window. Every colour the next frame
    /// builds, hover and press tints included, is premultiplied in sRGB encoding
    /// (`Color::premul_srgb`). A runtime drawn by the app itself (`Runtime::with_text`, a
    /// headless snapshot) then draws with `Renderer::set_srgb_blend(true)` into a non-sRGB
    /// target (`format.remove_srgb_suffix()`), cleared to `clear_color()`.
    pub fn set_srgb_blend(&mut self, enabled: bool) {
        if self.srgb_blend != enabled {
            self.srgb_blend = enabled;
            self.dirty = true;
        }
    }

    pub fn srgb_blend(&self) -> bool {
        self.srgb_blend
    }

    pub fn clear_color(&self) -> [f32; 4] {
        if self.transparent { return [0.0; 4]; }
        self.nodes.first().map_or([0.0, 0.0, 0.0, 1.0], |n| {
            let c = self.color(n.id, P_FILL, n.vis.fill);
            if self.srgb_blend {
                [srgb_encode(c.r), srgb_encode(c.g), srgb_encode(c.b), 1.0]
            } else {
                [c.r, c.g, c.b, 1.0]
            }
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_block_decorations(
    instances: &mut Vec<Instance>,
    text_sys: &mut TextSystem,
    images: &mut crate::pixels::ImageAtlas,
    glyph_scratch: &mut Vec<crate::text::GlyphQuad>,
    spec: &TextSpec,
    layout: &Layout<Ink>,
    origin: [f32; 2],
    (x0, x1): (f32, f32),
    s: f32,
    tk: f32,
    raster: f32,
    text_color: Color,
    o: f32,
    srgb: bool,
    clip: [f32; 4],
    fade: [f32; 4],
) {
    use crate::edit::{Block, MAX_LIST_LEVEL};
    let line_h_default = spec.style.line_px() * s * tk;
    let box_w = (x1 - x0).max(layout.full_width() * tk).max(40.0 * s * tk);
    let mut para_idx = 0usize;
    let mut first_line_of_para = true;
    let mut list_counts = [0u32; (MAX_LIST_LEVEL as usize) + 1];
    let mut code_run: Option<(f32, f32)> = None;
    let mut quote_run: Option<(f32, f32)> = None;
    let mut markers: Vec<(f32, f32, Block, u32)> = Vec::new();

    let flush_code = |instances: &mut Vec<Instance>, run: &mut Option<(f32, f32)>| {
        if let Some((top, bot)) = run.take() {
            let pad_y = (2.0 * s * tk).round();
            instances.push(rect_inst(
                [x0, top - pad_y, box_w, (bot - top) + 2.0 * pad_y],
                premul(spec.rich_style.code_block_bg, o, srgb),
                6.0 * s * tk,
                clip,
                fade,
            ));
        }
    };
    let flush_quote = |instances: &mut Vec<Instance>, run: &mut Option<(f32, f32)>| {
        if let Some((top, bot)) = run.take() {
            let bar_w = (3.0 * s * tk).round().max(2.0);
            let bar_x = (origin[0] + 2.0 * s * tk).round();
            let inset_y = (2.0 * s * tk).round();
            instances.push(rect_inst(
                [bar_x, top + inset_y, bar_w, (bot - top - 2.0 * inset_y).max(4.0 * s * tk)],
                premul(spec.rich_style.quote_bar, o, srgb),
                bar_w * 0.5,
                clip,
                fade,
            ));
        }
    };

    let mut had_lines = false;
    for line in layout.lines() {
        had_lines = true;
        let m = line.metrics();
        let blk = spec.blocks.get(para_idx).copied().unwrap_or(Block::Paragraph);
        let is_soft = spec.soft_breaks.binary_search(&(para_idx as u32)).is_ok();
        let top = origin[1] + m.block_min_coord * tk;
        let bot = if m.block_max_coord > m.block_min_coord {
            origin[1] + m.block_max_coord * tk
        } else {
            top + line_h_default
        };
        let baseline = origin[1] + m.baseline * tk;

        if blk == Block::Code {
            flush_quote(instances, &mut quote_run);
            match &mut code_run {
                Some((_, b)) => *b = bot,
                None => code_run = Some((top, bot)),
            }
        } else if blk == Block::Quote {
            flush_code(instances, &mut code_run);
            match &mut quote_run {
                Some((_, b)) => *b = bot,
                None => quote_run = Some((top, bot)),
            }
        } else {
            flush_code(instances, &mut code_run);
            flush_quote(instances, &mut quote_run);
        }

        if first_line_of_para {
            match blk {
                Block::Bullet(lvl) => {
                    let l = (lvl.min(MAX_LIST_LEVEL)) as usize;
                    for c in &mut list_counts[l + 1..] {
                        *c = 0;
                    }
                    list_counts[l] = 0;
                    if !is_soft {
                        let indent = (blk.indent_pt(spec.style.size) * s).round();
                        markers.push((indent, baseline, blk, 0));
                    }
                }
                Block::Numbered(lvl) => {
                    let l = (lvl.min(MAX_LIST_LEVEL)) as usize;
                    for c in &mut list_counts[l + 1..] {
                        *c = 0;
                    }
                    if !is_soft {
                        list_counts[l] += 1;
                        let indent = (blk.indent_pt(spec.style.size) * s).round();
                        markers.push((indent, baseline, blk, list_counts[l]));
                    }
                }
                Block::Paragraph | Block::Quote | Block::Code => {
                    if !is_soft {
                        list_counts = [0; (MAX_LIST_LEVEL as usize) + 1];
                    }
                }
            }
        }

        if line.break_reason() == parley::BreakReason::Explicit {
            para_idx += 1;
            first_line_of_para = true;
        } else {
            first_line_of_para = false;
        }
    }

    if !had_lines
        && let Some(&blk) = spec.blocks.first()
    {
        let top = origin[1];
        let bot = top + line_h_default;
        let baseline = top + spec.style.size * 0.8 * s * tk;
        match blk {
            Block::Code => code_run = Some((top, bot)),
            Block::Quote => quote_run = Some((top, bot)),
            Block::Bullet(_) => {
                let indent = (blk.indent_pt(spec.style.size) * s).round();
                markers.push((indent, baseline, blk, 0));
            }
            Block::Numbered(_) => {
                let indent = (blk.indent_pt(spec.style.size) * s).round();
                markers.push((indent, baseline, blk, 1));
            }
            Block::Paragraph => {}
        }
    }

    flush_code(instances, &mut code_run);
    flush_quote(instances, &mut quote_run);

    if !markers.is_empty() {
        let marker_color = spec.rich_style.marker.unwrap_or_else(|| text_color.alpha(0.68));
        let mc = premul(marker_color, o, srgb);
        let m_gamma = glyph_gamma(marker_color, s, srgb);
        for (indent_px, baseline_y, blk, num) in markers {
            match blk {
                Block::Bullet(lvl) => {
                    let d = (4.5 * s * tk).round().max(3.0);
                    let cx = origin[0] + (indent_px - (7.0 * s).round()) * tk - d;
                    let cy = (baseline_y - 0.34 * spec.style.size * s * tk - d * 0.5).round();
                    let radius = if lvl >= 2 { 1.0 * s * tk } else { d * 0.5 };
                    let c = if lvl == 1 { premul(marker_color.alpha(marker_color.a * 0.65), o, srgb) } else { mc };
                    instances.push(rect_inst([cx.round(), cy, d, d], c, radius, clip, fade));
                }
                Block::Numbered(_) => {
                    let label = format!("{num}.");
                    let ml = text_sys.layout_rich(&label, &[], spec.style, s, None);
                    let mw = ml.full_width();
                    let m_base = ml.lines().next().map_or(spec.style.size * 0.8 * s, |l| l.metrics().baseline);
                    let mx = origin[0] + (indent_px - (5.0 * s).round() - mw) * tk;
                    let my = baseline_y - m_base * tk;
                    let m_origin = [mx.round(), my.round()];
                    glyph_scratch.clear();
                    text_sys.glyphs_for_layout_scaled(&ml, s, m_origin, raster, false, glyph_scratch);
                    for q in &*glyph_scratch {
                        instances.extend(
                            glyph_or_image(q, text_sys, images, mc, m_gamma, clip, fade)
                                .map(|g| zoom_glyph(g, m_origin, m_origin, tk / raster)),
                        );
                    }
                }
                _ => {}
            }
        }
    }
}

/// Pixel caret and logical-point IME area use the same shaped cursor geometry.
fn input_caret(n: &Node, r: [f32; 4], scale: f32, s: f32, scrolls: &mut HashMap<String, f32>) -> Option<([f32; 4], [f32; 4])> {
    let (spec, layout, input) = (n.text.as_ref()?, n.text_layout.as_ref()?, n.input.as_ref()?);
    let e = &input.edit;
    let ins = n.inset.map(|v| v * scale * s);
    let (x0, x1) = (r[0] + ins[0], r[2] - ins[2]);
    // The text scales with its box, so the caret does too.
    let mut x = text_x(spec, x0, x1, (layout.full_width() * scale).ceil());
    if input.single_line { x -= single_line_scroll(scrolls, &n.path, input, &spec.content, layout, s, (x1 - x0) / scale.max(1e-4)) * scale; }
    let ci = caret_index(e, &spec.content);
    let cur = Cursor::from_byte_index(layout, ci, Affinity::Downstream);
    let bb = crate::text::cursor_geometry(&cur, layout, 1.5 * s);
    let (cx, cy) = (x + bb.x0 as f32 * scale, r[1] + ins[1] + bb.y0 as f32 * scale);
    let h = (if bb.y1 > bb.y0 { (bb.y1 - bb.y0) as f32 } else { spec.style.line_px() * s }) * scale;
    Some(([cx.round(), cy + h * 0.1, (1.5 * s * scale).round().max(1.0), h * 0.8], [cx / s, cy / s, 2.0 * scale, h / s]))
}

/// A horizontal scroll viewport's content without a width of its own, or one filling the
/// viewport (`w_full`), takes its natural one, unwrapped, and at least the viewport's (CSS
/// `width: max-content; min-width: 100%`). Any other width stays as set, narrower than the
/// viewport too. The content never shrinks. The viewport lays it out as a row, where taffy
/// measures a max-content main size unwrapped (a column measures its height wrapped at the
/// viewport's width). With vertical scrolling too it keeps its natural height instead of
/// stretching to the viewport's.
fn unwrapped_width(style: &mut Style, vertical: bool) {
    if style.size.width.is_auto() || style.size.width == Dimension::percent(1.0) {
        style.size.width = Dimension::max_content();
        if style.min_size.width.is_auto() { style.min_size.width = percent(1.0); }
    }
    style.flex_shrink = 0.0;
    if vertical && style.align_self.is_none() { style.align_self = Some(AlignSelf::FLEX_START); }
}

/// The (x, y) scroll offsets of node `i`'s scroll viewports, innermost first.
fn scroll_offsets<'a>(scrolls: &'a HashMap<NodeId, ScrollState>, nodes: &'a [Node], i: usize) -> impl Iterator<Item = [f32; 2]> + 'a {
    std::iter::successors(Some(i), |&p| nodes[p].parent.filter(|_| nodes[p].above.is_none()))
        .skip(1)
        .filter_map(|p| scrolls.get(&nodes[p].id).map(|st| [st.x.offset as f32, st.offset as f32]))
}

/// Marks every ancestor of a marked node.
fn mark_ancestors(nodes: &[Node], marked: &mut [bool]) {
    for i in 0..nodes.len() {
        if !marked[i] { continue; }
        let mut p = nodes[i].parent;
        while let Some(j) = p {
            if marked[j] { break; }
            marked[j] = true;
            p = nodes[j].parent;
        }
    }
}

/// Whether `looping`'s sampled value multiplies the element's opacity.
fn fades(looping: Option<rune_motion::LoopSpec>) -> bool {
    matches!(looping.map(|l| l.kind), Some(rune_motion::LoopKind::Opacity { .. } | rune_motion::LoopKind::OpacityCycle(_) | rune_motion::LoopKind::Sequence(_)))
}

/// The handler an event goes to, when its `Tx::live_only` can keep the event from committing.
/// Events Rune does its own work for (focus, editing, pastes, file drops) always commit.
fn live_only_handler(action: &Action) -> Option<&'static str> {
    Some(match action {
        Action::WindowFocus(_) => "on_window_focus",
        Action::DrawnLayout(..) => "on_drawn_layout_tx",
        Action::PointerMove(..) => "on_pointer_move_local",
        Action::Hover(_) => "on_hover",
        Action::HoverChange(..) => "on_hover_change",
        Action::Press(..) => "on_press",
        Action::DoublePress(_) => "on_double_press",
        Action::Link(..) => "on_link",
        Action::SecondaryPress(..) => "on_secondary_press",
        Action::Drag(..) => "on_drag",
        Action::DragStart(_) => "on_drag_start",
        Action::DragEnd(..) => "on_drag_end",
        Action::Key { .. } => "on_key",
        Action::KeyUp(_) => "on_key_up",
        Action::Wheel(..) => "on_wheel",
        Action::Gesture(_, gesture_dispatch::Gesture::Magnify(_)) => "on_magnify",
        Action::Gesture(_, gesture_dispatch::Gesture::Rotate(_)) => "on_rotate",
        Action::Gesture(_, gesture_dispatch::Gesture::SmartMagnify(..)) => "on_smart_magnify",
        _ => return None,
    })
}

/// Folds one handler's `Tx::live_only` into its event's: live-only when every handler that
/// took the event said so.
pub(crate) fn fold_live(live: &mut Option<bool>, took: bool, one: &Tx) {
    if took { *live = Some(live.unwrap_or(true) && one.live_only); }
}

#[cfg(debug_assertions)]
fn action_path(action: &Action) -> Option<&str> {
    match action {
        Action::PointerMove(path, _) | Action::DrawnLayout(path, ..) | Action::Hover(path) | Action::HoverChange(path, _) | Action::Press(path, _) | Action::DoublePress(path)
        | Action::Link(path, _) | Action::SecondaryPress(path, _) | Action::Drag(path, ..) | Action::DragStart(path)
        | Action::DragEnd(path, _) => Some(path),
        Action::Wheel(_, chain, _) | Action::Gesture(chain, _) => chain.first().map(String::as_str),
        _ => None,
    }
}

fn merge(tx: &mut Tx, one: Tx) {
    tx.pick_reads.extend(one.pick_reads);
    tx.paste |= one.paste;
    tx.clipboard_reads.extend(one.clipboard_reads);
    if let Some(one) = one.trace {
        let trace = tx.trace.get_or_insert_with(Default::default);
        trace.events.extend(one.events);
        trace.channels.extend(one.channels);
    }
    tx.scroll_to.extend(one.scroll_to);
    tx.scroll_bottom.extend(one.scroll_bottom);
    tx.scroll_pages.extend(one.scroll_pages);
    tx.scroll_offsets.extend(one.scroll_offsets);
    tx.keep_on_screen.extend(one.keep_on_screen);
    tx.wakes.extend(one.wakes);
    tx.text_flights.extend(one.text_flights);
    tx.popups.extend(one.popups);
    tx.file_dialogs.extend(one.file_dialogs);
    if let Some(request) = one.window_frame { tx.window_frame = Some(tx.window_frame.unwrap_or_default().merge(request)); }
    if one.clear_secret && tx.copy.as_ref().is_some_and(crate::ClipboardWrite::is_secret) { tx.copy = None; }
    tx.clear_secret |= one.clear_secret;
    if one.copy.is_some() { tx.copy = one.copy; }
    tx.shell = tx.shell.max(one.shell);
    tx.minimize |= one.minimize;
    tx.srgb_blend = one.srgb_blend.or(tx.srgb_blend);
    tx.blur |= one.blur;
    tx.instant |= one.instant;
    tx.settle |= one.settle;
    tx.motion = one.motion.or(tx.motion);
    tx.color_motion = one.color_motion.or(tx.color_motion);
}

trait CeilTo {
    fn ceil_to(self, scale: f32) -> f32;
}

impl CeilTo for f32 {
    /// Rounds up to whole device pixels.
    fn ceil_to(self, scale: f32) -> f32 {
        (self * scale - 1e-3).ceil() / scale
    }
}

/// How far a single-line field's text shifts left (device px) so its caret stays inside
/// `visible` device px. Unfocused fields rest at their start, like a native text field.
fn single_line_scroll(scrolls: &mut HashMap<String, f32>, path: &str, inp: &crate::el::InputSpec, content: &str, layout: &Layout<Ink>, s: f32, visible: f32) -> f32 {
    if !inp.edit.focused {
        scrolls.remove(path);
        return 0.0;
    }
    let scroll = single_line_offset(scrolls.get(path).copied().unwrap_or(0.0), inp, content, layout, s, visible);
    scrolls.insert(path.to_owned(), scroll / s);
    scroll
}

/// [`single_line_scroll`] for a focused field, from its previous shift `prev` in points.
fn single_line_offset(prev: f32, inp: &crate::el::InputSpec, content: &str, layout: &Layout<Ink>, s: f32, visible: f32) -> f32 {
    let caret_w = (1.5 * s).round();
    let caret = Cursor::from_byte_index(layout, caret_index(&inp.edit, content), Affinity::Downstream);
    let cx = crate::text::cursor_geometry(&caret, layout, 1.5 * s).x0 as f32;
    let mut scroll = prev * s;
    if cx - scroll > visible - caret_w {
        scroll = cx - visible + caret_w;
    }
    scroll.min(cx).clamp(0.0, (layout.full_width() + caret_w - visible).max(0.0))
}

/// The display byte index the caret sits at, inside any IME preedit.
fn caret_index(e: &crate::edit::TextEdit, content: &str) -> usize {
    match &e.preedit {
        Some((p, Some((_, c)))) => (e.range().0 + (*c).min(p.len())).min(content.len()),
        Some((p, None)) => (e.range().0 + p.len()).min(content.len()),
        None => e.display_index(e.focus).min(content.len()),
    }
}

/// Parley line alignment for a text node: only wrapped text aligns per line.
fn line_alignment(spec: &TextSpec) -> parley::Alignment {
    match spec.align {
        _ if spec.wrap && spec.justify => parley::Alignment::Justify,
        Align::Center if spec.wrap => parley::Alignment::Center,
        Align::End if spec.wrap => parley::Alignment::End,
        _ => parley::Alignment::Start,
    }
}

/// The rectangle a text node's laid-out lines cover inside its drawn box `r` (logical points),
/// and the layout origin its glyphs are placed from. Lines aligned inside a wrap width count
/// from their own offsets, so the rect hugs the text rather than the box.
fn text_extent(n: &Node, spec: &TextSpec, layout: &Layout<Ink>, r: [f32; 4], scale: f32) -> (MRect, [f32; 2]) {
    let (x0, x1) = (r[0] + n.inset[0], r[0] + r[2] - n.inset[2]);
    let (lo, hi) = layout.lines().map(|l| (l.metrics().offset, l.metrics().advance))
        .fold((f32::MAX, 0.0f32), |(lo, hi), (offset, advance)| (lo.min(offset), hi.max(offset + advance)));
    let (lo, hi) = if lo > hi { (0.0, layout.full_width()) } else { (lo, hi) };
    let origin = [text_x(spec, x0, x1, layout.full_width() / scale), r[1] + n.inset[1]];
    let x = origin[0] + lo / scale;
    (MRect::new(x, origin[1], ((hi - lo) / scale).min(x1 - x).max(0.0),
        (layout_height(layout, spec.style, scale) / scale).min(r[3] - n.inset[1] - n.inset[3])), origin)
}

/// Where a `width`-wide text line starts between `x0` and `x1`. Wrapped text aligns each line
/// inside the wrap width itself, so it starts at `x0`.
fn text_x(spec: &TextSpec, x0: f32, x1: f32, width: f32) -> f32 {
    match spec.align {
        _ if line_alignment(spec) != parley::Alignment::Start => x0,
        Align::Start => x0,
        Align::Center => x0 + (x1 - x0 - width) * 0.5,
        Align::End => x1 - width,
    }
}

/// Where truncation cut a text: it draws `content[..head]`, "…", then `content[tail..]`.
/// Selections and links stay in content bytes and map through this onto the drawn layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Elision {
    head: usize,
    tail: usize,
}

impl Elision {
    const ELLIPSIS: &str = "…";

    /// The drawn byte just past the ellipsis.
    fn after(self) -> usize { self.head + Self::ELLIPSIS.len() }

    /// The content byte at drawn byte `i`. Past the ellipsis is the end of what it hides.
    pub(crate) fn content(self, i: usize) -> usize {
        if i <= self.head { i } else if i < self.after() { self.tail } else { self.tail + (i - self.after()) }
    }

    /// The drawn byte at content byte `i`. A hidden byte lands before the ellipsis, or after it
    /// for a range's `end`, so a range reaching into the cut covers the ellipsis.
    pub(crate) fn drawn(self, i: usize, end: bool) -> usize {
        if i <= self.head { i } else if i < self.tail { if end { self.after() } else { self.head } } else { self.after() + (i - self.tail) }
    }

    /// Selected content bytes `a..b` as drawn bytes: a selection reaching into the cut
    /// highlights the ellipsis.
    pub(crate) fn range(self, a: usize, b: usize) -> (usize, usize) { (self.drawn(a, false), self.drawn(b, true)) }

    /// A span's or link's content bytes `a..b` as drawn bytes. A range wholly inside the cut
    /// comes back empty and one crossing an edge keeps its visible side. A link takes the
    /// ellipsis only when it shows on both sides of it. A span also takes it when it covers the
    /// first byte the cut hides and shows next to it, so "…" wears the style of the text it
    /// replaces, as AppKit's truncation draws it.
    pub(crate) fn mark(self, a: usize, b: usize, link: bool) -> (usize, usize) {
        let (h, t, after) = (self.head, self.tail, self.after());
        if b <= h { return (a, b) }
        if a >= t { return (after + a - t, after + b - t) }
        let (before, beyond) = (a < h, b > t);
        let ellipsis = if link { before && beyond } else { a <= h && (before || beyond) };
        let start = if before { a } else if ellipsis { h } else { after };
        let end = if beyond { after + b - t } else if ellipsis { after } else { h };
        if start < end { (start, end) } else { (start, start) }
    }

    /// The drawn string's length for content `len` bytes long.
    pub(crate) fn drawn_len(self, len: usize) -> usize { self.drawn(len, true) }

    /// `spans` moved onto the drawn string.
    fn spans(self, spans: &[crate::text::Span]) -> Vec<crate::text::Span> {
        spans.iter().map(|sp| {
            let (start, end) = self.mark(sp.start as usize, sp.end as usize, false);
            crate::text::Span { start: start as u32, end: end as u32, ..sp.clone() }
        }).collect()
    }

    /// `rings` moved onto the drawn string.
    fn rings(self, rings: &[(u32, u32, Color, f32)]) -> Vec<(u32, u32, Color, f32)> {
        rings.iter().filter_map(|&(a, b, c, w)| {
            let (start, end) = self.mark(a as usize, b as usize, false);
            (start < end).then_some((start as u32, end as u32, c, w))
        }).collect()
    }
}

/// `spec`'s content cut to fit `max_w` device px with an ellipsis, keeping as many whole
/// graphemes as fit at the start, the end or both ends (half each) as its `elide` says.
/// Whitespace graphemes beside the ellipsis are dropped whole.
fn truncated(text: &mut TextSystem, spec: &TextSpec, scale: f32, max_w: f32) -> (Rc<Layout<Ink>>, Elision) {
    use unicode_segmentation::UnicodeSegmentation;
    let content = spec.content.as_str();
    let bounds: Vec<usize> = content.grapheme_indices(true).map(|(i, _)| i).chain([content.len()]).collect();
    let n = bounds.len() - 1;
    let blank = |g: usize| content[bounds[g]..bounds[g + 1]].chars().all(char::is_whitespace);
    let cut = |k: usize| {
        let (mut head, tail) = match spec.elide {
            crate::el::Elide::End => (k, 0),
            crate::el::Elide::Middle => (k - k / 2, k / 2),
            crate::el::Elide::Head => (0, k),
        };
        let mut tail = n - tail;
        while head > 0 && blank(head - 1) { head -= 1; }
        while tail < n && blank(tail) { tail += 1; }
        Elision { head: bounds[head], tail: bounds[tail] }
    };
    let make = |text: &mut TextSystem, e: Elision| {
        let s = [&content[..e.head], Elision::ELLIPSIS, &content[e.tail..]].concat();
        text.layout_rich(&s, &e.spans(&spec.spans), spec.style, scale, None)
    };
    let (mut lo, mut hi) = (0usize, n);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if make(text, cut(mid)).full_width() <= max_w + 0.5 { lo = mid } else { hi = mid - 1 }
    }
    let e = cut(lo);
    (make(text, e), e)
}

/// Lays out the flattened tree in `room`, measuring text leaves. Not generic over the app state,
/// so Taffy's layout code is instantiated once, not once per `Runtime<S>`.
fn compute_layout(taffy: &mut TaffyTree<usize>, root: taffy::NodeId, room: Size<AvailableSpace>, nodes: &[Node], text: &mut TextSystem, scale: f32) {
    taffy
        .compute_layout_with_measure(root, room, |inputs, _id, ctx, style| {
            taffy::compute_leaf_layout(inputs, style, |_, _| 0.0, |known, avail| {
                let Some(&mut i) = ctx else { return Size::ZERO };
                let spec = nodes[i].text.as_ref().unwrap();
                let w = if spec.wrap {
                    known.width.or(match avail.width {
                        AvailableSpace::Definite(w) => Some(w),
                        AvailableSpace::MinContent => Some(0.0),
                        AvailableSpace::MaxContent => None,
                    })
                } else {
                    None
                };
                if let Some((run, at)) = &spec.advance_in {
                    let r = text.run_layout(run, spec.style, scale);
                    if let Some(w) = r.advance(*at, scale) {
                        return Size {
                            width: known.width.unwrap_or(w),
                            height: known.height.unwrap_or(layout_height(&r.layout, spec.style, scale) / scale),
                        };
                    }
                }
                let l = text.layout_for_spec(spec, scale, w.map(|w| w * scale), parley::Alignment::Start);
                let mut lw = (l.full_width() / scale).ceil_to(scale);
                if (spec.truncate || spec.truncate_fade || spec.max_lines > 0)
                    && let AvailableSpace::Definite(a) = avail.width
                {
                    lw = lw.min(a);
                }
                Size {
                    width: known.width.unwrap_or(lw),
                    height: known.height.unwrap_or(clamped_height(&l, spec, scale) / scale),
                }
            })
        })
        .unwrap();
}

/// The height (device px) of `l` as `spec` shows it: its first `max_lines` lines when set.
fn clamped_height(l: &Layout<Ink>, spec: &TextSpec, scale: f32) -> f32 {
    let full = layout_height(l, spec.style, scale);
    match spec.max_lines.checked_sub(1).and_then(|last| l.lines().nth(last)).filter(|_| l.len() > spec.max_lines) {
        Some(line) => line.metrics().block_max_coord.ceil().min(full),
        None => full,
    }
}

/// The advance of the last line `l` shows when clamped to `lines`, in device px.
fn last_line_width(l: &Layout<Ink>, lines: usize) -> f32 {
    l.lines().take(lines).last().map_or(0.0, |line| line.metrics().advance - line.metrics().hanging_advance)
}

/// `spec`'s content wrapped at `max_w` device px and cut after `spec.max_lines` lines, "…"
/// ending the last with as many whole graphemes before it as fit within `max_w`, a word that
/// wraps or overflows included. None when `full` (the uncut layout) already fits.
/// Whitespace and line breaks before the ellipsis are dropped.
fn clamped(text: &mut TextSystem, spec: &TextSpec, scale: f32, max_w: f32, full: &Layout<Ink>) -> Option<(Rc<Layout<Ink>>, Elision)> {
    use unicode_segmentation::UnicodeSegmentation;
    let lines = spec.max_lines;
    if lines == 0 || full.len() <= lines && last_line_width(full, lines) <= max_w + 0.5 {
        return None;
    }
    let content = spec.content.as_str();
    // The cut falls in the last shown line or the next, whose first word may fill the last.
    let start = full.lines().nth(lines.min(full.len()).max(1) - 1)?.text_range().start.min(content.len());
    let end = full.lines().nth(lines).map_or(content.len(), |l| l.text_range().end.min(content.len()));
    let bounds: Vec<usize> = content[start..end].grapheme_indices(true).map(|(i, _)| start + i).chain([end]).collect();
    let blank = |g: usize| content[bounds[g]..bounds[g + 1]].chars().all(char::is_whitespace);
    let cut = |mut k: usize| {
        while k > 0 && blank(k - 1) { k -= 1; }
        let head = if k == 0 { content[..start].trim_end().len() } else { bounds[k] };
        Elision { head, tail: content.len() }
    };
    let make = |text: &mut TextSystem, e: Elision| {
        let s = [&content[..e.head], Elision::ELLIPSIS].concat();
        text.layout_aligned(&s, &e.spans(&spec.spans), spec.style, scale, Some(max_w), line_alignment(spec))
    };
    let fits = |l: &Layout<Ink>| l.len() <= lines && last_line_width(l, lines) <= max_w + 0.5;
    let (mut lo, mut hi) = (0usize, bounds.len() - 1);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(&make(text, cut(mid))) { lo = mid } else { hi = mid - 1 }
    }
    let e = cut(lo);
    Some((make(text, e), e))
}

// Carry the declaring edge with the inherited width. Descendant clipping must not restart
// its ramp; an inner fade takes over once the ancestor's ramp is fully opaque there.
fn inherit_fade(width: &mut [f32; 4], origin: &mut [f32; 4], rect: [f32; 4], own: [f32; 4]) {
    for edge in 0..4 {
        let inset = if edge < 2 { rect[edge] - origin[edge] } else { origin[edge] - rect[edge] };
        if own[edge] > 0.0 && (own[edge] >= width[edge] || inset >= width[edge]) {
            width[edge] = own[edge];
            origin[edge] = rect[edge];
        }
    }
}

/// `rect` (x, y, w, h) magnified `k` times about `origin`.
fn scale_about(rect: [f32; 4], origin: [f32; 2], k: f32) -> [f32; 4] {
    [origin[0] + (rect[0] - origin[0]) * k, origin[1] + (rect[1] - origin[1]) * k, rect[2] * k, rect[3] * k]
}

/// A glyph instance drawn `k` times its rasterized size about `origin`. An alpha glyph keeps
/// its atlas footprint in `uv.zw`, so the shader samples it filtered.
fn zoom_glyph(mut g: Instance, origin: [f32; 2], r_origin: [f32; 2], k: f32) -> Instance {
    if k == 1.0 && origin == r_origin { return g; }
    if g.params[1] == KIND_GLYPH && g.uv[2] == 0.0 { (g.uv[2], g.uv[3]) = (g.rect[2], g.rect[3]); }
    g.rect = [
        origin[0] + (g.rect[0] - r_origin[0]) * k,
        origin[1] + (g.rect[1] - r_origin[1]) * k,
        g.rect[2] * k,
        g.rect[3] * k,
    ];
    g
}

fn glyph_uv_page(uv: [f32; 2]) -> ([f32; 2], f32) {
    let page = (uv[1] as u32) / crate::text::ATLAS_SIZE;
    ([uv[0], uv[1] - (page * crate::text::ATLAS_SIZE) as f32], page as f32)
}

fn glyph(q: &GlyphQuad, fill: [f32; 4], gamma: f32, clip: [f32; 4], fade: [f32; 4]) -> Instance {
    let (uv, page) = glyph_uv_page(q.uv);
    Instance {
        rect: q.rect,
        radii: [0.0; 4],
        fill,
        border: [0.0; 4],
        params: [0.0, KIND_GLYPH, page, gamma],
        uv: [uv[0], uv[1], 0.0, 0.0],
        clip,
        fade: crate::render::pack_fade(fade, clip, clip),
    }
}

/// A text glyph's instance: alpha-atlas coverage tinted by `fill`, or a colour glyph's image
/// at `fill`'s alpha. None when the image atlas can't place it.
fn glyph_or_image(q: &GlyphQuad, text: &TextSystem, images: &mut crate::pixels::ImageAtlas, fill: [f32; 4], gamma: f32, clip: [f32; 4], fade: [f32; 4]) -> Option<Instance> {
    if q.color == 0 {
        return Some(glyph(q, fill, gamma, clip, fade));
    }
    let image = images.place(text.color_glyph(q.color), [q.rect[2], q.rect[3]])?;
    Some(Instance {
        rect: q.rect,
        radii: [0.0; 4],
        fill: [0.0, 0.0, 0.0, fill[3]],
        border: [0.0; 4],
        params: [image.page as f32, KIND_IMAGE, image.premultiplied as u8 as f32, 1.0],
        uv: image.rect.map(|v| v as f32),
        clip,
        fade: crate::render::pack_fade(fade, clip, clip),
    })
}

/// Where an anchored overlay of `size` goes next to `t` (x0, y0, x1, y1), flipped to the
/// opposite side when it doesn't fit, then kept 8 points inside the viewport.
fn anchor_position(a: &crate::el::Anchor, t: [f32; 4], size: (f32, f32), vp: (f32, f32)) -> (f32, f32) {
    use crate::el::Side;
    let (w, h) = size;
    let margin = 8.0;
    let along = |lo: f32, hi: f32, len: f32| match a.align {
        Align::Start => lo,
        Align::Center => (lo + hi - len) * 0.5,
        Align::End => hi - len,
    };
    let (mut x, mut y) = match a.side {
        Side::Top => (along(t[0], t[2], w), t[1] - h - a.gap),
        Side::Bottom => (along(t[0], t[2], w), t[3] + a.gap),
        Side::Left => (t[0] - w - a.gap, along(t[1], t[3], h)),
        Side::Right => (t[2] + a.gap, along(t[1], t[3], h)),
    };
    // Flip the side when the other one fits (Floating UI's `flip`).
    match a.side {
        Side::Bottom if y + h > vp.1 - margin && t[1] - h - a.gap >= margin => y = t[1] - h - a.gap,
        Side::Top if y < margin && t[3] + a.gap + h <= vp.1 - margin => y = t[3] + a.gap,
        Side::Right if x + w > vp.0 - margin && t[0] - w - a.gap >= margin => x = t[0] - w - a.gap,
        Side::Left if x < margin && t[2] + a.gap + w <= vp.0 - margin => x = t[2] + a.gap,
        _ => {}
    }
    // Flip the alignment when it overflows and the other end fits, then shift along the side.
    let vertical = matches!(a.side, Side::Top | Side::Bottom);
    let (pos, lo, hi, len, room) = if vertical { (&mut x, t[0], t[2], w, vp.0) } else { (&mut y, t[1], t[3], h, vp.1) };
    match a.align {
        Align::End if *pos < margin && lo + len <= room - margin => *pos = lo,
        Align::Start if *pos + len > room - margin && hi - len >= margin => *pos = hi - len,
        _ => {}
    }
    *pos = pos.clamp(margin, (room - len - margin).max(margin));
    // Along the main axis only keep the start edge inside; a long list may run off the far end.
    if vertical { y = y.max(margin) } else { x = x.max(margin) }
    (x, y)
}

/// Instances for a vector shape whose origin sits at `origin` (device px), with `k` device px per
/// coordinate unit on each axis and `k_px` device px per point for stroke widths.
#[allow(clippy::too_many_arguments)]
fn push_shape(out: &mut Vec<Instance>, shape: &Shape, origin: [f32; 2], k: [f32; 2], k_px: f32, opacity: f32, srgb: bool, clip: [f32; 4], fade: [f32; 4]) {
    let at = |p: [f32; 2]| [origin[0] + p[0] * k[0], origin[1] + p[1] * k[1]];
    for op in &shape.ops {
        match op {
            Op::Stroke { points, closed, width, cap, color } => {
                let fill = premul(*color, opacity, srgb);
                if fill[3] <= 0.0005 {
                    continue;
                }
                let hw = width * k_px * 0.5;
                let pad = hw.max(0.5) + 1.0;
                let n = points.len();
                let count = if *closed { n } else { n - 1 };
                for i in 0..count {
                    let (a, b) = (at(points[i]), at(points[(i + 1) % n]));
                    let (x0, y0) = (a[0].min(b[0]) - pad, a[1].min(b[1]) - pad);
                    out.push(Instance {
                        rect: [x0, y0, a[0].max(b[0]) + pad - x0, a[1].max(b[1]) + pad - y0],
                        radii: [a[0], a[1], b[0], b[1]],
                        fill,
                        border: [0.0; 4],
                        params: [hw, KIND_SEGMENT, if *cap == LineCap::Round { 1.0 } else { 0.0 }, 1.0],
                        uv: [0.0; 4],
                        clip,
                        fade: crate::render::pack_fade(fade, clip, clip),
                    });
                }
            }
            Op::Fill { points, color } => {
                let fill = premul(*color, opacity, srgb);
                if fill[3] <= 0.0005 {
                    continue;
                }
                for ([ia, ib, ic], aa) in crate::shape::triangulate(points) {
                    let (a, b, c) = (at(points[ia]), at(points[ib]), at(points[ic]));
                    let (x0, y0) = (a[0].min(b[0]).min(c[0]), a[1].min(b[1]).min(c[1]));
                    let (x1, y1) = (a[0].max(b[0]).max(c[0]), a[1].max(b[1]).max(c[1]));
                    out.push(Instance {
                        rect: [x0, y0, x1 - x0, y1 - y0],
                        radii: [a[0], a[1], b[0], b[1]],
                        fill,
                        border: [0.0; 4],
                        params: [aa as f32, KIND_TRIANGLE, 0.0, 1.0],
                        uv: [c[0], c[1], 0.0, 0.0],
                        clip,
                        fade: crate::render::pack_fade(fade, clip, clip),
                    });
                }
            }
        }
    }
}

fn rect_inst(rect: [f32; 4], fill: [f32; 4], radius: f32, clip: [f32; 4], fade: [f32; 4]) -> Instance {
    Instance {
        rect,
        radii: [radius; 4],
        fill,
        border: [0.0; 4],
        params: [0.0, KIND_RECT, 0.0, 1.0],
        uv: [0.0; 4],
        clip,
        fade: crate::render::pack_fade(fade, clip, clip),
    }
}

fn premul(c: Color, opacity: f32, srgb: bool) -> [f32; 4] {
    if srgb { c.premul_srgb(opacity) } else { c.premul(opacity) }
}

fn glyph_gamma(c: Color, scale: f32, srgb: bool) -> f32 {
    if srgb { 1.0 } else { crate::text::glyph_coverage_exponent(c, scale) }
}

/// Slots the ends hosts reported since the last frame into this frame's actions. One OS drag
/// runs at a time and hosts report ends in order, so each belongs to the oldest open drag: it
/// runs before any later drag's start, and after its own start when both are queued.
fn order_drag_ends(queue: Vec<Action>, ended: Vec<(String, crate::DragOutcome)>, open: &mut usize) -> Vec<Action> {
    let mut ended = ended.into_iter().map(|(path, outcome)| Action::DragEnd(path, outcome));
    let mut out = Vec::with_capacity(queue.len() + ended.len());
    for action in queue {
        match &action {
            Action::DragStart(_) => {
                while *open > 0 && let Some(end) = ended.next() {
                    out.push(end);
                    *open -= 1;
                }
                *open += 1;
            }
            Action::DragEnd(..) => *open = open.saturating_sub(1),
            _ => {}
        }
        out.push(action);
    }
    for end in ended {
        *open = open.saturating_sub(1);
        out.push(end);
    }
    out
}

/// An `aspect_fit` element's first layout: it takes the free space along its parent's main axis
/// and the whole cross side, within its max sizes, as the room its fitted box must fit. Its
/// content never sizes the room: with no max along the main axis it starts from 0 and grows.
fn fit_room(own: &Style, parent_row: bool) -> Style {
    let zero = |d: LengthPercentageAuto| if d.is_auto() { length(0.0) } else { d };
    let main_max = if parent_row { own.max_size.width } else { own.max_size.height };
    Style {
        size: own.max_size.map(Into::into),
        min_size: Size { width: zero(own.min_size.width), height: zero(own.min_size.height) },
        flex_grow: 1.0,
        flex_shrink: 1.0,
        flex_basis: if main_max.is_auto() { length(0.0) } else { auto() },
        align_self: Some(AlignSelf::STRETCH),
        aspect_ratio: None,
        ..own.clone()
    }
}

/// The largest box at `ratio` inside `room`, as the fixed size an `aspect_fit` element lays
/// out at once its room is known.
fn fitted(ratio: f32, own: &Style, room: Size<f32>) -> Style {
    let w = room.width.min(room.height * ratio).max(0.0);
    Style {
        size: Size { width: length(w), height: length(w / ratio) },
        flex_grow: 0.0,
        flex_shrink: 0.0,
        flex_basis: auto(),
        aspect_ratio: None,
        ..own.clone()
    }
}

/// Width over height of the image in `pixels`, or 0 while it's empty.
fn pixel_ratio(pixels: &crate::Pixels) -> f32 {
    let (w, h) = pixels.size();
    if w > 0 && h > 0 { w as f32 / h as f32 } else { 0.0 }
}

// Build the owned path directly, without cloning a key or formatting an index into a
// temporary segment first. Measurement and flattening must use the same identity rule.
/// Whether `path` lies inside an element keyed `scope` (always, without one).
pub(crate) fn in_scope(path: &str, scope: Option<&str>) -> bool {
    scope.is_none_or(|s| path.split('/').any(|seg| seg == s))
}

fn node_path(parent: &str, key: Option<&str>, index: usize) -> String {
    match (parent.is_empty(), key) {
        (true, Some(key)) => key.to_owned(),
        (true, None) => format!("#{index}"),
        (false, Some(key)) => format!("{parent}/{key}"),
        (false, None) => format!("{parent}/#{index}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn flatten(
    el: El,
    parent: Option<usize>,
    parent_path: &str,
    index: usize,
    motion: Spring,
    in_row: RowLayout,
    // The parent lays its children out in a row (its main axis is horizontal).
    parent_row: bool,
    taffy: &mut TaffyTree<usize>,
    nodes: &mut Vec<Node>,
    entries: &mut HashMap<String, LayoutEntry>,
    scale: f32,
) -> taffy::NodeId {
    let mut el = el;
    el.expand_memo();
    let path = node_path(parent_path, el.key.as_deref(), index);
    let own_motion = el.motion;
    let motion = own_motion.unwrap_or(motion);
    let me = nodes.len();
    let parent_is_stack = parent.is_some_and(|p| nodes[p].growth.is_some());
    let animate_layout = el.animate_layout;
    let in_row = if el.above.is_some() && (animate_layout || in_row == RowLayout::Horizontal) {
        RowLayout::Free
    } else if animate_layout && in_row == RowLayout::Snaps {
        RowLayout::Horizontal
    } else {
        in_row
    };
    let virtual_mount = el.virtual_mount;
    #[cfg(debug_assertions)]
    let scroll_source = el.scroll_source;
    let trace_namespace = el.trace_namespace.or_else(|| parent.and_then(|p| nodes[p].trace_namespace));
    let El {
        on_paste,
        on_window_focus,
        focus,
        modal,
        looping,
        key,
        trace_name,
        role,
        label,
        style,
        vis,
        live,
        text,
        children,
        on_press,
        on_double_press,
        on_secondary_press,
        on_hover,
        on_hover_change,
        hover_delay,
        on_pointer_move,
        gestures,
        above,
        on_drag,
        drag_out,
        drag_threshold,
        on_drag_start,
        on_drag_end,
        drop,
        opacity_motion,
        lead,
        width_leads,
        transition,
        matched,
        scroll,
        scroll_axes,
        on_at_bottom,
        on_visible_rows,
        on_layout,
        on_screen_layout,
        on_drawn_layout,
        sticky,
        scroll_edges,
        growth,
        streaming,
        input,
        cursor,
        window_drag,
        press_scale,
        scale: static_scale,
        time_scale,
        help,
        help_cut,
        material,
        aspect_fit,
        selection_group,
        a11y,
        inert,
        ..
    } = el;
    let mut text = text;
    if input.as_ref().is_some_and(|i| i.edit.is_hidden_multiline()) && let Some(t) = &mut text {
        // The fixed mask stays one line even in a field narrower than twelve bullets.
        t.wrap = false;
    }
    let inert = inert || parent.is_some_and(|p| nodes[p].inert);
    let mut vis = vis;
    if vis.hover_within.is_none() {
        vis.hover_within = above.as_ref()
            .and_then(|a| nodes.iter().position(|n| n.key.as_ref() == Some(&a.key)))
            .and_then(|j| nodes[j].vis.hover_within)
            .or_else(|| parent.and_then(|p| nodes[p].vis.hover_within));
    }
    if vis.hover_motion.is_none() {
        vis.hover_motion = vis.hover_within;
    }
    if let Some(t) = &text && t.live.is_some() {
        assert!(input.is_none() && !t.wrap && t.spans.is_empty() && t.reveal < 0.0 && !t.truncate,
            "live_text requires plain single-line text; use truncate_fade for overflow");
    }
    let mut style = style;
    let pixel_aspect = vis.pixels.as_ref()
        .filter(|_| style.aspect_ratio.is_none() && style.size.width.is_auto() != style.size.height.is_auto())
        .map(pixel_ratio);
    if let Some(ratio) = pixel_aspect.filter(|&r| r > 0.0) {
        style.aspect_ratio = Some(ratio);
    }
    let aspect_fit = (aspect_fit > 0.0).then(|| {
        let room = fit_room(&style, parent_row);
        Box::new((aspect_fit, std::mem::replace(&mut style, room)))
    });
    let mut text = text;
    if input.is_none() && let Some(t) = &mut text { t.apply_case(); }
    let measured = text.is_some();
    let measure = text.as_ref().map(|t| crate::text::text_hash(&t.content, &t.spans, t.style, scale, None) ^ u64::from(t.wrap) ^ (u64::from(t.truncate || t.truncate_fade) << 1)
        ^ ((t.max_lines as u64) << 2)
        ^ (u64::from(input.as_ref().is_some_and(|i| i.edit.is_revealed())) << 3)
        ^ (u64::from(t.wrap_anywhere) << 4)
        ^ t.advance_in.as_ref().map_or(0, |(run, at)| run.hash.rotate_left(3) ^ (*at as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)));
    nodes.push(Node {
        on_paste,
        on_window_focus,
        focus,
        modal,
        looping,
        id: NodeId(0),
        path: path.clone(),
        key,
        trace_name,
        trace_namespace,
        role,
        label,
        vis,
        live,
        text,
        text_layout: None,
        elided: None,
        run_layout: None,
        live_text_seen: None,
        on_press,
        on_double_press,
        on_secondary_press,
        on_hover,
        on_hover_change,
        hover_delay,
        on_pointer_move,
        gestures,
        above,
        on_drag,
        drag_out,
        drag_threshold,
        on_drag_start,
        on_drag_end,
        drop,
        motion,
        opacity_motion,
        lead,
        width_leads,
        transition,
        matched: matched.map(|id| (id, own_motion)),
        scroll,
        scroll_axes,
        on_at_bottom,
        on_visible_rows,
        on_layout,
        on_screen_layout,
        on_drawn_layout,
        sticky,
        scroll_edges,
        growth,
        streaming,
        input,
        cursor,
        window_drag,
        selection_group,
        a11y,
        inert,
        press_scale,
        static_scale,
        scale_track: false,
        time_scale,
        pixel_aspect,
        aspect_fit,
        material,
        in_row,
        virtual_mount,
        parent,
        taffy: taffy::NodeId::from(0u64),
        target: [0.0; 4],
        local: MRect::ZERO,
        inset: [0.0; 4],
        help,
        help_cut,
        #[cfg(debug_assertions)]
        scroll_source,
    });
    let child_in_row = match in_row {
        RowLayout::Free if parent_is_stack => if animate_layout { RowLayout::Horizontal } else { RowLayout::Snaps },
        r => r,
    };
    let mut children = children;
    if scroll.is_some() && scroll_axes.x() && let Some(content) = children.first_mut() {
        style.flex_direction = FlexDirection::Row;
        unwrapped_width(&mut content.style, scroll_axes.y());
    }
    let row = matches!(style.flex_direction, FlexDirection::Row | FlexDirection::RowReverse);
    let kids: Vec<taffy::NodeId> = children
        .into_iter()
        .enumerate()
        .map(|(i, c)| flatten(c, Some(me), &path, i, motion, child_in_row, row, taffy, nodes, entries, scale))
        .collect();
    let id = if let Some(entry) = entries.get_mut(&path) {
        let id = entry.id;
        if taffy.style(id).unwrap() != &style {
            taffy.set_style(id, style).unwrap();
        }
        if !taffy.child_ids(id).eq(kids.iter().copied()) {
            taffy.set_children(id, &kids).unwrap();
        }
        // Context indices can move after insertion/reorder without invalidating measurements.
        // Only text/style/scale changes invalidate the cached leaf result.
        if entry.measure.is_some() != measured {
            taffy.set_node_context(id, measured.then_some(me)).unwrap();
        } else if measured {
            *taffy.get_node_context_mut(id).unwrap() = me;
        }
        if entry.measure != measure {
            taffy.mark_dirty(id).unwrap();
        }
        entry.measure = measure;
        entry.used = true;
        id
    } else if !kids.is_empty() {
        taffy.new_with_children(style, &kids).unwrap()
    } else if measured {
        taffy.new_leaf_with_context(style, me).unwrap()
    } else {
        taffy.new_leaf(style).unwrap()
    };
    if !entries.contains_key(&path) {
        if measured && taffy.get_node_context(id).is_none() {
            taffy.set_node_context(id, Some(me)).unwrap();
        }
        entries.insert(path, LayoutEntry { id, measure, used: true });
    }
    nodes[me].taffy = id;
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PointerPress;
    use crate::el::{col, el};

    thread_local! {
        /// Runs once inside the next frame, after layout and just before live values latch: a
        /// publication from another thread landing at the worst moment.
        pub(super) static MID_FRAME: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    }

    #[test]
    fn dissolve_prepass_only_runs_for_trees_with_dissolve_nodes() {
        fn view(enabled: &bool, _: &mut Cx<bool>) -> El {
            let root = el().size(64.0, 32.0).child(el().size(32.0, 32.0).opacity(0.5).fill(Color::WHITE));
            if *enabled { root.dissolve() } else { root }
        }
        let mut rt = Runtime::new(false, view, (64.0, 32.0), Env::new(true, 1.0));
        for enabled in [false, true, false] {
            rt.state = enabled; rt.invalidate(); rt.frame(0.0);
            assert_eq!(rt.has_dissolve, enabled);
            assert_eq!(rt.dissolve_scratch.is_empty(), !enabled);
            if enabled { assert!(rt.dissolve_scratch[0]); }
        }
    }

    #[test]
    fn a_child_too_faint_to_paint_does_not_isolate_its_dissolve_parent() {
        fn view(opacity: &f32, _: &mut Cx<f32>) -> El {
            el().size(64.0, 32.0).dissolve().child(el().size(32.0, 32.0).opacity(*opacity).fill(Color::WHITE))
        }
        let mut rt = Runtime::new(0.0005, view, (64.0, 32.0), Env::new(true, 1.0));
        rt.frame(0.0);
        assert!(!rt.dissolve_scratch[0]);
        rt.state = 0.5; rt.invalidate();
        for _ in 0..60 { rt.frame(1.0 / 60.0); }
        assert!(rt.dissolve_scratch[0]);
    }

    #[test]
    fn window_frame_requests_take_the_change_spring() {
        let mut rt = Runtime::new((), |_: &(), _: &mut Cx<()>| el(), (100.0, 100.0), Env::new(true, 1.0));
        let spring = |rt: &mut Runtime<()>, f: fn(&mut Tx)| {
            rt.update(move |_, tx| f(tx));
            rt.window_frame.take().unwrap().spring
        };
        assert_eq!(spring(&mut rt, |tx| tx.resize_window((1.0, 2.0))), Some(motion::DEFAULT));
        assert_eq!(spring(&mut rt, |tx| { tx.motion(motion::SNAP); tx.resize_window((1.0, 2.0)) }), Some(motion::SNAP));
        assert_eq!(spring(&mut rt, |tx| { tx.instant(); tx.resize_window((1.0, 2.0)) }), None);
        assert_eq!(spring(&mut rt, |tx| tx.resize_window_instant((1.0, 2.0))), None);
        rt.set_system_reduced_motion(true);
        rt.frame(0.0);
        assert_eq!(spring(&mut rt, |tx| tx.resize_window((1.0, 2.0))), None);
        rt.update(|_, tx| { tx.resize_window((1.0, 2.0)); tx.center_window(); });
        let request = rt.window_frame.take().unwrap();
        assert_eq!((request.size, request.center), (Some((1.0, 2.0)), true));
    }

    #[test]
    fn a_window_resize_step_keeps_springs_and_flights_going() {
        fn view(grown: &bool, _: &mut Cx<bool>) -> El {
            let tile = || el().key("tile").matched("tile").size(40.0, 40.0).fill(Color::WHITE);
            let (mut left, mut right) = (el().key("left").size(100.0, 60.0), el().key("right").size(100.0, 60.0).ml(150.0));
            if *grown { right = right.child(tile()) } else { left = left.child(tile()) }
            col().key("root").w_full().h_full()
                .child(el().key("box").size(50.0, if *grown { 150.0 } else { 50.0 }).fill(Color::WHITE))
                .child(left).child(right)
                .child(crate::el::spacer())
                .child(el().key("bar").w_full().h(20.0).fill(Color::WHITE))
        }
        let resize = |user: bool| {
            let mut rt = Runtime::new(false, view, (300.0, 400.0), Env::new(true, 1.0));
            rt.frame(0.0);
            rt.update(|grown, tx| { *grown = true; tx.resize_window((300.0, 600.0)) });
            rt.frame(1.0 / 60.0);
            rt.window_frame.take();
            if user { rt.set_viewport((300.0, 450.0), 1.0) } else { rt.step_viewport((300.0, 450.0), 1.0) }
            rt.frame(1.0 / 60.0);
            rt
        };
        let rt = resize(false);
        let rect = |rt: &Runtime<bool>, key: &str| rt.presented(key).unwrap().0;
        let h = rect(&rt, "box")[3];
        assert!(h > 50.0 && h < 150.0, "the box's spring keeps going: {h}");
        assert!(rt.elements().iter().any(|e| e.key.as_deref() == Some("tile") && e.moving), "the tile is still in flight");
        let x = rect(&rt, "tile")[0];
        assert!(x > 0.0 && x < 150.0, "mid-flight: {x}");
        assert_eq!(rect(&rt, "bar")[1], 430.0, "content at rest follows the window");
        assert_eq!(rect(&resize(true), "box")[3], 150.0, "a user's resize lands running motion");
    }

    #[test]
    fn a_window_that_fits_its_content_refits_after_a_user_resize() {
        fn view(_: &(), _: &mut Cx<()>) -> El { col().key("root").w_full().child(el().size(100.0, 90.0)) }
        let mut rt = Runtime::new((), view, (300.0, 400.0), Env::new(true, 1.0));
        rt.fit_height = true;
        let height = rt.content_height();
        rt.set_viewport((300.0, height), 1.0);
        rt.frame(0.0);
        rt.window_frame.take();
        // Mid-move, the window's own frame changes aren't a user's resize.
        rt.set_viewport((300.0, height + 40.0), 1.0);
        rt.frame(1.0 / 60.0);
        assert!(rt.window_frame.take().is_none());
        rt.window_resting = true;
        rt.set_viewport((300.0, height + 100.0), 1.0);
        rt.frame(1.0 / 60.0);
        assert_eq!(rt.window_frame.take().and_then(|r| r.size), Some((300.0, height)));
    }

    /// A window that fits its content, and grew while hidden, learns its new height before it
    /// shows: its first frame isn't clipped, then resized in view.
    #[test]
    fn a_hidden_window_that_fits_its_content_learns_its_height_before_it_shows() {
        fn view(s: &(usize, bool), _: &mut Cx<(usize, bool)>) -> El {
            col().key("root").w_full().children((0..s.0).map(|_| el().size(100.0, 40.0)))
        }
        let mut rt = Runtime::new((1, false), view, (300.0, 400.0), Env::new(true, 1.0));
        rt.fit_height = true;
        let height = rt.content_height();
        rt.set_viewport((300.0, height), 1.0);
        rt.frame(0.0);
        rt.window_frame.take();
        assert!(!rt.build_for_show(), "unchanged, the last frame still shows it");
        rt.poll = Some(|s: &mut (usize, bool), _, _: &mut Tx| {
            if !std::mem::take(&mut s.1) { return false; }
            s.0 += 1;
            true
        });
        rt.state.1 = true;
        let commits = rt.stats.commits;
        assert!(rt.poll_hidden());
        assert!(rt.window_frame.is_none() && rt.stats.commits == commits, "hidden, nothing builds yet");
        assert!(rt.build_for_show());
        let request = rt.window_frame.take().expect("the new height, before the window shows");
        assert_eq!(request.size, Some((300.0, 80.0)));
        assert!(!request.animate, "it opens at that height");
        rt.set_viewport((300.0, 80.0), 1.0);
        rt.frame(0.0);
        assert_eq!(rt.stats.commits, commits + 1, "the first frame shown has nothing left to build");
        assert!(rt.window_frame.is_none());
    }

    #[test]
    fn sequences_run_without_commits_then_sleep_and_settle() {
        fn view(_: &(), _: &mut Cx<()>) -> El {
            const BEATS: &[crate::Beat] = &[
                crate::Beat::new(0.0).opacity(0.0).scale(0.8),
                crate::Beat::new(0.3).easing(crate::Easing::LINEAR),
                crate::Beat::new(0.6).opacity(0.5).scale(1.1).easing(crate::Easing::LINEAR),
                crate::Beat::new(0.9).easing(crate::Easing::LINEAR),
            ];
            el().key("root").size(200.0, 100.0).child(el().key("beat").size(80.0, 40.0)
                .sequence(crate::Sequence::new(BEATS).delay(0.2)))
        }
        let make = || Runtime::new((), view, (200.0, 100.0), Env { scale: 1.0, system_dark: true });
        let mut rt = make();
        assert!(!rt.frame(0.0));
        assert_eq!(rt.wake_deadline(), Some(0.2));
        let commits = rt.stats.commits;
        assert!(rt.frame(0.35));
        let pose = |rt: &Runtime<()>| rt.elements().into_iter().find(|e| e.key.as_deref() == Some("beat")).unwrap();
        assert!((pose(&rt).opacity - 0.5).abs() < 1e-5);
        assert!((pose(&rt).scale - 0.9).abs() < 1e-5);
        assert_eq!(rt.stats.commits, commits);
        rt.invalidate(); rt.frame(0.0);
        assert!((pose(&rt).opacity - 0.5).abs() < 1e-5, "unrelated commits must not restart the sequence");
        assert!(!rt.frame(1.0));
        assert_eq!(rt.wake_deadline(), None);
        assert_eq!(pose(&rt).opacity, 1.0);
        assert_eq!(pose(&rt).scale, 1.0);
        let mut settled = make(); settled.frame(0.0); settled.settle_all();
        assert!(!settled.frame(0.0));
        assert_eq!(pose(&settled).opacity, 1.0);
        assert_eq!(settled.wake_deadline(), None);
        let mut reduced = make(); reduced.set_system_reduced_motion(true);
        assert!(!reduced.frame(0.0));
        assert_eq!(pose(&reduced).scale, 1.0);
        assert_eq!(pose(&reduced).opacity, 1.0);
        assert_eq!(pose(&reduced).loop_phase, Some(1.0));
        assert_eq!(reduced.wake_deadline(), None);
    }

    #[test]
    fn layout_motion_does_not_replace_explicit_transition_exit() {
        fn view(s: &(bool, f32), _: &mut Cx<(bool, f32)>) -> El {
            col().size(200.0, 100.0).children(s.0.then(|| el().key("pill").size(100.0, 40.0)
                .motion(Spring::new(s.1, 0.0))
                .transition(Transition::scale(0.96).springs(rune_motion::SpringPair {
                    enter: Spring::new(0.3, 0.0), exit: Spring::new(0.8, 0.0),
                }))))
        }
        let make = |duration| Runtime::new((true, duration), view, (200.0, 100.0), Env { scale: 1.0, system_dark: true });
        let (mut fast, mut slow) = (make(0.05), make(0.6));
        for rt in [&mut fast, &mut slow] {
            rt.frame(0.0);
            rt.state.0 = false; rt.invalidate(); rt.frame(0.0);
            rt.frame(0.1);
        }
        let opacity = |rt: &Runtime<(bool, f32)>| rt.elements().into_iter()
            .find(|e| e.key.as_deref() == Some("pill")).unwrap().opacity;
        assert!(opacity(&fast) > 0.5, "the explicit slow exit must survive fast layout tuning");
        assert!((opacity(&fast) - opacity(&slow)).abs() < 1e-6);
    }

    #[test]
    fn a_node_leaves_with_the_transition_of_its_last_render() {
        // (shown, way): the page slides out the way of the last render.
        fn view(s: &(bool, f32), _: &mut Cx<(bool, f32)>) -> El {
            col().size(200.0, 100.0).children(s.0.then(|| el().key("page").size(100.0, 40.0)
                .transition(Transition::offset(40.0 * s.1, 0.0))))
        }
        let x = |rt: &Runtime<(bool, f32)>| rt.elements().into_iter()
            .find(|e| e.key.as_deref() == Some("page")).unwrap().rect[0];
        let make = |shown| Runtime::new((shown, -1.0), view, (200.0, 100.0), Env { scale: 1.0, system_dark: true });

        // Entering from the left; a new render mid-entrance doesn't change the entrance.
        let (mut kept, mut turned) = (make(false), make(false));
        for rt in [&mut kept, &mut turned] {
            rt.frame(0.0);
            rt.state.0 = true; rt.invalidate(); rt.frame(0.0); rt.frame(0.05);
        }
        turned.state.1 = 1.0; turned.invalidate();
        kept.frame(0.05); turned.frame(0.05);
        assert!(x(&turned) < 0.0, "still entering from the left: {}", x(&turned));
        assert!((x(&kept) - x(&turned)).abs() < 1e-4, "the running entrance is unaffected");

        // Removed after the render that turned it: it leaves to the right.
        let mut rt = make(true);
        rt.frame(0.0); rt.frame(2.0);
        assert_eq!(x(&rt), 0.0);
        rt.state.1 = 1.0; rt.invalidate(); rt.frame(0.0);
        rt.state.0 = false; rt.invalidate(); rt.frame(0.0); rt.frame(0.1);
        assert!(x(&rt) > 1.0, "leaves the way of its last render: {}", x(&rt));
    }

    #[test]
    fn element_effects_include_ancestor_opacity_scale_and_exit_blur() {
        fn view(show: &bool, _: &mut Cx<bool>) -> El {
            col().size(200.0, 100.0).children(show.then(|| el().key("parent").size(100.0, 60.0)
                .opacity(0.5).live_scale(&crate::live::Live::new(0.8)).transition(Transition::blur(4.0))
                .child(el().key("child").size(20.0, 20.0).opacity(0.5).live_scale(&crate::live::Live::new(0.5)))))
        }
        let mut rt = Runtime::new(true, view, (200.0, 100.0), Env { scale: 2.0, system_dark: true });
        rt.frame(0.0);
        let child = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("child")).unwrap();
        assert!((child.opacity - 0.25).abs() < 1e-6);
        assert!((child.scale - 0.4).abs() < 1e-6);
        assert_eq!(child.blur, 0.0);
        rt.state = false; rt.invalidate(); rt.frame(0.0); rt.frame(0.05);
        let elements = rt.elements();
        let parent = elements.iter().find(|e| e.key.as_deref() == Some("parent")).unwrap();
        let child = elements.iter().find(|e| e.key.as_deref() == Some("child")).unwrap();
        assert!(parent.blur > 0.0);
        assert_eq!(child.blur, parent.blur);
        assert!((child.opacity - parent.opacity * 0.5).abs() < 1e-6);
    }

    #[test]
    fn exiting_elements_are_flagged_and_gone_from_key_lookups() {
        fn view(show: &bool, _: &mut Cx<bool>) -> El {
            col().size(200.0, 100.0).children(show.then(|| el().key("card").size(100.0, 60.0)
                .transition(Transition::blur(4.0))
                .child(el().key("label").size(20.0, 20.0))))
        }
        let mut rt = Runtime::new(true, view, (200.0, 100.0), Env { scale: 1.0, system_dark: true });
        rt.frame(0.0);
        assert!(rt.find("card").is_some() && rt.find("label").is_some());
        assert!(rt.elements().iter().all(|e| !e.exiting));
        rt.state = false; rt.invalidate(); rt.frame(0.0); rt.frame(0.05);
        let elements = rt.elements();
        for key in ["card", "label"] {
            let e = elements.iter().find(|e| e.key.as_deref() == Some(key)).expect("still drawn while it exits");
            assert!(e.exiting && e.inert, "{key} is exiting");
            assert_eq!(rt.find(key), None, "{key} is gone from lookups");
            assert!(rt.presented(key).is_some(), "presented still reads {key}");
        }
    }

    fn duplicate_view(_: &(), _: &mut Cx<()>) -> El {
        col().children([el().key("card"), el().key("card")])
    }

    #[test]
    fn overlay_hit_regions_follow_live_geometry_and_ancestor_clips_in_the_same_frame() {
        struct State { offset: crate::Live<[f32; 2]>, opacity: crate::Live<f32>, pressed: u32, hovered: u32, visible: bool }
        fn view(s: &State, _: &mut Cx<State>) -> El {
            el().child(el().size(60.0, 60.0).clip_soft(0.0)
                .children(s.visible.then(|| el().key("chip").size(40.0, 20.0).pin(Some(30.0), Some(0.0), None, None)
                    .live_offset(&s.offset).live_opacity(&s.opacity).hit()
                    .on_hover(|s: &mut State| s.hovered += 1).on_press(|s: &mut State| s.pressed += 1))))
                .child(el().size(10.0, 10.0).on_hover(|_: &mut State| {}))
        }
        let offset = crate::Live::new([0.0, 0.0]);
        let opacity = crate::Live::new(1.0);
        let mut rt = Runtime::new(State { offset: offset.clone(), opacity: opacity.clone(), pressed: 0, hovered: 0, visible: true }, view, (100.0, 100.0), Env { system_dark: true, scale: 2.0 });
        rt.native_overlay = true;
        rt.frame(0.0);
        assert_eq!(rt.hit_regions(), &[[30.0, 0.0, 60.0, 20.0]]);
        rt.pointer_move(15.0, 10.0);
        offset.set([20.0, 0.0]);
        rt.frame(0.0);
        assert_eq!(rt.hit_regions(), &[[50.0, 0.0, 60.0, 20.0]], "logical regions must use this frame's live offset, not the previous layout");
        offset.set([-20.0, 0.0]);
        rt.frame(0.0);
        assert_eq!(rt.hit_regions(), &[[10.0, 0.0, 50.0, 20.0]]);
        rt.pointer_down(15.0, 10.0);
        rt.pointer_up(15.0, 10.0);
        rt.frame(0.0);
        assert_eq!(rt.state.pressed, 1, "the native hit and Rune release must agree on the moved chip");
        assert_eq!(rt.state.hovered, 1, "a chip moving under a stationary pointer enters hover once");
        assert!(!rt.has_pending_input(), "stationary hover must not generate an endless frame loop");
        rt.pointer_down(15.0, 10.0);
        rt.pointer_up(55.0, 10.0);
        rt.frame(0.0);
        assert_eq!(rt.state.pressed, 1, "release inside the old layout but outside the drawn chip must cancel");
        opacity.set(0.0);
        rt.frame(0.0);
        assert!(rt.hit_regions().is_empty(), "transparent nodes and hover-only elements do not block native input");
        opacity.set(1.0);
        rt.update(|s, _| s.visible = false);
        rt.frame(0.0);
        assert!(rt.hit_regions().is_empty(), "exiting chips must immediately release native input");
    }

    #[test]
    fn overlay_pointer_observation_does_not_require_a_native_hit_region() {
        fn view(_: &Vec<PointerPress>, _: &mut Cx<Vec<PointerPress>>) -> El {
            el().on_pointer_move(|s: &mut Vec<PointerPress>, p| s.push(p))
        }
        let mut rt = Runtime::new(Vec::new(), view, (100.0, 100.0), Env { system_dark: true, scale: 1.0 });
        rt.frame(0.0);
        rt.pointer_move(20.0, 40.0);
        rt.frame(0.0);
        assert_eq!(rt.state, [PointerPress { x: 20.0, y: 40.0 }]);
        assert!(rt.hit_regions().is_empty());
    }

    #[test]
    fn overlay_idle_pointer_does_not_request_unchanged_frames() {
        fn view(_: &u32, _: &mut Cx<u32>) -> El {
            el().child(el().size(40.0, 20.0).hit().on_hover(|n: &mut u32| *n += 1))
        }
        let mut rt = Runtime::new(0, view, (100.0, 100.0), Env { system_dark: true, scale: 1.0 });
        rt.frame(0.0);
        assert!(rt.observe_pointer(5.0, 5.0), "enter must run the hover callback");
        rt.frame(0.0);
        assert_eq!(rt.state, 1);
        assert!(!rt.observe_pointer(6.0, 5.0), "movement within an unchanged chip needs no frame");
        assert!(!rt.observe_pointer(6.0, 5.0), "duplicate monitor/winit observations need no frame");
        assert!(!rt.observe_pointer(90.0, 90.0));
        assert!(!rt.observe_pointer(200.0, 90.0), "movement outside the canvas needs no frame");
        assert!(!rt.has_pending_input());
    }

    #[test]
    fn hit_tests_leave_out_the_right_and_bottom_edges() {
        fn tile(k: &'static str) -> El {
            el().key(k).size(40.0, 20.0)
                .on_press(move |s: &mut Vec<String>| s.push(format!("press {k}")))
                .on_hover(move |s: &mut Vec<String>| s.push(format!("hover {k}")))
                .on_hover_change(move |s: &mut Vec<String>, inside, _| if inside { s.push(format!("enter {k}")) })
                .on_pointer_move(move |s: &mut Vec<String>, _| s.push(format!("move {k}")))
        }
        fn view(_: &Vec<String>, _: &mut Cx<Vec<String>>) -> El {
            col().gap(20.0).child(crate::row().child(tile("a")).child(tile("b"))).child(tile("lone"))
        }
        let mut rt = Runtime::new(Vec::new(), view, (200.0, 200.0), Env { system_dark: true, scale: 1.0 });
        rt.frame(0.0);
        assert_eq!(rt.presented("lone").unwrap().0, [0.0, 40.0, 40.0, 20.0]);
        let mut click = |x, y| {
            rt.pointer_move(x, y);
            rt.pointer_down(x, y);
            rt.pointer_up(x, y);
            rt.frame(0.0);
            let mut events = std::mem::take(&mut rt.state);
            rt.pointer_leave();
            rt.frame(0.0);
            rt.state.clear();
            events.sort();
            events.dedup();
            events
        };
        assert_eq!(click(40.0, 10.0), ["enter b", "hover b", "move b", "press b"], "the shared edge is b's alone");
        assert_eq!(click(40.0, 50.0), Vec::<String>::new(), "a lone node's right edge is outside it");
        assert_eq!(click(20.0, 60.0), Vec::<String>::new(), "and so is its bottom edge");
        assert_eq!(click(0.0, 40.0), ["enter lone", "hover lone", "move lone", "press lone"], "its top-left corner is inside");
    }

    #[test]
    fn deleting_a_focused_row_does_not_call_its_stale_edit_accessor() {
        fn view(s: &Vec<crate::TextEdit>, _: &mut Cx<Vec<crate::TextEdit>>) -> El {
            col().children(s.iter().enumerate().map(|(i, edit)| {
                crate::text_input(edit, crate::TextStyle::new(14.0, 400.0))
                    .key(format!("row-{i}"))
                    .on_edit(crate::edit_access(move |s: &mut Vec<crate::TextEdit>| &mut s[i]))
            })).child(el().key("delete").size(40.0, 40.0).on_press(|s: &mut Vec<crate::TextEdit>| { s.clear(); }))
        }
        let mut edit = crate::TextEdit::default();
        edit.focused = true;
        let mut rt = Runtime::new(vec![edit], view, (100.0, 100.0), Env::new(true, 1.0));
        rt.frame(0.0);
        let path = rt.nodes.iter().find(|n| n.key.as_deref() == Some("delete")).unwrap().path.clone();
        rt.queue.push(Action::Press(path, crate::PointerButton::Primary));
        rt.frame(0.0);
        assert!(rt.state.is_empty());
        assert!(!rt.has_focus());
    }

    #[test]
    fn secondary_only_children_preserve_primary_click_hover_and_background_blur() {
        type State = (u32, u32, bool);
        fn view(s: &State, _: &mut Cx<State>) -> El {
            let parent = el().key("parent").size(80.0, 80.0);
            let parent = if s.2 { parent } else {
                parent.on_press(|s: &mut State| s.0 += 1)
                    .on_hover(|s: &mut State| s.1 += 1)
            };
            parent.child(el().key("child").size(40.0, 40.0)
                .on_secondary_press(|_: &mut State, _, _| {}))
        }
        let mut rt = Runtime::new((0, 0, false), view, (100.0, 100.0), Env::new(true, 1.0));
        rt.frame(0.0);
        rt.pointer_move(10.0, 10.0);
        rt.pointer_down(10.0, 10.0);
        rt.pointer_up(10.0, 10.0);
        rt.frame(0.0);
        assert_eq!(rt.state, (1, 1, false));
        rt.state.2 = true;
        rt.invalidate();
        rt.frame(0.0);
        rt.focused = Some("previous-input".into());
        rt.pointer_move(10.0, 10.0);
        rt.pointer_down(10.0, 10.0);
        assert!(rt.hovered.is_none());
        assert!(matches!(rt.queue.last(), Some(Action::Blur)));
        assert!(rt.secondary_target(10.0, 10.0).is_some());
    }

    #[test]
    fn secondary_press_routes_to_ancestor_without_primary_and_cancels_outside() {
        type State = (u32, Vec<PointerPress>, bool);
        fn view(s: &State, _: &mut Cx<State>) -> El {
            col().child(if s.2 {
                el()
            } else {
                el().key("parent").size(80.0, 80.0)
                    .on_secondary_press(|s: &mut State, p, _| s.1.push(p))
                    .child(el().key("child").size(40.0, 40.0).on_press(|s: &mut State| s.0 += 1))
            })
        }
        let mut rt = Runtime::new((0, Vec::new(), false), view, (100.0, 100.0), Env::new(true, 1.0));
        rt.frame(0.0);
        rt.secondary_down(10.0, 10.0);
        rt.secondary_up(12.0, 12.0);
        rt.frame(0.0);
        assert_eq!(rt.state.0, 0);
        assert_eq!(rt.state.1, [PointerPress { x: 12.0, y: 12.0 }]);
        rt.secondary_down(10.0, 10.0);
        rt.secondary_up(90.0, 90.0);
        rt.frame(0.0);
        assert_eq!(rt.state.1.len(), 1);
        rt.secondary_down(10.0, 10.0);
        rt.state.2 = true;
        rt.invalidate();
        rt.frame(0.0);
        rt.secondary_up(12.0, 12.0);
        rt.frame(0.0);
        assert_eq!(rt.state.1.len(), 1);
    }

    #[test]
    fn timed_wakes_are_exact_coalesced_and_do_not_commit_or_animate() {
        use std::time::Duration;
        fn view(_: &Vec<f64>, cx: &mut Cx<Vec<f64>>) -> El {
            cx.time_scale(0.1);
            el()
        }
        fn poll(seen: &mut Vec<f64>, clock: f64, tx: &mut Tx) -> bool {
            seen.push(clock);
            if clock == 0.0 {
                tx.wake_after(Duration::from_millis(250));
                tx.wake_after(Duration::from_millis(250));
                tx.wake_after(Duration::from_millis(375));
            }
            false
        }
        let mut rt = Runtime::new(Vec::new(), view, (100.0, 100.0), Env::new(true, 1.0));
        rt.poll = Some(poll);
        assert!(!rt.frame(0.0));
        assert_eq!(rt.wake_deadlines, [0.25, 0.375]);
        assert!(!rt.frame(0.5));
        assert_eq!(rt.state, [0.0, 0.25, 0.375, 0.5]);
        assert_eq!(rt.commits, 1);
        assert_eq!(rt.wake_deadline(), None);
        assert!((rt.t - 0.05).abs() < 1e-8);
    }

    #[test]
    fn wakes_a_poll_keeps_asking_for_collapse_to_one_poll_a_frame() {
        use std::time::Duration;
        // (polls this frame, delay each poll asks for)
        fn poll(s: &mut (usize, f64), _: f64, tx: &mut Tx) -> bool {
            s.0 += 1;
            tx.wake_after(Duration::from_secs_f64(s.1));
            false
        }
        // A wake every frame, a wake that lands between frames, a save timer that rearms on
        // every poll, and one that lands just past a frame boundary.
        for delay in [0.0, 1.0 / 60.0, 1.0 / 120.0, 0.505, 0.5] {
            fn view(_: &(usize, f64), _: &mut Cx<(usize, f64)>) -> El { el() }
            let mut rt = Runtime::new((0, delay), view, (100.0, 100.0), Env::new(true, 1.0));
            rt.poll = Some(poll);
            for frame in 0..1000 {
                rt.state.0 = 0;
                rt.frame(1.0 / 60.0);
                assert_eq!(rt.state.0, 1, "delay {delay}: frame {frame} polled {} times", rt.state.0);
            }
            assert!(rt.wake_deadlines.len() <= 32, "delay {delay}: {} deadlines pending", rt.wake_deadlines.len());
        }
    }

    #[test]
    fn a_zero_wake_does_not_hide_a_later_wake_in_a_coarse_frame() {
        use std::time::Duration;
        fn poll(s: &mut Vec<f64>, now: f64, tx: &mut Tx) -> bool {
            s.push((now * 1000.0).round() / 1000.0);
            if s.len() == 1 {
                tx.wake_after(Duration::ZERO);
                tx.wake_after(Duration::from_millis(50));
            }
            false
        }
        fn view(_: &Vec<f64>, _: &mut Cx<Vec<f64>>) -> El { el() }
        let mut rt = Runtime::new(Vec::new(), view, (100.0, 100.0), Env::new(true, 1.0));
        rt.poll = Some(poll);
        rt.frame(0.0);
        rt.frame(0.1);
        assert_eq!(rt.state, [0.0, 0.017, 0.05, 0.1]);
    }

    #[test]
    fn timed_wakes_from_handlers_survive_transaction_merging() {
        fn view(_: &(), _: &mut Cx<()>) -> El {
            el().key("button").size(100.0, 100.0).on_press_tx(|_: &mut (), tx| {
                tx.wake_after(std::time::Duration::from_millis(250));
            })
        }
        let mut rt = Runtime::new((), view, (100.0, 100.0), Env::new(true, 1.0));
        rt.frame(0.0);
        rt.pointer_down(10.0, 10.0);
        rt.pointer_up(10.0, 10.0);
        rt.frame(0.1);
        assert_eq!(rt.wake_deadline(), Some(0.35));
        rt.frame(0.25);
        assert_eq!(rt.wake_deadline(), None);
    }

    #[test]
    fn duplicate_sibling_keys_keep_the_first_and_lay_out_without_a_panic() {
        fn view(s: &u32, _: &mut Cx<u32>) -> El {
            col().children((0..*s).map(|i| el().key("row:R-94").size(10.0, 10.0 + i as f32)))
        }
        let mut rt = Runtime::new(2, view, (100.0, 100.0), Env::new(true, 1.0));
        rt.frame(0.0);
        let h = |rt: &Runtime<u32>, key: &str| rt.presented(key).map(|(r, _)| r[3]);
        assert_eq!(h(&rt, "row:R-94"), Some(10.0), "the first keeps the key");
        assert_eq!(h(&rt, "row:R-94#dup1"), Some(11.0), "the duplicate still draws");
        rt.update(|s, _| *s = 3);
        rt.frame(0.1);
        assert_eq!(h(&rt, "row:R-94#dup2"), Some(12.0));
    }

    #[test]
    fn duplicate_sibling_keys_tooling_reports_and_lays_out() {
        let mut rt = Runtime::new((), duplicate_view, (100.0, 100.0), Env::new(true, 1.0));
        rt.key_warnings = Some(Vec::new());
        rt.frame(0.0);
        assert_eq!(rt.key_warnings.as_ref().unwrap().len(), 1);
        assert_eq!(rt.nodes.len(), 3);
        assert_eq!(rt.nodes[0].local.w, 100.0);
    }

    #[test]
    fn a_duplicate_matched_id_matches_the_last_one_and_warns() {
        fn view(_: &(), _: &mut Cx<()>) -> El {
            col().child(el().key("a").matched("tile").size(10.0, 10.0))
                .child(el().key("b").matched("tile").size(10.0, 10.0))
        }
        let mut rt = Runtime::new((), view, (100.0, 100.0), Env::new(true, 1.0));
        rt.key_warnings = Some(Vec::new());
        rt.frame(0.0);
        let warnings = rt.key_warnings.as_ref().unwrap();
        assert!(warnings.len() == 1 && warnings[0].contains("duplicate matched id \"tile\""), "{warnings:?}");
        let b = rt.nodes.iter().find(|n| n.key.as_deref() == Some("b")).unwrap().id;
        assert_eq!(rt.matched.iter().map(|m| m.1).collect::<Vec<_>>(), [b]);
    }

    #[test]
    fn side_panel_wraps_once_and_releases_height_after_stable_width() {
        use crate::el::{row, text};
        fn view(s: &(f32, bool), cx: &mut Cx<(f32, bool)>) -> El {
            cx.reduced_motion(s.1);
            row().key("window").w_full().h_full()
                .child(col().key("viewport").grow(1.0).shrink().h_full()
                    .scroll(ScrollSpec { stick_bottom: true }).clip_soft(18.0)
                    .child(col().key("stack").w_full().growth(GrowthSpec::default())
                        .child(col().key("message").w_full().child(text("Wrap once at the landing width. ".repeat(30)).wrap().w_full()))))
                .child(el().key("side-panel").w(s.0).h_full())
        }
        fn run(fps: usize, reduced: bool) -> Vec<f32> {
            let mut rt = Runtime::new((0.0, reduced), view, (480.0, 700.0), Env::new(true, 2.0));
            rt.frame(0.0);
            let id = rt.nodes.iter().find(|n| n.key.as_deref() == Some("message")).unwrap().id;
            let original = rt.grows[&id].h;
            let mut trace = std::env::var_os("RUNE_WIDTH_TRACE").map(|dir| {
                let dir = std::path::PathBuf::from(dir).join(format!("width-{fps}-{}", if reduced { "reduced" } else { "normal" }));
                std::fs::create_dir_all(&dir).unwrap();
                crate::tooling::Trace::open(&dir.join("trace.jsonl"), (480.0, 700.0), fps as f64, 2.0)
            });
            if let Some(trace) = &mut trace { trace.frame(&mut rt, 0.0, 0.0); }
            rt.state.0 = 160.0;
            rt.invalidate();
            rt.frame(0.0);
            let commits = rt.stats.commits;
            let measured = rt.nodes.iter().find(|n| n.id == id).unwrap().local.h;
            assert!(measured > original);
            if reduced { assert_eq!(rt.grows[&id].h, measured); }
            else { assert_eq!(rt.grows[&id].target, original); }
            let mut samples = vec![];
            for frame in 1..=fps * 2 {
                rt.frame(1.0 / fps as f64);
                assert_eq!(rt.stats.commits, commits, "presentation must not remeasure the wrapping");
                assert_eq!(rt.stats.shaped, 0);
                if frame as f64 / (fps as f64) < 0.12 && !reduced {
                    assert_eq!(rt.grows[&id].h, original);
                    assert!(rt.animating, "the hold must keep the shared clock awake");
                }
                if frame % (fps / 30) == 0 { samples.push(rt.grows[&id].h); }
                if let Some(trace) = &mut trace { trace.frame(&mut rt, frame as f64 * 1000.0 / fps as f64, 0.0); }
            }
            assert!((rt.grows[&id].h - measured).abs() < 0.05);
            // A second target during a hold restarts the stable-width interval.
            rt.state.0 = 80.0;
            rt.invalidate();
            rt.frame(0.0);
            for _ in 0..fps / 10 { rt.frame(1.0 / fps as f64); }
            rt.state.0 = 120.0;
            rt.invalidate();
            rt.frame(0.0);
            for _ in 0..fps / 10 { rt.frame(1.0 / fps as f64); }
            if !reduced {
                assert_eq!(rt.grows[&id].h, measured, "a changing width must restart the hold");
                assert!(!rt.stacks.values().next().unwrap().pending_heights.is_empty());
            }
            // Settle/resume bypasses even an active hold completely.
            rt.settle_all();
            rt.frame(0.0);
            assert!(rt.stacks.values().all(|s| s.pending_heights.is_empty()));
            assert!(!rt.animating);
            samples
        }
        for reduced in [false, true] {
            let a = run(30, reduced);
            let b = run(120, reduced);
            for (a, b) in a.into_iter().zip(b) { assert!((a - b).abs() < 0.05, "frame-rate dependent hold: {a} vs {b}"); }
        }
    }

    #[test]
    fn presses_land_inside_a_scrolled_view() {
        struct State { pressed: Vec<usize> }
        fn view(_: &State, _: &mut Cx<State>) -> El {
            let mut list = col().key("list").w_full();
            for i in 0..10 {
                list = list.child(el().key(format!("b{i}")).size(200.0, 40.0).on_press(move |s: &mut State| s.pressed.push(i)));
            }
            col().size(200.0, 200.0).child(col().key("scroll").size(200.0, 200.0).scroll(ScrollSpec { stick_bottom: false }).child(list))
        }
        let mut rt = Runtime::new(State { pressed: Vec::new() }, view, (200.0, 200.0), Env { system_dark: true, scale: 1.0 });
        rt.frame(0.0);
        rt.wheel(100.0, 100.0, -120.0);
        rt.frame(1.0 / 120.0);
        let (x, y) = rt.find("b5").expect("b5 is drawn");
        assert!(y < 200.0, "b5 scrolled into view at {y}");
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(1.0 / 120.0);
        assert_eq!(rt.state.pressed, [5]);
    }

    #[test]
    fn a_titlebar_inset_change_rebuilds_the_view() {
        fn view(_: &(), cx: &mut Cx<()>) -> El {
            crate::row().size(300.0, 40.0).child(el().key("leading").w(cx.titlebar_inset())).child(el().key("title").size(50.0, 20.0))
        }
        let x = |rt: &Runtime<()>| rt.presented("title").unwrap().0[0];
        let mut rt = Runtime::new((), view, (300.0, 40.0), Env::new(true, 1.0));
        rt.frame(0.0);
        assert_eq!(x(&rt), 0.0, "no traffic lights by default");
        rt.set_titlebar_inset(crate::Chrome::DEFAULT.titlebar_inset());
        rt.frame(0.1);
        rt.frame(5.0);
        assert_eq!(x(&rt), 67.0, "AppKit's default lights end at 7 + 2 × 23 + 14");
        rt.set_titlebar_inset(0.0);
        rt.frame(5.1);
        rt.frame(10.0);
        assert_eq!(x(&rt), 0.0, "full screen hides them");
        let titled = crate::Chrome { full_size_content: false, ..crate::Chrome::DEFAULT };
        assert_eq!(titled.titlebar_inset(), 0.0);
    }

    #[test]
    fn a_new_system_accent_rebuilds_the_view_and_its_memos() {
        // Widths stand in for colours: the view draws what it read.
        fn view(_: &(), cx: &mut Cx<()>) -> El {
            let (accent, focus) = (cx.accent(), cx.focus_color());
            crate::row().size(300.0, 40.0)
                .child(el().key("accent").size(accent.g * 100.0, 10.0))
                .child(crate::memo("ring", 0, || el().key("ring").size(focus.a * 100.0, 10.0)))
        }
        let mut rt = Runtime::new((), view, (300.0, 40.0), Env::new(true, 1.0));
        rt.frame(0.0);
        let w = |rt: &Runtime<()>, key: &str| (rt.presented(key).unwrap().0[2] * 10.0).round() / 10.0;
        assert_eq!((w(&rt, "accent"), w(&rt, "ring")), (23.1, 50.0), "macOS's dark blue, rings at 50%");
        let pink = crate::Color { r: 1.0, g: 0.2, b: 0.6, a: 1.0 };
        rt.set_system_accent(crate::SystemAccent::new(pink, pink).with_focus(pink, pink.alpha(0.7)));
        rt.frame(0.1);
        rt.frame(5.0);
        assert_eq!((w(&rt, "accent"), w(&rt, "ring")), (20.0, 70.0));
    }

    #[test]
    fn page_keys_reach_an_input_and_scroll_its_scroll_view_by_most_of_a_viewport() {
        struct State { edit: crate::TextEdit, keys: Vec<crate::Key> }
        fn view(s: &State, _: &mut Cx<State>) -> El {
            let field = crate::text_input(&s.edit, crate::TextStyle::new(14.0, 400.0)).key("field").size(200.0, 40.0)
                .on_edit(|s: &mut State| &mut s.edit)
                .on_input_key(|s: &mut State, ev, _| { s.keys.push(ev.key.clone()); false });
            let list = col().key("list").w_full().child(field)
                .children((0..20).map(|i| el().key(format!("r{i}")).size(200.0, 40.0)));
            col().size(200.0, 200.0).child(col().key("scroll").size(200.0, 200.0).scroll(ScrollSpec { stick_bottom: false }).child(list))
        }
        let mut edit = crate::TextEdit::new("hello");
        edit.focused = true;
        let mut rt = Runtime::new(State { edit, keys: Vec::new() }, view, (200.0, 200.0), Env::new(true, 1.0));
        rt.frame(0.0);
        let top = |rt: &Runtime<State>| rt.presented("r0").unwrap().0[1];
        assert_eq!(top(&rt), 40.0);
        let page = |rt: &mut Runtime<State>, key: crate::Key, t: f64| {
            rt.key(KeyEvent { key, mods: Default::default() });
            rt.frame(t);
        };
        page(&mut rt, crate::Key::PageDown, 0.1);
        assert_eq!(rt.state.keys, [crate::Key::PageDown], "the input's key handler sees it");
        assert_eq!(top(&rt), 40.0 - 180.0, "a page is 90% of the viewport");
        assert_eq!(rt.state.edit.text, "hello");
        page(&mut rt, crate::Key::PageDown, 0.2);
        page(&mut rt, crate::Key::PageDown, 0.3);
        page(&mut rt, crate::Key::PageDown, 0.4);
        assert_eq!(top(&rt), 40.0 - 640.0, "clamped at the bottom");
        page(&mut rt, crate::Key::PageUp, 0.5);
        assert_eq!(top(&rt), 40.0 - 460.0);
        assert_eq!(crate::tooling::key_named("PageDown"), Some(crate::Key::PageDown));
    }

    #[test]
    fn page_keys_keep_paging_the_list_of_a_focused_field_that_scrolled_out() {
        struct State { edit: crate::TextEdit }
        fn view(s: &State, _: &mut Cx<State>) -> El {
            let field = crate::text_input(&s.edit, crate::TextStyle::new(14.0, 400.0)).key("field").size(100.0, 40.0)
                .on_edit(|s: &mut State| &mut s.edit);
            let rows = col().key("rows").w_full().growth(crate::GrowthSpec::default())
                .child(col().key("r0").child(field))
                .children((1..100).map(|i| el().key(format!("r{i}")).size(100.0, 40.0)))
                .virtualize(40.0);
            let other = col().key("other").w_full().children((0..100).map(|i| el().key(format!("o{i}")).size(100.0, 40.0)));
            crate::row().size(200.0, 200.0)
                .child(col().key("left").size(100.0, 200.0).scroll(ScrollSpec { stick_bottom: false }).child(rows))
                .child(col().key("right").size(100.0, 200.0).scroll(ScrollSpec { stick_bottom: false }).child(other))
        }
        let mut edit = crate::TextEdit::new("hello");
        edit.focused = true;
        let mut rt = Runtime::new(State { edit }, view, (200.0, 200.0), Env::new(true, 1.0));
        rt.frame(0.0);
        rt.pointer_move(150.0, 100.0);
        let y = |rt: &Runtime<State>, key: &str| rt.presented(key).map(|p| p.0[1]);
        for n in 1..=8 {
            rt.key(KeyEvent { key: crate::Key::PageDown, mods: Default::default() });
            rt.frame(n as f64 * 0.1);
        }
        assert!(y(&rt, "field").is_none(), "the field's row scrolled out of the window");
        assert_eq!(y(&rt, "r40"), Some(40.0 * 40.0 - 8.0 * 180.0), "every page moved the focused field's list");
        assert_eq!(y(&rt, "o0"), Some(0.0), "the list under the pointer stayed");
        assert!(rt.state.edit.focused);
    }

    #[test]
    fn text_inside_a_scale_draws_at_that_size() {
        #[derive(Clone, Copy, PartialEq)]
        enum How { Static, Live, Press }
        struct State { k: f32, how: How, pressed: u32 }
        fn view(s: &State, _: &mut Cx<State>) -> El {
            let label = crate::el::text("Hamburgefonstiv").key("label");
            let card = el().key("card").size(200.0, 80.0).on_press(|s: &mut State| s.pressed += 1).child(label);
            let card = match s.how {
                How::Static => card.scale(s.k),
                How::Live => card.live_scale(&crate::live::Live::new(s.k)),
                How::Press => card.press_scale(s.k),
            };
            col().size(300.0, 200.0).child(card)
        }
        // Device px: the glyphs' bounding box, and whether all of them sample their atlas 1:1.
        fn glyphs(rt: &Runtime<State>) -> ([f32; 4], bool) {
            let gs: Vec<_> = rt.instances.iter().filter(|q| q.params[1] == KIND_GLYPH).collect();
            assert!(gs.len() >= 10, "the label draws its glyphs");
            let b = gs.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, q| {
                [b[0].min(q.rect[0]), b[1].min(q.rect[1]), b[2].max(q.rect[0] + q.rect[2]), b[3].max(q.rect[1] + q.rect[3])]
            });
            (b, gs.iter().all(|q| q.uv[2] == 0.0))
        }
        let run = |k: f32, how: How| {
            let mut rt = Runtime::new(State { k, how, pressed: 0 }, view, (300.0, 200.0), Env::new(true, 2.0));
            rt.frame(0.0);
            rt.frame(1.0 / 120.0);
            rt
        };
        let (rest, _) = glyphs(&run(1.0, How::Static));
        let (w, h) = (rest[2] - rest[0], rest[3] - rest[1]);
        // The card's centre in device px, which the scale is about.
        let c = [200.0, 80.0];
        for how in [How::Static, How::Live] {
            let rt = run(0.5, how);
            let (half, crisp) = glyphs(&rt);
            let (hw, hh) = (half[2] - half[0], half[3] - half[1]);
            assert!((hw / w - 0.5).abs() < 0.03 && (hh / h - 0.5).abs() < 0.06, "glyphs draw at half size: {hw}x{hh} vs {w}x{h}");
            for axis in 0..2 {
                let want = c[axis] + (rest[axis] - c[axis]) * 0.5;
                assert!((half[axis] - want).abs() < 1.5, "about the card's centre: {} vs {want}", half[axis]);
            }
            assert!(crisp, "a still scale rasterizes at the scaled size");
        }

        // Static scale needs no layout change, and hits follow the drawn rect: 50..150 x 20..60.
        let mut rt = run(0.5, How::Static);
        let card = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("card")).unwrap();
        assert_eq!(card.target, [0.0, 0.0, 200.0, 80.0], "layout keeps the unscaled size");
        assert!((card.rect[0] - 50.0).abs() < 0.05 && (card.rect[2] - 100.0).abs() < 0.05, "drawn about its centre: {:?}", card.rect);
        for (x, hit) in [(20.0, 0), (100.0, 1)] {
            rt.pointer_move(x, 40.0);
            rt.pointer_down(x, 40.0);
            rt.pointer_up(x, 40.0);
            rt.frame(1.0 / 120.0);
            assert_eq!(rt.state.pressed, hit, "a press at x {x}");
        }

        // A press scale's text shrinks too; while it springs back, it samples rest-size glyphs.
        let mut rt = run(0.9, How::Press);
        rt.pointer_move(100.0, 40.0);
        rt.pointer_down(100.0, 40.0);
        for _ in 0..30 { rt.frame(1.0 / 120.0); }
        let (held, crisp) = glyphs(&rt);
        assert!(((held[2] - held[0]) / w - 0.9).abs() < 0.02, "held text is 90%: {held:?}");
        assert!(crisp, "the held size rasterizes crisp");
        rt.pointer_up(100.0, 40.0);
        rt.frame(1.0 / 120.0);
        let (moving, crisp) = glyphs(&rt);
        let mw = (moving[2] - moving[0]) / w;
        assert!(mw > 0.9 && mw < 1.0, "springing back: {mw}");
        assert!(!crisp, "a moving scale samples its rest-size glyphs");
        for _ in 0..120 { rt.frame(1.0 / 120.0); }
        assert_eq!(glyphs(&rt), (rest, true), "back at rest exactly as before");
    }

    #[test]
    fn nested_scales_compose_about_each_mapped_centre() {
        #[derive(Clone, Copy, PartialEq, Debug)]
        enum How { Press, Static, ProductOfOne, ThreeLevels }
        struct State { how: How, pressed: u32 }
        fn view(s: &State, _: &mut Cx<State>) -> El {
            let leaf = el().key("c").size(40.0, 40.0).on_press(|s: &mut State| s.pressed += 1);
            let inner = match s.how {
                How::Press => leaf.press_scale(0.96),
                How::Static => leaf.scale(0.8),
                How::ProductOfOne => leaf.scale(2.0),
                How::ThreeLevels => el().key("mid").size(100.0, 100.0).items_start()
                    .live_scale(&crate::live::Live::new(0.8)).child(leaf.press_scale(0.9)),
            };
            col().size(400.0, 400.0).items_start()
                .child(el().key("p").size(200.0, 200.0).items_start().scale(0.5).child(inner))
        }
        let edges = |rt: &Runtime<State>| {
            let r = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("c")).unwrap().rect;
            [r[0], r[1], r[0] + r[2], r[1] + r[3]]
        };
        let near = |a: [f32; 4], b: [f32; 4]| a.iter().zip(b).all(|(a, b)| (a - b).abs() < 0.3);
        let run = |how| {
            let mut rt = Runtime::new(State { how, pressed: 0 }, view, (400.0, 400.0), Env::new(true, 2.0));
            rt.frame(0.0);
            rt.frame(1.0 / 120.0);
            rt
        };
        let press = |rt: &mut Runtime<State>, x: f32, y: f32| {
            rt.pointer_move(x, y);
            rt.pointer_down(x, y);
            rt.pointer_up(x, y);
            rt.frame(1.0 / 120.0);
        };
        // The parent draws [50, 50, 150, 150]; the child's centre (20, 20) lands at (60, 60).
        for (how, rest, held) in [
            (How::Press, [50.0, 50.0, 70.0, 70.0], Some([50.4, 50.4, 69.6, 69.6])),
            (How::Static, [52.0, 52.0, 68.0, 68.0], None),
            // 0.5 × 2.0 is 1, but the child still moves with its parent.
            (How::ProductOfOne, [40.0, 40.0, 80.0, 80.0], None),
            // The middle level draws [50, 50, 90, 90] at 0.4, putting the child's centre at 63.
            (How::ThreeLevels, [55.0, 55.0, 71.0, 71.0], Some([55.8, 55.8, 70.2, 70.2])),
        ] {
            let mut rt = run(how);
            assert!(near(edges(&rt), rest), "{how:?} at rest: {:?} vs {rest:?}", edges(&rt));
            press(&mut rt, rest[0] - 1.0, rest[1] - 1.0);
            press(&mut rt, rest[0] + 1.0, rest[1] + 1.0);
            assert_eq!(rt.state.pressed, 1, "{how:?}: hits follow the drawn rect");
            let Some(held) = held else { continue };
            let (cx, cy) = ((rest[0] + rest[2]) * 0.5, (rest[1] + rest[3]) * 0.5);
            rt.pointer_move(cx, cy);
            rt.pointer_down(cx, cy);
            for _ in 0..30 { rt.frame(1.0 / 120.0); }
            assert!(near(edges(&rt), held), "{how:?} held: {:?} vs {held:?}", edges(&rt));
            // Still hit at its resting size: a release just inside its resting corner presses.
            rt.pointer_up(rest[2] - 0.2, rest[3] - 0.2);
            rt.frame(1.0 / 120.0);
            assert_eq!(rt.state.pressed, 2, "{how:?}: a held press hits at the resting rect");
        }
    }

    #[test]
    fn a_changing_static_scale_keeps_the_glyph_atlas_bounded() {
        struct State { k: f32 }
        fn view(s: &State, _: &mut Cx<State>) -> El {
            col().size(300.0, 200.0)
                .child(el().key("card").size(200.0, 80.0).scale(s.k).scale_motion(rune_motion::Curve::Instant)
                    .child(crate::el::text("Hamburgefonstiv")))
        }
        let crisp = |rt: &Runtime<State>| rt.instances.iter().filter(|q| q.params[1] == KIND_GLYPH).all(|q| q.uv[2] == 0.0);
        let mut rt = Runtime::new(State { k: 1.0 }, view, (300.0, 200.0), Env::new(true, 2.0));
        rt.frame(0.0);
        let rest = rt.text.atlas.glyph_entries();
        // A slider or pinch: a new scale every frame, holding still now and then.
        for i in 0..1000 {
            rt.state.k = 0.5 + i as f32 * 0.0007;
            rt.invalidate();
            rt.frame(1.0 / 120.0);
            assert!(i == 0 || !crisp(&rt), "a scale changing every frame samples rest-size glyphs");
            if i % 100 == 99 { for _ in 0..30 { rt.frame(1.0 / 120.0); } }
        }
        let grown = rt.text.atlas.glyph_entries() - rest;
        assert!(grown < 1000, "1,000 scales added {grown} glyph entries (one size each would add 15,000)");
        rt.state.k = 0.75;
        rt.invalidate();
        rt.frame(1.0 / 120.0);
        assert!(!crisp(&rt));
        for _ in 0..30 { rt.frame(1.0 / 120.0); }
        assert!(crisp(&rt), "once it holds still, text rasterizes at the scale");
        assert!(!rt.frame(1.0 / 120.0), "and nothing keeps animating");
    }

    #[test]
    fn a_held_press_scale_keeps_its_hover_and_a_scroll_cancels_it() {
        struct State { pressed: Vec<usize> }
        fn view(_: &State, _: &mut Cx<State>) -> El {
            let mut list = col().key("list").w_full();
            for i in 0..10 {
                list = list.child(el().key(format!("b{i}")).size(200.0, 40.0).hover(crate::color::oklcha(1.0, 0.0, 0.0, 0.1))
                    .press_scale(0.96).on_press(move |s: &mut State| s.pressed.push(i)));
            }
            col().size(200.0, 200.0).child(col().key("scroll").size(200.0, 200.0).scroll(ScrollSpec { stick_bottom: false }).child(list))
        }
        let mut rt = Runtime::new(State { pressed: Vec::new() }, view, (200.0, 200.0), Env::new(true, 2.0));
        rt.frame(0.0);
        let id = rt.nodes.iter().find(|n| n.key.as_deref() == Some("b0")).unwrap().id;
        // Held at its right edge, which the shrunk row no longer covers: still hovered.
        rt.pointer_move(199.0, 20.0);
        rt.pointer_down(199.0, 20.0);
        for _ in 0..30 { rt.frame(1.0 / 120.0); }
        assert_eq!(rt.scene.get(id, P_PRESS_SCALE), Some(0.96));
        assert_eq!(rt.scene.get(id, P_HOVER), Some(1.0), "the held row keeps its highlight");
        rt.pointer_up(199.0, 20.0);
        rt.frame(1.0 / 120.0);
        assert_eq!(rt.state.pressed, [0]);

        // Scrolling under a held press cancels it: the row lets go, and the release presses nothing.
        for _ in 0..60 { rt.frame(1.0 / 120.0); }
        rt.pointer_move(100.0, 20.0);
        rt.pointer_down(100.0, 20.0);
        for _ in 0..10 { rt.frame(1.0 / 120.0); }
        assert!(rt.scene.get(id, P_PRESS_SCALE).unwrap() < 0.97, "held");
        rt.wheel(100.0, 100.0, -120.0);
        for _ in 0..60 { rt.frame(1.0 / 120.0); }
        assert!((rt.scene.get(id, P_PRESS_SCALE).unwrap() - 1.0).abs() < 1e-3, "the scrolled-away row springs back");
        rt.pointer_up(100.0, 20.0);
        rt.frame(1.0 / 120.0);
        assert_eq!(rt.state.pressed, [0], "a scroll cancels the press");
        // A fling does too.
        rt.pointer_move(100.0, 20.0);
        rt.pointer_down(100.0, 20.0);
        rt.fling(100.0, 100.0, 600.0);
        rt.pointer_up(100.0, 20.0);
        rt.frame(1.0 / 120.0);
        assert_eq!(rt.state.pressed, [0]);
    }

    #[test]
    fn press_scale_shrinks_while_held_and_springs_back() {
        struct State { pressed: u32, reduced: bool }
        fn view(s: &State, cx: &mut Cx<State>) -> El {
            cx.reduced_motion(s.reduced);
            col().size(200.0, 200.0).child(el().key("tile").size(100.0, 100.0).press_scale(0.96)
                .on_press(|s: &mut State| s.pressed += 1)
                .child(el().key("dot").size(10.0, 10.0)))
        }
        fn info(rt: &Runtime<State>, key: &str) -> ElementInfo {
            rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap()
        }
        let settle = |rt: &mut Runtime<State>| for _ in 0..90 { rt.frame(1.0 / 120.0); };
        let mut rt = Runtime::new(State { pressed: 0, reduced: false }, view, (200.0, 200.0), Env::new(true, 1.0));
        rt.frame(0.0);
        let rest = info(&rt, "tile");
        assert_eq!(rest.scale, 1.0);
        // Near the right edge, which the shrunken tile no longer covers.
        rt.pointer_move(99.0, 50.0);
        rt.pointer_down(99.0, 50.0);
        settle(&mut rt);
        let held = info(&rt, "tile");
        assert!((held.scale - 0.96).abs() < 1e-4, "held scale {}", held.scale);
        assert_eq!(held.target, rest.target, "layout keeps the resting size");
        assert!((held.rect[0] - 2.0).abs() < 0.05 && (held.rect[2] - 96.0).abs() < 0.05, "shrinks about its centre: {:?}", held.rect);
        assert!((info(&rt, "dot").scale - 0.96).abs() < 1e-4, "children shrink with it");
        assert!(rt.capture_inside, "a press near the edge stays inside the resting rect");
        rt.pointer_move(150.0, 50.0);
        settle(&mut rt);
        assert!((info(&rt, "tile").scale - 1.0).abs() < 1e-4, "leaving springs back");
        rt.pointer_move(99.0, 50.0);
        rt.frame(1.0 / 120.0);
        let returning = info(&rt, "tile").scale;
        assert!(returning < 1.0 && returning > 0.96, "re-entering shrinks again, from where it was: {returning}");
        settle(&mut rt);
        rt.pointer_up(99.0, 50.0);
        rt.frame(1.0 / 120.0);
        assert_eq!(rt.state.pressed, 1);
        let mut last = info(&rt, "tile").scale;
        assert!(last > 0.96 && last < 1.0, "the release springs, not jumps: {last}");
        let mut peak: f32 = last;
        for _ in 0..90 {
            rt.frame(1.0 / 120.0);
            let s = info(&rt, "tile").scale;
            assert!((s - last).abs() < 0.01, "no frame jumps more than 1%: {last} -> {s}");
            peak = peak.max(s);
            last = s;
        }
        assert!((last - 1.0).abs() < 1e-4, "settles at rest: {last}");
        assert!(peak > 1.0, "the release has a little bounce: {peak}");

        let mut reduced = Runtime::new(State { pressed: 0, reduced: true }, view, (200.0, 200.0), Env::new(true, 1.0));
        reduced.frame(0.0);
        reduced.pointer_move(50.0, 50.0);
        reduced.pointer_down(50.0, 50.0);
        settle(&mut reduced);
        assert_eq!(info(&reduced, "tile").scale, 1.0, "Reduce Motion turns press scale off");
    }

    #[test]
    fn a_double_press_follows_the_first_press_within_the_interval_and_tolerance() {
        #[derive(Default)]
        struct State { presses: u32, doubles: u32, other: u32 }
        fn view(_: &State, _: &mut Cx<State>) -> El {
            crate::row().size(300.0, 100.0)
                .child(el().key("row").size(100.0, 100.0)
                    .on_press(|s: &mut State| s.presses += 1)
                    .on_double_press(|s: &mut State| s.doubles += 1))
                .child(el().key("other").size(100.0, 100.0).on_press(|s: &mut State| s.other += 1))
        }
        let tick = |rt: &mut Runtime<State>, seconds: f64| for _ in 0..(seconds * 120.0).round() as usize { rt.frame(1.0 / 120.0); };
        let click = |rt: &mut Runtime<State>, (x, y): (f32, f32)| {
            rt.pointer_move(x, y);
            rt.pointer_down(x, y);
            rt.frame(1.0 / 120.0);
            rt.pointer_up(x, y);
            rt.frame(1.0 / 120.0);
        };
        let counts = |rt: &Runtime<State>| (rt.state.presses, rt.state.doubles);
        let mut rt = Runtime::new(State::default(), view, (300.0, 100.0), Env::new(true, 1.0));
        rt.frame(0.0);

        click(&mut rt, (50.0, 50.0));
        assert_eq!(counts(&rt), (1, 0), "the first press runs on_press right away");
        tick(&mut rt, 0.1);
        click(&mut rt, (53.0, 47.0));
        assert_eq!(counts(&rt), (2, 1), "the second press inside the interval and 4 pt is a double press");
        assert!(rt.events.iter().any(|e| e.starts_with("double press")));
        tick(&mut rt, 0.1);
        click(&mut rt, (50.0, 50.0));
        assert_eq!(counts(&rt), (3, 1), "a third press starts a new pair");
        tick(&mut rt, 0.1);
        click(&mut rt, (50.0, 50.0));
        assert_eq!(counts(&rt), (4, 2));

        tick(&mut rt, 0.1);
        click(&mut rt, (50.0, 50.0));
        let slow = rt.double_click_interval + 0.05;
        tick(&mut rt, slow);
        click(&mut rt, (50.0, 50.0));
        assert_eq!(counts(&rt), (6, 2), "too slow");

        tick(&mut rt, 0.6);
        click(&mut rt, (50.0, 50.0));
        click(&mut rt, (55.0, 50.0));
        assert_eq!(counts(&rt), (8, 2), "moved more than 4 pt");

        tick(&mut rt, 0.6);
        click(&mut rt, (50.0, 50.0));
        click(&mut rt, (150.0, 50.0));
        click(&mut rt, (50.0, 50.0));
        assert_eq!((counts(&rt), rt.state.other), ((10, 2), 1), "a press elsewhere breaks the pair");

        tick(&mut rt, 0.6);
        click(&mut rt, (50.0, 50.0));
        rt.pointer_down(50.0, 50.0);
        rt.frame(1.0 / 120.0);
        rt.pointer_move(250.0, 50.0);
        rt.pointer_up(250.0, 50.0);
        rt.frame(1.0 / 120.0);
        assert_eq!(counts(&rt), (11, 2), "a second press released outside is cancelled");

        // A second press that drags 40 pt (still inside) and lets go 5 s later is no click.
        tick(&mut rt, 0.6);
        click(&mut rt, (50.0, 50.0));
        rt.pointer_down(50.0, 50.0);
        rt.frame(1.0 / 120.0);
        rt.pointer_move(90.0, 50.0);
        rt.pointer_move(60.0, 50.0);
        tick(&mut rt, 5.0);
        rt.pointer_up(60.0, 50.0);
        rt.frame(1.0 / 120.0);
        assert_eq!(counts(&rt), (13, 2), "a drag or a long press doesn't finish a double press");
        // Held without moving past the interval, it's no click either.
        tick(&mut rt, 0.6);
        click(&mut rt, (50.0, 50.0));
        rt.pointer_down(50.0, 50.0);
        let long = rt.double_click_interval + 0.05;
        tick(&mut rt, long);
        rt.pointer_up(50.0, 50.0);
        rt.frame(1.0 / 120.0);
        assert_eq!(counts(&rt), (15, 2), "a long second press");
        // A first press that drags doesn't arm one.
        tick(&mut rt, 0.6);
        rt.pointer_down(50.0, 50.0);
        rt.pointer_move(50.0, 90.0);
        rt.pointer_move(50.0, 50.0);
        rt.pointer_up(50.0, 50.0);
        rt.frame(1.0 / 120.0);
        click(&mut rt, (50.0, 50.0));
        assert_eq!(counts(&rt), (17, 2), "a dragged first press");

        // Headless, a slower system setting is honoured on the same virtual clock.
        rt.double_click_interval = 1.0;
        tick(&mut rt, 1.1);
        click(&mut rt, (50.0, 50.0));
        tick(&mut rt, 0.8);
        click(&mut rt, (50.0, 50.0));
        assert_eq!(counts(&rt), (19, 3));
    }

    #[test]
    fn an_element_with_only_a_double_press_is_a_press_target() {
        struct State { doubles: u32, behind: u32 }
        fn view(_: &State, _: &mut Cx<State>) -> El {
            el().size(100.0, 100.0).on_press(|s: &mut State| s.behind += 1)
                .child(el().size(100.0, 100.0).on_double_press(|s: &mut State| s.doubles += 1))
        }
        let mut rt = Runtime::new(State { doubles: 0, behind: 0 }, view, (100.0, 100.0), Env::new(true, 1.0));
        rt.frame(0.0);
        for _ in 0..2 {
            rt.pointer_move(50.0, 50.0);
            rt.pointer_down(50.0, 50.0);
            rt.frame(1.0 / 120.0);
            rt.pointer_up(50.0, 50.0);
            rt.frame(1.0 / 120.0);
        }
        assert_eq!((rt.state.doubles, rt.state.behind), (1, 0));
    }

    /// A picture with one side set takes the other from its image, as SwiftUI's
    /// `.resizable().aspectRatio(contentMode:)` does; both sides set keep the box.
    #[test]
    fn a_picture_with_one_side_sizes_the_other_from_its_image() {
        use crate::Pixels;
        use crate::SurfaceFit::{Fill, Fit};
        fn view(p: &Pixels, _: &mut Cx<Pixels>) -> El {
            crate::col().w(240.0).items_start()
                .child(el().key("w").w(120.0).pixels(p))
                .child(el().key("h").h(30.0).pixels_fit(p, Fit))
                .child(el().key("full").w_full().pixels_fit(p, Fill))
                .child(el().key("box").size(100.0, 100.0).pixels_fit(p, Fit))
                .child(el().key("neither").pixels(p))
                .child(el().key("explicit").w(120.0).aspect_ratio(4.0).pixels(p))
                .child(el().key("plain").h(25.0).aspect_ratio(2.0))
        }
        let size = |rt: &Runtime<Pixels>, key: &str| {
            let e = rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap();
            [e.target[2], e.target[3]]
        };
        let px = Pixels::new();
        let mut rt = Runtime::new(px.clone(), view, (240.0, 600.0), Env::new(true, 1.0));
        rt.frame(0.0);
        assert_eq!(size(&rt, "w"), [120.0, 0.0], "an empty (still decoding) image has no ratio yet");
        assert_eq!(size(&rt, "explicit"), [120.0, 30.0]);
        assert_eq!(size(&rt, "plain"), [50.0, 25.0], "any box can set a ratio");

        // The image arrives without the view re-running, as a decode completing does.
        px.set(4, 2, vec![255; 4 * 4 * 2]);
        rt.frame(1.0 / 120.0);
        assert_eq!(size(&rt, "w"), [120.0, 60.0]);
        assert_eq!(size(&rt, "h"), [60.0, 30.0]);
        assert_eq!(size(&rt, "full"), [240.0, 120.0]);
        assert_eq!(size(&rt, "box"), [100.0, 100.0], "a fixed box keeps its size and letterboxes");
        assert_eq!(size(&rt, "neither"), [0.0, 0.0], "a picture with no side set is unchanged");
        assert_eq!(size(&rt, "explicit"), [120.0, 30.0], "an explicit ratio wins");
        assert!(!rt.elements().iter().any(|e| e.moving), "the new shape lands without animating");

        px.set(2, 4, vec![255; 4 * 2 * 4]);
        rt.frame(1.0 / 120.0);
        assert_eq!(size(&rt, "w"), [120.0, 240.0], "a stream that changes shape relays out");
        let commits = rt.commits;
        for _ in 0..4 { rt.frame(1.0 / 120.0); }
        assert_eq!(rt.commits, commits, "an unchanged image costs no commits");

        // The image lands on a frame that also re-runs the view (`Pixels::set`, then the
        // handle's `update`, or a hover that frame): it still appears at its size.
        let px = Pixels::new();
        let mut rt = Runtime::new(px.clone(), view, (240.0, 600.0), Env::new(true, 1.0));
        rt.frame(0.0);
        px.set(4, 2, vec![255; 4 * 4 * 2]);
        rt.invalidate();
        rt.frame(1.0 / 120.0);
        let w = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("w")).unwrap();
        assert_eq!((w.rect[3], w.target[3]), (60.0, 60.0), "drawn at its size, not springing open");
        assert!(!w.moving);
    }

    /// A decode or stream landing after this frame read the picture's size still gets a frame
    /// that sizes it, though the frame latched live values after the set.
    #[test]
    fn a_picture_set_mid_frame_wakes_another_frame_to_size_it() {
        use crate::Pixels;
        fn view(p: &Pixels, _: &mut Cx<Pixels>) -> El {
            col().w(240.0).items_start().child(el().key("w").w(120.0).pixels(p))
        }
        let height = |rt: &Runtime<Pixels>| rt.elements().into_iter().find(|e| e.key.as_deref() == Some("w")).unwrap().target[3];
        let px = Pixels::new();
        let mut rt = Runtime::new(px.clone(), view, (240.0, 600.0), Env::new(true, 1.0));
        let wakes = counting_waker(&mut rt);
        rt.frame(0.0);
        assert_eq!(height(&rt), 0.0);

        let set = px.clone();
        MID_FRAME.with(|hook| *hook.borrow_mut() = Some(Box::new(move || set.set(4, 2, vec![255; 4 * 4 * 2]))));
        assert!(!rt.frame(1.0 / 120.0));
        assert_eq!(wakes.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(rt.live_pending(), "the set's wake outlives the frame it landed in");
        rt.frame(1.0 / 120.0);
        assert_eq!(height(&rt), 60.0);
        assert!(!rt.live_pending());
    }

    fn counting_waker<S: 'static>(rt: &mut Runtime<S>) -> Arc<std::sync::atomic::AtomicUsize> {
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = wakes.clone();
        rt.set_live_waker(Arc::new(move || { count.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }));
        wakes
    }

    #[test]
    fn a_picture_removed_outright_stops_waking_in_the_frame_that_removes_it() {
        use crate::Pixels;
        fn view(s: &(Pixels, bool), _: &mut Cx<(Pixels, bool)>) -> El {
            let root = el().size(8.0, 8.0);
            if s.1 { root.child(el().key("pic").size(4.0, 4.0).pixels(&s.0)) } else { root }
        }
        let px = Pixels::new();
        let mut rt = Runtime::new((px.clone(), true), view, (8.0, 8.0), Env::new(true, 1.0));
        let wakes = counting_waker(&mut rt);
        rt.frame(0.0);
        rt.update(|s, tx| { s.1 = false; tx.settle(); });
        let set = px.clone();
        MID_FRAME.with(|hook| *hook.borrow_mut() = Some(Box::new(move || set.set(1, 1, vec![255; 4]))));
        rt.frame(0.0);
        assert_eq!(wakes.load(std::sync::atomic::Ordering::Relaxed), 0, "unmounted before the set");
        assert!(!rt.live_pending());
    }

    /// Everything an exit still draws keeps updating: an exiting picture and a picture inside
    /// an exiting element both wake until the exit ends.
    #[test]
    fn pictures_in_an_exit_wake_until_it_ends() {
        use crate::Pixels;
        use std::sync::atomic::Ordering::Relaxed;
        type State = (Pixels, Pixels, bool);
        fn view(s: &State, _: &mut Cx<State>) -> El {
            let root = el().size(20.0, 20.0);
            if !s.2 { return root; }
            root.child(el().key("direct").size(4.0, 4.0).pixels(&s.0).transition(Transition::opacity()))
                .child(el().key("card").size(8.0, 8.0).transition(Transition::opacity())
                    .child(el().key("child").size(4.0, 4.0).pixels(&s.1)))
        }
        let (direct, child) = (Pixels::new(), Pixels::new());
        let mut rt = Runtime::new((direct.clone(), child.clone(), true), view, (20.0, 20.0), Env::new(true, 1.0));
        let wakes = counting_waker(&mut rt);
        rt.frame(0.0);
        rt.update(|s, _| s.2 = false);
        assert!(rt.frame(0.0), "the exit runs");
        direct.set(1, 1, vec![1; 4]);
        assert_eq!(wakes.load(Relaxed), 1, "an exiting picture wakes");
        rt.frame(1.0 / 120.0);
        child.set(1, 1, vec![1; 4]);
        assert_eq!(wakes.load(Relaxed), 2, "a picture inside an exiting element wakes");
        for _ in 0..240 { if !rt.frame(1.0 / 120.0) { break; } }
        assert!(!rt.elements().iter().any(|e| e.key.as_deref() == Some("child")), "the exit ended");
        direct.set(1, 1, vec![2; 4]);
        child.set(1, 1, vec![2; 4]);
        assert_eq!(wakes.load(Relaxed), 2, "gone pictures stay asleep");
        assert!(!rt.live_pending());
    }

    #[test]
    fn presses_on_a_window_drag_region_move_the_window_but_not_its_buttons() {
        struct State { pressed: u32 }
        fn view(_: &State, _: &mut Cx<State>) -> El {
            col().size(200.0, 200.0)
                .child(crate::el::row().key("titlebar").size(200.0, 40.0).window_drag()
                    .child(el().key("button").size(40.0, 40.0).on_press(|s: &mut State| s.pressed += 1)))
                .child(el().key("body").size(200.0, 40.0))
        }
        let mut rt = Runtime::new(State { pressed: 0 }, view, (200.0, 200.0), Env::new(true, 1.0));
        rt.frame(0.0);
        for (x, y, drags) in [(100.0, 20.0, true), (20.0, 20.0, false), (100.0, 60.0, false)] {
            rt.pointer_move(x, y);
            rt.pointer_down(x, y);
            assert_eq!(rt.take_titlebar_action() == Some(TitlebarAction::Drag), drags, "at {x}, {y}");
            assert!(rt.take_titlebar_action().is_none(), "a drag request is taken once");
            rt.pointer_up(x, y);
        }
        rt.frame(1.0 / 120.0);
        assert_eq!(rt.state.pressed, 1, "the button inside the title bar still presses");
    }

    #[test]
    fn hover_change_spans_descendants_and_holds_through_a_press() {
        type State = (Vec<bool>, u32, u32);
        fn view(_: &State, _: &mut Cx<State>) -> El {
            col().size(200.0, 200.0)
                .child(el().key("pill").size(100.0, 40.0).on_hover_change(|s: &mut State, inside, _| s.0.push(inside))
                    .child(el().key("button").size(40.0, 40.0)
                        .on_press(|s: &mut State| s.1 += 1).on_hover(|s: &mut State| s.2 += 1)))
        }
        let mut rt = Runtime::new((Vec::new(), 0, 0), view, (200.0, 200.0), Env::new(true, 1.0));
        rt.frame(0.0);
        rt.pointer_move(80.0, 20.0);
        rt.frame(0.0);
        assert_eq!(rt.state.0, [true]);
        rt.pointer_move(20.0, 20.0);
        rt.pointer_down(20.0, 20.0);
        rt.pointer_up(20.0, 20.0);
        rt.frame(0.0);
        assert_eq!(rt.state, (vec![true], 1, 1), "the child keeps its hover and press; entering it is not a leave");
        rt.pointer_down(20.0, 20.0);
        rt.pointer_move(20.0, 120.0);
        rt.pointer_leave();
        rt.frame(0.0);
        assert_eq!(rt.state.0, [true], "a held press stays inside");
        rt.pointer_up(20.0, 120.0);
        rt.frame(0.0);
        assert_eq!(rt.state.0, [true, false]);
        rt.pointer_move(50.0, 20.0);
        rt.pointer_leave();
        rt.frame(0.0);
        assert_eq!(rt.state.0, [true, false, true, false]);
    }

    #[test]
    fn a_drag_reports_pointer_travel_when_its_window_moves() {
        type State = Vec<(f32, f32)>;
        fn view(_: &State, _: &mut Cx<State>) -> El {
            el().key("pill").size(100.0, 40.0).on_drag(|s: &mut State, d, _| s.push((d.dx, d.dy)))
        }
        let mut rt = Runtime::new(Vec::new(), view, (200.0, 200.0), Env::new(true, 1.0));
        rt.frame(0.0);
        rt.pointer_move(10.0, 10.0);
        rt.pointer_down(10.0, 10.0);
        rt.pointer_move(40.0, 14.0);
        rt.window_moved(30.0, 4.0);
        rt.pointer_move(15.0, 10.0);
        rt.pointer_up(15.0, 10.0);
        rt.frame(0.0);
        assert_eq!(rt.state.last(), Some(&(35.0, 4.0)), "travel is measured on screen, not in the moving window");
        assert!(rt.state.contains(&(30.0, 4.0)));
    }

    #[test]
    fn taking_the_pointer_away_ends_a_held_drag_cancelled() {
        type State = Vec<(Phase, bool)>;
        fn view(_: &State, _: &mut Cx<State>) -> El {
            el().key("pill").size(100.0, 40.0).on_drag(|s: &mut State, d, _| s.push((d.phase, d.cancelled())))
        }
        let mut rt = Runtime::new(Vec::new(), view, (200.0, 200.0), Env::new(true, 1.0));
        rt.frame(0.0);
        rt.pointer_down(10.0, 10.0);
        rt.pointer_move(40.0, 10.0);
        rt.cancel_pointer();
        rt.frame(0.0);
        assert_eq!(rt.state.last(), Some(&(Phase::Up, true)));
        rt.pointer_up(40.0, 10.0);
        rt.frame(0.0);
        assert_eq!(rt.state.iter().filter(|d| d.0 == Phase::Up).count(), 1, "the release after it does nothing");
    }

    #[test]
    fn native_escape_with_a_logical_fallback_cancels_the_drag() {
        fn view(_: &Vec<bool>, _: &mut Cx<Vec<bool>>) -> El {
            el().size(100.0, 100.0).on_drag(|s: &mut Vec<bool>, d, _| {
                if d.phase == Phase::Up { s.push(d.cancelled()); }
            })
        }
        let mut rt = Runtime::new(Vec::new(), view, (200.0, 200.0), Env::new(true, 1.0));
        rt.frame(0.0);
        rt.pointer_down(10.0, 10.0);
        rt.frame(0.0);
        rt.physical_key_input(crate::PhysicalInput::Key { key: crate::PhysicalKeyCode::Escape,
            mods: crate::Mods::default(), pressed: true, repeat: false }, KeyEvent { key: crate::Key::Escape, mods: crate::Mods::default() });
        rt.frame(0.0);
        assert_eq!(rt.state, [true]);
        rt.pointer_up(10.0, 10.0);
        rt.frame(0.0);
        assert_eq!(rt.state, [true]);
    }

    #[derive(Default)]
    struct DragOutState { presses: u32, drags: u32, offer: bool, ended: Vec<crate::DragOutcome>, dragging: bool, starts: u32, log: Vec<String> }

    fn drag_out_view(_: &DragOutState, _: &mut Cx<DragOutState>) -> El {
        col().size(200.0, 200.0).child(el().key("file").size(100.0, 40.0)
            .on_press(|s: &mut DragOutState| s.presses += 1)
            .on_drag(|s: &mut DragOutState, _, _| s.drags += 1)
            .drag_out(|s: &DragOutState| s.offer.then(|| crate::DragItem::new().file("/Applications/Example.app")))
            .on_drag_start(|s: &mut DragOutState, _| { s.dragging = true; s.starts += 1; s.log.push("start".into()); })
            .on_drag_end(|s: &mut DragOutState, outcome, _| { s.dragging = false; s.ended.push(outcome); s.log.push(format!("end {outcome:?}")); }))
    }

    fn drag_out_rt(offer: bool) -> Runtime<DragOutState> {
        let mut rt = Runtime::new(DragOutState { offer, ..Default::default() }, drag_out_view, (200.0, 200.0), Env::new(true, 1.0));
        rt.frame(0.0);
        rt.pointer_move(10.0, 10.0);
        rt.pointer_down(10.0, 10.0);
        rt
    }

    #[test]
    fn crossing_the_threshold_hands_a_drag_out_to_the_host_and_releases_the_pointer() {
        let mut rt = drag_out_rt(true);
        let t = crate::drag_out::DRAG_THRESHOLD;
        rt.pointer_move(10.0 + t, 10.0);
        assert!(rt.take_drag_out().is_none(), "at the threshold it's still a press");
        assert!(rt.pointer_captured());
        rt.pointer_move(12.0 + t, 10.0);
        let start = rt.take_drag_out().expect("drag-out requested");
        assert_eq!(start.item.paths(), [std::path::PathBuf::from("/Applications/Example.app")]);
        assert_eq!(start.press, (10.0, 10.0));
        assert_eq!(start.rect, [0.0, 0.0, 100.0, 40.0]);
        assert!(rt.events.iter().any(|e| e.starts_with("drag-out ") && e.ends_with("file")), "{:?}", rt.events);
        assert!(!rt.pointer_captured() && rt.pointer().is_none(), "nothing stays pressed or hovered");
        rt.pointer_move(40.0, 30.0);
        assert!(rt.take_drag_out().is_none(), "one request per press");
        rt.pointer_up(40.0, 30.0);
        rt.frame(0.0);
        assert_eq!((rt.state.presses, rt.state.drags), (0, 0), "neither on_press nor on_drag runs");
    }

    #[test]
    fn an_element_sets_its_own_drag_threshold() {
        fn view(_: &DragOutState, _: &mut Cx<DragOutState>) -> El {
            col().size(200.0, 200.0)
                .child(el().key("near").size(100.0, 40.0).drag_threshold(3.0)
                    .drag_out(|_: &DragOutState| Some(crate::DragItem::new().text("near"))))
                .child(el().key("far").size(100.0, 40.0).drag_threshold(12.0)
                    .drag_out(|_: &DragOutState| Some(crate::DragItem::new().text("far"))))
        }
        let mut rt = Runtime::new(DragOutState::default(), view, (200.0, 200.0), Env::new(true, 1.0));
        rt.frame(0.0);
        rt.pointer_move(10.0, 10.0);
        rt.pointer_down(10.0, 10.0);
        rt.pointer_move(13.0, 10.0);
        assert!(rt.take_drag_out().is_none(), "exactly 3 pt is still a press");
        rt.pointer_move(13.5, 10.0);
        assert!(rt.take_drag_out().is_some_and(|s| s.path.ends_with("near")), "3.5 pt passes a 3 pt threshold, under the default");
        rt.pointer_up(13.5, 10.0);
        rt.frame(0.0);
        rt.pointer_move(10.0, 50.0);
        rt.pointer_down(10.0, 50.0);
        rt.pointer_move(18.0, 50.0);
        assert!(rt.take_drag_out().is_none(), "8 pt is past the default but under this element's 12");
        rt.pointer_move(23.0, 50.0);
        assert!(rt.take_drag_out().is_some_and(|s| s.path.ends_with("far")));
    }

    #[test]
    fn a_drag_out_released_before_the_threshold_is_a_press() {
        let mut rt = drag_out_rt(true);
        rt.pointer_move(11.0, 11.0);
        rt.pointer_up(11.0, 11.0);
        rt.frame(0.0);
        assert!(rt.take_drag_out().is_none());
        assert_eq!((rt.state.presses, rt.state.drags), (1, 0), "on_drag never runs on a drag-out element");
        assert!(rt.events.iter().all(|e| !e.starts_with("drag-out")));
    }

    #[test]
    fn a_drag_out_that_offers_nothing_stays_a_press() {
        let mut rt = drag_out_rt(false);
        rt.pointer_move(40.0, 10.0);
        assert!(rt.take_drag_out().is_none());
        assert!(rt.pointer_captured(), "the press holds on");
        rt.pointer_move(20.0, 10.0);
        rt.pointer_up(20.0, 10.0);
        rt.frame(0.0);
        assert_eq!(rt.state.presses, 1);
    }

    #[test]
    fn a_drag_out_that_offers_nothing_tracks_leaving_the_element() {
        let mut rt = drag_out_rt(false);
        assert!(rt.capture_inside);
        rt.pointer_move(150.0, 10.0);
        assert!(rt.pointer_captured(), "the press holds on");
        assert!(!rt.capture_inside, "the move that declined the drag left the element, so it no longer shows pressed");
    }

    #[test]
    fn hosts_without_os_drags_keep_on_drag() {
        let mut rt = Runtime::new(DragOutState { offer: true, ..Default::default() }, drag_out_view, (200.0, 200.0), Env::new(true, 1.0));
        rt.set_os_drag(false);
        rt.frame(0.0);
        rt.pointer_move(10.0, 10.0);
        rt.pointer_down(10.0, 10.0);
        rt.pointer_move(40.0, 10.0);
        rt.pointer_up(40.0, 10.0);
        rt.frame(0.0);
        assert!(rt.take_drag_out().is_none());
        assert!(rt.state.drags > 0, "on_drag runs as before");
    }

    #[test]
    fn a_reported_drag_end_reaches_on_drag_end_next_frame() {
        let mut rt = drag_out_rt(true);
        rt.pointer_move(40.0, 10.0);
        let start = rt.take_drag_out().unwrap();
        let sink = rt.drag_end_sink();
        sink.borrow_mut().push((start.path, crate::DragOutcome::Copied));
        assert!(rt.drag_ends_pending());
        rt.frame(0.0);
        assert_eq!(rt.state.ended, [crate::DragOutcome::Copied]);
        assert!(!rt.drag_ends_pending());
        assert!(rt.events.iter().any(|e| e.starts_with("drag-end Copied ")));
        assert!(rt.elements().iter().any(|e| e.key.as_deref() == Some("file") && e.drag_out));
    }

    #[test]
    fn on_drag_start_runs_once_the_drag_is_handed_over_and_before_its_end() {
        let mut rt = drag_out_rt(true);
        rt.pointer_move(12.0, 10.0);
        rt.frame(0.0);
        assert_eq!(rt.state.starts, 0, "not before the threshold");
        rt.pointer_move(40.0, 10.0);
        let start = rt.take_drag_out().unwrap();
        assert_eq!(rt.state.starts, 0, "handlers run between frames, after the host took the drag image");
        rt.frame(0.0);
        assert!(rt.state.dragging && rt.state.starts == 1);
        rt.drag_end_sink().borrow_mut().push((start.path, crate::DragOutcome::Cancelled));
        rt.frame(0.0);
        assert!(!rt.state.dragging);
        assert_eq!((rt.state.starts, rt.state.ended.as_slice()), (1, &[crate::DragOutcome::Cancelled][..]));
    }

    #[test]
    fn a_drag_start_and_end_reported_together_run_in_order() {
        let mut rt = drag_out_rt(true);
        rt.pointer_move(40.0, 10.0);
        let start = rt.take_drag_out().unwrap();
        rt.drag_end_sink().borrow_mut().push((start.path, crate::DragOutcome::Copied));
        rt.frame(0.0);
        assert!(!rt.state.dragging, "the end runs after the start");
        assert_eq!((rt.state.starts, rt.state.ended.as_slice()), (1, &[crate::DragOutcome::Copied][..]));
    }

    #[test]
    fn a_drag_out_no_host_took_ends_cancelled_on_the_next_frame() {
        let mut rt = drag_out_rt(true);
        rt.pointer_move(40.0, 10.0);
        rt.frame(0.0);
        assert!(!rt.state.dragging);
        assert_eq!(rt.state.log, ["start", "end Cancelled"]);
        assert!(rt.events.iter().any(|e| e.starts_with("drag-end Cancelled ")));
        rt.frame(0.0);
        assert_eq!(rt.state.log.len(), 2, "exactly one end");
    }

    #[test]
    fn a_cancelled_press_ends_a_drag_out_no_host_took() {
        let mut rt = drag_out_rt(true);
        rt.pointer_move(40.0, 10.0);
        rt.cancel_pointer();
        assert!(rt.take_drag_out().is_none(), "the cancel ended the request");
        rt.frame(0.0);
        assert_eq!(rt.state.log, ["start", "end Cancelled"]);
    }

    #[test]
    fn untaken_drags_started_in_one_turn_end_in_order() {
        let mut rt = drag_out_rt(true);
        rt.pointer_move(40.0, 10.0);
        rt.pointer_up(40.0, 10.0);
        rt.pointer_move(10.0, 10.0);
        rt.pointer_down(10.0, 10.0);
        rt.pointer_move(40.0, 10.0);
        rt.frame(0.0);
        assert_eq!(rt.state.log, ["start", "end Cancelled", "start", "end Cancelled"]);
    }

    #[test]
    fn a_stale_sessions_end_runs_before_the_next_drags_start() {
        let mut rt = drag_out_rt(true);
        rt.pointer_move(40.0, 10.0);
        let first = rt.take_drag_out().unwrap();
        rt.frame(0.0);
        rt.pointer_move(10.0, 10.0);
        rt.pointer_down(10.0, 10.0);
        rt.pointer_move(40.0, 10.0);
        let second = rt.take_drag_out().unwrap();
        // macOS reports a session it never ended when the next one replaces it.
        rt.drag_end_sink().borrow_mut().push((first.path, crate::DragOutcome::Cancelled));
        rt.frame(0.0);
        assert_eq!(rt.state.log, ["start", "end Cancelled", "start"]);
        assert!(rt.state.dragging, "the second drag is still running");
        rt.drag_end_sink().borrow_mut().push((second.path, crate::DragOutcome::Copied));
        rt.frame(0.0);
        assert_eq!(rt.state.log, ["start", "end Cancelled", "start", "end Copied"]);
    }

    #[test]
    fn a_drag_image_over_the_element_gets_its_unclipped_bounds() {
        fn view(_: &DragOutState, _: &mut Cx<DragOutState>) -> El {
            col().child(col().size(200.0, 100.0).clip_soft(0.0).child(el().size(200.0, 80.0)).child(el().key("row").size(200.0, 40.0)
                .drag_out(|_: &DragOutState| Some(crate::DragItem::new().text("row")))))
        }
        let mut rt = Runtime::new(DragOutState::default(), view, (200.0, 200.0), Env::new(true, 1.0));
        rt.frame(0.0);
        rt.pointer_move(10.0, 90.0);
        rt.pointer_down(10.0, 90.0);
        rt.pointer_move(40.0, 90.0);
        let start = rt.take_drag_out().unwrap();
        assert_eq!(start.rect, [0.0, 80.0, 200.0, 20.0], "the visible strip");
        assert_eq!(start.bounds, [0.0, 80.0, 200.0, 40.0], "the whole row");
    }

    #[test]
    fn cursor_follows_the_hovered_target_and_its_ancestors() {
        use crate::el::Cursor;
        struct State { field: crate::edit::TextEdit }
        fn view(s: &State, _: &mut Cx<State>) -> El {
            col().size(200.0, 200.0)
                .child(el().key("button").size(200.0, 40.0).cursor(Cursor::Pointer).on_press(|_: &mut State| {}))
                .child(el().key("handle").size(200.0, 40.0).cursor(Cursor::Grab)
                    .child(el().key("inner").size(100.0, 40.0).on_press(|_: &mut State| {})))
                .child(crate::el::text_input(&s.field, crate::tok::TextStyle::new(14.0, 400.0)).key("field").size(200.0, 40.0)
                    .on_edit(|s: &mut State| &mut s.field))
                .child(el().key("plain").size(200.0, 40.0).on_press(|_: &mut State| {}))
        }
        let mut rt = Runtime::new(State { field: crate::edit::TextEdit::new("") }, view, (200.0, 200.0), Env::new(true, 1.0));
        rt.frame(0.0);
        assert_eq!(rt.cursor(), Cursor::Default);
        for (y, cursor) in [(20.0, Cursor::Pointer), (60.0, Cursor::Grab), (100.0, Cursor::Text), (140.0, Cursor::Default)] {
            rt.pointer_move(50.0, y);
            assert_eq!(rt.cursor(), cursor, "at y {y}");
        }
        rt.pointer_move(50.0, 20.0);
        rt.pointer_down(50.0, 20.0);
        rt.pointer_move(50.0, 140.0);
        assert_eq!(rt.cursor(), Cursor::Pointer, "a held press keeps its cursor");
        rt.pointer_up(50.0, 140.0);
        assert_eq!(rt.cursor(), Cursor::Default);
        rt.pointer_leave();
        assert_eq!(rt.cursor(), Cursor::Default);
    }

    #[test]
    fn hidden_cursor_ends_with_its_hover_and_its_element() {
        use crate::el::Cursor;
        fn view(picking: &bool, _: &mut Cx<bool>) -> El {
            let picker = el().key("picker").size(200.0, 100.0).cursor(Cursor::Hidden).on_press(|_: &mut bool| {})
                .child(el().key("area").size(100.0, 100.0).cursor(Cursor::Crosshair).on_press(|_: &mut bool| {}));
            col().size(200.0, 200.0).child(if *picking { picker } else { el().key("idle").size(200.0, 100.0) })
        }
        let mut rt = Runtime::new(true, view, (200.0, 200.0), Env::new(true, 1.0));
        rt.frame(0.0);
        rt.pointer_move(50.0, 50.0);
        assert_eq!(rt.cursor(), Cursor::Crosshair);
        rt.pointer_move(150.0, 50.0);
        assert_eq!(rt.cursor(), Cursor::Hidden);
        rt.pointer_move(150.0, 150.0);
        assert_eq!(rt.cursor(), Cursor::Default, "hover left the element");
        rt.pointer_move(150.0, 50.0);
        rt.pointer_leave();
        assert_eq!(rt.cursor(), Cursor::Default, "pointer left the window");
        rt.pointer_move(150.0, 50.0);
        assert_eq!(rt.cursor(), Cursor::Hidden);
        rt.update(|picking: &mut bool, _| *picking = false);
        rt.frame(0.0);
        assert_eq!(rt.cursor(), Cursor::Default, "the element went away");
    }

    #[test]
    fn hidden_cursor_ends_when_a_drag_is_released_outside_the_window() {
        use crate::el::Cursor;
        fn view(_: &(), _: &mut Cx<()>) -> El {
            el().size(200.0, 200.0).child(el().key("picker").size(200.0, 100.0).cursor(Cursor::Hidden).on_press(|_: &mut ()| {}))
        }
        let mut rt = Runtime::new((), view, (200.0, 200.0), Env::new(true, 1.0));
        rt.frame(0.0);
        rt.pointer_move(50.0, 50.0);
        rt.pointer_down(50.0, 50.0);
        assert_eq!(rt.cursor(), Cursor::Hidden);
        rt.pointer_move(250.0, 50.0);
        assert_eq!(rt.cursor(), Cursor::Default, "never an element's cursor outside the window");
        rt.pointer_up(250.0, 50.0);
        assert_eq!(rt.pointer(), None, "a release outside the window is a leave");

        // The host saw the pointer leave mid-press, then releases at its last inside position.
        rt.pointer_move(50.0, 50.0);
        rt.pointer_down(50.0, 50.0);
        rt.pointer_leave();
        assert_eq!(rt.cursor(), Cursor::Hidden, "the press still holds the pointer");
        rt.pointer_up(50.0, 50.0);
        assert_eq!(rt.pointer(), None, "the deferred leave lands on release");
        assert_eq!(rt.cursor(), Cursor::Default);
    }

    #[test]
    fn cursor_follows_a_sibling_that_replaces_an_exiting_element_under_a_still_pointer() {
        use crate::el::Cursor;
        fn view(first: &bool, _: &mut Cx<bool>) -> El {
            let item = |key: &str, cursor: Cursor| el().key(key).absolute(0.0, 0.0).size(100.0, 100.0).cursor(cursor)
                .on_press(|_: &mut bool| {}).transition(Transition::opacity());
            el().size(200.0, 200.0).child(if *first { item("a", Cursor::Crosshair) } else { item("b", Cursor::Hidden) })
        }
        let mut rt = Runtime::new(true, view, (200.0, 200.0), Env::new(true, 1.0));
        rt.frame(0.0);
        rt.pointer_move(50.0, 50.0);
        assert_eq!(rt.cursor(), Cursor::Crosshair);
        rt.update(|first: &mut bool, _| *first = false);
        rt.frame(1.0 / 120.0);
        assert!(rt.nodes.iter().any(|n| n.key.as_deref() == Some("a")), "a is still exiting");
        assert_eq!(rt.cursor(), Cursor::Hidden, "the sibling under the still pointer takes over");
    }

    #[test]
    fn hover_lands_at_once_and_fades_out() {
        fn view(_: &(), _: &mut Cx<()>) -> El {
            col().size(200.0, 80.0)
                .child(el().key("row").size(200.0, 40.0).hover(crate::color::oklcha(1.0, 0.0, 0.0, 0.1)).on_press(|_: &mut ()| {}))
        }
        let mut rt = Runtime::new((), view, (200.0, 80.0), Env::new(true, 1.0));
        rt.frame(0.0);
        let id = rt.nodes.iter().find(|n| n.key.as_deref() == Some("row")).unwrap().id;
        let hover = |rt: &Runtime<()>| rt.scene.get(id, P_HOVER).unwrap_or(0.0);
        rt.pointer_move(50.0, 20.0);
        rt.frame(1.0 / 120.0);
        assert_eq!(hover(&rt), 1.0, "the first frame under the pointer is fully hovered");
        rt.pointer_move(50.0, 60.0);
        rt.frame(1.0 / 120.0);
        let leaving = hover(&rt);
        assert!(leaving > 0.0 && leaving < 1.0, "leaving fades: {leaving}");
        rt.frame(0.5);
        assert_eq!(hover(&rt), 0.0);
    }

    #[test]
    fn single_line_input_scrolls_to_the_caret_instead_of_wrapping() {
        fn view(s: &crate::TextEdit, _: &mut Cx<crate::TextEdit>) -> El {
            crate::text_input(s, crate::TextStyle::new(16.0, 400.0)).single_line()
                .key("search").size(100.0, 24.0).on_edit(|s: &mut crate::TextEdit| s)
        }
        let text = "a very long search query that keeps going past the edge";
        let mut edit = crate::TextEdit::new(text);
        edit.focused = true;
        let mut rt = Runtime::new(edit, view, (100.0, 24.0), Env::new(true, 2.0));
        rt.frame(0.0);
        let n = rt.nodes.iter().find(|n| n.key.as_deref() == Some("search")).unwrap();
        let path = n.path.clone();
        assert_eq!(n.text_layout.as_ref().unwrap().lines().count(), 1);
        let scroll = rt.input_scroll[&path];
        assert!(scroll > 100.0, "the end of the text is in view: {scroll}");
        let caret_x = rt.ime_area.unwrap()[0];
        assert!((0.0..=100.0).contains(&caret_x), "caret {caret_x} stays inside the field");

        // A click lands on the scrolled text, not on the hidden start.
        rt.queue.push(Action::Focus(path.clone(), 1.0, 12.0));
        rt.frame(0.0);
        assert!(rt.state.focus > 10, "clicked index {}", rt.state.focus);

        rt.queue.push(Action::Blur);
        rt.frame(0.0);
        assert!(!rt.state.focused);
        assert!(!rt.input_scroll.contains_key(&path), "unfocused fields rest at their start");
    }

    #[test]
    fn grapheme_arrow_caret_hit_tests_roundtrip_after_single_line_scroll() {
        fn view(s: &crate::TextEdit, _: &mut Cx<crate::TextEdit>) -> El {
            crate::text_input(s, crate::TextStyle::new(16.0, 400.0)).single_line()
                .key("search").size(100.0, 40.0).on_edit(|s: &mut crate::TextEdit| s)
        }
        let graphemes = ["W", "W", "W", "W", "W", "W", "W", "W", "W", "W", " ",
            "👨\u{200D}👩\u{200D}👧", "🇯🇵", "👋🏽", "e\u{301}", "❤\u{FE0F}", "Z"];
        let mut edit = crate::TextEdit::new(graphemes.concat());
        edit.focused = true;
        let mut rt = Runtime::new(edit, view, (100.0, 40.0), Env::new(true, 1.0));
        rt.frame(0.0);
        rt.key(KeyEvent { key: crate::Key::Home, mods: Default::default() });
        rt.frame(0.0);
        let path = rt.nodes.iter().find(|n| n.key.as_deref() == Some("search")).unwrap().path.clone();
        let mut offset = 0;
        for grapheme in graphemes {
            offset += grapheme.len();
            rt.key(KeyEvent { key: crate::Key::Right, mods: Default::default() });
            rt.frame(0.0);
            assert_eq!(rt.state.focus, offset, "arrow must cross the whole {grapheme:?}");
            let [x, y, _, height] = rt.ime_area.unwrap();
            assert!((0.0..=100.0).contains(&x), "caret must remain in the scrolled field");
            rt.queue.push(Action::Focus(path.clone(), x + 0.1, y + height * 0.5));
            rt.frame(0.0);
            assert_eq!(rt.state.focus, offset, "clicking the reported caret after {grapheme:?} must hit that boundary");
        }
        assert!(rt.input_scroll[&path] > 0.0, "the fixture must exercise scrolled caret geometry");
    }

    #[test]
    fn secure_input_masks_layout_inspector_and_unicode_pointer_indices() {
        fn view(s: &crate::TextEdit, _: &mut Cx<crate::TextEdit>) -> El {
            crate::text_input(s, crate::TextStyle::new(16.0, 400.0)).single_line().secure()
                .key("secret").size(200.0, 40.0).on_edit(|s: &mut crate::TextEdit| s)
        }
        let mut edit = crate::TextEdit::new("hé🙂");
        edit.focused = true;
        let mut rt = Runtime::new(edit, view, (200.0, 40.0), Env::new(true, 1.0));
        rt.frame(0.0);
        assert!(rt.state.is_secure() && rt.has_secure_focus());
        assert!(rt.ime_area.is_none());
        assert!(rt.elements().iter().all(|e| e.text.is_none()));
        let n = rt.nodes.iter().find(|n| n.key.as_deref() == Some("secret")).unwrap();
        assert_eq!(n.text.as_ref().unwrap().content, "•••");
        let path = n.path.clone();
        let layout = rt.focused_layout().unwrap();
        let caret = Cursor::from_byte_index(&layout, 6, Affinity::Downstream).geometry(&layout, 1.0);
        rt.queue.push(Action::Focus(path, caret.x0 as f32 + 0.1, ((caret.y0 + caret.y1) * 0.5) as f32));
        rt.frame(0.0);
        assert_eq!(rt.state.focus, 3);
        rt.paste("界"); rt.frame(0.0);
        assert_eq!(rt.state.text, "hé界🙂");
        rt.key(KeyEvent { key: crate::Key::Command('a'), mods: Default::default() }); rt.frame(0.0);
        rt.key(KeyEvent { key: crate::Key::Command('c'), mods: Default::default() }); rt.frame(0.0);
        assert!(rt.copy_request.is_none());
        rt.ime(Ime::Commit("ignored".into())); rt.frame(0.0);
        assert_eq!(rt.state.text, "hé界🙂");
        assert!(rt.events.iter().all(|e| !e.contains("hé")));
    }

    #[test]
    fn clipboard_paste_routes_to_window_or_field_and_discards_late_reads() {
        #[derive(Default)]
        struct State { field: crate::TextEdit, pasted: String }
        fn view(s: &State, _: &mut Cx<State>) -> El {
            col().size(200.0, 100.0)
                .on_paste(|s: &mut State, text, tx| { s.pasted = text.to_owned(); tx.copy_secret(text, std::time::Duration::from_secs(30)); })
                .child(crate::text_input(&s.field, crate::TextStyle::new(16.0, 400.0)).key("field").size(200.0, 40.0)
                    .on_edit(|s: &mut State| &mut s.field))
        }
        let mut rt = Runtime::new(State::default(), view, (200.0, 100.0), Env::new(true, 1.0));
        rt.frame(0.0);
        assert!(rt.accepts_paste());
        rt.paste("window fixture"); rt.frame(0.0);
        assert_eq!(rt.state.pasted, "window fixture");
        assert!(rt.events.iter().any(|e| e == "paste window"));
        assert!(!format!("{:?}", rt.copy_request).contains("window fixture"));
        rt.update(|s, _| s.field.focused = true); rt.frame(0.0);
        rt.paste_result(None, crate::ClipboardContent::new("late window reply")); rt.frame(0.0);
        assert!(rt.state.field.text.is_empty());
        assert_eq!(rt.state.pasted, "window fixture");
        let target = rt.paste_target();
        rt.paste_result(target, crate::ClipboardContent::new("field fixture")); rt.frame(0.0);
        assert_eq!(rt.state.field.text, "field fixture");
        assert!(rt.events.iter().any(|e| e == "paste field"));
    }

    #[test]
    fn rich_pastes_reach_paste_hooks_and_plain_fields_keep_pasting_text() {
        use crate::ClipboardContent;
        use crate::clipboard::Forms;
        #[derive(Default)]
        struct State { plain: crate::TextEdit, rich: crate::TextEdit, take: bool, hook: Vec<Option<String>>, changes: u32, window: Vec<ClipboardContent> }
        fn view(s: &State, _: &mut Cx<State>) -> El {
            let style = crate::TextStyle::new(16.0, 400.0);
            col().size(200.0, 100.0)
                .on_paste_content(|s: &mut State, content, _| s.window.push(content.clone()))
                .child(crate::text_input(&s.plain, style).key("plain").size(200.0, 40.0).on_edit(|s: &mut State| &mut s.plain))
                .child(crate::text_input(&s.rich, style).key("rich").size(200.0, 40.0).on_edit(|s: &mut State| &mut s.rich)
                    .on_input_change(|s: &mut State, _| s.changes += 1)
                    .on_input_paste(|s: &mut State, content, _| {
                        s.hook.push(content.html().map(Into::into));
                        if s.take { s.rich.paste("[rich]"); }
                        s.take
                    }))
        }
        let m = crate::clipboard::memory();
        let mut rt = Runtime::new(State::default(), view, (200.0, 100.0), Env::new(true, 1.0));
        rt.use_clipboard(&m);
        rt.frame(0.0);
        let paste = |rt: &mut Runtime<State>| { rt.update(|_, tx| tx.request_paste()); rt.frame(0.0); };
        m.set_content(ClipboardContent::new("Hello").with_html("<b>Hello</b><script>steal()</script>"));

        paste(&mut rt);
        let window = rt.state.window.pop().unwrap();
        assert_eq!((window.text(), window.html()), ("Hello", Some("<b>Hello</b>")), "the window hears every form, sanitised");

        rt.update(|s, _| s.plain.focused = true); rt.frame(0.0);
        assert_eq!(rt.paste_forms(rt.paste_target().as_deref()), Forms::Text);
        paste(&mut rt);
        assert_eq!(rt.state.plain.text, "Hello", "a field without a hook pastes the text, as before");

        rt.update(|s, _| { s.plain.focused = false; s.rich.focused = true; }); rt.frame(0.0);
        assert_eq!(rt.paste_forms(rt.paste_target().as_deref()), Forms::All);
        paste(&mut rt);
        assert_eq!((rt.state.rich.text.as_str(), rt.state.changes), ("Hello", 1), "a declining hook leaves the paste to the editor");
        rt.update(|s, _| { s.take = true; s.rich.text.clear(); s.rich.anchor = 0; s.rich.focus = 0; }); rt.frame(0.0);
        paste(&mut rt);
        assert_eq!((rt.state.rich.text.as_str(), rt.state.changes), ("[rich]", 2), "a hook that takes it edits the field; on_input_change runs");
        assert_eq!(rt.state.hook, vec![Some("<b>Hello</b>".to_owned()); 2]);

        // HTML with no plain text: only rich hooks hear it, never a plain field or text handler.
        m.set_content(ClipboardContent::new("").with_html("<i>only</i>"));
        rt.update(|s, _| s.take = false); rt.frame(0.0);
        paste(&mut rt);
        assert_eq!((rt.state.rich.text.as_str(), rt.state.hook.last().cloned().flatten().as_deref()), ("[rich]", Some("<i>only</i>")));
        assert!(rt.state.window.is_empty(), "a focused field's paste never falls through to the window");
        assert_eq!(rt.paste_forms(None), Forms::All);

        let mut plain_root = Runtime::new(Vec::<String>::new(), |_: &Vec<String>, _: &mut Cx<Vec<String>>| {
            col().size(200.0, 100.0).on_paste(|s: &mut Vec<String>, text, _| s.push(text.to_owned()))
        }, (200.0, 100.0), Env::new(true, 1.0));
        plain_root.use_clipboard(&m);
        plain_root.frame(0.0);
        assert_eq!(plain_root.paste_forms(None), Forms::Text, "a plain root on_paste never requests HTML or RTF");
        plain_root.update(|_, tx| tx.request_paste());
        plain_root.frame(0.0);
        assert!(plain_root.state.is_empty(), "an HTML-only clipboard never triggers a plain root on_paste");
    }

    #[test]
    fn pasted_images_reach_hooks_only() {
        use crate::{ClipboardContent, ClipboardImage};
        use crate::clipboard::Forms;
        #[derive(Default)]
        struct State { plain: crate::TextEdit, rich: crate::TextEdit, hooked: crate::TextEdit, hook: Vec<ClipboardContent>, window: Vec<ClipboardContent>, reads: Vec<ClipboardContent> }
        fn view(s: &State, _: &mut Cx<State>) -> El {
            let style = crate::TextStyle::new(16.0, 400.0);
            col().size(200.0, 150.0)
                .on_paste_content(|s: &mut State, content, _| s.window.push(content.clone()))
                .child(crate::text_input(&s.plain, style).key("plain").size(200.0, 40.0).on_edit(|s: &mut State| &mut s.plain))
                .child(crate::text_input(&s.rich, style).key("rich").size(200.0, 40.0).on_edit(|s: &mut State| &mut s.rich))
                .child(crate::text_input(&s.hooked, style).key("hooked").size(200.0, 40.0).on_edit(|s: &mut State| &mut s.hooked)
                    .on_input_paste(|s: &mut State, content, _| {
                        s.hook.push(content.clone());
                        let Some(image) = content.image() else { return false };
                        let (w, h) = image.size();
                        s.hooked.insert_pill(format!("image:{}", image.png().len()), &format!("{w}×{h}"));
                        true
                    }))
        }
        let rgba: Vec<u8> = (0..12u8).flat_map(|i| [i * 20, 0, 255 - i * 20, 255]).collect();
        let image = ClipboardImage::from_rgba(4, 3, &rgba).unwrap();
        assert_eq!(image.size(), (4, 3));
        assert!(ClipboardImage::from_png(&b"GIF89a"[..]).is_none());
        let m = crate::clipboard::memory();
        let mut rt = Runtime::new(State { rich: crate::TextEdit::new("").rich(), hooked: crate::TextEdit::new("").rich(), ..State::default() }, view, (200.0, 150.0), Env::new(true, 1.0));
        rt.use_clipboard(&m);
        rt.frame(0.0);
        let paste = |rt: &mut Runtime<State>| { rt.update(|_, tx| tx.request_paste()); rt.frame(0.0); };
        let focus = |rt: &mut Runtime<State>, which: fn(&mut State) -> &mut crate::TextEdit| {
            rt.update(move |s, _| { s.plain.focused = false; s.rich.focused = false; s.hooked.focused = false; which(s).focused = true; });
            rt.frame(0.0);
        };

        // A screenshot: an image and no text.
        m.set_content(ClipboardContent::new("").with_image(image.clone()));
        paste(&mut rt);
        assert_eq!(rt.state.window.pop().and_then(|c| c.image().cloned()).as_ref(), Some(&image), "the root's on_paste_content gets the image");
        focus(&mut rt, |s| &mut s.plain);
        paste(&mut rt);
        focus(&mut rt, |s| &mut s.rich);
        assert_eq!(rt.paste_forms(rt.paste_target().as_deref()), Forms::Rich, "a rich field with no hook never fetches the image");
        paste(&mut rt);
        assert_eq!((rt.state.plain.text.as_str(), rt.state.rich.text.as_str()), ("", ""), "an image alone pastes nothing into a field without a hook");
        focus(&mut rt, |s| &mut s.hooked);
        assert_eq!(rt.paste_forms(rt.paste_target().as_deref()), Forms::All);
        paste(&mut rt);
        assert_eq!(rt.state.hooked.text, "4×3");
        assert_eq!(rt.state.hooked.pills().map(|(_, id)| id.to_owned()).collect::<Vec<_>>(), [format!("image:{}", image.png().len())]);
        assert!(rt.state.window.is_empty(), "a focused field's paste never reaches the root");

        // Text beside an image: a declining hook leaves the text to the editor.
        m.set_content(ClipboardContent::new(" caption"));
        paste(&mut rt);
        assert_eq!(rt.state.hooked.text, "4×3 caption");
        assert!(rt.state.hook.last().is_some_and(|c| c.image().is_none()));

        m.set_content(ClipboardContent::new("alt").with_image(image.clone()));
        rt.update(|_, tx| tx.read_clipboard(|s: &mut State, content, _| s.reads.push(content.clone())));
        rt.frame(0.0);
        rt.frame(0.0);
        assert_eq!(rt.state.reads.iter().map(|c| (c.text(), c.image().map(ClipboardImage::size))).collect::<Vec<_>>(), [("alt", Some((4, 3)))]);
    }

    #[test]
    fn read_clipboard_answers_once_even_when_empty_and_pastes_nowhere() {
        use crate::ClipboardContent;
        #[derive(Default)]
        struct State { field: crate::TextEdit, reads: Vec<ClipboardContent> }
        fn view(s: &State, _: &mut Cx<State>) -> El {
            col().size(200.0, 100.0).child(crate::text_input(&s.field, crate::TextStyle::new(16.0, 400.0)).key("field").size(200.0, 40.0)
                .on_edit(|s: &mut State| &mut s.field))
        }
        let m = crate::clipboard::memory();
        let mut rt = Runtime::new(State::default(), view, (200.0, 100.0), Env::new(true, 1.0));
        rt.use_clipboard(&m);
        rt.frame(0.0);
        rt.update(|s, _| s.field.focused = true); rt.frame(0.0);
        let read = |rt: &mut Runtime<State>| {
            rt.update(|_, tx| tx.read_clipboard(|s: &mut State, content, _| s.reads.push(content.clone())));
            rt.frame(0.0);
            rt.frame(0.0);
        };
        read(&mut rt);
        assert_eq!(rt.state.reads, [ClipboardContent::default()], "an empty clipboard still answers");
        m.set_content(ClipboardContent::new("a").with_html("<a href='javascript:x'>a</a>").with_rtf("{\\rtf1 a}"));
        read(&mut rt);
        let got = rt.state.reads.pop().unwrap();
        assert_eq!((got.text(), got.html(), got.rtf()), ("a", Some("<a>a</a>"), Some("{\\rtf1 a}")));
        assert!(rt.state.field.text.is_empty() && rt.state.reads.len() == 1);
        assert!(rt.clipboard_reads.is_empty() && rt.read_requests.is_empty());
    }

    #[test]
    fn delayed_loops_sleep_on_the_shared_deadline_and_resume_after_native_idle() {
        fn view(s: &(f32, f32), cx: &mut Cx<(f32, f32)>) -> El {
            cx.time_scale(s.1);
            el().size(100.0, 100.0).opacity(s.0).child(el().size(20.0, 20.0)
                .looping(rune_motion::LoopSpec::opacity(1.0, 2.0, 0.0, 1.0)))
        }
        let make = |opacity, scale| Runtime::new((opacity, scale), view, (100.0, 100.0), Env::new(true, 1.0));
        let mut rt = make(1.0, 1.0);
        assert!(!rt.frame(0.0));
        assert_eq!(rt.wake_deadline(), Some(2.0));
        let commits = rt.stats.commits;
        assert!(!rt.frame(1.0));
        assert_eq!(rt.wake_deadline(), Some(2.0));
        assert!(rt.frame(1.0), "a zero-opacity first pose must still start the loop");
        assert_eq!(rt.stats.commits, commits);
        assert_eq!(rt.wake_deadline(), None);
        let mut rt = make(1.0, 1.0);
        rt.frame(0.0);
        rt.wall_start = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        assert!(!rt.frame(1.0 / 120.0));
        assert!((rt.wake_deadline().unwrap() - 2.0).abs() < 0.001, "an input during idle must not postpone the loop");
        rt.wall_start = Some(std::time::Instant::now() - std::time::Duration::from_secs(2));
        assert!(rt.frame(1.0 / 120.0));
        assert!((rt.t - 2.0).abs() < 0.001, "idle dt must not postpone the start again");
        let mut rt = make(1.0, 0.5);
        assert!(!rt.frame(0.0));
        assert_eq!(rt.wake_deadline(), Some(4.0));
        rt.set_live_visible(false);
        assert_eq!(rt.wake_deadline(), None);
        rt.frame(0.0);
        assert_eq!(rt.wake_deadline(), None);
        for (opacity, scale) in [(0.0, 1.0), (1.0, 0.0)] {
            let mut rt = make(opacity, scale);
            assert!(!rt.frame(0.0));
            assert_eq!(rt.wake_deadline(), None);
        }
    }

    #[test]
    fn cycles_share_deadlines_and_sleep_when_hidden_by_hover_groups() {
        fn view(hidden: &bool, _: &mut Cx<bool>) -> El {
            el().size(100.0, 100.0).hover_group().child(el().key("cycle").size(20.0, 20.0)
                .group_opacity(if *hidden { 0.0 } else { 1.0 }, 1.0)
                .cycle(crate::Cycle { period: 1.0, phase: 0.0,
                    stops: &[(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)], still: 0.4, fps: 30.0 }))
        }
        let mut rt = Runtime::new(false, view, (100.0, 100.0), Env::new(true, 1.0));
        assert!(!rt.frame(0.0), "cycles must sleep rather than request continuous frames");
        assert!((rt.wake_deadline().unwrap() - 1.0 / 30.0).abs() < 1e-7);
        assert_eq!(rt.loops.len(), 1);
        let commits = rt.stats.commits;
        rt.frame(0.5);
        assert_eq!(rt.stats.commits, commits);
        assert!((rt.ambient.unwrap() - 1.0 / 30.0).abs() < 1e-7);
        rt.state = true; rt.invalidate(); rt.frame(0.0);
        assert_eq!(rt.wake_deadline(), None, "group-opacity zero must not wake");
        rt.state = false; rt.invalidate(); rt.frame(0.0);
        assert!(rt.wake_deadline().is_some());
        rt.set_live_visible(false); rt.frame(0.0);
        assert_eq!(rt.wake_deadline(), None);
        rt.set_live_visible(true); rt.frame(0.0);
        assert!(rt.wake_deadline().is_some());
        rt.reduced_motion = true; rt.frame(0.0);
        assert_eq!(rt.wake_deadline(), None);
    }

    #[test]
    fn loops_sleep_below_one_8_bit_step_of_opacity() {
        fn view(rest: &f32, _: &mut Cx<f32>) -> El {
            el().size(100.0, 100.0).hover_group().child(el().key("cycle").size(20.0, 20.0)
                .group_opacity(*rest, 1.0)
                .cycle(crate::Cycle { period: 1.0, phase: 0.0,
                    stops: &[(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)], still: 0.4, fps: 30.0 }))
        }
        for (rest, wakes) in [(0.003, false), (0.005, true)] {
            let mut rt = Runtime::new(rest, view, (100.0, 100.0), Env::new(true, 1.0));
            rt.frame(0.0);
            assert_eq!(rt.wake_deadline().is_some(), wakes, "rest opacity {rest}");
        }
    }

    #[test]
    fn loops_sleep_offscreen_hidden_reduced_and_after_exit() {
        fn view(s: &(f32, bool, bool), cx: &mut Cx<(f32, bool, bool)>) -> El {
            cx.reduced_motion(s.2);
            let mut root = col().key("root").size(100.0, 100.0).clip_soft(0.0);
            if s.1 {
                root = root.child(el().key("loop").absolute(0.0, s.0).size(20.0, 20.0).fill(Color::WHITE)
                    .looping(rune_motion::LoopSpec::opacity(1.0, 0.0, 0.4, 1.0)));
            }
            root
        }
        for (y, shown, reduced, active) in [(0.0, true, false, true), (200.0, true, false, false),
            (0.0, false, false, false), (0.0, true, true, false)] {
            let mut rt = Runtime::new((y, shown, reduced), view, (100.0, 100.0), Env::new(true, 2.0));
            assert_eq!(rt.frame(0.0), active);
            // Model the shell's scheduling gate: zero requested frames over ten seconds at rest.
            let mut requested = 0;
            for _ in 0..1200 { if rt.animating { requested += 1; rt.frame(1.0 / 120.0); } }
            assert_eq!(requested, if active { 1200 } else { 0 });
            if active {
                rt.state.1 = false; rt.invalidate(); rt.frame(0.0);
                assert!(rt.elements().iter().any(|e| e.key.as_deref() == Some("loop") && e.opacity > 0.0));
                for _ in 0..240 { rt.frame(1.0 / 120.0); }
                assert!(!rt.animating);
                assert!(!rt.elements().iter().any(|e| e.key.as_deref() == Some("loop")));
            }
        }
    }

    #[test]
    fn slides_sweep_through_their_clip_and_sleep_when_hidden_or_reduced() {
        type S = (bool, bool, Option<rune_motion::Easing>);
        fn view(s: &S, cx: &mut Cx<S>) -> El {
            cx.reduced_motion(s.1);
            let mut track = crate::row().key("track").size(100.0, 8.0).clip_soft(0.0);
            if s.0 {
                let mut spec = rune_motion::LoopSpec::slide(1.0, [-1.0, 0.0], [4.0, 0.0]);
                if let Some(e) = s.2 { spec = spec.easing(e); }
                track = track.child(el().key("bar").w_frac(0.25).h_full().fill(Color::WHITE).looping(spec));
            }
            col().size(100.0, 100.0).child(track)
        }
        let bar = |rt: &Runtime<S>| {
            rt.elements().into_iter().find(|e| e.key.as_deref() == Some("bar")).map(|e| e.rect[0])
        };
        for easing in [None, Some(rune_motion::Easing::LINEAR)] {
            let mut rt = Runtime::new((true, false, easing), view, (100.0, 100.0), Env::new(true, 2.0));
            assert!(rt.frame(0.0), "a slide starting outside its clip must still run");
            assert_eq!(bar(&rt), Some(-25.0));
            let mut xs = Vec::new();
            for _ in 0..128 { assert!(rt.frame(1.0 / 128.0)); xs.push(bar(&rt).unwrap()); }
            assert!((xs[63] - 37.5).abs() < 0.01, "halfway through the period is the sweep's midpoint: {}", xs[63]);
            assert!(xs[..127].windows(2).all(|w| w[1] >= w[0]));
            if easing == Some(rune_motion::Easing::LINEAR) {
                let step = 125.0 / 128.0;
                assert!(xs[..126].windows(2).all(|w| (w[1] - w[0] - step).abs() < 0.05));
            }
            assert_eq!(xs[127], -25.0, "the sweep wraps");
            rt.state.0 = false; rt.invalidate();
            for _ in 0..240 { rt.frame(1.0 / 120.0); }
            assert!(!rt.animating);
            let mut rt = Runtime::new((true, true, easing), view, (100.0, 100.0), Env::new(true, 2.0));
            assert!(!rt.frame(0.0));
            assert_eq!(bar(&rt), Some(37.5), "reduced motion holds the midpoint");
            let mut rt = Runtime::new((true, false, easing), view, (100.0, 100.0), Env::new(true, 2.0));
            rt.frame(0.0);
            rt.set_live_visible(false);
            assert!(!rt.frame(1.0 / 120.0));
        }
    }

    #[test]
    fn delayed_opacity_handoff_never_overprints_the_outgoing_label() {
        use rune_motion::AnimExt;
        fn view(s: &(bool, bool), cx: &mut Cx<(bool, bool)>) -> El {
            cx.reduced_motion(s.1);
            let fade = |show| rune_motion::Curve::ease_in_out(0.18).delay(if show { 0.18 } else { 0.0 });
            col().child(el().key("copy").size(28.0, 28.0).opacity(if s.0 { 0.0 } else { 1.0 }).opacity_motion(fade(!s.0)))
                .child(el().key("working").size(28.0, 28.0).opacity(if s.0 { 1.0 } else { 0.0 }).opacity_motion(fade(s.0)))
        }
        for reduced in [false, true] {
            let mut rt = Runtime::new((false, reduced), view, (100.0, 100.0), Env::new(true, 2.0));
            rt.frame(0.0);
            for busy in [true, false] {
                rt.state.0 = busy;
                rt.invalidate();
                for _ in 0..60 {
                    rt.frame(1.0 / 120.0);
                    let alpha = |key| rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap().opacity;
                    assert!(alpha("copy").min(alpha("working")) < 0.001);
                }
                assert!(!rt.animating);
            }
        }
    }

    #[test]
    fn measurement_cannot_change_presented_geometry_or_motion_policy() {
        fn view(changed: &bool, cx: &mut Cx<bool>) -> El {
            cx.reduced_motion(*changed);
            col().key("stack").w_full().growth(GrowthSpec::default())
                .child(el().key("row").w_full().h(if *changed { 160.0 } else { 40.0 }))
        }
        let mut rt = Runtime::new(false, view, (300.0, 300.0), Env::new(true, 2.0));
        rt.frame(0.0);
        let id = rt.nodes.iter().find(|n| n.key.as_deref() == Some("row")).unwrap().id;
        let before = rt.scene.presented(id).unwrap().rect;
        rt.state = true;
        let measured = rt.measure(&Tx::default());
        assert_eq!(rt.scene.presented(id).unwrap().rect, before);
        assert_eq!(rt.scene.store().active_len(), 0);
        assert_eq!(rt.grows[&id].h, 40.0);
        assert!(!rt.reduced_motion && !rt.scene.store().reduced_motion());
        rt.apply(measured, &Tx::default());
        assert!(rt.reduced_motion && rt.scene.store().reduced_motion());
        assert_eq!(rt.grows[&id].h, 160.0);
    }

    #[test]
    fn send_flight_follows_the_measured_bubble_while_earlier_rows_land() {
        struct State { draft: crate::edit::TextEdit, sent: bool, height: f32 }
        fn view(s: &State, _: &mut Cx<State>) -> El {
            let mut rows = col().w_full().growth(GrowthSpec::default())
                .child(el().key("earlier").w_full().h(s.height));
            if s.sent { rows = rows.child(col().key("bubble").w_full().py(8.0)
                .child(crate::el::text("héllo flight").key("destination").wrap())); }
            col().w_full().h_full()
                .child(col().key("scroll").w_full().grow(1.0).shrink().scroll(ScrollSpec { stick_bottom: true }).child(rows))
                .child(crate::el::text_input(&s.draft, crate::tok::TextStyle::new(14.0, 400.0)).key("source").w_full().h(60.0)
                    .on_edit(|s: &mut State| &mut s.draft)
                    .on_submit(|s: &mut State, tx: &mut Tx| {
                        s.sent = true;
                        s.draft.clear();
                        tx.fly_text("source", "destination");
                    }))
        }
        let mut draft = crate::edit::TextEdit::new("héllo flight");
        draft.focused = true;
        let mut rt = Runtime::new(State { draft, sent: false, height: 80.0 }, view, (400.0, 300.0), Env::new(true, 2.0));
        rt.frame(0.0);
        rt.state.height = 400.0;
        rt.invalidate();
        rt.frame(1.0 / 120.0);
        rt.key(KeyEvent { key: crate::edit::Key::Enter, mods: Default::default() });
        rt.frame(1.0 / 120.0);
        assert_eq!(rt.flights.len(), 1);
        assert!(!rt.flights[0].glyphs.is_empty(), "the visible composer glyphs survive clearing the model");
        let mut previous_target = rt.flights[0].destination;
        let mut retargets = 0;
        for _ in 0..240 {
            rt.frame(1.0 / 120.0);
            if let Some(f) = rt.flights.first() {
                assert!(rt.instances.iter().any(|q| q.params[1] == KIND_GLYPH && q.uv[2] > 0.0 && q.fill[3] > 0.1
                    && q.rect[0].max(q.clip[0]) + 1.0 < (q.rect[0] + q.rect[2]).min(q.clip[2])
                    && q.rect[1].max(q.clip[1]) + 1.0 < (q.rect[1] + q.rect[3]).min(q.clip[3])),
                    "the source/destination handoff never loses every visible glyph");
                let target = rt.nodes.iter().position(|n| n.key.as_deref() == Some("destination")).unwrap();
                assert!((f.destination.y - rt.drawn[target][1]).abs() < 0.01, "destination is this frame's post-scroll geometry");
                if f.destination != previous_target { retargets += 1; }
                previous_target = f.destination;
            }
        }
        assert!(retargets > 3, "earlier row growth actually moved the destination");
        assert!(rt.flights.is_empty(), "flight lands and releases the captured glyphs");
        assert!(!rt.animating);
    }

    #[test]
    fn virtual_rows_match_full_layout_while_paging_and_keep_a_keyed_anchor() {
        struct State { virtualized: bool, keys: Vec<usize> }
        fn view(s: &State, _: &mut Cx<State>) -> El {
            let mut stack = col().key("rows").w_full().pt(8.0).pb(24.0).growth(GrowthSpec::default());
            if s.virtualized { stack = stack.virtualize(80.0); }
            for &i in &s.keys {
                stack = stack.child(col().key(format!("r{i}")).w_full().pb(9.0).child(
                    crate::el::text(format!("Row {i}: {}", "variable height héllo naïve ".repeat(i % 7 + 1))).wrap().w_full()
                ));
            }
            col().key("scroll").w_full().h_full().clip_fade(0.0, 8.0, 0.0, 8.0).scroll(ScrollSpec { stick_bottom: true }).child(stack)
        }
        let env = Env::new(true, 2.0);
        let mut full = Runtime::new(State { virtualized: false, keys: (0..600).collect() }, view, (240.0, 300.0), env);
        let mut window = Runtime::new(State { virtualized: true, keys: (0..600).collect() }, view, (240.0, 300.0), env);
        full.frame(0.0);
        window.frame(0.0);
        assert!(window.nodes.len() < 100, "offscreen rows aren't laid out or retained in the scene");
        assert!(window.text.shaped < full.text.shaped / 5, "offscreen text isn't shaped");
        let visible = |r: &Runtime<State>| r.elements().into_iter()
            .filter(|e| e.key.as_ref().is_some_and(|k| k.starts_with('r') && k != "rows") && e.visible[3] > 0.0)
            .map(|e| (e.key.unwrap(), e.rect)).collect::<Vec<_>>();
        for step in 0..135 {
            let a = visible(&full);
            let b = visible(&window);
            assert_eq!(a.len(), b.len(), "visible count at step {step}: {a:?} vs {b:?}");
            for ((ak, ar), (bk, br)) in a.iter().zip(&b) {
                assert_eq!(ak, bk, "visible identity at step {step}");
                assert!(ar.iter().zip(br).all(|(a, b)| (a - b).abs() < 0.05), "anchor at step {step}: {ak} {ar:?} vs {br:?}");
            }
            let dy = if step < 90 { 137.25 } else { -137.25 };
            full.wheel(120.0, 150.0, dy);
            window.wheel(120.0, 150.0, dy);
            full.frame(1.0 / 120.0);
            window.frame(1.0 / 120.0);
        }
        let before = visible(&window);
        window.state.keys.insert(0, 9000);
        window.invalidate();
        window.frame(1.0 / 120.0);
        assert_eq!(before, visible(&window), "prepending outside the window preserves its keyed anchor");
        assert!(window.nodes.len() < 150 && window.grows.len() < 70, "paging releases old row state");
        let anchor = visible(&window)[0].clone();
        window.set_viewport((320.0, 300.0), 2.0);
        window.frame(0.0);
        for _ in 0..60 {
            let after = visible(&window);
            let rect = after.iter().find(|(key, _)| key == &anchor.0).expect("resize retains the visible keyed anchor");
            assert!((rect.1[1] - anchor.1[1]).abs() < 0.05, "resize anchor moved: {:?} vs {:?}", anchor, rect);
            window.frame(1.0 / 120.0);
        }
        window.settle_all();
        window.frame(0.0);
        let last = visible(&window).pop().unwrap();
        assert_eq!(last.0, "r599");
        assert!((last.1[1] + last.1[3] - 276.0).abs() < 0.05, "return lands at the exact bottom");
        assert!(!window.animating, "moving: {:?}; active: {}", window.elements().into_iter().filter(|e| e.moving).map(|e| (e.path, e.rect, e.target, e.opacity)).collect::<Vec<_>>(), window.scene.store().active_len());
    }

    #[test]
    fn reduced_motion_rows_land_immediately_but_keep_the_presence_fade() {
        fn view(show: &bool, cx: &mut Cx<bool>) -> El {
            cx.reduced_motion(true);
            let mut stack = col().w_full().growth(GrowthSpec::default());
            if *show {
                stack = stack.child(el().key("row").size(200.0, 100.0));
            }
            stack
        }
        let mut rt = Runtime::new(false, view, (200.0, 300.0), Env::new(true, 2.0));
        assert!(!rt.frame(0.0));
        rt.state = true;
        rt.invalidate();
        rt.frame(1.0 / 120.0);
        let row = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("row")).unwrap();
        assert_eq!(row.rect[3], 100.0, "reduced motion removes growth travel");
        assert!(row.opacity > 0.0 && row.opacity < 1.0, "new rows still fade in");
        rt.frame(0.2);
        assert!(!rt.animating);
        assert_eq!(rt.elements().into_iter().find(|e| e.key.as_deref() == Some("row")).unwrap().opacity, 1.0);
        rt.state = false;
        rt.invalidate();
        rt.frame(1.0 / 120.0);
        let row = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("row")).unwrap();
        assert!(row.opacity > 0.0 && row.opacity < 1.0, "removed rows stay for their fade");
        for _ in 0..60 { rt.frame(1.0 / 120.0); }
        assert!(!rt.animating);
        assert!(rt.elements().iter().all(|e| e.key.as_deref() != Some("row")));
    }

    #[test]
    fn clipboard_requests_follow_handler_order_and_are_consumed_once() {
        use crate::edit::{Key, KeyEvent, Mods, TextEdit};
        fn view(edit: &TextEdit, _: &mut Cx<TextEdit>) -> El {
            col()
                .child(el().key("copy").size(40.0, 28.0).on_press_tx(|_: &mut TextEdit, tx| tx.copy("response")))
                .child(crate::el::text_input(edit, crate::tok::TextStyle::new(14.0, 400.0)).key("input").on_edit(|e: &mut TextEdit| e))
        }
        let mut edit = TextEdit::new("héllo 👋");
        edit.anchor = 0;
        let mut rt = Runtime::new(edit, view, (300.0, 200.0), Env::new(true, 2.0));
        rt.frame(0.0);
        let button = rt.nodes.iter().find(|n| n.key.as_deref() == Some("copy")).unwrap().path.clone();
        let input = rt.nodes.iter().find(|n| n.key.as_deref() == Some("input")).unwrap().path.clone();
        rt.set_focus(Some(input), true);
        let copy_key = KeyEvent { key: Key::Command('c'), mods: Mods { cmd: true, ..Mods::default() } };
        rt.queue.push(Action::Press(button.clone(), crate::PointerButton::Primary));
        rt.queue.push(Action::Key { ev: copy_key.clone(), repeat: false });
        rt.frame(1.0 / 120.0);
        assert_eq!(rt.copy_request.take().as_deref(), Some("héllo 👋"));
        rt.frame(1.0 / 120.0);
        assert!(rt.copy_request.is_none());
        rt.queue.push(Action::Key { ev: copy_key, repeat: false });
        rt.queue.push(Action::Press(button, crate::PointerButton::Primary));
        rt.frame(1.0 / 120.0);
        assert_eq!(rt.copy_request.take().as_deref(), Some("response"));
    }

    #[test]
    fn streaming_fill_and_width_rewrap_land_on_completion() {
        fn view(state: &(String, bool), _: &mut Cx<(String, bool)>) -> El {
            col().w_full().growth(GrowthSpec::default()).child(
                col().key("row").w_full().streaming(state.1).child(crate::el::text(state.0.clone()).wrap().w_full().reveal(0.18))
            )
        }
        let mut rt = Runtime::new(("one".into(), true), view, (180.0, 500.0), Env::new(true, 2.0));
        rt.frame(0.0);
        rt.state.0 = "one two three four five six seven eight nine ten eleven".into();
        rt.invalidate();
        rt.frame(1.0 / 120.0);
        let row = rt.nodes.iter().find(|n| n.key.as_deref() == Some("row")).unwrap().id;
        let n = &rt.nodes[rt.node_index[&row]];
        let full = n.local.h;
        assert!(rt.grows[&row].target < full, "new line is held back while filling");
        for _ in 0..240 { rt.frame(1.0 / 120.0); }
        assert!(!rt.animating, "partial line at rest must stop ticking");
        let old_h = rt.grows[&row].h;
        rt.set_viewport((600.0, 500.0), 2.0);
        rt.state.0.push_str(" twelve");
        rt.invalidate();
        rt.frame(1.0 / 120.0);
        assert_eq!(rt.grows[&row].target, old_h, "rewrap waits for stable width");
        for _ in 0..15 { rt.frame(1.0 / 120.0); }
        assert!(rt.grows[&row].target < old_h, "width change may shrink a streaming row");
        assert!(rt.grows[&row].h > rt.grows[&row].target, "rewrapped height eases instead of snapping");
        rt.state.0 = "one two".into();
        rt.invalidate();
        rt.frame(0.0);
        assert!(rt.grows[&row].target < rt.grows[&row].h, "streaming may still shrink inside the 400 ms rewrap window");
        for _ in 0..120 { rt.frame(1.0 / 120.0); }
        let landed = rt.grows[&row].h;
        rt.state.0 = "o".into();
        rt.invalidate();
        rt.frame(0.0);
        assert_eq!(rt.grows[&row].target, landed, "streaming cannot shrink after the rewrap window");
        rt.state.1 = false;
        rt.invalidate();
        for _ in 0..240 { rt.frame(1.0 / 120.0); }
        assert!(!rt.animating);
        assert_eq!(rt.grows[&row].h, rt.nodes[rt.node_index[&row]].local.h);
    }

    #[test]
    fn retained_layout_matches_fresh_tree_across_edits() {
        fn view(seed: &usize, _: &mut Cx<usize>) -> El {
            let mut root = col().key("root").w_full();
            for k in 0..8 {
                let i = (k + seed) % 8;
                if (i + seed) % 5 == 0 { continue; }
                let body = "some words ".repeat((i + seed) % 10 + 1);
                let child = if (i + seed) % 7 == 0 { el().h(31.0) } else { crate::el::text(body).wrap() };
                root = root.child(child.key(format!("row-{i}")).w_full().px((seed % 3) as f32));
            }
            root
        }
        let env = Env::new(true, 2.0);
        let mut cached = Runtime::new(0usize, view, (320.0, 600.0), env);
        for seed in 0..40 {
            let size = (240.0 + (seed % 4) as f32 * 30.0, 600.0);
            let scale = if seed % 3 == 0 { 1.0 } else { 2.0 };
            cached.state = seed;
            cached.set_viewport(size, scale);
            cached.settle_all();
            cached.frame(0.0);
            let mut fresh = Runtime::new(seed, view, size, Env { scale, ..env });
            fresh.frame(0.0);
            let rects = |r: &Runtime<usize>| r.elements().into_iter().filter(|e| e.opacity > 0.0).map(|e| (e.path, e.target)).collect::<Vec<_>>();
            assert_eq!(rects(&cached), rects(&fresh), "edit {seed}");
        }
    }

    #[test]
    fn presence_exits_stay_drawn_and_revive_mid_flight() {
        fn view(show: &bool, _cx: &mut Cx<bool>) -> El {
            let mut root = col().key("root").size(200.0, 100.0);
            if *show {
                root = root.child(
                    el().key("badge")
                        .size(80.0, 30.0)
                        .fill(Color::WHITE)
                        .transition(Transition::blur_replace()),
                );
            }
            root
        }

        let mut rt = Runtime::new(true, view, (200.0, 100.0), Env::new(true, 2.0));
        assert!(!rt.frame(0.0), "first mount never animates");
        assert_eq!(rt.elements().len(), 2);

        // Remove the badge: it stays in the tree while its blur-replace exit runs.
        rt.state = false;
        rt.dirty = true;
        assert!(rt.frame(1.0 / 60.0));
        let badge = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("badge")).unwrap();
        assert!(badge.opacity > 0.0 && badge.opacity < 1.0);

        // Revive mid-exit: continues without a jump.
        let before = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("badge")).unwrap().opacity;
        rt.state = true;
        rt.dirty = true;
        rt.frame(1.0 / 60.0);
        let after = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("badge")).unwrap().opacity;
        assert!((after - before).abs() < 0.15, "{before} -> {after}");

        // Remove again and run until settled: unmounts only after opacity reaches 0.
        rt.state = false;
        rt.dirty = true;
        for _ in 0..120 {
            rt.frame(1.0 / 60.0);
        }
        assert!(!rt.animating);
        assert!(rt.elements().iter().all(|e| e.key.as_deref() != Some("badge")));
    }

    /// Target opacities of a dot outside every `time_scale` and one inside `outer > inner`,
    /// plus an exiting badge under `inner`.
    #[derive(Clone, Copy)]
    struct Lab { outside: f32, inside: f32, badge: bool, outer: f32, inner: f32 }

    impl Lab {
        fn at(outer: f32, inner: f32) -> Lab { Lab { outside: 1.0, inside: 1.0, badge: true, outer, inner } }
        fn runtime(self) -> Runtime<Lab> {
            fn view(s: &Lab, _: &mut Cx<Lab>) -> El {
                let dot = |key: &str, o: f32| el().key(key).size(20.0, 20.0).fill(Color::WHITE).opacity(o);
                let mut inner = col().key("inner").time_scale(s.inner).child(dot("inside", s.inside));
                if s.badge { inner = inner.child(el().key("badge").size(20.0, 20.0).fill(Color::WHITE).transition(Transition::opacity())); }
                col().key("root").size(200.0, 200.0)
                    .child(dot("outside", s.outside))
                    .child(col().key("outer").time_scale(s.outer).child(inner))
            }
            let mut rt = Runtime::new(self, view, (200.0, 200.0), Env::new(true, 1.0));
            assert!(!rt.frame(0.0));
            rt
        }
    }

    const LAB_DT: f64 = 1.0 / 60.0;

    fn lab_set(rt: &mut Runtime<Lab>, f: impl FnOnce(&mut Lab)) {
        f(&mut rt.state);
        rt.dirty = true;
    }

    fn lab_steps(rt: &mut Runtime<Lab>, n: usize) {
        for _ in 0..n { rt.frame(LAB_DT); }
    }

    fn lab_opacity(rt: &Runtime<Lab>, key: &str) -> Option<f32> {
        rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).map(|e| e.opacity)
    }

    #[test]
    fn time_scale_zero_freezes_its_subtree_while_the_rest_moves() {
        let mut rt = Lab::at(1.0, 0.0).runtime();
        lab_set(&mut rt, |s| { s.outside = 0.0; s.inside = 0.0; });
        lab_steps(&mut rt, 12);
        let outside = lab_opacity(&rt, "outside").unwrap();
        assert!(outside < 0.9, "outside fades: {outside}");
        assert_eq!(lab_opacity(&rt, "inside"), Some(1.0), "inside holds exactly");
    }

    #[test]
    fn half_time_scale_reaches_the_same_point_at_twice_the_time() {
        let mut rt = Lab::at(1.0, 0.5).runtime();
        lab_set(&mut rt, |s| { s.outside = 0.0; s.inside = 0.0; });
        lab_steps(&mut rt, 10);
        let reference = lab_opacity(&rt, "outside").unwrap();
        assert!(reference > 0.05 && reference < 0.95, "mid-flight: {reference}");
        lab_steps(&mut rt, 10);
        let inside = lab_opacity(&rt, "inside").unwrap();
        assert!((inside - reference).abs() < 1e-4, "{inside} at 2t vs {reference} at t");
    }

    #[test]
    fn nested_time_scales_multiply() {
        let mut rt = Lab::at(0.5, 0.5).runtime();
        lab_set(&mut rt, |s| { s.outside = 0.0; s.inside = 0.0; });
        lab_steps(&mut rt, 5);
        let reference = lab_opacity(&rt, "outside").unwrap();
        assert!(reference > 0.05 && reference < 0.95, "mid-flight: {reference}");
        lab_steps(&mut rt, 15);
        let inside = lab_opacity(&rt, "inside").unwrap();
        assert!((inside - reference).abs() < 1e-4, "{inside} at 4t vs {reference} at t");
    }

    #[test]
    fn a_frozen_subtree_with_pending_motion_requests_no_frames() {
        let mut rt = Lab::at(1.0, 0.0).runtime();
        lab_set(&mut rt, |s| s.inside = 0.0);
        for _ in 0..5 {
            assert!(!rt.frame(LAB_DT), "a frozen fade asks for no frames");
        }
        assert!(rt.scene.store().active_len() > 0, "the fade is still pending");
        assert_eq!(rt.wake_deadline(), None);
        assert_eq!(lab_opacity(&rt, "inside"), Some(1.0));
        lab_set(&mut rt, |s| s.inner = 1.0);
        assert!(rt.frame(LAB_DT), "resuming runs it");
        assert!(lab_opacity(&rt, "inside").unwrap() < 1.0);
    }

    #[test]
    fn resuming_a_frozen_subtree_continues_from_the_same_value() {
        let mut rt = Lab::at(1.0, 1.0).runtime();
        lab_set(&mut rt, |s| { s.outside = 0.0; s.inside = 0.0; });
        lab_steps(&mut rt, 5);
        let held = lab_opacity(&rt, "inside").unwrap();
        assert!(held < 1.0);
        lab_set(&mut rt, |s| s.inner = 0.0);
        lab_steps(&mut rt, 5);
        // The dot outside is ten frames in; the frozen one is still five frames in.
        let reference = lab_opacity(&rt, "outside").unwrap();
        lab_steps(&mut rt, 5);
        assert_eq!(lab_opacity(&rt, "inside"), Some(held), "no drift while frozen");
        lab_set(&mut rt, |s| s.inner = 1.0);
        rt.frame(0.0);
        assert_eq!(lab_opacity(&rt, "inside"), Some(held), "resumes from the held value");
        lab_steps(&mut rt, 5);
        let inside = lab_opacity(&rt, "inside").unwrap();
        assert!((inside - reference).abs() < 1e-4, "continues on the same curve: {inside} vs {reference}");
    }

    #[test]
    fn an_exit_in_a_frozen_subtree_finishes_after_resuming() {
        let mut rt = Lab::at(1.0, 0.0).runtime();
        lab_set(&mut rt, |s| s.badge = false);
        lab_steps(&mut rt, 120);
        assert!(!rt.animating);
        let badge = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("badge")).expect("still exiting");
        assert!(badge.exiting && badge.opacity == 1.0, "held at the start of its exit: {}", badge.opacity);
        lab_set(&mut rt, |s| s.inner = 1.0);
        lab_steps(&mut rt, 3);
        assert!(lab_opacity(&rt, "badge").is_some_and(|o| o < 1.0), "the exit runs");
        lab_steps(&mut rt, 120);
        assert!(!rt.animating);
        assert_eq!(lab_opacity(&rt, "badge"), None, "and is removed");
    }

    #[test]
    fn a_node_unmounted_at_once_in_a_scaled_subtree_drops_its_clock() {
        let mut rt = Lab::at(1.0, 0.5).runtime();
        let badge = rt.nodes.iter().find(|n| n.key.as_deref() == Some("badge")).unwrap().id;
        assert_eq!(rt.scene.store().node_time_scale(badge), 0.5);
        lab_set(&mut rt, |s| s.badge = false);
        rt.settle_all();
        rt.frame(LAB_DT);
        assert_eq!(lab_opacity(&rt, "badge"), None);
        assert_eq!(rt.scene.store().node_time_scale(badge), 1.0, "its clock went with it");
    }

    #[test]
    fn an_exiting_element_drops_its_own_time_scale() {
        fn view(show: &bool, _: &mut Cx<bool>) -> El {
            let mut root = col().key("root").size(200.0, 100.0);
            if *show {
                root = root.child(el().key("card").size(80.0, 30.0).fill(Color::WHITE)
                    .time_scale(0.0).transition(Transition::opacity()));
            }
            root
        }
        let mut rt = Runtime::new(true, view, (200.0, 100.0), Env::new(true, 1.0));
        assert!(!rt.frame(0.0));
        rt.state = false;
        rt.dirty = true;
        for _ in 0..3 { rt.frame(LAB_DT); }
        let card = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("card")).expect("exiting");
        assert!(card.exiting && card.opacity < 1.0, "its exit runs: {}", card.opacity);
        for _ in 0..120 { rt.frame(LAB_DT); }
        assert!(rt.elements().iter().all(|e| e.key.as_deref() != Some("card")), "and it leaves");
    }

    #[test]
    fn growth_rows_grow_and_sticky_scroll_lands_at_bottom() {
        use crate::el::{GrowthSpec, ScrollSpec};
        fn view(n: &usize, _cx: &mut Cx<usize>) -> El {
            let mut stack = col().key("stack").growth(GrowthSpec::default());
            for i in 0..*n {
                stack = stack.child(el().key(format!("r{i}")).h(100.0).w_full());
            }
            col().key("root").child(col().key("scroll").h(300.0).w_full().scroll(ScrollSpec { stick_bottom: true }).child(stack))
        }
        let mut rt = Runtime::new(10usize, view, (200.0, 300.0), Env::new(true, 2.0));
        rt.frame(0.0);
        // First frame: at the bottom, nothing animating (guarantee 8).
        assert!(!rt.animating);
        let last = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("r9")).unwrap();
        assert!((last.rect[1] + last.rect[3] - 300.0).abs() < 0.05, "{:?}", last.rect);
        // A new row grows from 0 and the end stays pinned.
        rt.state = 11;
        rt.dirty = true;
        rt.frame(1.0 / 60.0);
        assert!(rt.animating);
        let r10 = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("r10")).unwrap();
        assert!(r10.rect[3] < 100.0 && r10.rect[3] > 0.0);
        for _ in 0..120 {
            rt.frame(1.0 / 60.0);
        }
        assert!(!rt.animating);
        let r10 = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("r10")).unwrap();
        assert!((r10.rect[1] + r10.rect[3] - 300.0).abs() < 0.05, "{:?}", r10.rect);
    }

    #[test]
    fn animate_layout_springs_a_growth_row_s_contents() {
        use crate::el::GrowthSpec;
        fn view(s: &(f32, bool), _cx: &mut Cx<(f32, bool)>) -> El {
            let mut trigger = crate::row().key("trigger").h(28.0)
                .child(el().key("label").size(s.0, 16.0).width_leads())
                .child(el().key("chevron").size(14.0, 14.0));
            if s.1 { trigger = trigger.animate_layout(); }
            col().key("stack").w_full().growth(GrowthSpec::default())
                .child(col().key("r0").w_full().child(trigger))
        }
        let chevron_x = |rt: &Runtime<(f32, bool)>| rt.elements().into_iter()
            .find(|e| e.key.as_deref() == Some("chevron")).unwrap().rect[0];
        let label_w = |rt: &Runtime<(f32, bool)>| rt.elements().into_iter()
            .find(|e| e.key.as_deref() == Some("label")).unwrap().rect[2];
        for animate in [false, true] {
            let mut rt = Runtime::new((53.0, animate), view, (300.0, 100.0), Env::new(true, 2.0));
            rt.frame(0.0);
            assert_eq!(chevron_x(&rt), 53.0);
            rt.update(|s, _| s.0 = 88.0);
            rt.frame(1.0 / 120.0);
            let (x, w) = (chevron_x(&rt), label_w(&rt));
            if animate {
                assert!(x > 53.0 && x < 60.0, "springs from where it was: {x}");
                assert!(w > x, "width_leads still leads inside a growth row: {w} vs {x}");
                for _ in 0..120 { rt.frame(1.0 / 120.0); }
                assert_eq!(chevron_x(&rt), 88.0);
            } else {
                assert_eq!(x, 88.0, "a growth row's contents snap by default");
            }
        }
    }

    #[test]
    fn animate_layout_in_a_growth_row_leaves_vertical_to_growth() {
        use crate::el::GrowthSpec;
        // (label width, space above the header, a second row)
        fn view(s: &(f32, f32, bool), _cx: &mut Cx<(f32, f32, bool)>) -> El {
            let head = |k: &str, w: f32| crate::row().key(k).h(28.0).animate_layout()
                .child(el().key(format!("{k}-names")).size(w, 16.0))
                .child(el().key(format!("{k}-chevron")).size(14.0, 14.0));
            let mut stack = col().key("stack").w_full().growth(GrowthSpec::default())
                .child(col().key("r0").w_full().child(el().key("above").w_full().h(s.1)).child(head("h0", s.0)));
            if s.2 { stack = stack.child(col().key("r1").w_full().child(head("h1", 40.0))); }
            stack
        }
        let find = |rt: &Runtime<(f32, f32, bool)>, key: &str| rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap();
        let mut rt = Runtime::new((31.5, 0.0, false), view, (300.0, 200.0), Env::new(true, 2.0));
        rt.frame(0.0);
        rt.update(|s, _| *s = (92.0, 20.0, true));
        rt.frame(1.0 / 120.0);
        let (chevron, names) = (find(&rt, "h0-chevron").rect, find(&rt, "h0-names").rect);
        assert!(chevron[0] > 31.5 && chevron[0] < 40.0, "x springs: {chevron:?}");
        assert!(names[2] > 31.5 && names[2] < 40.0, "width springs: {names:?}");
        assert_eq!(chevron[1], 20.0, "y lands at once; the row's growth carries it");
        assert_eq!(find(&rt, "h1-names").opacity, 1.0, "a new row's contents ride its growth");
    }

    #[test]
    fn layout_out_of_sight_before_and_after_moves_at_once() {
        fn view(wide: &bool, _cx: &mut Cx<bool>) -> El {
            let rows = (0..40).map(|i| el().key(format!("r{i}")).w(if *wide { 200.0 } else { 100.0 }).h(40.0).fill(Color::WHITE));
            col().w_full().h_full()
                .child(col().key("scroll").w_full().h(100.0)
                    .scroll(ScrollSpec { stick_bottom: false }).child(col().children(rows)))
                .child(el().key("crosser").absolute(if *wide { -80.0 } else { 380.0 }, 20.0).size(20.0, 20.0).fill(Color::WHITE))
                .child(col().key("drawer").w_full().h(if *wide { 100.0 } else { 0.0 }).clip_soft(0.0)
                    .child(el().key("inside-closing").absolute(10.0, if *wide { 180.0 } else { 20.0 }).size(40.0, 20.0).fill(Color::WHITE)))
        }
        let mut rt = Runtime::new(true, view, (300.0, 300.0), Env::new(true, 2.0));
        rt.frame(0.0);
        rt.frame(1.0 / 120.0);
        rt.update(|s, _| *s = false);
        rt.frame(1.0 / 120.0);
        let rect = |rt: &Runtime<bool>, key: &str| rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap().rect;
        assert!(rect(&rt, "r0")[2] > 190.0, "a row in sight springs: {}", rect(&rt, "r0")[2]);
        assert_eq!(rect(&rt, "r30")[2], 100.0, "a row out of sight before and after lands at once");
        assert!(rect(&rt, "crosser")[0] < 0.0, "an element sweeping across the window animates: {:?}", rect(&rt, "crosser"));
        assert!(rect(&rt, "inside-closing")[1] > 150.0, "an element entering a still-closing clip animates: {:?}", rect(&rt, "inside-closing"));
    }

    #[test]
    fn animate_layout_lets_a_growth_row_s_new_contents_fade_in() {
        use crate::el::GrowthSpec;
        fn view(s: &(bool, bool), _cx: &mut Cx<(bool, bool)>) -> El {
            let trigger = crate::row().key("trigger").h(28.0).animate_layout()
                .child(el().key(if s.0 { "after" } else { "before" }).size(if s.0 { 88.0 } else { 53.0 }, 16.0));
            let mut stack = col().key("stack").w_full().growth(GrowthSpec::default());
            if s.1 { stack = stack.virtualize(40.0); }
            col().w_full().h_full().child(col().key("scroll").w_full().h(100.0).scroll(ScrollSpec { stick_bottom: true })
                .child(stack.child(col().key("r0").w_full().child(col().key("part").child(trigger)))))
        }
        let opacity = |rt: &Runtime<(bool, bool)>, key: &str| rt.elements().into_iter()
            .find(|e| e.key.as_deref() == Some(key)).map(|e| e.opacity);
        for virtualized in [false, true] {
            let mut rt = Runtime::new((false, virtualized), view, (300.0, 100.0), Env::new(true, 2.0));
            rt.frame(0.0);
            for _ in 0..30 { rt.frame(1.0 / 120.0); }
            rt.update(|s, _| s.0 = true);
            rt.frame(1.0 / 120.0);
            let (before, after) = (opacity(&rt, "before").unwrap_or(0.0), opacity(&rt, "after").unwrap());
            assert!(before > 0.5 && after < 0.5, "virtualized {virtualized}: crossfades, {before} out and {after} in");
        }
    }

    #[test]
    fn explicit_transitions_play_inside_a_growth_row() {
        use crate::el::GrowthSpec;
        fn view(s: &(bool, bool), _cx: &mut Cx<(bool, bool)>) -> El {
            let mut trigger = crate::row().key("trigger").h(28.0).child(el().key("label").size(53.0, 16.0));
            if s.0 {
                trigger = trigger.child(el().key("badge").size(16.0, 16.0)
                    .transition(Transition::offset(0.0, 8.0).combined(Transition::opacity())));
            }
            if s.1 { trigger = trigger.animate_layout(); }
            col().key("stack").w_full().growth(GrowthSpec::default())
                .child(col().key("r0").w_full().child(trigger))
        }
        let badge = |rt: &Runtime<(bool, bool)>| rt.elements().into_iter()
            .find(|e| e.key.as_deref() == Some("badge")).map(|e| (e.opacity, e.exiting));
        for animate in [false, true] {
            let mut rt = Runtime::new((false, animate), view, (300.0, 100.0), Env::new(true, 2.0));
            rt.frame(0.0);
            for _ in 0..30 { rt.frame(1.0 / 120.0); }
            rt.update(|s, _| s.0 = true);
            rt.frame(1.0 / 120.0);
            let (op, _) = badge(&rt).unwrap();
            assert!(op < 0.5, "animate_layout {animate}: it enters on its transition: {op}");
            for _ in 0..120 { rt.frame(1.0 / 120.0); }
            rt.update(|s, _| s.0 = false);
            rt.frame(1.0 / 120.0);
            let (op, exiting) = badge(&rt).expect("it plays its exit");
            assert!(exiting && op > 0.5, "animate_layout {animate}: it exits on its transition: {op}");
        }
    }

    /// A long text in a scroll view walks only the lines its clip shows: the glyphs of lines
    /// far below aren't rasterized until scrolled to, while a tall span whose line box sits
    /// more than two base ems below the clip still paints the ascenders that reach inside.
    #[test]
    fn a_long_scrolled_text_paints_only_its_visible_lines() {
        use crate::{text, text_input, Span, TextEdit, TextStyle};
        struct Note { edit: TextEdit }
        fn view(s: &Note, _: &mut Cx<Note>) -> El {
            col().w_full().h_full().child(col().key("viewport").w_full().h(200.0).scroll(ScrollSpec { stick_bottom: false })
                .child(text_input(&s.edit, TextStyle::new(14.0, 400.0).lh(20.0)).key("note").w_full().on_edit(|s: &mut Note| &mut s.edit)))
        }
        let mut content = "plain lowercase words only\n".repeat(500);
        content += "QUIZ JOKE VEX";
        let mut rt = Runtime::new(Note { edit: TextEdit::new(content) }, view, (300.0, 300.0), Env::new(true, 2.0));
        rt.frame(0.0);
        let top = rt.text.atlas.glyph_entries();
        let glyphs = rt.stats.glyphs;
        assert!(glyphs > 0 && glyphs < 400, "about ten visible lines painted: {glyphs}");
        rt.wheel(150.0, 100.0, -1e5);
        rt.frame(1.0 / 60.0);
        assert!(rt.text.atlas.glyph_entries() > top, "the capitals on the last line rasterize only once shown");

        // Line 6 sits at y = 72..84 pt, 22 pt (> 2 * 10 pt) below the 50 pt clip, but its 120 pt
        // span's ascender reaches up into the clip and must still paint.
        fn span_view(_: &(), _: &mut Cx<()>) -> El {
            let s = "a\n".repeat(6) + "T";
            col().w_full().h_full().child(col().w_full().h(50.0).scroll(ScrollSpec { stick_bottom: false })
                .child(text(&s).font(TextStyle::new(10.0, 400.0).lh(12.0)).spans(vec![Span::new(12, 13).size(120.0)])))
        }
        let mut rt = Runtime::new((), span_view, (200.0, 200.0), Env::new(true, 2.0));
        rt.frame(0.0);
        assert_eq!(rt.stats.glyphs, 5, "4 visible 'a' lines plus the overhanging 'T' glyph");
        assert_eq!(rt.text.atlas.glyph_entries(), 2, "'a' and overhanging 'T' both rasterized");
    }

    /// The app clears its password as the card leaves; the exiting copy of the field must
    /// hold no plaintext from that frame on, yet paint the same dots and no placeholder.
    #[test]
    fn an_exiting_secure_field_paints_dots_without_its_secret() {
        use crate::{text_input, TextEdit, TextStyle};
        struct Card { pw: TextEdit, open: bool }
        fn view(s: &Card, _: &mut Cx<Card>) -> El {
            let mut root = col().key("root").size(200.0, 100.0);
            if s.open {
                root = root.child(text_input(&s.pw, TextStyle::new(14.0, 400.0)).single_line()
                    .placeholder("Enter your password", Color::WHITE).key("pw").size(200.0, 40.0)
                    .transition(Transition::blur_replace()).on_edit(|s: &mut Card| &mut s.pw));
            }
            root
        }
        let mut rt = Runtime::new(Card { pw: TextEdit::new("hunter2").secure(), open: true }, view, (200.0, 100.0), Env::new(true, 2.0));
        rt.frame(0.0);
        let painted = rt.instances.iter().filter(|i| i.params[1] == KIND_GLYPH).count();
        rt.update(|s, _| { s.pw.clear(); s.open = false; });
        assert!(rt.frame(1.0 / 60.0));
        let n = rt.nodes.iter().find(|n| n.key.as_deref() == Some("pw")).expect("exiting field stays drawn");
        assert!(rt.is_exiting(n.id));
        let edit = &n.input.as_ref().unwrap().edit;
        assert_eq!(edit.text, "•".repeat(7), "only the mask remains");
        assert!(!edit.has_history() && !edit.focused);
        assert_eq!(n.text.as_ref().unwrap().content, "•".repeat(7));
        // Exit blur adds balanced layer markers; compare the actual dots, not scope records.
        assert_eq!(rt.instances.iter().filter(|i| i.params[1] == KIND_GLYPH).count(), painted, "the same dots, no placeholder");
        assert!(!format!("{:?}", rt.elements()).contains("hunter2"));

        // A revealed secure field must also conceal to bullets on the very first exit frame,
        // leaving no plaintext in `input.edit`, `text.content`, `text_layout`, or `Debug`.
        let mut rt = Runtime::new(
            Card { pw: TextEdit::new("hunter2").secure().revealed(true), open: true },
            view,
            (200.0, 100.0),
            Env::new(true, 2.0),
        );
        rt.frame(0.0);
        let n_open = rt.nodes.iter().find(|n| n.key.as_deref() == Some("pw")).unwrap();
        assert_eq!(n_open.text.as_ref().unwrap().content, "hunter2");
        assert!(!format!("{:?}", n_open.input).contains("hunter2"));
        assert!(!format!("{:?}", n_open.text).contains("hunter2"));
        rt.update(|s, _| { s.pw.clear(); s.open = false; });
        assert!(rt.frame(1.0 / 60.0));
        let n = rt.nodes.iter().find(|n| n.key.as_deref() == Some("pw")).expect("exiting field stays drawn");
        assert!(rt.is_exiting(n.id));
        let edit = &n.input.as_ref().unwrap().edit;
        assert!(!edit.is_revealed(), "conceal resets revealed on exit");
        assert_eq!(edit.text, "•".repeat(7), "only the mask remains");
        assert_eq!(n.text.as_ref().unwrap().content, "•".repeat(7));
        assert_eq!(rt.instances.iter().filter(|i| i.params[1] == KIND_GLYPH).count(), painted, "exiting revealed field paints bullets");
        assert!(!format!("{:?}", rt.elements()).contains("hunter2"));
        assert!(!format!("{:?}", n.input).contains("hunter2"));
        assert!(!format!("{:?}", n.text).contains("hunter2"));
    }

    #[test]
    fn secure_revealed_display_does_not_survive_in_layout_or_raster_caches() {
        use crate::{text_input, TextEdit, TextStyle};
        fn view(s: &TextEdit, _: &mut Cx<TextEdit>) -> El {
            col().key("cache").cache().size(240.0, 100.0).child(
                text_input(s, TextStyle::new(16.0, 400.0)).key("secret")
                    .single_line().size(220.0, 40.0).selectable(true)
                    .text_case(crate::TextCase::Uppercase)
                    .on_edit(|s: &mut TextEdit| s),
            )
        }
        let mut rt = Runtime::new(TextEdit::new("sensitive fixture").secure().revealed(true), view,
            (240.0, 100.0), Env::new(true, 1.0));
        let cached = rt.text.cached_layout_count();
        rt.frame(0.0);
        let i = rt.nodes.iter().position(|n| n.key.as_deref() == Some("secret")).unwrap();
        let n = &rt.nodes[i];
        assert!(!rt.is_selectable(i), "selectable cannot opt secure input into text selection");
        assert_eq!(n.text.as_ref().unwrap().content, "sensitive fixture");
        assert!(n.text.as_ref().unwrap().source.is_none(), "case conversion is ignored on inputs");
        assert_eq!(Rc::strong_count(n.text_layout.as_ref().unwrap()), 1, "only the node owns the shaped run");
        assert_eq!(rt.text.cached_layout_count(), cached, "measurement and paint leave no cached run");
        assert!(rt.node_caches.is_empty(), "an unfocused secret also disables ancestor raster caches");
        assert!(rt.text.atlas.resident_caches.is_empty());
        let path = n.path.clone();
        assert!(!rt.selection_menu(&path, 10.0, 10.0), "no context-menu copy");
        rt.set_focus(Some(path), true);
        rt.frame(0.0);
        let layout = rt.focused_layout().unwrap();
        assert_eq!(Rc::strong_count(&layout), 1, "input hit testing also bypasses the layout cache");
        drop(layout);
        let mut tx = Tx::default();
        // Application key-up hooks may observe their own events, but Rune must not keep
        // typed secrets in the dev/event log when such a hook handles the release.
        let root = rt.nodes.iter().position(|n| n.key.as_deref() == Some("cache")).unwrap();
        rt.nodes[root].focus.key_up = Some(Rc::new(|_, _, _| true));
        rt.events.clear();
        assert!(rt.route_key_up(&KeyEvent { key: crate::Key::Text("private fixture".into()), mods: Default::default() }, &mut tx));
        assert!(rt.events.iter().any(|e| e == "key-up [redacted]"));
        assert!(!format!("{:?}", rt.events).contains("private fixture"));
        rt.update(|s, _| s.set_revealed(false));
        rt.frame(0.0);
        assert_eq!(rt.text.cached_layout_count(), cached);
        rt.update(|s, _| s.clear());
        rt.frame(0.0);
        let n = rt.nodes.iter().find(|n| n.key.as_deref() == Some("secret")).unwrap();
        assert!(rt.state.text.is_empty() && n.input.as_ref().unwrap().edit.text.is_empty());
        assert!(n.text.as_ref().unwrap().content.is_empty(), "clearing without exit wipes the display");
        assert_eq!(rt.text.cached_layout_count(), cached);
    }

    #[test]
    fn revealed_secure_input_matches_plain_layout_and_keeps_protections() {
        use crate::{text_input, TextEdit, TextStyle};
        struct Form {
            plain: TextEdit,
            secret: TextEdit,
        }
        fn view(s: &Form, _: &mut Cx<Form>) -> El {
            let style = TextStyle::new(16.0, 400.0);
            col().size(240.0, 100.0)
                .child(text_input(&s.plain, style).single_line().key("plain").size(200.0, 40.0).on_edit(|s: &mut Form| &mut s.plain))
                .child(text_input(&s.secret, style).single_line().key("secret").size(200.0, 40.0).on_edit(|s: &mut Form| &mut s.secret))
        }
        let mut plain = TextEdit::new("hé🙂 world");
        plain.set_selection(1, 7);
        let mut secret = TextEdit::new("hé🙂 world").secure().revealed(true);
        secret.set_selection(1, 7);
        secret.focused = true;
        let mut rt = Runtime::new(Form { plain, secret }, view, (240.0, 100.0), Env::new(true, 1.0));
        rt.frame(0.0);

        assert!(rt.state.secret.is_secure() && rt.state.secret.is_revealed());
        assert!(rt.has_secure_focus());
        assert!(rt.ime_area.is_none());
        let els = rt.elements();
        let plain_el = els.iter().find(|e| e.key.as_deref() == Some("plain")).unwrap();
        let secret_el = els.iter().find(|e| e.key.as_deref() == Some("secret")).unwrap();
        assert_eq!(plain_el.text.as_deref(), Some("hé🙂 world"));
        assert!(secret_el.text.is_none(), "accessibility text remains None when revealed");

        let np = rt.nodes.iter().find(|n| n.key.as_deref() == Some("plain")).unwrap();
        let ns = rt.nodes.iter().find(|n| n.key.as_deref() == Some("secret")).unwrap();
        assert_eq!(ns.text.as_ref().unwrap().content, "hé🙂 world");
        assert!(!format!("{:?}", ns.input).contains("hé🙂"));
        assert!(!format!("{:?}", ns.text).contains("hé🙂"));
        let lp = np.text_layout.as_ref().unwrap().clone();
        let ls = ns.text_layout.as_ref().unwrap().clone();
        assert_eq!(lp.full_width(), ls.full_width(), "revealed layout width matches plain text");
        let secret_caret = secret_el.caret.expect("focused revealed field has caret");
        let secret_sel: Vec<[f32; 4]> = rt.instances.iter()
            .filter(|i| i.params[1] == KIND_RECT && i.rect[1] >= secret_el.rect[1] && i.rect[1] < secret_el.rect[1] + secret_el.rect[3])
            .map(|i| [i.rect[0] - secret_el.rect[0], i.rect[1] - secret_el.rect[1], i.rect[2], i.rect[3]])
            .collect();
        assert!(!secret_sel.is_empty());

        // Focus the plain field with the same selection and verify caret and selection rects match.
        rt.update(|s, _| { s.secret.focused = false; s.plain.focused = true; });
        rt.frame(0.0);
        let plain_el = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("plain")).unwrap();
        let plain_caret = plain_el.caret.expect("focused plain field has caret");
        assert_eq!([plain_caret[0] - plain_el.rect[0], plain_caret[1] - plain_el.rect[1], plain_caret[2], plain_caret[3]],
            [secret_caret[0] - secret_el.rect[0], secret_caret[1] - secret_el.rect[1], secret_caret[2], secret_caret[3]]);
        let plain_sel: Vec<[f32; 4]> = rt.instances.iter()
            .filter(|i| i.params[1] == KIND_RECT && i.rect[1] >= plain_el.rect[1] && i.rect[1] < plain_el.rect[1] + plain_el.rect[3])
            .map(|i| [i.rect[0] - plain_el.rect[0], i.rect[1] - plain_el.rect[1], i.rect[2], i.rect[3]])
            .collect();
        assert_eq!(plain_sel, secret_sel);

        // Refocus secret field: copy, cut, undo and IME must do nothing, and toggling revealed re-measures.
        rt.update(|s, _| { s.plain.focused = false; s.secret.focused = true; });
        rt.frame(0.0);
        for ch in ['c', 'x'] {
            rt.key(KeyEvent { key: crate::Key::Command(ch), mods: crate::Mods { cmd: true, ..Default::default() } });
            rt.frame(0.0);
            assert!(rt.copy_request.is_none(), "⌘{} on revealed field must never write to clipboard", ch.to_uppercase());
            assert_eq!(rt.state.secret.text, "hé🙂 world");
        }
        rt.ime(Ime::Preedit("comp".into(), Some((0, 4))));
        rt.ime(Ime::Commit("commit".into()));
        rt.frame(0.0);
        assert_eq!(rt.state.secret.text, "hé🙂 world");
        rt.key(KeyEvent { key: crate::Key::Text("X".into()), mods: Default::default() });
        rt.frame(0.0);
        assert_eq!(rt.state.secret.text, "hX world");
        rt.key(KeyEvent { key: crate::Key::Command('z'), mods: crate::Mods { cmd: true, ..Default::default() } });
        rt.frame(0.0);
        assert_eq!(rt.state.secret.text, "hX world", "no undo on revealed field");

        let revealed_w = rt.nodes.iter().find(|n| n.key.as_deref() == Some("secret")).unwrap().text_layout.as_ref().unwrap().full_width();
        rt.update(|s, _| { s.secret.set_selection(1, 4); s.secret.set_revealed(false); });
        rt.frame(0.0);
        assert_eq!(rt.state.secret.range(), (1, 4), "toggling revealed keeps selection");
        let ns = rt.nodes.iter().find(|n| n.key.as_deref() == Some("secret")).unwrap();
        assert_eq!(ns.text.as_ref().unwrap().content, "••••••••");
        let hidden_w = ns.text_layout.as_ref().unwrap().full_width();
        assert_ne!(revealed_w, hidden_w, "toggling revealed re-layouts the text");
    }

    #[test]
    fn multiline_secure_input_hides_length_and_lines_wraps_anywhere_and_conceals_on_exit() {
        use crate::{text_input, TextEdit, TextStyle};
        struct State {
            secret: TextEdit,
            open: bool,
        }
        fn view(s: &State, _: &mut Cx<State>) -> El {
            let mut root = col().size(240.0, 200.0);
            if s.open {
                root = root.child(
                    text_input(&s.secret, TextStyle::new(14.0, 400.0))
                        .secure()
                        .key("secret")
                        .w(180.0)
                        .on_edit(|s: &mut State| &mut s.secret),
                );
            }
            root
        }

        let short_secret = "short-key!";
        let long_secret = (0..50)
            .map(|i| format!("FAKEPEMLINE{i:02}AAAAB3NzaC1yc2EAAAADAQABAAABAQDfakebase64payloadblock0123456789ABCDEFGH=="))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(long_secret.len() >= 4096);

        let mut short_edit = TextEdit::new(short_secret);
        short_edit.focused = true;
        let mut rt_short = Runtime::new(State { secret: short_edit, open: true }, view, (240.0, 200.0), Env::new(true, 1.0));
        rt_short.frame(0.0);

        let mut long_edit = TextEdit::new(long_secret.clone());
        long_edit.focused = true;
        let mut rt_long = Runtime::new(State { secret: long_edit, open: true }, view, (240.0, 200.0), Env::new(true, 1.0));
        rt_long.frame(0.0);

        let el_short = rt_short.elements().into_iter().find(|e| e.key.as_deref() == Some("secret")).unwrap();
        let el_long = rt_long.elements().into_iter().find(|e| e.key.as_deref() == Some("secret")).unwrap();
        assert!(el_short.text.is_none() && el_long.text.is_none());
        assert_eq!(el_short.rect, el_long.rect, "hidden multi-line rect must not leak content length or line count");
        assert_eq!(el_short.caret, el_long.caret, "hidden multi-line caret rect must be identical");

        let n_short = rt_short.nodes.iter().find(|n| n.key.as_deref() == Some("secret")).unwrap();
        let n_long = rt_long.nodes.iter().find(|n| n.key.as_deref() == Some("secret")).unwrap();
        assert_eq!(n_short.text.as_ref().unwrap().content, crate::edit::MULTILINE_SECURE_MASK);
        assert_eq!(n_long.text.as_ref().unwrap().content, crate::edit::MULTILINE_SECURE_MASK);
        let l_short = n_short.text_layout.as_ref().unwrap();
        let l_long = n_long.text_layout.as_ref().unwrap();
        assert_eq!(l_short.len(), 1);
        assert_eq!(l_long.len(), 1);
        assert_eq!(l_short.full_width(), l_long.full_width());
        let glyphs_short = rt_short.instances.iter().filter(|i| i.params[1] == KIND_GLYPH).count();
        let glyphs_long = rt_long.instances.iter().filter(|i| i.params[1] == KIND_GLYPH).count();
        assert_eq!(glyphs_short, 12);
        assert_eq!(glyphs_long, 12);

        // Clicking near the left edge of the hidden mask pins the caret to text.len(), never leaking an interior byte index.
        rt_long.pointer_down(el_long.rect[0] + 4.0, el_long.rect[1] + 4.0);
        rt_long.pointer_up(el_long.rect[0] + 4.0, el_long.rect[1] + 4.0);
        rt_long.frame(0.0);
        assert_eq!(rt_long.state.secret.focus, long_secret.len());

        // Revealing wraps unbroken lines at character boundaries within 180pt width.
        rt_long.update(|s, _| s.secret.set_revealed(true));
        rt_long.frame(0.0);
        let n_rev = rt_long.nodes.iter().find(|n| n.key.as_deref() == Some("secret")).unwrap();
        let l_rev = n_rev.text_layout.as_ref().unwrap();
        assert!(l_rev.len() > 100, "50 long unbroken lines must wrap into >100 visual lines");
        assert!(l_rev.full_width() <= 180.5, "wrapped unbroken lines must fit within field width (got {})", l_rev.full_width());

        // Exiting while revealed conceals back to the fixed multi-line mask on the first exit frame.
        rt_long.update(|s, _| s.open = false);
        rt_long.frame(0.0);
        let exiting = rt_long.nodes.iter().find(|n| n.key.as_deref() == Some("secret")).expect("exiting node present");
        let exit_edit = &exiting.input.as_ref().unwrap().edit;
        assert!(!exit_edit.is_revealed());
        assert_eq!(exit_edit.text, crate::edit::MULTILINE_SECURE_MASK);
        assert_eq!(exiting.text.as_ref().unwrap().content, crate::edit::MULTILINE_SECURE_MASK);
        assert_eq!(exiting.text_layout.as_ref().unwrap().len(), 1);
        assert!(!format!("{:?}", exiting.input).contains("FAKEPEMLINE"));
        assert!(!format!("{:?}", exiting.text).contains("FAKEPEMLINE"));
    }

    #[test]
    fn secure_multiline_conceal_snaps_geometry_scroll_and_bypasses_caches() {
        fn view(s: &crate::TextEdit, _: &mut Cx<crate::TextEdit>) -> El {
            col().cache().w(120.0).child(crate::auto_grow(
                crate::text_input(s, crate::TextStyle::new(14.0, 400.0))
                    .w_full().key("secret").on_edit(|s: &mut crate::TextEdit| s), 1, 3))
        }
        let mut edit = crate::TextEdit::new("fictional line\n".repeat(60)).secure().revealed(true);
        edit.focused = true;
        let mut rt = Runtime::new(edit, view, (180.0, 200.0), Env::new(true, 1.0));
        rt.frame(0.0);
        assert_eq!(rt.text.cached_layout_count(), 0);
        assert!(rt.node_caches.is_empty());
        let scroll_id = rt.nodes.iter().find(|n| n.scroll.is_some()).unwrap().id;
        assert!(rt.scrolls[&scroll_id].max > 100.0);
        rt.scrolls.get_mut(&scroll_id).unwrap().offset = 300.0;
        rt.update(|s, _| s.set_revealed(false)); // No tx.instant(): conceal must do this itself.
        rt.frame(0.0);
        let st = &rt.scrolls[&scroll_id];
        assert_eq!((st.offset, st.max), (0.0, 0.0));
        assert!(st.glide.is_none());
        let elements = rt.elements();
        for e in &elements {
            assert_eq!(e.rect[3], e.target[3], "concealed height must not spring: {}", e.path);
        }
        let secret = rt.nodes.iter().find(|n| n.key.as_deref() == Some("secret")).unwrap();
        assert_eq!(secret.text_layout.as_ref().unwrap().len(), 1);
        assert_eq!(Rc::strong_count(secret.text_layout.as_ref().unwrap()), 1);
        assert_eq!(rt.text.cached_layout_count(), 0);
        assert!(rt.ime_area.is_none());
        let info = elements.iter().find(|e| e.key.as_deref() == Some("secret")).unwrap();
        assert!(info.text.is_none() && info.selected.is_none());

        // Even a narrow hidden field keeps one mask line, including after wrap_anywhere.
        fn narrow(s: &crate::TextEdit, _: &mut Cx<crate::TextEdit>) -> El {
            crate::text_input(s, crate::TextStyle::new(14.0, 400.0)).wrap_anywhere().w(12.0)
        }
        let mut narrow = Runtime::new(rt.state.clone(), narrow, (50.0, 100.0), Env::new(true, 1.0));
        narrow.frame(0.0);
        assert_eq!(narrow.nodes[0].text_layout.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn wrap_anywhere_wraps_unbroken_tokens_keeps_word_breaks_and_lowers_min_content_width() {
        use crate::{rich, row, text, Span};
        const LONG_BASE64: &str = "AAAAB3NzaC1yc2EAAAADAQABAAABAQDfakebase64publickeymaterialwithoutspaces0123456789ABCDEF==";
        const WORDS: &str = "alpha beta gamma delta epsilon zeta eta theta";

        let mut explicit = text("token");
        explicit.style.flex_shrink = 0.25;
        assert_eq!(explicit.wrap_anywhere().style.flex_shrink, 0.25);
        assert_eq!(text("token").wrap_anywhere().style.flex_shrink, 0.0);

        fn view(_: &(), _: &mut Cx<()>) -> El {
            col()
                .size(400.0, 500.0)
                // Default .wrap() vs .wrap_anywhere() in a narrow 160pt box.
                .child(col().w(160.0).child(text(LONG_BASE64).wrap().key("default_wrap")))
                .child(col().w(160.0).child(text(LONG_BASE64).wrap_anywhere().key("anywhere_wrap")))
                // In a flex row, wrap_anywhere lowers min-content width so the flex parent can shrink it.
                .child(row().w(160.0).child(text(LONG_BASE64).wrap_anywhere().shrink().key("row_anywhere")))
                // Normal words with spaces in the same 160pt width.
                .child(col().w(160.0).child(text(WORDS).wrap().key("words_default")))
                .child(col().w(160.0).child(text(WORDS).wrap_anywhere().key("words_anywhere")))
                // Rich text with wrap_anywhere.
                .child(col().w(160.0).child(rich(LONG_BASE64, vec![Span::new(0, 16).bold()]).wrap_anywhere().key("rich_anywhere")))
        }

        let mut rt = Runtime::new((), view, (400.0, 500.0), Env::new(true, 1.0));
        rt.frame(0.0);

        let node = |k: &str| rt.nodes.iter().find(|n| n.key.as_deref() == Some(k)).unwrap();
        let el = |k: &str| rt.elements().into_iter().find(|e| e.key.as_deref() == Some(k)).unwrap();

        // Default .wrap() does not break inside an unbroken token and keeps its full min-content width.
        let def_layout = node("default_wrap").text_layout.as_ref().unwrap();
        assert_eq!(def_layout.len(), 1, "default wrap leaves unbroken base64 on 1 line");
        assert!(def_layout.full_width() > 400.0, "default wrap keeps full unbroken token width");

        // .wrap_anywhere() wraps inside the 160pt box and shrinks inside a 160pt flex row with no glyph past the right edge.
        let any_el = el("anywhere_wrap");
        let any_node = node("anywhere_wrap");
        let any_layout = any_node.text_layout.as_ref().unwrap();
        assert!(any_el.rect[2] <= 160.5, "wrap_anywhere fits 160pt box (got {})", any_el.rect[2]);
        assert!(el("row_anywhere").rect[2] <= 160.5, "wrap_anywhere shrinks inside 160pt flex row (got {})", el("row_anywhere").rect[2]);
        assert!(any_layout.len() >= 4, "long base64 wraps across multiple lines (got {})", any_layout.len());
        assert!(any_layout.full_width() <= 160.5, "no line exceeds the 160pt box (got {})", any_layout.full_width());
        for line in any_layout.lines() {
            assert!(
                line.metrics().advance - line.metrics().hanging_advance <= 160.5,
                "line advance {} exceeds 160pt right edge",
                line.metrics().advance
            );
        }

        // Min-content width (max_width = Some(0.0)) shrinks from the full token width to a single character's width.
        let min_def = rt.text.layout_for_spec(node("default_wrap").text.as_ref().unwrap(), 1.0, Some(0.0), parley::Alignment::Start);
        let min_any = rt.text.layout_for_spec(any_node.text.as_ref().unwrap(), 1.0, Some(0.0), parley::Alignment::Start);
        assert!(min_def.full_width() > 400.0);
        assert!(min_any.full_width() < 20.0, "wrap_anywhere min-content width is a single grapheme (got {})", min_any.full_width());

        // Normal words that fit on a line still break at spaces identically to .wrap().
        let w_def = node("words_default").text_layout.as_ref().unwrap();
        let w_any = node("words_anywhere").text_layout.as_ref().unwrap();
        let ranges_def: Vec<_> = w_def.lines().map(|l| l.text_range()).collect();
        let ranges_any: Vec<_> = w_any.lines().map(|l| l.text_range()).collect();
        assert_eq!(ranges_def, ranges_any, "normal words break at spaces identically under wrap_anywhere");

        // Rich text also supports wrap_anywhere.
        let rich_layout = node("rich_anywhere").text_layout.as_ref().unwrap();
        assert!(rich_layout.len() >= 4 && rich_layout.full_width() <= 160.5);
    }
}

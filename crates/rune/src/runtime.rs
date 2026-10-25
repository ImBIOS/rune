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
use parley::editing::{Cursor, Selection};
use parley::layout::Affinity;
use rune_motion::{
    Anim, Corners, LayoutAnim, Mount, NodeId, NodeSpec, Phase as PresencePhase, Prop, PropClass, Rect as MRect, Scene,
    Spring, Transition, prop, snap::grid,
};
use taffy::prelude::*;
use taffy::TraversePartialTree;

use crate::color::{Color, over, srgb_encode};
use crate::edit::{EditOutcome, Ime, KeyEvent};
use crate::el::{Align, Drag, DragEndFn, DragFn, DragOutFn, El, GrowthSpec, InputSpec, Phase, PointerPress, PressFn, SecondaryFn, ScrollSpec, Shell, TextSpec, Tx, Visual};
use crate::growth::{GrowTrack, SUBSTEP};
// Half-open, like overlays, NSPointInRect and PtInRect: a shared edge belongs to one side.
use crate::overlay_options::contains;
use crate::flight::{TextFlight, transformed};
use crate::render::{Instance, KIND_DASHED, KIND_GLYPH, KIND_GRADIENT, KIND_IMAGE, KIND_INSET, KIND_RADIAL, KIND_RECT, KIND_SEGMENT, KIND_SHADOW, KIND_TRIANGLE, NO_CLIP};
use crate::shape::{LineCap, Op, Shape};
use crate::text::{Deco, GlyphQuad, Ink, TextSystem, layout_height};
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
pub(crate) use scroll_x::{SHIFT_TURNS_WHEEL, wheel_delta};
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
        _ => "custom",
    }
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
    on_secondary_press: Option<SecondaryFn>,
    on_hover: Option<PressFn>,
    on_hover_change: Option<crate::el::WindowFocusFn>,
    on_pointer_move: Option<SecondaryFn>,
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
    on_layout: Option<crate::el::LayoutFn>,
    on_screen_layout: Option<crate::el::LayoutFn>,
    scroll_edges: crate::el::ScrollEdges,
    growth: Option<GrowthSpec>,
    streaming: bool,
    input: Option<Box<InputSpec>>,
    cursor: Option<crate::el::Cursor>,
    window_drag: bool,
    /// `El::selection_group`.
    selection_group: bool,
    press_scale: Option<f32>,
    /// `El::scale`: a fixed factor on the element's own scale.
    static_scale: f32,
    /// `El::time_scale`: this element's own factor; the scene multiplies ancestors' in.
    time_scale: f32,
    /// The image ratio this picture's layout took, when it sizes one side from its pixels.
    pixel_aspect: Option<f32>,
    /// `El::aspect_fit`: the ratio and the element's own style, which the fitted size replaces.
    aspect_fit: Option<Box<(f32, Style)>>,
    material: Option<crate::SystemMaterial>,
    /// Inside a growth row: layout snaps (the row's height carries the motion).
    in_row: bool,
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
    #[cfg(debug_assertions)]
    scroll_source: Option<&'static std::panic::Location<'static>>,
}

enum Action {
    WindowFocus(bool),
    Physical(crate::PhysicalInput, Option<physical_dispatch::Fallback>),
    FocusControl(String),
    /// The pointer entered a `focus_highlight` row (`Some`) or left every such row.
    HoverFocus(Option<String>),
    Dismiss(String),
    PointerMove(String, PointerPress),
    Hover(String),
    HoverChange(String, bool),
    AtBottom(String, bool),
    Layout(String, [f32; 4]),
    ScreenLayout(String, [f32; 4]),
    Press(String),
    DoublePress(String),
    Link(String, usize),
    SecondaryPress(String, PointerPress),
    Drag(String, Drag),
    DragStart(String),
    DragEnd(String, crate::DragOutcome),
    /// A file drag from another app moved, left, or dropped (`drop_dispatch`).
    Files(drop_dispatch::FileEvent),
    Key { ev: KeyEvent, repeat: bool },
    Ime(Ime),
    Paste(zeroize::Zeroizing<String>, Option<Option<String>>),
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
    rounded: Option<([f32; 4], f32)>,
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
}

/// How a subtree's scale is changing. Text under a still scale rasterizes at its scaled size.
/// Under a moving one it samples its rest-size glyphs, so a spring doesn't rasterize every glyph
/// at every frame's size into the glyph atlas, which never evicts.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
enum ScaleMotion {
    #[default]
    Still,
    /// Bound to a live value, which an app may publish every frame: rasterize at rounded sizes.
    Live,
    Moving,
}

impl ScaleMotion {
    /// The scale text under `sc` rasterizes at. It draws at `sc`, filtered, when they differ.
    fn raster(self, sc: f32) -> f32 {
        let k = match self {
            ScaleMotion::Still => sc,
            ScaleMotion::Live => (sc * 32.0).round() / 32.0,
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
            scale_motion: ScaleMotion::Still }
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
    /// Velocity of a glide to the bottom (`Tx::scroll_to_bottom`) while one runs.
    glide: Option<f32>,
    /// At-bottom state last sent to `El::on_at_bottom`.
    at_bottom: Option<bool>,
    /// Whether the last frame drew the content's bottom on the viewport's (see `stuck_offset`).
    held: bool,
    /// The horizontal axis of `El::scroll_x` and `El::scroll_xy`; all zero on a vertical one.
    x: AxisScroll,
}

/// A scroll viewport's horizontal offset, momentum velocity and limit (points).
#[derive(Clone, Copy, Debug, Default)]
struct AxisScroll {
    offset: f64,
    v: f32,
    max: f64,
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
    width: f32,
    /// Measure at the landing width immediately; defer height targets until it is stable.
    width_hold_until: f64,
    width_shrink_until: f64,
    pending_heights: HashMap<NodeId, f32>,
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
    deco_scratch: Vec<Deco>,
    inh_scratch: Vec<Inh>,
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
    /// Some picture sizes a side from its image's ratio (`pixel_aspect`).
    has_pixel_aspect: bool,
    /// Pictures whose image changed shape since the last commit, which lays them out at once.
    aspect_snap: Vec<NodeId>,
    /// Drawn rect per node (points, root space), for tools and hit tests.
    drawn: Vec<[f32; 4]>,
    /// The scale each node's text drew at (its own and its ancestors'), for text hit tests.
    drawn_scale: Vec<f32>,
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
    /// The window's content top-left in global screen points, as its host last saw it.
    window_origin: (f32, f32),
    /// Commits in a row caused only by layout feedback (see `layout_read`).
    layout_echoes: u32,
    /// An edit ran this frame: keep the caret in its scroll viewport after the commit.
    caret_follow: bool,
    links: links::LinkState,
    /// The selection in `El::selectable` text.
    text_sel: selectable::TextSelection,
    /// `El::help` tooltips.
    help: help::HelpState,
    /// The shell cancels native composition before enabling the new text focus.
    pub(crate) ime_reset: bool,
    /// Text the focused field asked to copy (the shell writes it to the clipboard).
    pub copy_request: Option<crate::ClipboardWrite>,
    // Outer None: no read; inner None: app-level paste rather than a focused editor.
    pub(crate) paste_request: Option<Option<String>>,
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
            has_pixel_aspect: false,
            aspect_snap: Vec::new(),
            drawn: Vec::new(),
            drawn_scale: Vec::new(),
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
            virtual_inputs: HashMap::new(),
            reveals: HashMap::new(),
            flights: Vec::new(),
            row_dy: Vec::new(),
            row_h: Vec::new(),
            row_mask: Vec::new(),
            grow_acc: 0.0,
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
            file_drag: Default::default(),
            ime_area: None,
            caret: None,
            caret_watch: false,
            layout_seen: HashMap::new(),
            layout_fired: HashMap::new(),
            screen_fired: HashMap::new(),
            window_origin: (0.0, 0.0),
            layout_echoes: 0,
            caret_follow: false,
            links: Default::default(),
            text_sel: Default::default(),
            help: Default::default(),
            help_delay: 0.5,
            ime_reset: false,
            copy_request: None,
            paste_request: None,
            clear_secret_request: false,
            clipboard: None,
            shell_request: None,
            minimize_request: false,
            popups: Default::default(),
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
            self.queue.push(Action::WindowFocus(focused));
            if !focused { self.keys_lost(); }
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

    /// Keyboard input, a focus change or a file drag step is queued for the next frame.
    pub(crate) fn keys_queued(&self) -> bool {
        self.queue.iter().any(|a| matches!(a, Action::Physical(..) | Action::Key { .. } | Action::WindowFocus(_) | Action::Files(_)))
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
        let stale = changed || one.frame_work() || one.window_frame.is_some();
        self.latch_requests(&mut one);
        merge(&mut self.pending, one);
        self.dirty |= changed;
        stale
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
        self.shell_request = self.shell_request.max(tx.shell.take());
        self.minimize_request |= std::mem::take(&mut tx.minimize);
        if std::mem::take(&mut tx.clear_secret) {
            if self.copy_request.as_ref().is_some_and(crate::ClipboardWrite::is_secret) { self.copy_request = None; }
            self.clear_secret_request = true;
        }
        if let Some(copy) = tx.copy.take() { self.copy_request = Some(copy); }
        if std::mem::take(&mut tx.paste) { self.paste_request = Some(self.paste_target()); }
        self.popups.extend(tx.popups.drain(..).filter_map(|call| call.take()));
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
        self.latch_requests(&mut one);
        let stale = changed || one.frame_work() || patched;
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
        Tx { trace: self.tracing().then(crate::el::TraceCalls::default), ..Tx::default() }
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
        // A coarse headless step still polls at every timer deadline, independent of FPS.
        // Input queued for the requested frame stays at that frame's timestamp. The catch-up
        // is the earliest part of the step.
        if self.wall_start.is_none()
            && self.logical_wake_deadline().is_some_and(|deadline| deadline > self.clock && deadline < self.clock + elapsed)
        {
            let target = self.clock + elapsed;
            let input = std::mem::take(&mut self.queue);
            let mut catch_up = catch_up;
            while let Some(deadline) = self.logical_wake_deadline()
                && deadline > self.clock && deadline < target
            {
                let step = deadline - self.clock;
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
        // Hosts take a drag-out inside the move that asked for it; by now nothing will.
        self.drop_drag_out_request();
        let ended: Vec<_> = self.drag_ends.borrow_mut().drain(..).collect();
        let queue = order_drag_ends(std::mem::take(&mut self.queue), ended, &mut self.drags_open);
        let queue = self.hold_files(queue, ran || self.dirty);
        for action in queue {
            // State may have requested focus on a newly mounted field. Resolve the
            // new tree before routing this frame's text, not after it has gone to
            // the previous field. The same boundary covers an earlier handler, and lets a
            // file drag find only what is mounted and reachable now (a covered window builds
            // nothing until this).
            if (ran || self.dirty) && matches!(&action, Action::Key { .. } | Action::Physical(..) | Action::Ime(_) | Action::Paste(..) | Action::Files(_)) {
                for (source, target) in std::mem::take(&mut tx.text_flights) {
                    self.capture_text_flight(source, target);
                }
                if self.settle { tx.settle(); }
                self.settled |= tx.settle;
                self.commit(&tx);
                ran = false;
            }
            if matches!(action, Action::Layout(..) | Action::ScreenLayout(..)) {
                ran_layout |= self.run_action(action, &mut tx);
            } else {
                ran |= self.run_action(action, &mut tx);
            }
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
        let scroll_to = std::mem::take(&mut tx.scroll_to);
        let scroll_bottom = std::mem::take(&mut tx.scroll_bottom);
        let scroll_pages = std::mem::take(&mut tx.scroll_pages);
        let scroll_x = std::mem::take(&mut tx.scroll_x);
        if self.caret_watch && self.caret_moved(None) { self.dirty = true; }
        let read_moved = !self.layout_seen.is_empty() && self.layout_feedback_open() && self.layout_moved(None, None);
        if self.has_pixel_aspect && self.pixel_aspect_changed() { self.relayout = true; }
        let other = ran || self.dirty || self.relayout || self.settle || self.virtual_window_changed();
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
            self.commit(&tx);
        }
        let follow = std::mem::take(&mut self.caret_follow);
        if !scroll_to.is_empty() || follow { self.step_scroll(0.0); }
        if follow { self.follow_caret(); }
        for key in scroll_to { self.reveal_row(&key); }
        if !scroll_bottom.is_empty() { self.scroll_to_bottom(scroll_bottom); }
        if !scroll_pages.is_empty() { self.scroll_pages(scroll_pages); }
        if !scroll_x.is_empty() { self.scroll_to_x(scroll_x); }
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
        self.step_growth(step as f32);
        self.step_scroll(step as f32);
    }

    fn step_motion(&mut self, dt: f64) -> bool {
        let step = dt * self.time_scale as f64;
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
        self.report_at_bottom();
        self.report_layout();
        animating |= source(self.reveals_active(), AnimSource::REVEAL);
        self.build();
        self.images.end_frame();
        animating |= source(self.loops_active, AnimSource::LOOP);
        animating |= source(self.static_scale_settling, AnimSource::SCALE_SETTLE);
        animating |= source(self.videos_active, AnimSource::VIDEO);
        animating |= source(self.draw_text_flights(), AnimSource::TEXT_FLIGHT);
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
        let mut remap = vec![None; old.len()];
        self.node_index.clear();
        for (old_idx, mut n) in old.into_iter().enumerate() {
            if !self.scene.contains(n.id) {
                self.reveals.remove(&n.id);
                self.scrolls.remove(&n.id);
                self.loops.remove(&n.id);
                continue;
            }
            n.parent = n.parent.and_then(|p| remap[p]);
            let idx = self.nodes.len();
            remap[old_idx] = Some(idx);
            self.node_index.insert(n.id, idx);
            self.has_dissolve |= n.vis.blend == crate::render::Blend::PlusLighter;
            self.nodes.push(n);
        }
        self.virtuals.retain(|path, _| self.nodes.iter().any(|n| n.path == *path));
        self.refresh_modal_index();
        self.flying.retain(|(id, _)| self.node_index.contains_key(id));
        if !self.layer_paint_order.is_empty() || !self.flying.is_empty() { self.update_layer_paint_order(); }
        // A removed element's live sources (a `Pixels` stream) stop waking this window.
        if !self.live_subscriptions.is_empty() {
            let dirty = self.live_dirty;
            self.subscribe_live();
            self.live_dirty &= dirty;
        }
    }

    /// Subtree (preorder) paint order, then each flying element's subtree on top.
    fn update_layer_paint_order(&mut self) {
        let n = self.nodes.len();
        let mut first = vec![n; n + 1];
        let mut next = vec![n; n];
        for i in (0..n).rev() {
            let parent = self.nodes[i].parent.map_or(0, |p| p + 1);
            next[i] = first[parent]; first[parent] = i;
        }
        let mut lifted: Vec<usize> = self.flying.iter().filter_map(|(id, _)| self.node_index.get(id).copied()).collect();
        lifted.sort_unstable();
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
        let clip = n.parent.and_then(|p| self.inh_scratch.get(p)).map_or(NO_CLIP, |h| h.clip).map(|v| v / scale);
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
            let actual_clip = n.parent.and_then(|p| self.inh_scratch.get(p)).map_or(NO_CLIP, |h| h.clip).map(|v| v / s);
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
                return self.dispatch_physical(event, fallback, tx);
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
            Action::Press(path) => {
                let Some(f) = self.node(&path).and_then(|n| n.on_press.clone()) else { return false };
                self.events.push(format!("press {path}"));
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
            Action::Drag(path, mut d) => {
                let Some(f) = self.node(&path).and_then(|n| n.on_drag.clone()) else { return false };
                if d.phase == Phase::Move {
                    one.instant = true;
                }
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
                self.route_key(&ev, repeat, &mut one)
            }
            Action::Ime(ev) => self.with_focused(|e| e.ime(&ev), &mut one),
            Action::Paste(s, target) => {
                if target.is_some_and(|target| target != self.paste_target()) { return false; }
                if self.with_focused(|e| e.paste(&s), &mut one) {
                    self.events.push("paste field".into());
                    true
                } else if let Some(f) = self.app_paste_handler() {
                    f(handler_state(&mut self.state), &s, &mut one);
                    self.events.push("paste window".into());
                    true
                } else { false }
            }
            Action::Focus(path, x, y) => {
                let Some(access) = self.node(&path).and_then(|n| n.input.as_ref()?.access.clone()) else { return false };
                let layout = self.node(&path).and_then(|n| n.text_layout.clone());
                let s = self.env.scale;
                let scroll = self.input_scroll.get(&path).copied().unwrap_or(0.0);
                self.set_focus(Some(path), false);
                let e = access(handler_state(&mut self.state));
                e.break_history_group();
                if let Some(l) = layout {
                    let x = x + scroll;
                    let i = e.model_index(Cursor::from_point(&l, x * s, y * s).index());
                    e.anchor = i;
                    e.focus = i;
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
        merge(tx, one);
        ran
    }

    /// Offer a logical key that already passed `held_gate` to the app, the focused
    /// input's handler, focus traversal and the focused editor, in that order.
    fn route_key(&mut self, ev: &KeyEvent, repeat: bool, one: &mut Tx) -> bool {
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
        if self.default_button_key(ev, one) { return true; }
        if app_first && let Some(f) = self.on_key && app_call!(f, &mut self.state, ev, one) { return true; }
        if let Some(f) = self.focused_input().and_then(|input| input.key.clone())
            && f(handler_state(&mut self.state), ev, one)
        {
            return true;
        }
        if self.focus_key(ev, shown, one) { return true; }
        let input_handles = match &ev.key {
            crate::edit::Key::Function(_) | crate::edit::Key::Tab | crate::edit::Key::PageUp | crate::edit::Key::PageDown => false,
            crate::edit::Key::Command(c) => matches!(c, 'a' | 'c' | 'x' | 'z'),
            _ => !ev.is_command_chord() || ev.control_binding().is_some(),
        };
        if input_handles {
            let layout = self.focused_layout();
            if self.with_focused(|e| e.key(ev, layout.as_deref()), one) { return true; }
        }
        if !app_first && let Some(f) = self.on_key && app_call!(f, &mut self.state, ev, one) {
            self.events.push(format!("key {:?}", ev.key));
            return true;
        }
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
        let (spec, w) = (n.text.clone()?, n.target[2] - n.target[0] - n.inset[0] - n.inset[2]);
        let text = zeroize::Zeroizing::new(n.input.as_ref()?.edit.display().0);
        Some(self.text.layout_aligned(&text, &spec.spans, spec.style, s, spec.wrap.then_some(w * s), line_alignment(&spec)))
    }

    fn with_focused(&mut self, f: impl FnOnce(&mut crate::edit::TextEdit) -> EditOutcome, tx: &mut Tx) -> bool {
        let Some(path) = self.focused.clone() else { return false };
        let Some((access, submit, change, animate_layout, single_line)) =
            self.focused_input().map(|i| (i.access.clone(), i.submit.clone(), i.change.clone(), i.animate_layout, i.single_line))
        else {
            return false;
        };
        let Some(access) = access else { return false };
        access(handler_state(&mut self.state)).set_single_line(single_line);
        let before = change.as_ref().map(|_| zeroize::Zeroizing::new(access(handler_state(&mut self.state)).text.clone()));
        let mut out = f(access(handler_state(&mut self.state)));
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

    fn color(&self, id: NodeId, prop: Prop<Color>, rest: Color) -> Color {
        if self.scene.store().is_animating(id, prop) { self.scene.get(id, prop).unwrap_or(rest) } else { rest }
    }

    // ---------------------------------------------------------------------------------------
    // Commit: view → nodes → layout → Scene retarget

    fn virtual_window_changed(&self) -> bool {
        self.virtuals.iter().any(|(path, v)| self.wants_window(path, v))
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

fn collect_virtual_inputs(el: &El, parent: &str, index: usize, inputs: &mut HashMap<String, Box<InputSpec>>) {
    // Most offscreen rows hold no focused field; find that out before building any paths.
    if !crate::memo::has_focused_input(el) {
        return;
    }
    let input = el.input.as_ref().filter(|i| i.edit.focused && i.access.is_some());
    // A memo's shared children are skipped whole when none is a focused field.
    let skip = el.memo_children.as_ref().filter(|m| !m.focused_input).map_or(0, |m| m.children.len());
    if input.is_none() && el.children.is_empty() && skip == el.memo_children.as_ref().map_or(0, |m| m.children.len()) {
        return;
    }
    let segment = el.key.clone().unwrap_or_else(|| format!("#{index}"));
    let path = format!("{parent}/{segment}");
    if let Some(input) = input {
        inputs.insert(path.clone(), input.clone());
    }
    for (i, child) in el.all_children().enumerate().skip(skip) {
        collect_virtual_inputs(child, &path, i, inputs);
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
    fn window_tree(&mut self, el: &mut El, parent: &str, index: usize, scroll: Option<(&str, bool, f32)>, settle: bool, force_all: bool, bound: f32) {
        el.expand_memo();
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
            let source = el.virtual_rows.take();
            let state = handler_state_ref(self.state);
            if source.is_some() {
                assert!(el.children.is_empty(), "`.virtual_rows(..)` at {path:?} builds its own rows; it takes no children");
            }
            v.indexed = source.is_some();
            let same = match &source {
                Some(rows) => rows.count == v.keys.len() && (0..rows.count).all(|i| (rows.key)(state, i) == v.keys[i]),
                None => v.same_keys(el.children.iter().map(|c| c.key.as_deref())),
            };
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
                v.set_keys(keys, self.commits, (!pinned).then_some(offset))
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
                    collect_virtual_inputs(&build(i), &path, i, self.virtual_inputs);
                }
                el.children = v.range.clone().map(|i| {
                    let mut row = build(i);
                    row.virtual_mount = v.born(i) < self.commits;
                    row
                }).collect();
            } else {
                el.children = std::mem::take(&mut el.children).into_iter().enumerate().filter_map(|(i, mut child)| {
                    if !v.range.contains(&i) {
                        collect_virtual_inputs(&child, &path, i, self.virtual_inputs);
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
            self.window_tree(child, &path, i, scroll, settle, force_all, bound);
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
            let mounted_height = v.prefix(v.range.end) - v.before;
            // Never present a blank caused by an estimate. Expand and measure again first.
            retry |= if pinned {
                v.range.start > 0 && mounted_height < v.viewport
            } else {
                (v.range.start > 0 && offset < v.pad_top + v.before)
                    || (v.range.end < v.keys.len() && offset + v.viewport > v.pad_top + v.prefix(v.range.end))
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
        self.window_tree(&mut root, "", 0, None, tx.settle, force_all, bound);
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
        let root_id = flatten(root, None, "", 0, motion::DEFAULT, false, true, &mut taffy, &mut nodes, &mut self.layout_entries, scale);
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
                // Anchors inside scroll viewports sit where they are scrolled to.
                let mut target = nodes[j].target;
                for [ox, oy] in scroll_offsets(self.scrolls, &nodes, j) {
                    target[0] -= ox;
                    target[2] -= ox;
                    target[1] -= oy;
                    target[3] -= oy;
                }
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
                let mut layout = self.text.layout_aligned(&spec.content, &spec.spans, spec.style, scale, wrap, line_alignment(spec));
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
        self.memo_since = self.memo.next_pass();
        let mut measured = self.measure(tx);
        self.focus_before_commit = self.focused.clone();
        if self.sync_requested_focus(&mut measured.nodes) {
            self.layout = measured.layout;
            measured = self.measure(tx);
        }
        if measured.caret_read && self.caret_moved(Some(&measured.nodes)) {
            self.layout = measured.layout;
            measured = self.measure(&tx);
        }
        let measured = self.settle_layout_reads(measured, &tx);
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
            let under = self.focused.as_deref().and_then(|f| f.strip_prefix(path.as_str())).is_some_and(|rest| rest.starts_with('/'));
            if !under { v.focused_row = None; continue; }
            // Offscreen, the row keeps the key it had.
            let Some(mut i) = focused else { continue };
            while let Some(p) = self.nodes[i].parent {
                if self.nodes[p].path == *path {
                    v.focused_row = self.nodes[i].key.clone();
                    break;
                }
                i = p;
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
        for path in &self.virtual_culled {
            if let Some(id) = self.path_ids.get(path) {
                self.scene.unmount(*id);
                self.grows.remove(id);
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
        let instant = tx.instant || first;
        let now = self.t;
        if self.fit_height && let Some(root) = nodes.first() { self.fit_window(root.local.h, tx, first); }

        // Growth stacks: rows of a stack that existed before grow; a new stack mounts settled.
        for c in 0..nodes.len() {
            let Some(_spec) = nodes[c].growth else { continue };
            let cid = nodes[c].id;
            // A reader away from the tail is anchored using measured row heights. Land those
            // heights with the anchor correction; delayed growth would move the reading position.
            let reading = self.virtuals.get(&nodes[c].path)
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
            if first || !existed || tx.settle || self.reduced_motion || reading {
                stack.width_hold_until = 0.0;
                stack.width_shrink_until = 0.0;
                stack.pending_heights.clear();
            } else if (width - stack.width).abs() > 0.25 {
                stack.width_hold_until = now + 0.12;
                stack.width_shrink_until = now + 0.4;
            }
            stack.width = width;
            stack.pending_heights.retain(|id, _| new_set.contains(id));
            stack.pad_top = rows.first().map_or(0.0, |&j| nodes[j].target[1] - ctop);
            stack.pad_bottom = rows.last().map_or(nodes[c].target[3] - ctop, |&j| nodes[c].target[3] - nodes[j].target[3]);
            for &j in &rows {
                let id = nodes[j].id;
                let mut h = nodes[j].target[3] - nodes[j].target[1];
                stack.closing.remove(&id);
                stack.tops.insert(id, nodes[j].target[1] - ctop);
                let streaming = nodes[j].streaming;
                if streaming && existed && !tx.settle && !first && !self.reduced_motion && !reading {
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
                let g = self.grows.entry(id).or_insert_with(|| {
                    if existed && !instant && !nodes[j].virtual_mount { GrowTrack { streaming, ..GrowTrack::settled(0.0) } } else { GrowTrack::settled(h) }
                });
                g.streaming = streaming;
                if tx.settle || !existed || first || self.reduced_motion || reading {
                    *g = GrowTrack { streaming, ..GrowTrack::settled(h) };
                } else {
                    // Only a width/scale rewrap may shrink an active streaming row, including
                    // when fresh text arrives in the same frame as the resize.
                    let t = if streaming && !self.relayout && now >= stack.width_shrink_until { h.max(g.h.min(g.target)) } else { h };
                    if now < stack.width_hold_until {
                        stack.pending_heights.insert(id, t);
                    } else {
                        stack.pending_heights.remove(&id);
                        g.retarget(t);
                    }
                }
            }
            stack.order = order;
            self.stacks.insert(cid, stack);
        }

        // Put nodes on their subtree's clock before this commit animates them.
        let time_scaled = self.time_scaled || nodes.iter().any(|n| n.time_scale != 1.0);
        let mut time_scales = Vec::new();
        if time_scaled { self.sync_time_scales(&nodes, &mut time_scales, false); }

        // `El::matched` elements of this tree: (id, index, newly mounted).
        let mut arrivals: Vec<(Rc<str>, usize, bool)> = Vec::new();
        for i in 0..nodes.len() {
            let id = nodes[i].id;
            let parent_id = nodes[i].parent.map(|p| nodes[p].id);
            let spring_l = tx.motion.unwrap_or(nodes[i].motion);
            let spring_c = tx.color_motion.unwrap_or(nodes[i].motion);
            // Text leads its background so theme crossfades never dip through zero contrast.
            let spring_tc = Spring::new((spring_c.duration * 0.5).max(0.1), spring_c.bounce);
            let row_like = nodes[i].in_row || nodes[i].parent.is_some_and(|p| nodes[p].growth.is_some());
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
                // Growth carries a row's entrance; an explicit transition adds to it.
                let quiet_row = row_like && nodes[i].transition.is_none() && !was_exiting && !self.reduced_motion;
                let mount = if instant || nodes[i].virtual_mount || quiet_row { Mount::Initial } else { Mount::Animated };
                self.scene.insert(id, spec, mount);
            }
            if was_present {
                // As in SwiftUI, a node leaves with the transition of its last render.
                if !was_exiting {
                    self.scene.set_transition(id, nodes[i].transition.unwrap_or_else(Transition::opacity));
                }
                let stack_row = nodes[i].parent.is_some_and(|p| nodes[p].growth.is_some()) || self.virtuals.contains_key(&nodes[i].path);
                // A window's own resize step moves content at rest with the window; motion
                // already running retargets on its spring.
                let follows_window = tx.window_step && !self.flying.iter().any(|f| f.0 == id)
                    && !self.scene.store().is_animating(id, prop::FRAME);
                let layout_anim = if stack_row && !instant && !tx.relayout && !follows_window {
                    // Growth and virtual windows own a row's vertical place; its horizontal place
                    // still springs, or a re-centred column jumps against its moving parent.
                    LayoutAnim::axes(spring_l, Anim::INSTANT)
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
                self.set_or_anim(id, prop::RADIUS, Corners(nodes[i].vis.radius), spring_l, instant);
                if nodes[i].live.iter().any(|b| b.property == crate::live::Property::Opacity) {
                    // The live source owns this property, including across unrelated commits.
                } else if !instant && let Some(anim) = nodes[i].opacity_motion {
                    self.scene.animate(id, prop::OPACITY, nodes[i].vis.opacity, anim);
                } else {
                    self.set_or_anim(id, prop::OPACITY, nodes[i].vis.opacity, spring_l, instant);
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
            if vis.hover.a > 0.0 || self.scene.store().has(id, P_HOVER_C) {
                self.set_or_anim(id, P_HOVER_C, vis.hover, spring_c, instant);
            }
            if vis.press.a > 0.0 || self.scene.store().has(id, P_PRESS_C) {
                self.set_or_anim(id, P_PRESS_C, vis.press, spring_c, instant);
            }
            for (k, sh) in vis.shadows.iter().take(P_SHADOWS.len()).enumerate() {
                self.set_or_anim(id, P_SHADOWS[k], sh.color, spring_c, instant);
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
        for n in &nodes {
            if let Some(sp) = n.scroll {
                let e = self.scrolls.entry(n.id).or_insert(ScrollState {
                    offset: 0.0,
                    v: 0.0,
                    pinned: sp.stick_bottom,
                    max: 0.0,
                    direct: false,
                    scrolling: false,
                    glide: None,
                    at_bottom: None,
                    held: false,
                    x: AxisScroll::default(),
                });
                if tx.settle && sp.stick_bottom {
                    e.pinned = true;
                    e.v = 0.0;
                }
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
                input.access = None;
                input.key = None;
                input.change = None;
                input.edit.focused = false;
                input.edit.clear_preedit();
                input.edit.conceal();
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
        self.has_dissolve = self.nodes.iter().any(|n| n.vis.blend == crate::render::Blend::PlusLighter);
        self.has_pixel_aspect = self.nodes.iter().any(|n| n.pixel_aspect.is_some());
        // A flight that left the tree stays lifted and covered while it exits, so it doesn't pop.
        self.flying.retain(|(id, _)| self.node_index.contains_key(id));
        if self.nodes.len() > present_len || !self.flying.is_empty() { self.update_layer_paint_order(); }
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
            for (k, id) in stack.order.iter().enumerate() {
                let Some(g) = self.grows.get_mut(id) else { continue };
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
                moving |= g.moving();
            }
            stack.order.retain(|id| {
                let keep = !stack.closing.contains_key(id) || self.grows.get(id).is_some_and(|g| g.h > 0.01 || g.target > 0.0);
                if !keep {
                    self.grows.remove(id);
                }
                keep
            });
            stack.closing.retain(|id, _| stack.order.contains(id));
            // Keep the shared clock awake through the hold. No timer thread or remeasurement.
            moving |= !stack.pending_heights.is_empty();
        }
        moving || (steps == 0 && self.grows.values().any(|g| g.moving()))
    }

    /// Presented content extent of node `c` if it is a growth stack (points).
    fn stack_extent(&self, c: usize) -> Option<f64> {
        let n = &self.nodes[c];
        n.growth?;
        let stack = self.stacks.get(&n.id)?;
        let rows: f64 = stack.order.iter().filter_map(|id| self.grows.get(id)).map(|g| f64::from(g.h)).sum();
        Some(f64::from(stack.pad_top) + rows + f64::from(stack.pad_bottom))
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
            let Some(st) = self.scrolls.get_mut(&id) else { continue };
            st.scrolling = st.direct || st.v.abs() > 1.0 || st.x.v.abs() > 1.0;
            st.direct = false;
            st.max = overflow.max(0.0);
            if st.pinned && sp.stick_bottom {
                st.offset = st.max;
                st.v = 0.0;
                st.glide = None;
            } else if let Some(v) = st.glide {
                moving |= scroll_bottom::glide(st, v, dt);
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
            if sp.stick_bottom && st.offset >= st.max - 0.5 && st.v.abs() <= 1.0 {
                st.pinned = true;
            }
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

    /// The topmost element under (x, y) that takes primary input, selectable text included.
    fn top_target(&self, x: f32, y: f32) -> Option<usize> {
        self.hits.iter().rev().find_map(|&(id, r)| {
            if !contains(r, x, y) { return None; }
            let i = *self.node_index.get(&id)?;
            let n = &self.nodes[i];
            // A secondary-only child must not obscure primary input or background blur.
            (!self.subtree_exiting(i) && self.allowed(i) && (n.on_press.is_some() || n.on_double_press.is_some() || n.on_hover.is_some()
                || n.on_drag.is_some() || n.drag_out.is_some() || n.input.is_some() || n.window_drag || n.focus.enabled || n.modal.is_some()
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

    pub fn pointer_move(&mut self, x: f32, y: f32) {
        self.pointer = Some((x, y));
        if self.in_viewport(x, y) { self.left_in_capture = false; }
        for &(id, rect) in &self.hits {
            if contains(rect, x, y)
                && let Some(n) = self.node_index.get(&id).map(|&i| &self.nodes[i])
                && n.on_pointer_move.is_some()
            {
                self.queue.push(Action::PointerMove(n.path.clone(), PointerPress { x, y }));
            }
        }
        self.refresh_hover(x, y);
        let Some((path, d)) = self.capture.clone() else { return };
        if self.drag_out_armed && crate::drag_out::past_threshold(self.press_at, (x, y), self.drag_out_threshold) && self.start_drag_out(path.clone()) {
            return;
        }
        self.selection_drag(x, y);
        let Some((id, r, has_drag)) = self.node(&path).map(|n| (n.id, n.target, n.on_drag.is_some() && !self.drags_out(n))) else { return };
        self.press_moved |= (x - self.press_at.0).abs() > CLICK_SLOP || (y - self.press_at.1).abs() > CLICK_SLOP;
        // A hit rect is where the element was drawn (after scrolling); `target` is its
        // unscrolled layout rect, kept only for elements that were never drawn.
        let mut hits = self.hits.iter().filter(|(hit, _)| *hit == id).peekable();
        self.capture_inside = !self.press_cancelled && if self.native_overlay || hits.peek().is_some() {
            hits.any(|(_, r)| contains(*r, x, y))
        } else { contains(r, x, y) };
        if has_drag {
            let (dx, dy) = (x - self.press_at.0, y - self.press_at.1);
            let d = Drag { phase: Phase::Move, x: x - r[0], y: y - r[1], dx, dy, ..d };
            self.queue.push(Action::Drag(path, d));
        }
    }

    /// Overlay observation updates coordinates without presenting unchanged hover pixels.
    pub(crate) fn observe_pointer(&mut self, x: f32, y: f32) -> bool {
        if self.pointer == Some((x, y)) { return false; }
        let visual_changed = self.nodes.iter().enumerate().any(|(i, n)| {
            if n.vis.hover.a <= 0.0 && n.vis.press.a <= 0.0 && n.press_scale.is_none() { return false; }
            let r = self.drawn_edges(i);
            let inside = |p: Option<(f32, f32)>| p.is_some_and(|(x, y)| contains(r, x, y));
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
        self.drop_drag_out_request();
        self.drag_out_armed = false;
        self.text_sel.dragging = false;
        self.capture = None;
        self.secondary_capture = None;
        self.capture_inside = false;
        self.pointer_leave();
        self.drag_files_leave();
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
        let clip = n.parent.and_then(|pi| self.inh_scratch.get(pi)).map_or(NO_CLIP, |h| h.clip).map(|v| v / self.env.scale);
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
        self.drop_drag_out_request();
        self.pointer = Some((x, y));
        self.keyboard_focus = false;
        self.dismiss_help();
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
        if n.window_drag && n.on_press.is_none() && n.on_double_press.is_none() && n.on_drag.is_none() && n.drag_out.is_none() && n.input.is_none() {
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
        let r = self.drawn.get(ti).copied().unwrap_or([n.target[0], n.target[1], n.target[2] - n.target[0], n.target[3] - n.target[1]]);
        if n.input.is_some() {
            // Its text drew scaled with its box: map the press back to the rest layout.
            let k = self.drawn_scale.get(ti).copied().unwrap_or(1.0).max(1e-4);
            self.queue.push(Action::Focus(n.path.clone(), (x - r[0]) / k - n.inset[0], (y - r[1]) / k - n.inset[1]));
            return;
        }
        // Clicking a link leaves focus where it is (a composer keeps typing). Selectable text
        // off its links isn't a control: the press blurs, as a click on plain content does.
        if !on_link && n.focus.enabled && !self.is_selectable(ti) {
            self.queue.push(Action::FocusControl(n.path.clone()));
        } else if !on_link && self.focused.is_some() {
            self.queue.push(Action::Blur);
        }
        let r = n.target;
        let d = Drag {
            phase: Phase::Down,
            x: x - r[0],
            y: y - r[1],
            w: r[2] - r[0],
            h: r[3] - r[1],
            start_x: x - r[0],
            start_y: y - r[1],
            dx: 0.0,
            dy: 0.0,
            data: 0.0,
        };
        let path = n.path.clone();
        self.drag_out_armed = self.drags_out(n);
        self.drag_out_threshold = n.drag_threshold.unwrap_or(crate::drag_out::DRAG_THRESHOLD);
        if n.on_drag.is_some() && !self.drag_out_armed {
            self.queue.push(Action::Drag(path.clone(), d));
        }
        self.capture = Some((path, d));
        self.capture_inside = true;
    }

    /// The nearest element with `on_secondary_press` at (x, y), else the selectable text there
    /// (its Copy menu): an app's context menu on a row wins over its text's.
    fn secondary_target(&self, x: f32, y: f32) -> Option<&str> {
        let id = self.hits.iter().rev().find(|&&(_, r)| contains(r, x, y))?.0;
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
            self.queue.push(Action::SecondaryPress(path, PointerPress { x, y }));
        }
        self.leave_after_release();
    }

    pub fn pointer_up(&mut self, x: f32, y: f32) {
        // Hosts release at the last position they saw, which a leave doesn't move.
        let left = self.left_in_capture;
        self.release(x, y);
        self.left_in_capture = left;
        self.leave_after_release();
    }

    fn release(&mut self, x: f32, y: f32) {
        // A release never starts a drag-out: the system session needs the button held.
        self.drag_out_armed = false;
        self.pointer_move(x, y);
        // A drag that selected text is a selection, not a press.
        let selected = self.selected_by_drag();
        self.text_sel.dragging = false;
        let Some((path, d)) = self.capture.take() else { return };
        self.refresh_hover_within();
        let double = std::mem::take(&mut self.double_armed);
        let Some((r, has_drag, has_press, has_double)) = self.node(&path).map(|n| {
            (n.target, n.on_drag.is_some() && !self.drags_out(n), n.on_press.is_some(), n.on_double_press.is_some())
        }) else {
            return;
        };
        if has_drag {
            let (dx, dy) = (x - self.press_at.0, y - self.press_at.1);
            self.queue.push(Action::Drag(path.clone(), Drag { phase: Phase::Up, x: x - r[0], y: y - r[1], dx, dy, ..d }));
        }
        self.link_up(&path, x, y);
        if has_press && self.capture_inside && !selected {
            self.queue.push(Action::Press(path.clone()));
        }
        // Only clicks pair: a press that dragged or was held past the interval is neither half.
        let click = !self.press_moved && self.now_clock() - self.press_down_t <= self.double_click_interval;
        if has_double && self.capture_inside && click {
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
        if self.hover_within.is_empty() && !self.hits.iter().any(|(id, _)| self.node_index.get(id).is_some_and(|&i| self.nodes[i].on_hover_change.is_some())) {
            return;
        }
        let mut inside: Vec<String> = Vec::new();
        if let Some((x, y)) = self.pointer {
            for &(id, r) in &self.hits {
                if let Some(n) = self.node_index.get(&id).map(|&i| &self.nodes[i])
                    && n.on_hover_change.is_some() && contains(r, x, y) && !inside.contains(&n.path)
                {
                    inside.push(n.path.clone());
                }
            }
        }
        // A held press stays inside what it started in until it releases.
        if self.capture.is_some() {
            for path in &self.hover_within {
                if !inside.contains(path) { inside.push(path.clone()); }
            }
        }
        for path in &self.hover_within {
            if !inside.contains(path) { self.queue.push(Action::HoverChange(path.clone(), false)); }
        }
        for path in &inside {
            if !self.hover_within.contains(path) { self.queue.push(Action::HoverChange(path.clone(), true)); }
        }
        self.hover_within = inside;
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
            Some((p, _)) => (0..self.nodes.len()).find(|&i| self.nodes[i].path == *p && !self.subtree_exiting(i)),
            None => self.selectable_at(x, y).or_else(|| self.primary_target(x, y)),
        };
        let Some(mut i) = target else {
            return crate::el::Cursor::Default;
        };
        if self.nodes[i].cursor.is_none() && self.nodes[i].input.is_some() {
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
        self.queue.push(Action::Paste(zeroize::Zeroizing::new(text.into()), None));
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
    pub(crate) fn paste_result(&mut self, target: Option<String>, text: zeroize::Zeroizing<String>) {
        self.queue.push(Action::Paste(text, Some(target)));
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
    fn reveal_row(&mut self, key: &str) {
        if self.reveal_row_once(key, true) {
            for _ in 0..8 {
                if !self.reveal_row_once(key, false) { break; }
            }
        }
        self.reveal_row_x(key);
    }

    /// One placement of [`Self::reveal_row`]: whether it committed. After the first, it moves
    /// only when the row would land somewhere else.
    fn reveal_row_once(&mut self, key: &str, first: bool) -> bool {
        let geometry = self.virtuals.iter().find_map(|(_, v)| {
            let index = v.index_of(key)?;
            Some((v.scroll_path.clone(), f64::from(v.pad_top + v.prefix(index)), f64::from(v.height(key)), f64::from(v.viewport)))
        }).or_else(|| {
            let (i, row) = self.nodes.iter().enumerate().find(|(_, n)| n.key.as_deref() == Some(key) || n.path == key)?;
            let mut ancestor = self.nodes[i].parent;
            while let Some(p) = ancestor {
                let parent = &self.nodes[p];
                if parent.scroll.is_some() && parent.scroll_axes.y() {
                    return Some((parent.path.clone(), f64::from(row.target[1] - parent.target[1]),
                        f64::from(row.local.h), f64::from(parent.local.h)));
                }
                ancestor = parent.parent;
            }
            None
        });
        let Some((path, top, height, viewport)) = geometry else { return false };
        let margin = self.node(&path).map_or(0.0, |n| f64::from(n.scroll_edges.margin));
        let Some(&id) = self.path_ids.get(&path) else { return false };
        let Some(scroll) = self.scrolls.get_mut(&id) else { return false };
        let offset = scroll_edge::reveal(scroll.offset, top, height, viewport, margin).clamp(0.0, scroll.max);
        if !first && (offset - scroll.offset).abs() < 0.5 { return false; }
        scroll.offset = offset;
        scroll.pinned = false;
        scroll.v = 0.0;
        scroll.glide = None;
        scroll.direct = true;
        let window = self.virtual_window_changed();
        if window {
            self.commit(&Tx::default());
            // The commit measured rows that were estimates: the next placement clamps to the end
            // they make.
            if let Some(i) = self.nodes.iter().position(|n| n.id == id) {
                let max = self.scroll_overflow(i).max(0.0);
                if let Some(scroll) = self.scrolls.get_mut(&id) { scroll.max = max; }
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
            .find(|(i, _)| {
                let r = self.drawn.get(*i).copied().unwrap_or([0.0; 4]);
                contains([r[0], r[1], r[0] + r[2], r[1] + r[3]], x, y)
            })
            .map(|(_, n)| n.id)
    }

    /// Wheel or trackpad delta in points (positive `dy` moves content down, like a browser's
    /// negative deltaY). Applied 1:1: the platform supplies its own momentum phases.
    pub fn wheel(&mut self, x: f32, y: f32, dy: f32) {
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

    fn update_interaction(&mut self) {
        let captured = self.capture.as_ref().map(|(p, _)| p.as_str()).or(self.secondary_capture.as_deref());
        for i in 0..self.nodes.len() {
            let n = &self.nodes[i];
            let tracks_hover = n.vis.hover.a > 0.0 || n.vis.hover_group;
            if self.is_exiting(n.id) || (!tracks_hover && n.vis.press.a <= 0.0 && n.press_scale.is_none()) {
                continue;
            }
            let r = self.drawn_edges(i);
            // A group inside a clipped region (a scrolled list) only hovers where it shows.
            let c = n.parent.and_then(|p| self.inh_scratch.get(p)).map_or(NO_CLIP, |h| h.clip).map(|v| v / self.env.scale);
            let shown = [r[0].max(c[0]), r[1].max(c[1]), r[2].min(c[2]), r[3].min(c[3])];
            let inside = self.pointer.is_some_and(|(x, y)| contains(shown, x, y));
            // A group stays hovered while one of its descendants holds the press.
            let within = |p: &str| p == n.path || (p.len() > n.path.len() && p.starts_with(&n.path) && p.as_bytes()[n.path.len()] == b'/');
            let capture_inside = if self.secondary_capture.is_some() { inside } else { self.capture_inside };
            let held = captured == Some(n.path.as_str()) && capture_inside;
            // A held press stays hovered while it's inside, though a press scale shrank the
            // drawn rect away from the pointer.
            let hovered = (inside || held) && tracks_hover && captured.is_none_or(within);
            let pressed = held && n.vis.press.a > 0.0;
            let press_scale = n.press_scale;
            let id = n.id;
            // Like a native sidebar or menu: the highlight lands with the pointer and
            // fades briefly as it leaves.
            let hover_anim = match n.vis.hover_motion {
                Some((enter, exit)) => if hovered { enter } else { exit },
                None if hovered => Anim::INSTANT,
                None => rune_motion::Curve::tween(0.1, rune_motion::Easing::EASE_OUT).into(),
            };
            self.scene.animate(id, P_HOVER, hovered as u8 as f32, hover_anim);
            self.scene.animate(id, P_PRESS, pressed as u8 as f32, if pressed { motion::INSTANT } else { motion::SNAPPY });
            if let Some(k) = press_scale {
                // Down follows the hand; the release springs back with a little life.
                let down = held && !self.reduced_motion;
                self.scene.animate(id, P_PRESS_SCALE, if down { k } else { 1.0 }, if down { motion::INSTANT } else { motion::POP });
            }
        }
    }

    // ---------------------------------------------------------------------------------------
    // Frame build: Scene Presented → instances

    fn build(&mut self) {
        let _paint_time = crate::frame_profile::Scope::new(crate::frame_profile::Phase::Paint);
        let s = self.env.scale;
        let snap_px = |v: f32| grid(v, s) * s;
        self.instances.clear();
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
        self.inh_scratch.resize(n_nodes, Inh::default());
        self.drawn.clear();
        self.drawn.resize(n_nodes, [0.0; 4]);
        self.drawn_scale.clear();
        self.drawn_scale.resize(n_nodes, 1.0);
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
            self.dissolve_scratch[parent] |= opacity > 0.0 && opacity < 1.0;
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
            for id in &stack.order {
                let Some(g) = self.grows.get(id) else { continue };
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
        let tree_order = (flying && !self.layer_paint_order.is_empty()) || self.layer_paint_order.windows(2).any(|pair| {
            let n = &self.nodes[pair[0]];
            self.nodes[pair[1]].parent == Some(pair[0]) && ((self.has_dissolve && self.dissolve_scratch[pair[0]]) || n.vis.clip_radius.is_some() || n.vis.group_opacity.is_some()
                || fades(n.looping)
                || self.scene.presented(n.id).is_some_and(|p| (p.opacity > 0.0 && p.opacity < 1.0) || p.blur > 0.0))
        });
        let default_ring = self.default_ring_owner();
        for paint_index in 0..n_nodes {
            let i = if tree_order { self.layer_paint_order[paint_index] } else { paint_index };
            let n = &self.nodes[i];
            // A flight paints outside every ancestor's layer, above the whole window.
            let lift = flying && self.flying.iter().any(|f| f.0 == n.id);
            if lift {
                while let Some((_, start, surface_start)) = layers.pop() {
                    crate::render::end_layer(&mut self.instances, &mut self.surfaces, &mut self.marks, start, surface_start);
                }
            }
            while let Some(&(ancestor, start, surface_start)) = layers.last() {
                let mut parent = n.parent;
                while parent.is_some_and(|p| p != ancestor) { parent = self.nodes[parent.unwrap()].parent; }
                if parent == Some(ancestor) { break; }
                crate::render::end_layer(&mut self.instances, &mut self.surfaces, &mut self.marks, start, surface_start);
                layers.pop();
            }
            let Some(p) = self.scene.presented(n.id) else { continue };
            let mut pinh = n.parent.map_or(Inh::default(), |pr| self.inh_scratch[pr]);
            pinh.dy += self.row_dy[i];
            if lift {
                // Keep the destination's clips (consumed ones included) and offsets. Drop its
                // ancestors' opacity, scale and blur: an entering parent's fade or scale would
                // otherwise hide the flight or jump it off the rect it was seen at. Inside another
                // flight, also drop that flight's cover offset and clip.
                let [clip, fade, fade_origin] = pinh.outer.unwrap_or([pinh.clip, pinh.fade, pinh.fade_origin]);
                pinh = Inh { opacity: 1.0, paint_opacity: 1.0, clip, fade, fade_origin, paint_clip: clip, paint_fade: fade,
                    paint_fade_origin: fade_origin, rounded: None, scale: 1.0, anchor: [0.0; 2], anchor_to: [0.0; 2], blur: 0.0,
                    dx: pinh.dx - pinh.cover[0], dy: pinh.dy - pinh.cover[1], cover: [0.0; 2], outer: None,
                    scale_motion: ScaleMotion::Still, ..pinh };
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
            let own_sc = p.transform.scale.x * press_k * n.static_scale
                * sequence.and_then(|l| l.pose(lnow, self.reduced_motion)).map_or(1.0, |p| p.scale);
            // An `El::scale` an app drives from a slider or pinch counts as moving until it holds
            // still, then rasterizes at rounded sizes like a live scale.
            let (static_moving, static_dynamic) = if n.static_scale != 1.0 || self.static_scales.contains_key(&n.id) {
                let t = self.t;
                let seen = self.static_scales.entry(n.id).or_insert((n.static_scale, f64::NEG_INFINITY, false));
                if seen.0 != n.static_scale { *seen = (n.static_scale, t, true); }
                let moving = t - seen.1 < STATIC_SCALE_SETTLE;
                self.static_scale_settling |= moving;
                (moving, seen.2)
            } else { (false, false) };
            let store = self.scene.store();
            let scale_motion = pinh.scale_motion.max(
                if store.is_animating(n.id, prop::SCALE) || store.is_animating(n.id, prop::PRESENCE)
                    || (n.press_scale.is_some() && store.is_animating(n.id, P_PRESS_SCALE))
                    || sequence.is_some_and(|l| !self.reduced_motion && l.active(lnow)) || static_moving {
                    ScaleMotion::Moving
                } else if static_dynamic || n.live.iter().any(|b| b.property == crate::live::Property::Scale) {
                    ScaleMotion::Live
                } else {
                    ScaleMotion::Still
                });
            // Scale about this node's centre where its ancestors' scales put it, so a scaled child
            // of a scaled parent stays in place.
            let sc = pinh.scale * own_sc;
            let centre = [p.anchor.x + pinh.dx, p.anchor.y + pinh.dy];
            let centre_to = [pinh.anchor_to[0] + (centre[0] - pinh.anchor[0]) * pinh.scale,
                pinh.anchor_to[1] + (centre[1] - pinh.anchor[1]) * pinh.scale];
            let (anchor, anchor_to) = if (own_sc - 1.0).abs() > 1e-5 { (centre, centre_to) } else { (pinh.anchor, pinh.anchor_to) };
            // A pressed element still hits at its resting size, so a press near its edge stays inside.
            let press_anchor = centre_to;
            let blur = pinh.blur + p.blur * sc;
            let filter_margin = blur * s * 3.0;

            let mut raw_edges = p.rect.edges();
            raw_edges[0] += pinh.dx;
            raw_edges[2] += pinh.dx;
            raw_edges[1] += pinh.dy;
            raw_edges[3] += pinh.dy;
            if self.row_h[i] >= 0.0 {
                raw_edges[3] = raw_edges[1] + self.row_h[i];
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
            self.drawn[i] = [r[0] / s, r[1] / s, w / s, h / s];
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
            let visible = r[2] >= clip[0].max(vp[0]) - filter_margin - 2.0
                && r[0] <= clip[2].min(vp[2]) + filter_margin + 2.0
                && r[3] >= clip[1].max(vp[1]) - filter_margin - 2.0
                && r[1] <= clip[3].min(vp[3]) + filter_margin + 2.0;
            let text_tint = |c: Color| match n.vis.group_color {
                Some(g) if pinh.group > 0.0 => c.mix(g, pinh.group),
                _ => c,
            };

            // A slide may sweep wholly outside its clip; judge visibility at its resting place.
            let home = match n.looping.map(|l| l.kind) {
                Some(rune_motion::LoopKind::Slide { .. }) => {
                    let o = self.scene.get(n.id, prop::OFFSET).unwrap_or(rune_motion::Vec2::ZERO);
                    [r[0] - o.x * s, r[1] - o.y * s, r[2] - o.x * s, r[3] - o.y * s]
                }
                _ => r,
            };
            if self.surface_visible && base_opacity > 0.001 && !self.reduced_motion
                && home[2] > clip[0].max(vp[0] - filter_margin) && home[0] < clip[2].min(vp[2] + filter_margin)
                && home[3] > clip[1].max(vp[1] - filter_margin) && home[1] < clip[3].min(vp[3] + filter_margin)
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
            let intersect = |a: [f32; 4], b: [f32; 4]| [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])];
            // A negative clip radius rounds with continuous corners (`render::corner_extent`).
            let mut own_round = n.vis.clip_radius.map(|radius| {
                let radius = (radius * sc * s).min(w.min(h) * 0.5);
                (r, if n.vis.continuous_corners { -radius } else { radius })
            });
            let mut hit_clip = intersect(pinh.clip, clip);
            if let Some((rect, _)) = own_round { hit_clip = intersect(hit_clip, rect); }
            let (mut hit_fade, mut hit_origin) = (pinh.fade, pinh.fade_origin);
            inherit_fade(&mut hit_fade, &mut hit_origin, fade_origin, fade);
            // A rectangle wholly in either central strip misses all four corner squares.
            // Its hard edges already exclude everything the rounded mask would remove.
            for round in [&mut rounded, &mut own_round] {
                if let Some((mask, radius)) = *round {
                    let radius = crate::render::corner_extent(radius);
                    let cut = intersect(clip, mask);
                    if cut[2] <= cut[0] || cut[3] <= cut[1]
                        || (cut[0] >= mask[0] + radius && cut[2] <= mask[2] - radius)
                        || (cut[1] >= mask[1] + radius && cut[3] <= mask[3] - radius) {
                        clip = cut;
                        *round = None;
                    }
                }
            }
            // The packed ABI carries one clip rectangle. Preserve a rectangular cut through
            // a rounded corner on a separate outer scope, never recenter the rounded mask.
            if let Some((mask, _)) = rounded.or(own_round)
                && intersect(clip, mask) != mask {
                let mut marker = Instance::begin_layer([r[0], r[1], w, h], clip, Default::default());
                marker.fade = crate::render::pack_fade(fade, fade_origin, clip);
                layers.push((i, self.instances.len(), self.surfaces.len()));
                self.instances.push(marker);
                clip = NO_CLIP; fade = [0.0; 4]; fade_origin = NO_CLIP;
            }
            // Two rounded rectangles cannot generally be represented by one rounded rectangle.
            // Consume the ancestor on an outer composite, retaining both original corner centres.
            if rounded.is_some() && own_round.is_some() && rounded != own_round {
                let (mask, radius) = rounded.take().unwrap();
                let mut marker = Instance::begin_layer([r[0], r[1], w, h], mask,
                    crate::render::LayerSpec { clip_radius: radius, ..Default::default() });
                marker.fade = crate::render::pack_fade(fade, fade_origin, mask);
                marker.round_clip(radius);
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
            isolate |= rounded.is_some() && (n.text.as_ref().is_some_and(|t| t.truncate_fade || t.live.is_some())
                || n.input.as_ref().is_some_and(|i| i.single_line));
            let o = if isolate { 1.0 } else { (own * pinh.paint_opacity).clamp(0.0, 1.0) };
            if isolate {
                layers.push((i, self.instances.len(), self.surfaces.len()));
                let mut marker = Instance::begin_layer([r[0], r[1], w, h], clip,
                    crate::render::LayerSpec { opacity: own * pinh.paint_opacity,
                        blend: if dissolve { crate::render::Blend::PlusLighter } else { crate::render::Blend::SourceOver },
                        blur: p.blur * sc * s, clip_radius: rounded.map_or(0.0, |(_, radius)| radius) });
                marker.fade = crate::render::pack_fade(fade, fade_origin, clip);
                if let Some((_, radius)) = rounded { marker.round_clip(radius); }
                self.instances.push(marker);
                // Clip the completed layer, never its inputs (including off-wall blur taps).
                clip = NO_CLIP; fade = [0.0; 4]; fade_origin = NO_CLIP; rounded = None;
            }
            let primitive_start = self.instances.len();
            let surface_start = self.surfaces.len();
            // Include native surface geometry, which lives beside the primitive stream.
            if n.vis.surface.is_some() {
                for &(_, at, _) in &layers {
                    let b = &mut self.instances[at].rect;
                    let hi = [(b[0] + b[2]).max(r[2].min(clip[2])), (b[1] + b[3]).max(r[3].min(clip[3]))];
                    b[0] = b[0].min(r[0].max(clip[0])); b[1] = b[1].min(r[1].max(clip[1]));
                    b[2] = hi[0] - b[0]; b[3] = hi[1] - b[1];
                }
            }

            let first_instance = self.instances.len();
            // Native input readiness follows focus, not entrance opacity. An input
            // mounting at opacity zero still needs its IME area in this frame.
            let input_caret = n.input.as_ref().filter(|input| input.edit.focused && self.can_focus(i))
                .filter(|_| !self.flights.iter().any(|f| n.key.as_deref() == Some(&f.source_key)
                    && f.alphas(f.destination, now, self.reduced_motion).0 > 0.0))
                .and_then(|_| input_caret(n, r, sc, s, &mut self.input_scroll));
            if let Some((_, area)) = input_caret
                && n.input.as_ref().is_some_and(|input| !input.edit.is_secure()) { self.ime_area = Some(area); }
            if effective_opacity > 0.001 && o > 0.001 && visible {
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
                for (k, sh) in n.vis.shadows.iter().enumerate() {
                    let c = if k < P_SHADOWS.len() { self.color(n.id, P_SHADOWS[k], sh.color) } else { sh.color };
                    if c.a <= 0.0 || sh.inset {
                        continue;
                    }
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
                for (k, sh) in n.vis.shadows.iter().enumerate() {
                    if !sh.inset {
                        continue;
                    }
                    let c = if k < P_SHADOWS.len() { self.color(n.id, P_SHADOWS[k], sh.color) } else { sh.color };
                    if c.a <= 0.0 {
                        continue;
                    }
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
                    let px = (w.min(h)).round().max(1.0) as u32;
                    if let Some(q) = self.text.icon_quad(ic, px, [r[0], r[1]]) {
                        self.instances.push(Instance {
                            rect: q.rect,
                            radii: [0.0; 4],
                            fill: premul(color, o, srgb),
                            border: [0.0; 4],
                            params: [if matches!(n.looping.map(|l| l.kind), Some(rune_motion::LoopKind::Rotation)) { loop_value.unwrap_or(0.0) } else { 0.0 }, KIND_GLYPH, 0.0, 1.0],
                            uv: [q.uv[0], q.uv[1], 0.0, 0.0],
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
                    // Text fields: selection under the text, placeholder, caret over it.
                    let mut caret = None;
                    if let Some(inp) = &n.input {
                        let e = &inp.edit;
                        let departing = self.flights.iter().any(|f| f.departs(&n.path, n.key.as_deref())
                            && f.alphas(f.destination, now, self.reduced_motion).0 > 0.0);
                        if e.text.is_empty() && e.preedit.is_none() && !inp.placeholder.is_empty() && !departing {
                            let pl = self.text.layout_rich(&inp.placeholder, &[], spec.style, s, Some(w / tk.max(1e-4)));
                            self.glyph_scratch.clear();
                            self.text.glyphs_for_layout_scaled(&pl, s, origin, raster, false, &mut self.glyph_scratch);
                            let pc = premul(inp.placeholder_color, o, srgb);
                            for q in &self.glyph_scratch {
                                self.instances.extend(glyph_or_image(q, &self.text, &mut self.images, pc, glyph_gamma(inp.placeholder_color, s, srgb), clip, fade)
                                    .map(|g| zoom_glyph(g, origin, tk / raster)));
                            }
                        }
                        if e.focused && !departing {
                            let (a, b) = e.range();
                            if a != b && e.preedit.is_none() {
                                selectable::push_selection(&mut self.instances, layout, e.display_index(a), e.display_index(b),
                                    origin, tk, premul(inp.selection, o, srgb), s, clip, fade);
                            }
                            caret = input_caret.map(|(caret, _)| caret);
                            if let Some((_, _)) = &e.preedit
                                && let (_, Some((pa, pb))) = e.display()
                            {
                                let sel = Selection::new(
                                    Cursor::from_byte_index(layout, pa, Affinity::Downstream),
                                    Cursor::from_byte_index(layout, pb, Affinity::Upstream),
                                );
                                for (bb, _) in sel.geometry(layout) {
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
                    // Span backgrounds (inline code), a selection over them, then glyphs.
                    let link_fx = self.link_fx(i);
                    self.deco_scratch.clear();
                    match n.elided.filter(|_| !spec.spans.is_empty()) {
                        Some(e) => self.text.decorations(layout, &e.spans(&spec.spans), s, origin, &mut self.deco_scratch),
                        None => self.text.decorations(layout, &spec.spans, s, origin, &mut self.deco_scratch),
                    }
                    let drawn_len = n.elided.map_or(spec.content.len(), |e| e.drawn_len(spec.content.len()));
                    link_fx.decos(layout, drawn_len, s, origin, &mut self.deco_scratch);
                    if tk != 1.0 {
                        for d in &mut self.deco_scratch {
                            d.rect = scale_about(d.rect, origin, tk);
                            d.radius *= tk;
                        }
                    }
                    for d in &self.deco_scratch {
                        let c = premul(d.color, o, srgb);
                        self.instances.push(rect_inst(d.rect, c, d.radius, clip, fade));
                    }
                    if spec.selectable && let Some((a, b)) = selectable::range_in(&sel_ranges, i) {
                        let (a, b) = n.elided.map_or((a, b), |e| e.range(a, b));
                        selectable::push_selection(&mut self.instances, layout, a, b, origin, tk, premul(spec.selection, o, srgb), s, clip, fade);
                    }
                    let reveal = self.reveals.get(&n.id).filter(|st| !st.words.is_empty() && spec.advance_in.is_none());
                    self.glyph_scratch.clear();
                    // A cell of a longer run draws its slice of the run's glyphs, shaped in context.
                    let zoom = if let (Some(r), Some((_, at))) = (&n.run_layout, &spec.advance_in)
                        && let Some(x) = r.x(*at)
                    {
                        self.text.glyphs_in(r, *at, s, [origin[0] - x, origin[1]], &mut self.glyph_scratch);
                        tk
                    } else {
                        self.text.glyphs_for_layout_scaled(layout, s, origin, raster, reveal.is_some() || !link_fx.is_empty(), &mut self.glyph_scratch);
                        tk / raster
                    };
                    let gamma = glyph_gamma(color, s, srgb);
                    let base = premul(color, o, srgb);
                    for q in &self.glyph_scratch {
                        if self.flights.iter().any(|f| f.lands(&n.path, n.key.as_deref())) { continue; }
                        let [x0, y0] = if zoom == 1.0 { [q.rect[0], q.rect[1]] } else {
                            std::array::from_fn(|k| origin[k] + (q.rect[k] - origin[k]) * zoom)
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
                            .map(|g| zoom_glyph(g, origin, zoom)));
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
            if let Some((_, radius)) = rounded {
                for instance in &mut self.instances[primitive_start..] { instance.round_clip(radius); }
                for surface in &mut self.surfaces[surface_start..] { surface.fade[3] = radius.to_bits(); }
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
                dy -= grid((offset - origin) as f32, s);
                if let Some(st) = self.scrolls.get_mut(&self.nodes[i].id) { st.held = held; }
            }
            let (mut scale, mut anchor, mut anchor_to, mut dx) = (sc, anchor, anchor_to, pinh.dx);
            if n.scroll_axes.x() && let Some(st) = self.scrolls.get(&n.id) {
                dx -= grid(st.x.offset as f32, s);
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
            self.inh_scratch[i] = Inh { opacity: effective_opacity, paint_opacity: o,
                clip: intersect(hit_clip, clip), fade: hit_fade, fade_origin: hit_origin,
                paint_clip: clip, paint_fade: fade, paint_fade_origin: fade_origin, rounded, scale, anchor, anchor_to, blur, dy, dx,
                cover: cover_offset, outer, mark, group,
                scale_motion: if cover.is_some() { ScaleMotion::Moving } else { scale_motion } };
            let hits_before = self.hits.len();
            if (n.vis.hit || n.on_pointer_move.is_some() || n.on_press.is_some() || n.on_double_press.is_some() || n.on_secondary_press.is_some() || n.on_hover.is_some() || n.on_hover_change.is_some() || n.on_drag.is_some() || n.drag_out.is_some() || n.input.is_some() || n.window_drag || n.focus.enabled || n.modal.is_some()
                || n.text.as_ref().is_some_and(|t| t.selectable))
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
                    self.overlay_hits.push(rect);
                }
            }
            if self.nodes[i].help.is_some() && !self.subtree_exiting(i) && effective_opacity > 0.001 && visible {
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
            crate::render::end_layer(&mut self.instances, &mut self.surfaces, &mut self.marks, start, surface_start);
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
        if self.native_overlay && let Some((x, y)) = self.pointer { self.refresh_hover(x, y); }
    }

    // ---------------------------------------------------------------------------------------
    // Inspection

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
                let rect = flight.map_or_else(|| self.drawn.get(i).copied().unwrap_or([p.rect.x, p.rect.y, p.rect.w, p.rect.h]),
                    |f| [f.presented.x, f.presented.y, f.presented.w, f.presented.h]);
                let s = self.env.scale;
                let clip = flight.map_or_else(|| n.parent.and_then(|pi| self.inh_scratch.get(pi)).map_or(NO_CLIP, |h| h.clip),
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
                let mut a = n.parent;
                while let Some(pi) = a {
                    let parent = &self.nodes[pi];
                    moving |= animating(parent.id);
                    live_intent |= intent(parent);
                    scrolling |= self.scrolls.get(&parent.id).is_some_and(|s| s.scrolling);
                    in_growing_row |= self.grows.get(&parent.id).is_some_and(|g| g.moving());
                    a = parent.parent;
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
                    text: n.text.as_ref().filter(|_| !n.input.as_ref().is_some_and(|i| i.edit.is_secure())).map(|t| t.source_text().to_owned()),
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
                    help: n.help.clone(),
                    selected: n.text.as_ref().filter(|t| t.selectable).and_then(|t| {
                        let (a, b) = selectable::range_in(&sel_ranges, i)?;
                        t.copy_slice(a as u32, b as u32)
                    }),
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
                help: None, selected: None,
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

    /// Whether the root's own fill is dark, when it has an opaque one (headless `--keys` caps
    /// follow it, so they flip when the app changes theme itself).
    pub(crate) fn backdrop_dark(&self) -> Option<bool> {
        if self.transparent { return None; }
        let n = self.nodes.first()?;
        let c = self.color(n.id, P_FILL, n.vis.fill);
        (c.a > 0.5).then(|| 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b < 0.25)
    }

    /// Enable or disable gamma-space (sRGB) blending for translucent fills, shadows, blurs,
    /// images and video.
    pub(crate) fn set_srgb_blend(&mut self, enabled: bool) {
        self.srgb_blend = enabled;
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
    let bb = Cursor::from_byte_index(layout, ci, Affinity::Downstream).geometry(layout, 1.5 * s);
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
    std::iter::successors(nodes[i].parent, |&p| nodes[p].parent)
        .filter_map(|p| scrolls.get(&nodes[p].id).map(|st| [st.x.offset as f32, st.offset as f32]))
}

/// Whether `looping`'s sampled value multiplies the element's opacity.
fn fades(looping: Option<rune_motion::LoopSpec>) -> bool {
    matches!(looping.map(|l| l.kind), Some(rune_motion::LoopKind::Opacity { .. } | rune_motion::LoopKind::OpacityCycle(_) | rune_motion::LoopKind::Sequence(_)))
}

fn merge(tx: &mut Tx, one: Tx) {
    tx.paste |= one.paste;
    if let Some(one) = one.trace {
        let trace = tx.trace.get_or_insert_with(Default::default);
        trace.events.extend(one.events);
        trace.channels.extend(one.channels);
    }
    tx.scroll_to.extend(one.scroll_to);
    tx.scroll_bottom.extend(one.scroll_bottom);
    tx.scroll_pages.extend(one.scroll_pages);
    tx.scroll_x.extend(one.scroll_x);
    tx.wakes.extend(one.wakes);
    tx.text_flights.extend(one.text_flights);
    tx.popups.extend(one.popups);
    if let Some(request) = one.window_frame { tx.window_frame = Some(tx.window_frame.unwrap_or_default().merge(request)); }
    if one.clear_secret && tx.copy.as_ref().is_some_and(crate::ClipboardWrite::is_secret) { tx.copy = None; }
    tx.clear_secret |= one.clear_secret;
    if one.copy.is_some() { tx.copy = one.copy; }
    tx.shell = tx.shell.max(one.shell);
    tx.minimize |= one.minimize;
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
    let cx = caret.geometry(layout, 1.5 * s).x0 as f32;
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
                let l = text.layout_rich(&spec.content, &spec.spans, spec.style, scale, w.map(|w| w * scale));
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
fn zoom_glyph(mut g: Instance, origin: [f32; 2], k: f32) -> Instance {
    if k == 1.0 { return g; }
    if g.params[1] == KIND_GLYPH && g.uv[2] == 0.0 { (g.uv[2], g.uv[3]) = (g.rect[2], g.rect[3]); }
    g.rect = scale_about(g.rect, origin, k);
    g
}

fn glyph(q: &GlyphQuad, fill: [f32; 4], gamma: f32, clip: [f32; 4], fade: [f32; 4]) -> Instance {
    Instance {
        rect: q.rect,
        radii: [0.0; 4],
        fill,
        border: [0.0; 4],
        params: [0.0, KIND_GLYPH, 0.0, gamma],
        uv: [q.uv[0], q.uv[1], 0.0, 0.0],
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
    in_row: bool,
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
        on_pointer_move,
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
        on_layout,
        on_screen_layout,
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
        material,
        aspect_fit,
        selection_group,
        ..
    } = el;
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
        on_pointer_move,
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
        on_layout,
        on_screen_layout,
        scroll_edges,
        growth,
        streaming,
        input,
        cursor,
        window_drag,
        selection_group,
        press_scale,
        static_scale,
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
        #[cfg(debug_assertions)]
        scroll_source,
    });
    let child_in_row = in_row || parent_is_stack;
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
        rt.queue.push(Action::Press(path));
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
                .child(el().key("card").size(200.0, 80.0).scale(s.k).child(crate::el::text("Hamburgefonstiv")))
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
            crate::text_input(s, crate::TextStyle::new(16.0, 400.0)).secure()
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
        rt.paste_result(None, zeroize::Zeroizing::new("late window reply".into())); rt.frame(0.0);
        assert!(rt.state.field.text.is_empty());
        assert_eq!(rt.state.pasted, "window fixture");
        let target = rt.paste_target();
        rt.paste_result(target, zeroize::Zeroizing::new("field fixture".into())); rt.frame(0.0);
        assert_eq!(rt.state.field.text, "field fixture");
        assert!(rt.events.iter().any(|e| e == "paste field"));
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
        fn view(s: &(bool, bool), cx: &mut Cx<(bool, bool)>) -> El {
            cx.reduced_motion(s.1);
            let mut track = crate::row().key("track").size(100.0, 8.0).clip_soft(0.0);
            if s.0 {
                track = track.child(el().key("bar").w_frac(0.25).h_full().fill(Color::WHITE)
                    .looping(rune_motion::LoopSpec::slide(1.0, [-1.0, 0.0], [4.0, 0.0])));
            }
            col().size(100.0, 100.0).child(track)
        }
        let bar = |rt: &Runtime<(bool, bool)>| {
            rt.elements().into_iter().find(|e| e.key.as_deref() == Some("bar")).map(|e| e.rect[0])
        };
        let mut rt = Runtime::new((true, false), view, (100.0, 100.0), Env::new(true, 2.0));
        assert!(rt.frame(0.0), "a slide starting outside its clip must still run");
        assert_eq!(bar(&rt), Some(-25.0));
        let mut xs = Vec::new();
        for _ in 0..128 { assert!(rt.frame(1.0 / 128.0)); xs.push(bar(&rt).unwrap()); }
        assert!((xs[63] - 37.5).abs() < 0.01, "halfway through the period is the sweep's midpoint: {}", xs[63]);
        assert!(xs[..127].windows(2).all(|w| w[1] >= w[0]));
        assert_eq!(xs[127], -25.0, "the sweep wraps");
        rt.state.0 = false; rt.invalidate();
        for _ in 0..240 { rt.frame(1.0 / 120.0); }
        assert!(!rt.animating);
        let mut rt = Runtime::new((true, true), view, (100.0, 100.0), Env::new(true, 2.0));
        assert!(!rt.frame(0.0));
        assert_eq!(bar(&rt), Some(37.5), "reduced motion holds the midpoint");
        let mut rt = Runtime::new((true, false), view, (100.0, 100.0), Env::new(true, 2.0));
        rt.frame(0.0);
        rt.set_live_visible(false);
        assert!(!rt.frame(1.0 / 120.0));
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
        rt.queue.push(Action::Press(button.clone()));
        rt.queue.push(Action::Key { ev: copy_key.clone(), repeat: false });
        rt.frame(1.0 / 120.0);
        assert_eq!(rt.copy_request.take().as_deref(), Some("héllo 👋"));
        rt.frame(1.0 / 120.0);
        assert!(rt.copy_request.is_none());
        rt.queue.push(Action::Key { ev: copy_key, repeat: false });
        rt.queue.push(Action::Press(button));
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
    }
}

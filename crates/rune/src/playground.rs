//! [`App::playground`](crate::App::playground): a storybook built into the window (⌘⇧D). Its
//! stories, grouped by type, are the app's scenes and its own [`Story`]s and pages. One shows
//! at a time on a plain stage, live and interactive, with a column beside it for that story's
//! triggers and controls. Nothing touches the app's real state.
//!
//! While it's open, handlers in the stage act on the stage's state: a fresh scene state, a
//! story's own state, or the real state for a [`Playground::page`]. Its own controls act on
//! [`Controls`], beside it, through [`crate::states::States`]. The real state keeps polling
//! throughout and is back, untouched, after Back.

use std::any::Any;
use std::time::Duration;

use crate::states::{States, StatesRef};
use crate::tok::{self, Theme, radius, type_};
use crate::{Color, Cx, El, Icon, Key, KeyEvent, OverlaySpec, ScrollSpec, Shortcut, Tx, col, el, icon, row, spacer, text};

/// The bar's height when the playground is closed, hairline included.
pub(crate) const BAR: f32 = 26.0;
const SIDEBAR: f32 = 200.0;
const SIDE: f32 = 264.0;
const HEADER: f32 = 38.0;
const BAND: f32 = 28.0;
const ROW: f32 = 28.0;
const CONTROL: f32 = 24.0;

mod live;
mod nav;
mod stage;
mod story;
mod testing;
mod variants;
mod view;

pub(crate) use live::Live;
use nav::Row;
pub(crate) use stage::call;
pub use stage::{Play, Step};
use stage::{Stage, Trigger, trigger};
pub use story::Story;
use story::OwnStory;
pub(crate) use testing::stage_element;

/// A storybook for one window, given to [`App::playground`](crate::App::playground):
///
/// ```ignore
/// App::new(spec).playground(
///     Playground::<State>::new()
///         .page("Lists/Live devices", devices)
///         .trigger("Lists/Many devices", 'a', "Add device", |s, _| s.devices.push("New"))
///         .story(Story::new("Progress/Upload", Upload::new, upload).trigger('r', "Run", run))
///         .sizes(&[("Compact", (360.0, 240.0)), ("Wide", (900.0, 600.0))]),
/// )
/// ```
///
/// Name the app's state, as `Playground::<State>::new()`, when a closure comes before anything
/// that does: `Playground::new().trigger(.., |s, _| ..)` can't infer it (E0282).
///
/// Its stories are the window's `AppSpec::scenes`, then its pages and [`Story`]s, listed under
/// their groups: a `"Group/Name"` name puts a story in a group, and the groups keep the order
/// they first appear in. Scenes without a group list under "Scenes", pages and stories under
/// "Pages".
///
/// A scene opens as a fresh `AppSpec::init` with the scene fn applied (`App::init`'s closure
/// isn't used), polled by `AppSpec::poll` but never given `AppSpec::setup`, so it gets no
/// waker and starts nothing; with [`Playground::fresh`] it starts from the real state instead. Back drops every stage state and returns to the real one.
///
/// Keys, while it's open: ↑↓ move along the list, group headers included, and J K between
/// stories (⌘↑ ⌘↓ to the first and last). On a header, ← folds its group, → unfolds it and ↵
/// toggles it, as a click on it does; groups stay folded until the playground closes. R
/// replays the story from the start, 1 to 9 pick its variant, a trigger's letter runs it, → or Tab move into the side
/// column (↑↓ inside it, ←, Tab or Esc back out), and Esc or ⌘⇧D go back to the app. They come
/// before the stage's `AppSpec::on_key`, except Esc, which the stage gets first. A focused
/// text field keeps all its keys, and so does an element focused in the stage, until Esc.
pub struct Playground<S: 'static> {
    pages: Vec<Page<S>>,
    triggers: Vec<(&'static str, Trigger<S>)>,
    sides: Vec<(&'static str, Side<S>)>,
    sizes: Vec<(&'static str, (f32, f32))>,
    shortcut: Option<Shortcut>,
    bar: bool,
    enabled: bool,
    scenes: Option<fn(&str) -> bool>,
    fresh: Option<fn(&S) -> S>,
    keep: Option<fn(&S, &mut S)>,
    variants: Vec<(&'static str, &'static str, fn(&mut S))>,
    typing: Option<fn(&S) -> bool>,
    dark: Option<fn(&S) -> bool>,
}

impl<S: 'static> Default for Playground<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: 'static> Playground<S> {
    /// No pages, a bar, ⌘⇧D (Ctrl+Shift+D off macOS), and enabled in debug builds only.
    pub fn new() -> Self {
        let shortcut = if cfg!(target_os = "macos") { "Shift+Cmd+D" } else { "Ctrl+Shift+D" };
        Playground {
            pages: Vec::new(),
            triggers: Vec::new(),
            sides: Vec::new(),
            sizes: Vec::new(),
            shortcut: shortcut.parse().ok(),
            bar: true,
            enabled: cfg!(debug_assertions),
            scenes: None,
            fresh: None,
            keep: None,
            variants: Vec::new(),
            typing: None,
            dark: None,
        }
    }

    /// A page drawn by `view` from the app's live state. Its handlers change the real state,
    /// and it has no triggers or replay.
    pub fn page(mut self, name: &'static str, view: fn(&S, &mut Cx<S>) -> El) -> Self {
        self.pages.push(Page { name, kind: Kind::Live(view) });
        self
    }

    /// A story with its own state: see [`Story`].
    pub fn story<P: 'static>(mut self, story: Story<P>) -> Self {
        assert!(story.real().is_none_or(|t| t == std::any::TypeId::of::<S>()), "Story::from_real({:?}) derives from another app state than this playground's", story.name);
        self.pages.push(Page { name: story.name, kind: Kind::Own(Box::new(story)) });
        self
    }

    /// A trigger for the scene named `scene` (its path, or just its name): `key` runs `run` on the scene's stage state, as
    /// its button in the side column does. An `'r'` trigger runs when the scene is chosen and on
    /// each replay, so its motion plays at once. See
    /// [`Story::trigger`].
    pub fn trigger(mut self, scene: &'static str, key: char, label: &'static str, run: Step<S>) -> Self {
        self.triggers.push((scene, trigger(key, label, run)));
        self
    }

    /// The app's own controls for the scene named `scene` (its path, or just its name), at the
    /// top of the side column, drawn from and acting on the scene's stage state: knobs, say.
    /// See [`Story::side`].
    pub fn side(mut self, scene: &'static str, view: fn(&S, &mut Cx<S>) -> El) -> Self {
        self.sides.push((scene, Side::Stage(view)));
        self
    }

    /// A variant of the scene named `scene` (its path, or just its name): `apply` runs on its
    /// state after the scene fn. See [`Story::variant`].
    pub fn variant(mut self, scene: &'static str, name: &'static str, apply: fn(&mut S)) -> Self {
        self.variants.push((scene, name, apply));
        self
    }

    /// [`Playground::side`], drawn from the stage and the real state both: to mark the app's
    /// own choice beside the one the stage previews, say, with a control that calls
    /// [`Tx::keep_playground`] to write the preview back through [`Playground::keep`].
    pub fn side_real(mut self, scene: &'static str, view: fn(&S, &S, &mut Cx<S>) -> El) -> Self {
        self.sides.push((scene, Side::Real(view)));
        self
    }

    /// Which of `AppSpec::scenes` the list shows, by name: `|name| name.contains('/')` keeps
    /// ungrouped scenes to `--scene`. By default, all of them.
    pub fn scenes(mut self, show: fn(&str) -> bool) -> Self {
        self.scenes = Some(show);
        self
    }

    /// Scenes start from `fresh(&real)` rather than `AppSpec::init`, then their scene fn applies:
    /// `|real| real.clone()` previews them on a copy of the app as it is, its waker and handles
    /// included: a wake from another thread polls the stage.
    pub fn fresh(mut self, fresh: fn(&S) -> S) -> Self {
        self.fresh = Some(fresh);
        self
    }

    /// Runs before a scene's stage goes, on R, another story or Back, with the stage and the
    /// real state: what it copies over stays in the app, and the next [`Playground::fresh`]
    /// starts from it, so tuning in the side column sticks.
    pub fn keep(mut self, keep: fn(&S, &mut S)) -> Self {
        self.keep = Some(keep);
        self
    }

    /// While `typing` is true of the stage's state (a scene's, or the real state on a page),
    /// `AppSpec::on_key` gets every key before the playground and focused fields do, letters
    /// and arrows included, and what it refuses goes on as usual: for a view that takes text
    /// itself. A focused Rune field needs none of this; it keeps its keys anyway.
    pub fn typing(mut self, typing: fn(&S) -> bool) -> Self {
        self.typing = Some(typing);
        self
    }

    /// Whether the app is dark, read from its real state, for the playground's bar and chrome
    /// and the stage's appearance until the switch beside it changes it: for an app with its own
    /// light and dark setting. By default, the system's.
    pub fn dark(mut self, dark: fn(&S) -> bool) -> Self {
        self.dark = Some(dark);
        self
    }

    /// Stage sizes offered beside "Window" (the stage fills what the playground leaves).
    pub fn sizes(mut self, sizes: &[(&'static str, (f32, f32))]) -> Self {
        self.sizes = sizes.to_vec();
        self
    }

    /// The shortcut that opens and closes the playground, or `None` for none: by default ⌘⇧D,
    /// Ctrl+Shift+D off macOS. On macOS it's the View menu's Playground item's.
    pub fn shortcut(mut self, shortcut: Option<Shortcut>) -> Self {
        self.shortcut = shortcut;
        self
    }

    /// The bar along the window's bottom edge that opens the playground. With it, the app lays
    /// out 26 points shorter, as in a window that much shorter. Without it the app's layout is
    /// untouched and the playground opens from its shortcut, the View menu's Playground item,
    /// or [`Tx::open_playground`].
    pub fn bar(mut self, on: bool) -> Self {
        self.bar = on;
        self
    }

    /// Whether the window offers the playground at all; by default only in debug builds.
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn shortcut_key(&self) -> Option<&Shortcut> {
        self.shortcut.as_ref()
    }
}

/// A scene's side column view, from [`Playground::side`] or [`Playground::side_real`].
enum Side<S: 'static> {
    Stage(fn(&S, &mut Cx<S>) -> El),
    Real(fn(&S, &S, &mut Cx<S>) -> El),
}

struct Page<S: 'static> {
    name: &'static str,
    kind: Kind<S>,
}

enum Kind<S: 'static> {
    Live(fn(&S, &mut Cx<S>) -> El),
    Own(Box<dyn OwnStory>),
}

/// A story's group and name, from its `"Group/Name"` path.
fn split(path: &'static str, default: &'static str) -> (&'static str, &'static str) {
    path.split_once('/').unwrap_or((default, path))
}

/// What a handler asks of the playground (`Tx::open_playground`, its keys and its controls),
/// applied at the start of the next commit, before the view runs against the new stage.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Request {
    Open(Option<String>),
    Close,
    Toggle,
    Select(usize),
    /// R: the stage's state made again, then its `'r'` trigger.
    Replay,
    /// [`Tx::keep_playground`].
    Keep,
    /// A digit: the open story's variant, by index, made as R makes it.
    Variant(usize),
    Trigger(char),
    Dark(bool),
    Size(usize),
    ReduceMotion(bool),
    /// A group's header becomes the list's selection; the stage keeps its story.
    Header(&'static str),
    /// Folds a group (`Some(true)`), unfolds it, or toggles it (`None`).
    Fold(&'static str, Option<bool>),
}

impl Request {
    /// Whether this one changes the stage's state, so handlers built for the old one must go.
    fn swaps_stage(&self) -> bool {
        matches!(self, Request::Open(_) | Request::Close | Request::Toggle | Request::Select(_) | Request::Replay | Request::Variant(_))
    }
}

/// What a key does in the open playground, for the runtime to carry out.
pub(crate) enum Nav {
    Pass,
    Request(Request),
    /// Focus the side column's first control.
    IntoSide,
    /// Focus leaves the side column.
    OutOfSide,
    /// Focus moves this many controls along the side column.
    SideStep(isize),
}

/// The playground's own controls' state. Its controls change the playground through requests.
pub(crate) struct Controls;

/// Shows `playground` in `rt`'s window when this run offers it: an enabled playground, in a
/// window unless `RUNE_PLAYGROUND=0`, or wherever `--playground` asks for it, headless included,
/// opened at the story it names, on the variant `--variant` names.
pub(crate) fn attach<S: 'static>(rt: &mut crate::Runtime<S>, playground: Option<Playground<S>>, spec: &crate::AppSpec<S>, open: Option<Option<String>>, variant: Option<String>, windowed: bool) {
    let Some(playground) = playground.filter(Playground::is_enabled) else { return };
    if open.is_none() && (!windowed || hidden_by_env()) { return; }
    let mut live = Live::new(playground, spec, windowed && cfg!(target_os = "macos"));
    if let Some(name) = open {
        let request = live.open_at(name.as_deref(), variant.as_deref()).unwrap_or_else(|e| crate::tooling::die(&e));
        live.pending.push(request);
    }
    rt.playground = Some(Box::new(live));
}

/// `--playground [NAME]`'s optional story name, taking it when the next argument isn't a flag.
pub(crate) fn arg(argv: &[String], i: &mut usize) -> Option<String> {
    let name = argv.get(*i + 1).filter(|v| !v.starts_with("--")).cloned();
    if name.is_some() { *i += 1; }
    name
}

fn hidden_by_env() -> bool {
    std::env::var("RUNE_PLAYGROUND").is_ok_and(|v| v == "0")
}

pub(crate) fn offered_by_env() -> bool {
    !hidden_by_env()
}

impl Tx {
    /// Show [`App::playground`](crate::App::playground) in this window. Without one, or in a
    /// run that doesn't show it (a release build, `RUNE_PLAYGROUND=0`, a headless render
    /// without `--playground`), nothing happens.
    pub fn open_playground(&mut self) {
        self.playground.push(Request::Open(None));
    }

    /// Back to the app from the playground, as its Back button does.
    pub fn close_playground(&mut self) {
        self.playground.push(Request::Close);
    }

    /// Runs [`Playground::keep`] now, with the open scene's stage and the real state, as a
    /// switch would before the stage goes. The stage stays.
    pub fn keep_playground(&mut self) {
        self.playground.push(Request::Keep);
    }

    pub fn toggle_playground(&mut self) {
        self.playground.push(Request::Toggle);
    }
}

/// Whether `key` is the stage's, around the story's root.
pub(crate) fn is_stage(key: &str) -> bool {
    key.strip_prefix("playground-stage-").is_some_and(|g| g.parse::<u32>().is_ok())
}

#[cfg(test)]
#[path = "playground_tests.rs"]
mod tests;

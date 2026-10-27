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

mod nav;
mod stage;
mod story;
mod variants;
mod view;

use nav::Row;
pub(crate) use stage::call;
pub use stage::{Play, Step};
use stage::{Stage, Trigger, trigger};
pub use story::Story;
use story::OwnStory;

/// A storybook for one window, given to [`App::playground`](crate::App::playground):
///
/// ```ignore
/// App::new(spec).playground(
///     Playground::new()
///         .page("Lists/Live devices", devices)
///         .trigger("Lists/Many devices", 'a', "Add device", |s, _| s.devices.push("New"))
///         .story(Story::new("Progress/Upload", Upload::new, upload).trigger('r', "Run", run))
///         .sizes(&[("Compact", (360.0, 240.0)), ("Wide", (900.0, 600.0))]),
/// )
/// ```
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

/// A window's playground while it runs.
pub(crate) struct Live<S: 'static> {
    pages: Vec<Page<S>>,
    /// By the scene they're for.
    triggers: Vec<(usize, Trigger<S>)>,
    sides: Vec<(usize, Side<S>)>,
    variants: Vec<(usize, &'static str, fn(&mut S))>,
    /// Each story's variant, by its index into scenes then pages, until the playground closes.
    variant: Vec<usize>,
    sizes: Vec<(&'static str, (f32, f32))>,
    shortcut: Option<Shortcut>,
    /// The View menu's item takes the shortcut, so keys here leave it alone.
    menu: bool,
    bar: bool,
    init: fn() -> S,
    fresh: Option<fn(&S) -> S>,
    keep: Option<fn(&S, &mut S)>,
    typing: Option<fn(&S) -> bool>,
    /// [`Playground::dark`].
    app_dark: Option<fn(&S) -> bool>,
    scenes: &'static [(&'static str, fn(&mut S))],
    poll: Option<fn(&mut S, f64, &mut Tx) -> bool>,
    /// Stories, as indices into scenes then pages, in the order the list shows them.
    order: Vec<usize>,
    open: bool,
    /// Into scenes then pages.
    selected: usize,
    /// A group header is the list's selection rather than `selected`'s row.
    header: Option<&'static str>,
    /// Groups whose stories the list hides, until the playground closes.
    folded: Vec<&'static str>,
    scene: Option<Box<Stage<S>>>,
    owned: Vec<Option<Box<dyn Any>>>,
    controls: Controls,
    dark: Option<bool>,
    size: usize,
    reduce_motion: bool,
    pending: Vec<Request>,
    /// The clock at the last commit or poll, which triggers start from.
    now: f64,
    /// Bumped by every select and replay, so the new stage's elements start fresh: scroll
    /// offsets, focus and animations don't carry over from the last one.
    generation: u32,
    /// The node the app's view returned, which holds the window's root handlers.
    pub(crate) root: usize,
    /// The time scale the app had when the playground opened, which every new stage starts
    /// from and Back restores.
    pub(crate) app_time_scale: f32,
}

/// Shows `playground` in `rt`'s window when this run offers it: an enabled playground, in a
/// window unless `RUNE_PLAYGROUND=0`, or wherever `--playground` asks for it, headless included,
/// opened at the story it names.
pub(crate) fn attach<S: 'static>(rt: &mut crate::Runtime<S>, playground: Option<Playground<S>>, spec: &crate::AppSpec<S>, open: Option<Option<String>>, windowed: bool) {
    let Some(playground) = playground.filter(Playground::is_enabled) else { return };
    if open.is_none() && (!windowed || hidden_by_env()) { return; }
    let mut live = Live::new(playground, spec, windowed && cfg!(target_os = "macos"));
    if let Some(name) = open {
        if let Some(name) = &name && live.find(name).is_none() {
            let names: Vec<_> = live.order.iter().map(|&i| live.path(i)).collect();
            crate::tooling::die(&format!("no story {name:?} in the playground; it has: {}", names.join(", ")));
        }
        live.pending.push(Request::Open(name));
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

impl<S: 'static> Live<S> {
    fn new(playground: Playground<S>, spec: &crate::AppSpec<S>, menu: bool) -> Self {
        let Playground { pages, triggers, sides, sizes, shortcut, bar, enabled: _, scenes: show, fresh, keep, variants, typing, dark: app_dark } = playground;
        let mut live = Live {
            owned: pages.iter().map(|_| None).collect(),
            pages,
            triggers: Vec::new(),
            sides: Vec::new(),
            variants: Vec::new(),
            variant: Vec::new(),
            sizes,
            shortcut,
            menu,
            bar,
            init: spec.init,
            fresh,
            keep,
            typing,
            app_dark,
            scenes: spec.scenes,
            poll: spec.poll,
            order: Vec::new(),
            open: false,
            selected: 0,
            header: None,
            folded: Vec::new(),
            scene: None,
            controls: Controls,
            dark: None,
            size: 0,
            reduce_motion: false,
            pending: Vec::new(),
            now: 0.0,
            generation: 0,
            root: 0,
            app_time_scale: 1.0,
        };
        // Scene names as `find` takes them: a path or a name, matching case if it can.
        let scene = |live: &Self, name: &str| {
            let at = live.find_in(name, &(0..live.scenes.len()).collect::<Vec<_>>());
            if at.is_none() { eprintln!("rune: the playground has a trigger or side column for {name:?}, which isn't one of AppSpec::scenes"); }
            at
        };
        live.triggers = triggers.into_iter().filter_map(|(name, t)| Some((scene(&live, name)?, t))).collect();
        live.sides = sides.into_iter().filter_map(|(name, view)| Some((scene(&live, name)?, view))).collect();
        live.variants = variants.into_iter().filter_map(|(name, variant, apply)| Some((scene(&live, name)?, variant, apply))).collect();
        live.variant = vec![0; live.count()];
        let listed: Vec<usize> = (0..live.count()).filter(|&i| live.scenes.get(i).is_none_or(|(name, _)| show.is_none_or(|show| show(name)))).collect();
        let mut groups: Vec<&str> = Vec::new();
        for &i in &listed {
            let group = live.group(i);
            if !groups.contains(&group) { groups.push(group); }
        }
        live.order = groups.iter().flat_map(|g| listed.iter().copied().filter(|&i| live.group(i) == *g)).collect();
        live.selected = live.order.first().copied().unwrap_or(0);
        live
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    fn count(&self) -> usize {
        self.scenes.len() + self.pages.len()
    }

    fn path(&self, i: usize) -> &'static str {
        match self.scenes.get(i) {
            Some((name, _)) => name,
            None => self.pages[i - self.scenes.len()].name,
        }
    }

    fn group(&self, i: usize) -> &'static str {
        split(self.path(i), if i < self.scenes.len() { "Scenes" } else { "Pages" }).0
    }

    fn name(&self, i: usize) -> &'static str {
        split(self.path(i), "").1
    }

    /// The listed story called `name`: its path, or just its name, matching case only if need be.
    fn find(&self, name: &str) -> Option<usize> {
        let mut listed = self.order.clone();
        listed.sort_unstable();
        self.find_in(name, &listed)
    }

    /// [`Live::find`] among `ids`, in order.
    fn find_in(&self, name: &str, ids: &[usize]) -> Option<usize> {
        let exact = |i: &&usize| self.path(**i) == name || self.name(**i) == name;
        let loose = |i: &&usize| self.path(**i).eq_ignore_ascii_case(name) || self.name(**i).eq_ignore_ascii_case(name);
        ids.iter().find(exact).or_else(|| ids.iter().find(loose)).copied()
    }

    fn page(&self) -> Option<(usize, &Page<S>)> {
        let i = self.selected.checked_sub(self.scenes.len())?;
        self.pages.get(i).map(|p| (i, p))
    }

    fn own(&self) -> Option<(&dyn OwnStory, &dyn Any)> {
        let (i, page) = self.page()?;
        let Kind::Own(own) = &page.kind else { return None };
        Some((own.as_ref(), self.owned[i].as_deref()?))
    }

    fn is_live_page(&self) -> bool {
        matches!(self.page(), Some((_, Page { kind: Kind::Live(_), .. })))
    }

    /// The open story's triggers, R's first.
    fn triggers(&self) -> Vec<(char, &'static str)> {
        if self.is_live_page() { return Vec::new(); }
        let mut keys = match (self.scenes.get(self.selected), self.own()) {
            (Some(_), _) => self.triggers.iter().filter(|(s, _)| *s == self.selected).map(|(_, t)| (t.key, t.label)).collect(),
            (None, Some((own, _))) => own.triggers(),
            (None, None) => Vec::new(),
        };
        let replay = keys.iter().position(|(k, _)| *k == 'r').map_or((('r'), "Replay"), |i| keys.remove(i));
        keys.insert(0, replay);
        keys
    }

    pub(crate) fn request(&mut self, requests: Vec<Request>) {
        self.pending.extend(requests);
    }

    pub(crate) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub(crate) fn pending_swaps_stage(&self) -> bool {
        self.pending.iter().any(Request::swaps_stage)
    }

    /// Whether the only requests that swap the stage are replays, which keep its story.
    pub(crate) fn pending_replays_only(&self) -> bool {
        self.pending.iter().filter(|r| r.swaps_stage()).all(|r| matches!(r, Request::Replay | Request::Variant(_)))
    }

    /// Applies the requests handlers made since the last commit; true when anything changed.
    pub(crate) fn apply(&mut self, clock: f64, real: &mut S) -> bool {
        self.now = clock;
        let mut changed = false;
        for request in std::mem::take(&mut self.pending) {
            changed = true;
            match request {
                Request::Open(name) => {
                    let at = name.as_deref().and_then(|n| self.find(n));
                    if !self.open || at.is_some() {
                        self.open = true;
                        if let Some(i) = at { self.reveal(i); }
                        self.select(at.unwrap_or(self.selected), real);
                    }
                }
                Request::Close => self.close(real),
                Request::Toggle if self.open => self.close(real),
                Request::Toggle => {
                    self.open = true;
                    self.select(self.selected, real);
                }
                Request::Select(i) => {
                    self.reveal(i);
                    self.select(i, real);
                }
                Request::Replay => self.replay(real),
                Request::Variant(n) if n < self.variants().len() => {
                    self.variant[self.selected] = n;
                    self.replay(real);
                }
                Request::Variant(_) => {}
                Request::Keep => self.keep(real),
                Request::Trigger(key) => self.trigger(key),
                Request::Dark(dark) => self.dark = Some(dark),
                Request::Size(i) => self.size = i,
                Request::ReduceMotion(on) => self.reduce_motion = on,
                Request::Header(group) => self.header = Some(group),
                Request::Fold(group, fold) => self.fold(group, fold),
            }
        }
        changed
    }

    fn fold(&mut self, group: &'static str, fold: Option<bool>) {
        let folded = self.folded.contains(&group);
        match fold.unwrap_or(!folded) {
            true if !folded => {
                self.folded.push(group);
                // The selected story's row goes, so its header takes the selection.
                if self.header.is_none() && self.selected < self.count() && self.group(self.selected) == group { self.header = Some(group); }
            }
            false => self.folded.retain(|g| *g != group),
            true => {}
        }
    }

    /// Story `i`'s row becomes the list's selection, its group unfolded.
    fn reveal(&mut self, i: usize) {
        self.header = None;
        if i < self.count() {
            let group = self.group(i);
            self.folded.retain(|g| *g != group);
        }
    }

    /// The open story's stage made again, then its `'r'` trigger.
    fn replay(&mut self, real: &mut S) {
        if self.is_live_page() { return; }
        if let Some((i, _)) = self.page() { self.owned[i] = None; }
        self.select(self.selected, real);
    }

    /// [`Playground::keep`], from the scene's stage as it goes.
    fn keep(&mut self, real: &mut S) {
        if let (Some(keep), Some(scene)) = (self.keep, &self.scene) { keep(&scene.state, real); }
    }

    fn close(&mut self, real: &mut S) {
        self.keep(real);
        self.open = false;
        self.header = None;
        self.folded.clear();
        self.variant.iter_mut().for_each(|v| *v = 0);
        self.scene = None;
        self.owned.iter_mut().for_each(|o| *o = None);
    }

    /// Shows story `i`. A stage made new, as a scene's always is, runs its `'r'` trigger.
    fn select(&mut self, i: usize, real: &mut S) {
        self.keep(real);
        if let Some(left) = self.selected.checked_sub(self.scenes.len())
            && let (Some(Page { kind: Kind::Own(own), .. }), Some(Some(stage))) = (self.pages.get(left), self.owned.get_mut(left))
        {
            own.stop(stage.as_mut());
        }
        self.selected = i.min(self.count().saturating_sub(1));
        self.scene = None;
        self.generation += 1;
        let scene_clock = crate::SceneClock { now: self.now, dt: 1.0 / 60.0 };
        if let Some((_, f)) = self.scenes.get(self.selected) {
            let (init, fresh, variant) = (self.init, self.fresh, self.scene_variant());
            self.scene = Some(Box::new(Stage::new(crate::with_scene_clock(scene_clock, || {
                let mut s = fresh.map_or_else(init, |fresh| fresh(real));
                f(&mut s);
                if let Some(variant) = variant { variant(&mut s); }
                s
            }))));
        } else if let Some((i, page)) = self.page() && let Kind::Own(own) = &page.kind && self.owned[i].is_none() {
            let variant = self.variant[self.selected];
            let stage = crate::with_scene_clock(scene_clock, || own.init(&*real, variant));
            self.owned[i] = Some(stage);
        } else {
            return;
        }
        self.trigger('r');
    }

    /// Queues the open story's trigger on `key`, to run at the next poll from now.
    fn trigger(&mut self, key: char) {
        if !self.open { return; }
        let now = self.now;
        if let Some(scene) = &mut self.scene {
            if let Some((_, t)) = self.triggers.iter().find(|(s, t)| *s == self.selected && t.key == key) {
                scene.script.push((now, t.run));
            }
            return;
        }
        let Some(i) = self.selected.checked_sub(self.scenes.len()) else { return };
        if let (Some(Page { kind: Kind::Own(own), .. }), Some(Some(stage))) = (self.pages.get(i), self.owned.get_mut(i)) {
            own.trigger(stage.as_mut(), key, now);
        }
    }

    /// Whether `ev` is the shortcut that opens and closes the playground, where no menu item
    /// takes it first.
    pub(crate) fn is_shortcut(&self, ev: &KeyEvent) -> bool {
        !self.menu && self.shortcut.as_ref().is_some_and(|s| s.matches_key(ev))
    }

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

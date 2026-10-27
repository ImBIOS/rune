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

/// A step a trigger runs, now or later on the logical clock: see [`Play::after`].
pub type Step<X> = fn(&mut X, &mut Play<'_, X>);

/// What a trigger's steps get beside their state: the frame's [`Tx`] (`Play` derefs to it) and
/// [`Play::after`] for the steps that follow.
pub struct Play<'a, X> {
    tx: &'a mut Tx,
    script: &'a mut Vec<(f64, Step<X>)>,
    now: f64,
}

impl<X> Play<'_, X> {
    /// Runs `step` `secs` after this one, on the runtime's logical clock. Times count from when
    /// this step was due, not from the frame that ran it, so a replay runs the same every time,
    /// headless runs included.
    pub fn after(&mut self, secs: f64, step: Step<X>) {
        self.script.push((self.now + secs.max(0.0), step));
    }

    /// When this step was due, on the runtime's clock.
    pub fn now(&self) -> f64 {
        self.now
    }
}

impl<X> std::ops::Deref for Play<'_, X> {
    type Target = Tx;
    fn deref(&self) -> &Tx {
        self.tx
    }
}

impl<X> std::ops::DerefMut for Play<'_, X> {
    fn deref_mut(&mut self) -> &mut Tx {
        self.tx
    }
}

/// A trigger: a step on a single-letter key, shown as a button in the side column.
struct Trigger<X> {
    key: char,
    label: &'static str,
    run: Step<X>,
}

fn trigger<X>(key: char, label: &'static str, run: Step<X>) -> Trigger<X> {
    let key = key.to_ascii_lowercase();
    assert!(!matches!(key, 'j' | 'k'), "J and K move between stories; give the {label:?} trigger another key");
    Trigger { key, label, run }
}

/// A stage's state and its triggers' queued steps.
struct Stage<X> {
    state: X,
    script: Vec<(f64, Step<X>)>,
}

impl<X> Stage<X> {
    fn new(state: X) -> Self {
        Stage { state, script: Vec::new() }
    }

    /// Runs the steps due by `t`, earliest first, each seeing the time it was due. Asks for a
    /// frame when the next one is due.
    fn run(&mut self, t: f64, tx: &mut Tx) -> bool {
        let mut ran = false;
        // Bounded, in case steps keep queueing more with no delay.
        for _ in 0..64 {
            let due = self.script.iter().enumerate().filter(|(_, (at, _))| *at <= t).min_by(|a, b| a.1.0.total_cmp(&b.1.0)).map(|(i, _)| i);
            let Some(i) = due else { break };
            let (at, step) = self.script.remove(i);
            step(&mut self.state, &mut Play { tx: &mut *tx, script: &mut self.script, now: at });
            ran = true;
        }
        if let Some(next) = self.script.iter().map(|(at, _)| *at).min_by(f64::total_cmp) {
            tx.wake_after(Duration::from_secs_f64((next - t).max(0.0)));
        }
        ran
    }
}

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
/// waker and starts nothing. Back drops every stage state and returns to the real one.
///
/// Keys, while it's open: ↑↓ or J K move between stories (⌘↑ ⌘↓ to the first and last), R
/// replays the story from the start, a trigger's letter runs it, → or Tab move into the side
/// column (↑↓ inside it, Tab or Esc back out), and Esc or ⌘⇧D go back to the app. They come
/// before the stage's `AppSpec::on_key`, except Esc, which the stage gets first. A focused
/// text field keeps all its keys.
pub struct Playground<S: 'static> {
    pages: Vec<Page<S>>,
    triggers: Vec<(&'static str, Trigger<S>)>,
    sides: Vec<(&'static str, fn(&S, &mut Cx<S>) -> El)>,
    sizes: Vec<(&'static str, (f32, f32))>,
    shortcut: Option<Shortcut>,
    bar: bool,
    enabled: bool,
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
        self.pages.push(Page { name: story.name, kind: Kind::Own(Box::new(story)) });
        self
    }

    /// A trigger for the scene named `scene`: `key` runs `run` on the scene's stage state, as
    /// its button in the side column does. An `'r'` trigger runs after each replay. See
    /// [`Story::trigger`].
    pub fn trigger(mut self, scene: &'static str, key: char, label: &'static str, run: Step<S>) -> Self {
        self.triggers.push((scene, trigger(key, label, run)));
        self
    }

    /// The app's own controls for the scene named `scene`, at the top of the side column, drawn
    /// from and acting on the scene's stage state: knobs, say. See [`Story::side`].
    pub fn side(mut self, scene: &'static str, view: fn(&S, &mut Cx<S>) -> El) -> Self {
        self.sides.push((scene, view));
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

/// A story with its own state, made by `init` when it first shows and kept, timers and
/// animations included, until the playground closes. R makes it again. Handlers built for `P`
/// change it.
///
/// ```ignore
/// Story::new("Progress/Upload", Upload::new, upload_view)
///     .poll(Upload::tick)
///     .trigger('r', "Run", |u, play| play.after(0.5, |u, _| u.done = true))
///     .trigger('f', "Finish", |u, _| u.done = true)
///     .side(upload_knobs)
/// ```
pub struct Story<P: 'static> {
    name: &'static str,
    init: fn() -> P,
    view: fn(&P, &mut Cx<P>) -> El,
    poll: Option<fn(&mut P, f64, &mut Tx) -> bool>,
    frame: Option<(f32, f32)>,
    triggers: Vec<Trigger<P>>,
    side: Option<fn(&P, &mut Cx<P>) -> El>,
}

impl<P: 'static> Story<P> {
    /// `name` may be a `"Group/Name"` path.
    pub fn new(name: &'static str, init: fn() -> P, view: fn(&P, &mut Cx<P>) -> El) -> Self {
        Story { name, init, view, poll: None, frame: None, triggers: Vec::new(), side: None }
    }

    /// An overlay's view at its size, in a dashed frame over the stage (the overlay's clear
    /// parts stay clear), with a state made by `init`.
    pub fn overlay(name: &'static str, spec: &OverlaySpec<P>, init: fn() -> P) -> Self {
        Story { frame: Some(spec.size), ..Story::new(name, init, spec.view) }
    }

    /// Called every frame while the story shows, as `AppSpec::poll` is, after its due steps:
    /// true when it changed the state.
    pub fn poll(mut self, poll: fn(&mut P, f64, &mut Tx) -> bool) -> Self {
        self.poll = Some(poll);
        self
    }

    /// A trigger: `key` (a letter, or `' '` for Space) runs `run` on the story's state, as its
    /// button in the side column does. [`Play::after`] queues the steps that follow, on the
    /// logical clock. R always replays: the state is made again, then an `'r'` trigger, if
    /// there is one, runs. J and K are taken.
    pub fn trigger(mut self, key: char, label: &'static str, run: Step<P>) -> Self {
        self.triggers.push(trigger(key, label, run));
        self
    }

    /// The app's own controls for this story, at the top of the side column, drawn from and
    /// acting on its state: knobs, say.
    pub fn side(mut self, view: fn(&P, &mut Cx<P>) -> El) -> Self {
        self.side = Some(view);
        self
    }

    fn stage(stage: &dyn Any) -> &Stage<P> {
        stage.downcast_ref().expect("a story's stage is its own")
    }

    fn stage_mut(stage: &mut dyn Any) -> &mut Stage<P> {
        stage.downcast_mut().expect("a story's stage is its own")
    }
}

struct Page<S: 'static> {
    name: &'static str,
    kind: Kind<S>,
}

enum Kind<S: 'static> {
    Live(fn(&S, &mut Cx<S>) -> El),
    Own(Box<dyn OwnStory>),
}

/// A [`Story`] with its state type erased. Its stage is a `Stage<P>`.
trait OwnStory {
    fn init(&self) -> Box<dyn Any>;
    fn state<'a>(&self, stage: &'a mut dyn Any) -> &'a mut dyn Any;
    fn state_ref<'a>(&self, stage: &'a dyn Any) -> &'a dyn Any;
    fn view(&self, stage: &dyn Any, cx: &mut Cx<()>) -> El;
    fn side(&self, stage: &dyn Any, cx: &mut Cx<()>) -> Option<El>;
    fn poll(&self, stage: &mut dyn Any, t: f64, tx: &mut Tx) -> bool;
    fn polls(&self, stage: &dyn Any) -> bool;
    /// Queues the trigger on `key` at `now`; false when there's none.
    fn trigger(&self, stage: &mut dyn Any, key: char, now: f64) -> bool;
    /// Drops the steps still queued, so a story left part way doesn't burst on return.
    fn stop(&self, stage: &mut dyn Any);
    fn triggers(&self) -> Vec<(char, &'static str)>;
    fn frame(&self) -> Option<(f32, f32)>;
}

impl<P: 'static> OwnStory for Story<P> {
    fn init(&self) -> Box<dyn Any> {
        Box::new(Stage::new((self.init)()))
    }
    fn state<'a>(&self, stage: &'a mut dyn Any) -> &'a mut dyn Any {
        &mut Self::stage_mut(stage).state
    }
    fn state_ref<'a>(&self, stage: &'a dyn Any) -> &'a dyn Any {
        &Self::stage(stage).state
    }
    fn view(&self, stage: &dyn Any, cx: &mut Cx<()>) -> El {
        let mut own = cx.retype::<P>();
        let root = (self.view)(&Self::stage(stage).state, &mut own);
        cx.absorb(own);
        root
    }
    fn side(&self, stage: &dyn Any, cx: &mut Cx<()>) -> Option<El> {
        let side = self.side?;
        let mut own = cx.retype::<P>();
        let root = side(&Self::stage(stage).state, &mut own);
        cx.absorb(own);
        Some(root)
    }
    fn poll(&self, stage: &mut dyn Any, t: f64, tx: &mut Tx) -> bool {
        let stage = Self::stage_mut(stage);
        let ran = stage.run(t, tx);
        self.poll.is_some_and(|poll| poll(&mut stage.state, t, tx)) || ran
    }
    fn polls(&self, stage: &dyn Any) -> bool {
        self.poll.is_some() || !Self::stage(stage).script.is_empty()
    }
    fn trigger(&self, stage: &mut dyn Any, key: char, now: f64) -> bool {
        let Some(t) = self.triggers.iter().find(|t| t.key == key) else { return false };
        Self::stage_mut(stage).script.push((now, t.run));
        true
    }
    fn stop(&self, stage: &mut dyn Any) {
        Self::stage_mut(stage).script.clear();
    }
    fn triggers(&self) -> Vec<(char, &'static str)> {
        self.triggers.iter().map(|t| (t.key, t.label)).collect()
    }
    fn frame(&self) -> Option<(f32, f32)> {
        self.frame
    }
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
    Trigger(char),
    Dark(bool),
    Size(usize),
    ReduceMotion(bool),
}

impl Request {
    /// Whether this one changes the stage's state, so handlers built for the old one must go.
    fn swaps_stage(&self) -> bool {
        matches!(self, Request::Open(_) | Request::Close | Request::Toggle | Request::Select(_) | Request::Replay)
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
    triggers: Vec<(&'static str, Trigger<S>)>,
    sides: Vec<(&'static str, fn(&S, &mut Cx<S>) -> El)>,
    sizes: Vec<(&'static str, (f32, f32))>,
    shortcut: Option<Shortcut>,
    /// The View menu's item takes the shortcut, so keys here leave it alone.
    menu: bool,
    bar: bool,
    init: fn() -> S,
    scenes: &'static [(&'static str, fn(&mut S))],
    poll: Option<fn(&mut S, f64, &mut Tx) -> bool>,
    /// Stories, as indices into scenes then pages, in the order the list shows them.
    order: Vec<usize>,
    open: bool,
    /// Into scenes then pages.
    selected: usize,
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

fn hidden_by_env() -> bool {
    std::env::var("RUNE_PLAYGROUND").is_ok_and(|v| v == "0")
}

pub(crate) fn offered_by_env() -> bool {
    !hidden_by_env()
}

impl<S: 'static> Live<S> {
    fn new(playground: Playground<S>, spec: &crate::AppSpec<S>, menu: bool) -> Self {
        let Playground { pages, triggers, sides, sizes, shortcut, bar, enabled: _ } = playground;
        for name in triggers.iter().map(|(n, _)| n).chain(sides.iter().map(|(n, _)| n)) {
            if !spec.scenes.iter().any(|(s, _)| s == name) {
                eprintln!("rune: the playground has a trigger or side column for {name:?}, which isn't one of AppSpec::scenes");
            }
        }
        let mut live = Live {
            owned: pages.iter().map(|_| None).collect(),
            pages,
            triggers,
            sides,
            sizes,
            shortcut,
            menu,
            bar,
            init: spec.init,
            scenes: spec.scenes,
            poll: spec.poll,
            order: Vec::new(),
            open: false,
            selected: 0,
            scene: None,
            controls: Controls,
            dark: None,
            size: 0,
            reduce_motion: false,
            pending: Vec::new(),
            now: 0.0,
            generation: 0,
        };
        let mut groups: Vec<&str> = Vec::new();
        for i in 0..live.count() {
            let group = live.group(i);
            if !groups.contains(&group) { groups.push(group); }
        }
        live.order = groups.iter().flat_map(|g| (0..live.count()).filter(|&i| live.group(i) == *g).collect::<Vec<_>>()).collect();
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

    /// The story called `name`: its path, or just its name, matching case only if need be.
    fn find(&self, name: &str) -> Option<usize> {
        let exact = |i: &usize| self.path(*i) == name || self.name(*i) == name;
        let loose = |i: &usize| self.path(*i).eq_ignore_ascii_case(name) || self.name(*i).eq_ignore_ascii_case(name);
        (0..self.count()).find(exact).or_else(|| (0..self.count()).find(loose))
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
            (Some((name, _)), _) => self.triggers.iter().filter(|(s, _)| s == name).map(|(_, t)| (t.key, t.label)).collect(),
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

    /// Applies the requests handlers made since the last commit; true when anything changed.
    pub(crate) fn apply(&mut self, clock: f64) -> bool {
        self.now = clock;
        let mut changed = false;
        for request in std::mem::take(&mut self.pending) {
            changed = true;
            match request {
                Request::Open(name) => {
                    let at = name.as_deref().and_then(|n| self.find(n));
                    if !self.open || at.is_some() {
                        self.open = true;
                        self.select(at.unwrap_or(self.selected));
                    }
                }
                Request::Close => self.close(),
                Request::Toggle if self.open => self.close(),
                Request::Toggle => {
                    self.open = true;
                    self.select(self.selected);
                }
                Request::Select(i) => self.select(i),
                Request::Replay if self.is_live_page() => {}
                Request::Replay => {
                    if let Some((i, _)) = self.page() { self.owned[i] = None; }
                    self.select(self.selected);
                    self.trigger('r');
                }
                Request::Trigger(key) => self.trigger(key),
                Request::Dark(dark) => self.dark = Some(dark),
                Request::Size(i) => self.size = i,
                Request::ReduceMotion(on) => self.reduce_motion = on,
            }
        }
        changed
    }

    fn close(&mut self) {
        self.open = false;
        self.scene = None;
        self.owned.iter_mut().for_each(|o| *o = None);
    }

    fn select(&mut self, i: usize) {
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
            let init = self.init;
            self.scene = Some(Box::new(Stage::new(crate::with_scene_clock(scene_clock, || {
                let mut s = init();
                f(&mut s);
                s
            }))));
        } else if let Some((i, page)) = self.page() && let Kind::Own(own) = &page.kind && self.owned[i].is_none() {
            let stage = crate::with_scene_clock(scene_clock, || own.init());
            self.owned[i] = Some(stage);
        }
    }

    /// Queues the open story's trigger on `key`, to run at the next poll from now.
    fn trigger(&mut self, key: char) {
        if !self.open { return; }
        let now = self.now;
        if let Some(scene) = &mut self.scene {
            let name = self.scenes[self.selected].0;
            if let Some((_, t)) = self.triggers.iter().find(|(s, t)| *s == name && t.key == key) {
                scene.script.push((now, t.run));
            }
            return;
        }
        let Some(i) = self.selected.checked_sub(self.scenes.len()) else { return };
        if let (Some(Page { kind: Kind::Own(own), .. }), Some(Some(stage))) = (self.pages.get(i), self.owned.get_mut(i)) {
            own.trigger(stage.as_mut(), key, now);
        }
    }

    /// What one handler call can reach: the stage's state, or the app's while closed (or on a
    /// live page), and the playground's controls.
    pub(crate) fn states<'a>(&'a mut self, app: &'a mut S) -> States<'a> {
        let app = crate::runtime::handler_state(app);
        if !self.open { return States::two(app, &mut self.controls); }
        if let Some(scene) = &mut self.scene { return States::two(&mut scene.state, &mut self.controls); }
        let Some(i) = self.selected.checked_sub(self.scenes.len()) else { return States::two(app, &mut self.controls) };
        match (self.pages.get(i), self.owned.get_mut(i)) {
            (Some(Page { kind: Kind::Own(own), .. }), Some(Some(stage))) => States::two(own.state(stage.as_mut()), &mut self.controls),
            _ => States::two(app, &mut self.controls),
        }
    }

    pub(crate) fn states_ref<'a>(&'a self, app: &'a S) -> StatesRef<'a> {
        let app = crate::runtime::handler_state_ref(app);
        if !self.open { return StatesRef::two(app, &self.controls); }
        if let Some(scene) = &self.scene { return StatesRef::two(&scene.state, &self.controls); }
        match self.own() {
            Some((own, stage)) => StatesRef::two(own.state_ref(stage), &self.controls),
            None => StatesRef::two(app, &self.controls),
        }
    }

    /// The state `AppSpec::on_key` gets: the stage's when it's an app state, none on a story
    /// with its own.
    pub(crate) fn key_state<'a>(&'a mut self, app: &'a mut S) -> Option<&'a mut S> {
        if !self.open { return Some(app); }
        if matches!(self.page(), Some((_, Page { kind: Kind::Own(_), .. }))) { return None; }
        match &mut self.scene {
            Some(scene) => Some(&mut scene.state),
            None => Some(app),
        }
    }

    pub(crate) fn polls(&self) -> bool {
        if !self.open { return false; }
        match (&self.scene, self.own()) {
            (Some(scene), _) => self.poll.is_some() || !scene.script.is_empty(),
            (None, Some((own, stage))) => own.polls(stage),
            (None, None) => false,
        }
    }

    /// Polls the stage's state after the app's own poll: its due steps, then `AppSpec::poll`
    /// for a scene or the story's own.
    pub(crate) fn poll_stage(&mut self, t: f64, tx: &mut Tx) -> bool {
        self.now = t;
        if !self.open { return false; }
        if let Some(scene) = &mut self.scene {
            let ran = scene.run(t, tx);
            return self.poll.is_some_and(|poll| poll(&mut scene.state, t, tx)) || ran;
        }
        let Some(i) = self.selected.checked_sub(self.scenes.len()) else { return false };
        match (self.pages.get(i).map(|p| &p.kind), self.owned.get_mut(i).and_then(Option::as_mut)) {
            (Some(Kind::Own(own)), Some(stage)) => own.poll(stage.as_mut(), t, tx),
            _ => false,
        }
    }

    /// Whether `ev` is the shortcut that opens and closes the playground, where no menu item
    /// takes it first.
    pub(crate) fn is_shortcut(&self, ev: &KeyEvent) -> bool {
        !self.menu && self.shortcut.as_ref().is_some_and(|s| s.matches_key(ev))
    }

    /// What `ev` does in the open playground. `in_side`: the keyboard is in the side column.
    pub(crate) fn nav(&self, ev: &KeyEvent, in_side: bool) -> Nav {
        let m = ev.mods;
        // Control on macOS, the Windows key elsewhere: never the playground's.
        let other = m.alt || (cfg!(target_os = "macos") && m.ctrl) || (cfg!(not(target_os = "macos")) && m.logo);
        if !self.open || other { return Nav::Pass; }
        let letter = match &ev.key {
            Key::Text(t) if !m.cmd => t.chars().next().filter(|_| t.chars().count() == 1).map(|c| c.to_ascii_lowercase()),
            _ => None,
        };
        if let Some(c) = letter && self.triggers().iter().any(|(k, _)| *k == c) {
            return Nav::Request(if c == 'r' { Request::Replay } else { Request::Trigger(c) });
        }
        if in_side {
            return match ev.key {
                Key::Tab | Key::Escape => Nav::OutOfSide,
                Key::Up if !m.cmd => Nav::SideStep(-1),
                Key::Down if !m.cmd => Nav::SideStep(1),
                _ => Nav::Pass,
            };
        }
        let at = self.order.iter().position(|&i| i == self.selected).unwrap_or(0);
        let n = self.order.len();
        if n == 0 { return Nav::Pass; }
        let to = match (&ev.key, letter) {
            (Key::Up, _) if m.cmd => 0,
            (Key::Down, _) if m.cmd => n - 1,
            (_, _) if m.cmd => return Nav::Pass,
            (Key::Up, _) | (_, Some('k')) => (at + n - 1) % n,
            (Key::Down, _) | (_, Some('j')) => (at + 1) % n,
            (Key::Right, _) | (Key::Tab, _) if !m.shift => return Nav::IntoSide,
            _ => return Nav::Pass,
        };
        Nav::Request(Request::Select(self.order[to]))
    }

    /// The window's root: the app's view above the bar, or the playground around its stage.
    pub(crate) fn view(&self, app: &S, view: fn(&S, &mut Cx<S>) -> El, cx: &mut Cx<S>, viewport: (f32, f32)) -> El {
        let t = tok::theme(cx.system_dark());
        if !self.open {
            if !self.bar { return view(app, cx); }
            let mut root = view(app, cx);
            let focus = root.on_window_focus.take();
            // Laid out as the root of a window BAR shorter, as the runtime sizes a root.
            let mut out = col().key("playground-closed").w(viewport.0).h(viewport.1)
                .child(root.w(viewport.0).h((viewport.1 - BAR).max(0.0)))
                .child(self.bar_view(t));
            out.on_window_focus = focus;
            return out;
        }
        let dark = cx.system_dark();
        let stage_w = (viewport.0 - SIDEBAR - SIDE - 2.0).max(0.0);
        let stage = self.stage(app, view, cx, (stage_w, viewport.1), t);
        let side = self.side(app, cx, t, dark, viewport.1);
        row().key("playground").w(viewport.0).h(viewport.1).fill(t.bg)
            .child(self.sidebar(t, dark, cx.titlebar_inset(), viewport.1))
            .child(el().w(1.0).h(viewport.1).fill(t.separator))
            .child(stage)
            .child(el().w(1.0).h(viewport.1).fill(t.separator))
            .child(side)
    }

    fn bar_view(&self, t: Theme) -> El {
        let stories = match self.count() { 0 => String::new(), 1 => "1 story".into(), n => format!("{n} stories") };
        let hint = self.shortcut.as_ref().map(keycaps);
        let summary = [Some(stories).filter(|s| !s.is_empty()), hint].into_iter().flatten().collect::<Vec<_>>().join(" · ");
        let open = row().key("playground-open").h(20.0).ml(4.0).px(6.0).gap(6.0).center_y().radius(radius::SM - 1.0)
            .hover(t.hover).press(t.press)
            .child(icon(&GRID, 12.0).color(t.text_secondary))
            .child(text("Playground").font(type_::LABEL).color(t.text_secondary))
            .role("button").label("Open the playground")
            .on_press_tx(|_: &mut Controls, tx: &mut Tx| tx.open_playground());
        col().w_full().h(BAR).fill(t.bg)
            .child(el().w_full().h(1.0).fill(t.separator))
            .child(
                row().w_full().h(BAR - 1.0).center_y().justify_between()
                    .child(open)
                    .child(text(summary).font(type_::LABEL).color(t.text_secondary).pr(10.0)),
            )
    }

    fn sidebar(&self, t: Theme, dark: bool, inset: f32, height: f32) -> El {
        let back = row().key("playground-back").h(CONTROL).pl(4.0).pr(8.0).gap(2.0).center_y().radius(radius::SM)
            .hover(t.hover).press(t.press)
            .child(icon(&CHEVRON_LEFT, 16.0).color(t.text_secondary))
            .child(text("Back").font(type_::LABEL).color(t.text))
            .role("button").label("Back to the app")
            .on_press_tx(|_: &mut Controls, tx: &mut Tx| tx.close_playground());
        // Back sits at the list's text inset; where the window's own controls take the corner,
        // it goes in the row under them.
        let band = if inset > 0.0 { BAND } else { 0.0 };
        let header = row().w(SIDEBAR).h(HEADER).pl(8.0).center_y().child(back);
        let drag = (band > 0.0).then(|| el().w(SIDEBAR).h(band).window_drag());
        let mut list = col().key("playground-list").w(SIDEBAR).h(height - 2.0 * HEADER - band).px(8.0).pb(8.0).scroll(ScrollSpec { stick_bottom: false });
        let mut group = None;
        for (n, &i) in self.order.iter().enumerate() {
            if group != Some(self.group(i)) {
                group = Some(self.group(i));
                list = list.child(text(self.group(i)).key(format!("playground-group-{i}")).font(type_::SECTION).color(t.text_secondary)
                    .px(8.0).pt(if n == 0 { 4.0 } else { 16.0 }).pb(4.0));
            }
            let on = i == self.selected;
            let name = self.name(i);
            list = list.child(
                row().key(format!("playground-item-{i}")).w_full().h(ROW).px(8.0).center_y().radius(radius::MD)
                    .fill(if on { selected(t, dark) } else { Color::TRANSPARENT })
                    .hover(if on { selected(t, dark) } else { t.hover })
                    .child(text(name).font(type_::BODY).color(if on { t.text } else { t.text_secondary }).truncate())
                    .role("button").label(name)
                    .on_press_tx(move |_: &mut Controls, tx: &mut Tx| tx.playground.push(Request::Select(i))),
            );
        }
        if self.order.is_empty() {
            list = list.child(text("No stories").font(type_::BODY).color(t.text_secondary).px(8.0).pt(4.0));
        }
        col().w(SIDEBAR).h(height)
            .children(drag)
            .child(header)
            .child(list)
            .child(self.stage_controls(t, dark))
    }

    /// Along the sidebar's foot: the stage's size, motion and appearance.
    fn stage_controls(&self, t: Theme, window_dark: bool) -> El {
        let dark = self.dark.unwrap_or(window_dark);
        let (glyph, appearance) = if dark { (&MOON, "Dark appearance") } else { (&SUN, "Light appearance") };
        let theme = icon_button(t, "playground-theme", glyph, appearance, false, window_dark, Request::Dark(!dark));
        let reduce = icon_button(t, "playground-reduce-motion", &MOTION, "Reduce Motion", self.reduce_motion, window_dark, Request::ReduceMotion(!self.reduce_motion));
        let sizes = (!self.sizes.is_empty()).then(|| self.size_menu(t));
        row().key("playground-stage-controls").w(SIDEBAR).h(HEADER).px(8.0).gap(2.0).center_y()
            .children(sizes)
            .child(spacer())
            .child(reduce)
            .child(theme)
    }

    /// The stage's size: the window's room or a preset, from a pop-up menu (a press steps
    /// through them where there's no native one).
    fn size_menu(&self, t: Theme) -> El {
        let current = self.size.checked_sub(1).and_then(|i| self.sizes.get(i)).map_or("Window", |(n, _)| *n);
        let options: Vec<(String, String)> = std::iter::once(("0".to_owned(), "Window".to_owned()))
            .chain(self.sizes.iter().enumerate().map(|(i, (n, (w, h)))| ((i + 1).to_string(), format!("{n}  {w}×{h}"))))
            .collect();
        let (size, count) = (self.size, self.sizes.len() + 1);
        row().key("playground-size").h(CONTROL).pl(8.0).pr(4.0).gap(2.0).center_y().radius(radius::SM)
            .hover(t.hover).press(t.press)
            .child(text(current).font(type_::LABEL).color(t.text))
            .child(icon(&CHEVRON_DOWN, 16.0).color(t.text_secondary))
            .role("button").label("Stage size")
            .on_press_tx(move |_: &mut Controls, tx: &mut Tx| {
                if !crate::popup_menu_supported() {
                    tx.playground.push(Request::Size((size + 1) % count));
                    return;
                }
                let menu = crate::PopupMenu::picker(options.clone(), &size.to_string());
                tx.popup_menu(menu, "playground-size", |_: &mut Controls, picked, tx| {
                    if let Some(i) = picked.and_then(|id| id.parse().ok()) { tx.playground.push(Request::Size(i)); }
                });
            })
    }

    /// The column right of the stage: the story's group and name, the app's controls for it,
    /// its triggers, and a key hint.
    fn side(&self, app: &S, cx: &mut Cx<S>, t: Theme, dark: bool, height: f32) -> El {
        let i = self.selected;
        let title = (i < self.count()).then(|| {
            col().key("playground-title").px(4.0).gap(2.0)
                .child(text(self.group(i)).font(type_::SECTION).color(t.text_secondary))
                .child(text(self.name(i)).font(type_::TITLE).color(t.text).truncate())
        });
        let slot = self.side_slot(app, cx).map(|slot| el().key(format!("playground-slot-{}", self.generation)).w_full().child(slot));
        let triggers = self.triggers();
        let card = (!triggers.is_empty()).then(|| {
            col().key("playground-triggers").w_full().padding(4.0).gap(2.0).radius(radius::LG).fill(t.card).outline(1.0, 0.0, t.card_border)
                .children(triggers.iter().map(|&(key, label)| {
                    let request = if key == 'r' { Request::Replay } else { Request::Trigger(key) };
                    row().key(format!("playground-trigger-{key}")).w_full().h(ROW).px(4.0).gap(8.0).center_y().radius(radius::MD)
                        .focusable(true).focus_highlight(selected(t, dark)).press(t.press)
                        .child(row().key("cap").h(20.0).min_w(22.0).px(6.0).center().radius(radius::SM - 2.0).fill(t.control_track)
                            .child(text(cap(key)).font(type_::LABEL).color(t.text)))
                        .child(text(label).font(type_::BODY).color(t.text).truncate())
                        .role("button").label(label)
                        .on_press_tx(move |_: &mut Controls, tx: &mut Tx| tx.playground.push(request.clone()))
                }))
        });
        let hint = if triggers.is_empty() { "↑↓ stories · Esc back" } else { "↑↓ stories · → triggers · Esc back" };
        col().key("playground-side").w(SIDE).h(height).px(12.0).pt(12.0).pb(12.0).gap(12.0)
            .children(title)
            .children(slot)
            .children(card)
            .child(spacer())
            .child(text(hint).key("playground-hint").font(type_::SECTION).color(t.text_secondary).px(4.0))
    }

    /// The app's controls for the open story, from `Playground::side` or `Story::side`, acting
    /// on its stage state.
    fn side_slot(&self, app: &S, cx: &mut Cx<S>) -> Option<El> {
        let _ = app;
        if let Some(scene) = &self.scene {
            let name = self.scenes[self.selected].0;
            let (_, view) = self.sides.iter().find(|(s, _)| *s == name)?;
            let mut stage = cx.retype::<S>();
            let root = view(&scene.state, &mut stage);
            cx.absorb(stage);
            return Some(root);
        }
        let (own, stage) = self.own()?;
        let mut unit = cx.retype::<()>();
        let root = own.side(stage, &mut unit);
        cx.absorb(unit);
        root
    }

    fn stage(&self, app: &S, view: fn(&S, &mut Cx<S>) -> El, cx: &mut Cx<S>, area: (f32, f32), t: Theme) -> El {
        let mut env = cx.env();
        if let Some(dark) = self.dark { env.system_dark = dark; }
        let page = self.page();
        let frame = self.own().and_then(|(own, _)| own.frame());
        let preset = self.size.checked_sub(1).and_then(|i| self.sizes.get(i)).map(|(_, size)| *size);
        let (w, h) = frame.or(preset).unwrap_or(area);
        let content = match (&self.scene, page) {
            (Some(scene), _) => Some(self.nested(cx, env, |c| view(&scene.state, c))),
            (None, Some((_, Page { kind: Kind::Live(page), .. }))) => Some(self.nested(cx, env, |c| page(app, c))),
            (None, Some(_)) => self.own().map(|(own, stage)| {
                let mut unit = cx.retype::<()>();
                unit.set_env(env);
                unit.set_titlebar_inset(0.0);
                if self.reduce_motion { unit.reduced_motion(true); }
                let root = own.view(stage, &mut unit);
                cx.absorb(unit);
                root
            }),
            (None, None) => None,
        };
        let inner = tok::theme(env.system_dark);
        let content = content.unwrap_or_else(|| {
            col().center().child(text("Add scenes to AppSpec::scenes, or stories to the playground").font(type_::BODY).color(inner.text_secondary))
        });
        // Sized as a window sizes its root.
        let stage = el().key(format!("playground-stage-{}", self.generation)).w(w).h(h).child(content.w(w).h(h));
        let stage = match (frame, preset) {
            // An overlay's clear parts stay clear.
            (Some(_), _) => stage.border(1.0, t.text_secondary.alpha(0.35)).border_dash(4.0, 3.0),
            (None, Some(_)) => stage.outline(1.0, 0.0, inner.separator),
            (None, None) => stage,
        };
        col().w(area.0).h(area.1).center().fill(inner.bg).clip_rounded(0.0).child(stage)
    }

    /// Runs a stage view of the app's own type with the stage's appearance and motion.
    fn nested(&self, cx: &mut Cx<S>, env: crate::Env, f: impl FnOnce(&mut Cx<S>) -> El) -> El {
        let mut stage = cx.retype::<S>();
        stage.set_env(env);
        stage.set_titlebar_inset(0.0);
        if self.reduce_motion { stage.reduced_motion(true); }
        let root = f(&mut stage);
        cx.absorb(stage);
        root
    }
}

/// A selected row's fill, a step stronger in dark mode where the track is faint.
fn selected(t: Theme, dark: bool) -> Color {
    if dark { t.control_off } else { t.control_track }
}

/// A trigger's key as its cap shows it.
fn cap(key: char) -> String {
    if key == ' ' { "Space".into() } else { key.to_ascii_uppercase().to_string() }
}

/// A shortcut as the bar shows it: "⇧⌘D", or "Ctrl+Shift+D" off macOS.
fn keycaps(shortcut: &Shortcut) -> String {
    shortcut.keycaps().join(if cfg!(target_os = "macos") { "" } else { "+" })
}

/// A quiet square button showing `glyph`, named by `label` in its tooltip; `on` fills it as the
/// list's selection is filled.
fn icon_button(t: Theme, key: &str, glyph: &'static Icon, label: &str, on: bool, dark: bool, request: Request) -> El {
    row().key(key.to_owned()).size(CONTROL, CONTROL).center().radius(radius::SM)
        .fill(if on { selected(t, dark) } else { Color::TRANSPARENT })
        .hover(if on { selected(t, dark) } else { t.hover }).press(t.press)
        .child(icon(glyph, 16.0).color(if on { t.text } else { t.text_secondary }))
        .role("button").label(label.to_owned()).help(label.to_owned())
        .on_press_tx(move |_: &mut Controls, tx: &mut Tx| tx.playground.push(request.clone()))
}

static CHEVRON_LEFT: Icon = Icon {
    name: "playground-chevron-left",
    view: 16.0,
    body: r#"<path d="M9.5 4L5.5 8L9.5 12" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

static GRID: Icon = Icon {
    name: "playground-grid",
    view: 12.0,
    body: r#"<rect x="1" y="1" width="4" height="4" rx="1" fill="currentColor"/><rect x="7" y="1" width="4" height="4" rx="1" fill="currentColor"/><rect x="1" y="7" width="4" height="4" rx="1" fill="currentColor"/><rect x="7" y="7" width="4" height="4" rx="1" fill="currentColor"/>"#,
};

static CHEVRON_DOWN: Icon = Icon {
    name: "playground-chevron-down",
    view: 16.0,
    body: r#"<path d="M5 6.75L8 9.75L11 6.75" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
};

static SUN: Icon = Icon {
    name: "playground-sun",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="2.75" stroke="currentColor" stroke-width="1.5" fill="none"/><path d="M8 1.75V3M8 13V14.25M1.75 8H3M13 8H14.25M3.6 3.6L4.45 4.45M11.55 11.55L12.4 12.4M3.6 12.4L4.45 11.55M11.55 4.45L12.4 3.6" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>"#,
};

static MOON: Icon = Icon {
    name: "playground-moon",
    view: 16.0,
    body: r#"<path d="M13.25 9.5A5.5 5.5 0 0 1 6.5 2.75A5.5 5.5 0 1 0 13.25 9.5Z" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linejoin="round"/>"#,
};

static MOTION: Icon = Icon {
    name: "playground-motion",
    view: 16.0,
    body: r#"<circle cx="10" cy="8" r="3.25" stroke="currentColor" stroke-width="1.5" fill="none"/><path d="M1.75 5.5H4.5M1.25 8H4M1.75 10.5H4.5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>"#,
};

#[cfg(test)]
#[path = "playground_tests.rs"]
mod tests;

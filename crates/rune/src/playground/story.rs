//! [`Story`], and [`OwnStory`], which erases its state type so the playground holds stories
//! of any.

use super::*;

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
    pub(super) name: &'static str,
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
    /// logical clock. An `'r'` trigger runs whenever the state is made: when the story first
    /// shows, and on R, which always replays. J and K are taken.
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

/// A [`Story`] with its state type erased. Its stage is a `Stage<P>`.
pub(super) trait OwnStory {
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
        let root = call(self.view, &Self::stage(stage).state, &mut own);
        cx.absorb(own);
        root
    }
    fn side(&self, stage: &dyn Any, cx: &mut Cx<()>) -> Option<El> {
        let side = self.side?;
        let mut own = cx.retype::<P>();
        let root = call(side, &Self::stage(stage).state, &mut own);
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

//! A [`Story`] behind [`OwnStory`], so the playground holds stories of any state type.

use super::*;

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

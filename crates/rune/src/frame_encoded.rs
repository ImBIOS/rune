//! `App::on_frame_encoded`: what each of a window's frames came to, for apps that log their
//! own frame timing.

use std::rc::Rc;

use crate::time::Instant;

/// `App::on_frame_encoded`'s hook.
pub(crate) type OnFrameEncoded = Box<dyn FnMut(&FrameEncoded)>;

/// `App::on_frame_drawn`'s hook.
pub(crate) type OnFrameDrawn<S> = Box<dyn FnMut(&crate::Runtime<S>)>;

/// One frame a window ran, as `App::on_frame_encoded` reports it.
#[derive(Clone, Debug)]
pub struct FrameEncoded {
    at: Instant,
    outcome: FrameOutcome,
    window: Option<Rc<str>>,
    seq: u64,
    presents_at: Option<Instant>,
}

/// What a frame came to.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameOutcome {
    /// Encoded and submitted to the GPU, then presented: a visible window's frame, its first
    /// frame during creation included, or a hidden launcher's.
    Drawn,
    /// A frame only a timer asked for, whose build draws what is already on screen: it took
    /// no drawable and encoded nothing.
    Unchanged,
    /// Due but not drawn: the window is covered, minimised or occluded, or no drawable came in
    /// time. The runtime may still have stepped.
    Skipped,
}

impl FrameEncoded {
    /// For `Drawn`, when encoding finished: right after the frame's commands were submitted,
    /// before it was presented. Otherwise, when the window decided to draw nothing.
    pub fn at(&self) -> Instant { self.at }

    pub fn outcome(&self) -> FrameOutcome { self.outcome }

    /// The window's key (`WindowSpec::new`; `"main"` for `App::run`).
    pub fn window(&self) -> Option<&str> { self.window.as_deref() }

    /// This window's reports so far, from 0: one per call, whatever the outcome.
    pub fn seq(&self) -> u64 { self.seq }

    /// The display refresh the window's display link aimed this frame at, when one paced it:
    /// macOS windows that are visible and not being resized. `None` elsewhere and for
    /// `Skipped` frames.
    pub fn presents_at(&self) -> Option<Instant> { self.presents_at }
}

/// A window's hooks and the count it numbers reports with. Without a hook every call is one
/// `Option` check and nothing else, so reporting never changes how the window draws.
pub(crate) struct FrameReports<S: 'static>(Option<Reporter>, Option<OnFrameDrawn<S>>);

impl<S: 'static> Default for FrameReports<S> {
    fn default() -> Self { Self(None, None) }
}

struct Reporter {
    hook: OnFrameEncoded,
    window: Option<Rc<str>>,
    seq: u64,
}

impl<S: 'static> FrameReports<S> {
    pub(crate) fn new(hook: Option<OnFrameEncoded>, window: Option<&str>) -> Self {
        Self(hook.map(|hook| Reporter { hook, window: window.map(Rc::from), seq: 0 }), None)
    }

    pub(crate) fn inspecting(mut self, drawn: Option<OnFrameDrawn<S>>) -> Self {
        self.1 = drawn;
        self
    }

    /// The time to report a `Drawn` frame at, taken only when there's a hook.
    pub(crate) fn stamp(&self) -> Option<Instant> { self.0.as_ref().map(|_| Instant::now()) }

    /// A frame encoded at `at` (from `stamp`).
    pub(crate) fn drawn(&mut self, at: Option<Instant>, presents_at: Option<Instant>) {
        if let Some(at) = at { self.report(FrameOutcome::Drawn, at, presents_at); }
    }

    /// After a `Drawn` frame: `App::on_frame_drawn` reads the runtime as it presented.
    pub(crate) fn inspect(&mut self, rt: &crate::Runtime<S>) {
        if let Some(hook) = &mut self.1 { hook(rt); }
    }

    pub(crate) fn unchanged(&mut self, presents_at: Option<Instant>) {
        if self.0.is_some() { self.report(FrameOutcome::Unchanged, Instant::now(), presents_at); }
    }

    pub(crate) fn skipped(&mut self) {
        if self.0.is_some() { self.report(FrameOutcome::Skipped, Instant::now(), None); }
    }

    fn report(&mut self, outcome: FrameOutcome, at: Instant, presents_at: Option<Instant>) {
        let Some(r) = &mut self.0 else { return };
        let frame = FrameEncoded { at, outcome, window: r.window.clone(), seq: r.seq, presents_at };
        r.seq += 1;
        (r.hook)(&frame);
    }
}

#[cfg(test)]
#[path = "frame_encoded_tests.rs"]
mod tests;

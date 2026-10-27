//! A due timer on a window that draws nothing new. `poll` runs on its own first, and the view,
//! layout and drawing wait for a frame until that poll or something else changes what shows:
//! always on a hidden window, and on a shown one while it rests.
use super::*;

/// What a resting window's timer left for the frame that follows it.
#[derive(Default)]
pub(super) struct TimerWake {
    /// That timer ran `poll` and asked for the frame, which then doesn't poll again.
    polled: bool,
}

impl TimerWake {
    pub(super) fn take_polled(&mut self) -> bool { std::mem::take(&mut self.polled) }
}

impl<S: 'static> Runtime<S> {
    /// `poll` alone, for a hidden window's wake: its timers and inbox advance while nothing
    /// lays out, animates or draws. Like `update`, its changes wait, dirty, for the next frame.
    /// Returns whether the last frame went out of date: `poll` reported a change or asked for a
    /// scroll or text flight, or a live-edit patch arrived. A `wake_after` alone doesn't count.
    pub(crate) fn poll_hidden(&mut self) -> bool {
        self.timer_wake.polled = false;
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

    /// A shown window's due timer. While the window rests, `poll` runs alone, as on a hidden
    /// window, and the result says whether that wake needs a frame: the poll changed what
    /// shows, asked for a window frame, or asked for the next frame (`wake_after(ZERO)`).
    /// The frame that follows, if any, doesn't poll again. `None` when anything else wants
    /// this frame, which then draws as any other: motion, scrolls or a window frame in flight,
    /// queued input, a hover or tooltip delay, a loop, a live value, `SurfaceDraw` content, or
    /// a poll stepping every frame.
    pub(crate) fn resting_wake(&mut self) -> Option<bool> {
        if !self.rests() { return None; }
        let stale = self.poll_hidden();
        let frame = stale || self.window_frame.is_some() || self.steps_frames();
        self.timer_wake.polled = frame;
        Some(frame)
    }

    fn rests(&self) -> bool {
        #[cfg(all(feature = "dev", debug_assertions))]
        if self.dev.is_some() || self.dev_clock.is_some() { return false; }
        #[cfg(feature = "hot")]
        if self.hot.is_some() { return false; }
        !self.animating && !self.stale() && !self.settle && self.loop_wake.is_none() && self.window_frame.is_none()
            && self.hover_pending.is_empty() && !self.tooltip_waiting() && !self.drag_ends_pending()
            && self.drag_out_request.is_none() && !self.files_held() && !self.live_pending()
            && self.surfaces.is_empty() && !self.steps_frames()
    }

    /// The last poll asked for the next frame (`wake_after(ZERO)`, due at once): it steps
    /// something every frame.
    fn steps_frames(&self) -> bool {
        self.wake_deadlines.first().is_some_and(|t| *t <= self.clock)
    }
}

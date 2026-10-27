- **A window with a still compositor rests.** A window with a `Compositor` never counted as
  resting, so each due `wake_after` built and presented a frame even when `poll` changed
  nothing (four presents a second for a 250 ms timer). When `Compositor::animating` is false,
  the window now rests like any other: a due timer runs `poll` alone and draws only when that
  poll changed something, with one frame for that poll. A compositor's frame still never counts
  as unchanged, since the same instances may composite differently. A compositor that reports
  `animating` (even one that starts while the window rests) draws every frame and every due
  timer, as before. For every window, a resting wake's frame that runs covered or misses its
  drawable no longer polls a second time, the timers that poll armed survive the window being
  occluded or minimized before its frame, and a frame that leaves a commit pending (a
  compositor that updates or invalidates the state while drawing) no longer lets the window
  go idle.

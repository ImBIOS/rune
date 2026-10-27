- **A window with a still compositor rests.** A window with a `Compositor` never counted as
  resting, so each due `wake_after` built and presented a frame even when `poll` changed
  nothing (four presents a second for a 250 ms timer). When `Compositor::animating` is false,
  the window now rests like any other: a due timer runs `poll` alone and draws only when that
  poll changed something, with one frame for that poll. A compositor's frame still never counts
  as unchanged, since the same instances may composite differently, and an animating
  compositor draws every frame as before.

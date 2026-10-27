- **Instant changes, live resizes and invisible exits land in the frame; a still pointer keeps
  its hover while rows mount.** An instant commit lands a pinned viewport's slack at once
  instead of gliding it closed a moment later (zoom). During a window resize, rows that rewrap
  land at their new height in the frame, so a pinned tail stays put instead of sliding back
  after the drag; a streaming row still waits for the width to settle. A removed element that
  drew nothing last frame (faded out already, or under a parent that hid at once) unmounts
  with no exit, and an exit stops once it draws nothing. An instant close of a modal keeps a
  focus request outside it, as an animated close did. Nodes mounting earlier in the tree no
  longer drop the hover of a still pointer for a frame (a hover-revealed row's actions
  flashed out while a timeline streamed). Scripts take `{"type": "resize", "dw", "dh",
  "dur"}`, a person dragging the window's corner. `scripts/motion-audit/analyze.py` adds
  `cut-exit`, `reversal` and `dog-leg` findings, script `expect`ations (`instant`, `follow`,
  `smooth`, `leaves`) and an allowlist with `--fail`.

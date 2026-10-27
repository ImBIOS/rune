- **`App::on_frame_encoded` reports each frame of a desktop window.** The hook (also on
  `WindowSpec`) runs on the UI thread with a `FrameEncoded`: its `FrameOutcome` (`Drawn` once
  encoded, submitted and presented, the first frame and a hidden launcher's included;
  `Unchanged` for a timer's frame that drew what was on screen; `Skipped` when the window was
  covered, minimised or occluded or no drawable came), when encoding finished, the window's key,
  a per-window sequence number, and the display link's target refresh when it paced the frame.
  Frames, skips, pacing and redraws are the same with it as without; without it each frame
  checks an `Option`. Headless runs, overlays and the web don't call it.

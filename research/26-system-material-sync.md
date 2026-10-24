# 26. Native materials in step with Metal frames

Reader: the next agent changing `system_material/macos.rs`, frame pacing on macOS, or the
present path. Re-run the lab below before changing either.

`El::system_material` puts an `NSGlassEffectView` (macOS 26+) or an `NSVisualEffectView`
popover behind a transparent window's CAMetalLayer and sets its frame, corner radius and
alpha in the same `CATransaction` as the frame's drawable (`presentsWithTransaction`). This
note records what that buys, what it costs, and the one thing still limiting it.

## How it was measured

`examples/glass-card` with `GLASS_CARD_PATTERN=black` flies a card over a black window and
draws a 2 px magenta border on the card's edge. Over black the glass shows as a flat dark
grey, so each frame gives two edges: Rune's (magenta) and the native view's (grey). The
screen was captured at 120 fps (ScreenCaptureKit, 1x) on a MacBook Pro with a 120 Hz display
running macOS 27. For each frame with the card moving at least 2 px, a script measured how far
grey extends past the magenta edge, or black shows inside it, on rows and columns through the
card. The left and top edges are clear of the fading text and button, so they are the ones
counted. Cadence comes from `GLASS_CARD_TRACE` traces of the card window, as gaps between
submissions in display periods.

## Results

| Path | Moving frames with the glass edge off by more than 4 px | Frames one period after the last |
|---|---|---|
| Glass, synced (default) | 26 of 1421 (1.8%) | 70% |
| Glass, synced, main thread busy 8 ms a frame | 0 of 325 | about 60 Hz (the busy loop alone takes 8 ms) |
| Popover (`legacy`), synced | 2 of 538 (0.4%) | 98% |
| Glass, `unsynced` (views committed before the present) | 178 of 605 (29%) | 78% |
| Fallback fill (`off`) | n/a | 99% |

Seven synced Glass runs ranged from 0 to 4% of moving frames, clustered in a few flights.

- **The synced glass never lags.** Every miss is the glass one frame ahead: Rune's content
  shows frame N while the glass is already at N+1. At the card's speed that is 10 to 14 px.
  Unsynced shows the same one-frame gap on about a third of moving frames.
- **A busy main thread doesn't break the sync.** It delays the views and the drawable
  equally, because both wait for the same transaction.
- **Glass is expensive to move.** `NSGlassEffectView` runs a SwiftUI graph on every frame or
  radius change: `layoutSubtreeIfNeeded` takes 0.8 to 2 ms a frame (the radius makes no
  difference). Skipping that call only moves the cost into the transaction commit. The popover
  view costs about 0.2 ms.
- **That cost, not the transaction, drops Glass below 120 Hz.** With the fallback fill a
  frame takes about 0.5 ms of CPU, and with Glass about 3.2 ms (median). The display link
  refuses a tick that comes less than half a period after the last present (`permits` in
  `display_link.rs`), so any frame over 4.2 ms skips the next one. In one trace, 192 of the
  219 frames over that line were followed by a skip. Skipped frames let the CPU clock down,
  which makes the next glass layout slower.

## Getting Glass to 120 Hz (not landed)

Two experiments, env-gated and since removed, show what it would take:

| Pacing change | Frames one period after the last | Glass edge off by more than 4 px |
|---|---|---|
| Drop the half-period check | 97% | 11 of 313 (3.5%) |
| Measure the half period from the last frame's start, not its present | 98.5% | 97 of 555 (17%) |
| Frame start, and wait for the GPU before presenting | 99% | 11 of 703 (1.6%) |

Running at full rate without the GPU wait makes the misses much more common. wgpu's
`presentsWithTransaction` path only waits until the command buffer is scheduled
(`waitUntilScheduled`), so a drawable can join the transaction before it has rendered. The
WindowServer then applies the views and keeps showing the previous drawable for a frame.
Waiting for the GPU before the present (`device.poll(Wait)`) closes most of that gap. With the
stock pacer, Glass runs near 80 fps; there the wait made no clear difference to the misses
(0 and 21 frames in two runs) and lowered cadence to 62%.

The pacing change affects every app's frame timing, so it belongs in its own change. The
existing pacer test (`late_frames_and_idle_restarts_cannot_catch_up_in_a_burst`) expects a
present-based check in one case, where a callback arrives 2 ms after an early present.

## Notes for the implementation

- `presentsWithTransaction` is read when wgpu acquires a drawable, so the first frame that
  finds a material is dropped and redrawn once the flag is set.
- AppKit builds a glass view's backdrop layers on its first visible frame, so views are made
  and parked at zero alpha, never hidden. A hidden one shows unrefracted for a frame when shown.
- The host view is flipped and returns nil from `hitTest:`, so winit's view gets every event.
  AppKit can reorder sublayers, so each frame checks that the Metal layer is still above the
  host's layer.

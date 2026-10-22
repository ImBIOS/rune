# 25. Periodic motion, keyboard focus and modal layers

2026-10-22. Worktree `motion-focus`, starting at `9fb5400`. Coordination thread
`C0CB59ARQLE / 1792685407.384429` read before changes. The available Slack CLI is
read-only. This subagent has no agent-spawn tool, so the required independent
Opus review must be arranged by its parent before the first push.

## Scope and source

- `rune-motion::LoopTrack`: analytic phase, phase-preserving retime, eased opacity
  pulse and clockwise rotation. The envelope uses a .18 s / 0 spring. Renderer
  integration uses Rune animation time, retained keyed tracks and existing spring
  presence for mount/removal; app views never run for a periodic frame.
- `El::focusable`, tab indices, roving groups, keyboard-only theme rings and
  Enter/Space activation. Text inputs participate in the same order. Inspector
  flags expose focus and inertness for a future platform accessibility adapter.
- `El::modal`: nested focus traps, Escape, configurable outside dismissal, inert
  background hit-testing/scrolling, focus restoration. `rune_kit::Dialog` adds the
  themed panel and spring-faded scrim. A small shell change stops redraws while
  winit reports the window occluded.
- Kit changes wire existing controls to focus and existing spinner/loader visuals
  to loops. Existing forced states, colour/geometry tokens and goldens are retained.

Fetched OpenCode `origin/dev` before reading these sources (revision `cdf1c200a6`):

- `packages/ui/src/components/spinner.tsx` and `styles/animations.css`: independent
  random periods in [1, 2] seconds, delays in [0, 1.5] seconds, CSS ease-in-out.
  Outer cells pulse .15–.35; inner cells .4–1. Rune fixes per-cell seeds for replay.
  There is no single web phase to reproduce, because module initialization uses
  `Math.random()`.
- `feedback/loader/loader.css`: 900 ms linear clockwise rotation; reduced-motion
  disables the animation. Rune preserves the existing ring/arc and rotates its
  atlas quad, without rasterizing or allocating a new icon each frame.
- `overlays/dialog/dialog.css` and `.stories.tsx`: normal 480×368 surface, overlay
  elevation and scrim tokens. `ui-dialog--basic` is the new comparison scene;
  `motion-focus-modal` is the scripted interaction scene.

## Exact-count target

Primary metric: **requested animation frames while a loop is absent, offscreen,
clipped or in reduced motion: 0**. Baseline spinner/loader had no loop and requested
0 frames even while visible. New visible loops must advance with zero extra view
commits; cold renders and forced headless frames are not native idle measurements.

`loops_sleep_offscreen_hidden_reduced_and_after_exit` simulates the shell's
`animating` scheduling gate over 1,200 opportunities (10 seconds at 120 Hz):
visible 1,200; absent/offscreen/reduced 0. Removal stays drawn through its exit,
then requests no frames. Exact counts need no noisy wall-time repetitions.

## Verification

- Three `rune-motion` loop tests pass: retime continuity across 200 changes, cooling
  and eventual rest, and clock pause/scale/step/seek against an independent formula.
- 13 runtime tests pass, including document order/overrides, disabled controls,
  radio roving and activation, nested modal trap/restoration, outside policy and
  offscreen loop scheduling; existing layout, presence, text-flight and virtual-row
  tests also pass. A later focus-modality correction requires a targeted rerun.
- Tests/builds use HiveMind only, with `RUSTC_WRAPPER=`. Two initial shell timeouts
  interrupted queue/cold compile; the next cold release build exposed helper-method
  privacy (fixed). A subsequent spill from mini to Pro rebuilt dependencies and
  hit HiveMind's 3,600 s limit during gallery-test compilation. Narrowed checks on
  the warmed mini then completed successfully. No Studio window or screen capture.
- Headless `motion-focus-modal` script: 720 frames at 120 Hz, keyboard open/Tab
  cycle/Escape, Space toggle of switch/checkbox, segmented arrow selection, pointer
  reopen and outside dismissal. `motion-lint analyze ... --check`: **0 candidates,
  0 hitches**. A 12-tile strip was inspected. The final tree is idle with the opener
  restored; switch/checkbox and second segment selected. CPU p50 .071 ms, p95
  .352 ms in this single diagnostic run (not a benchmark claim).
- The local OpenCode checkout has no `packages/storybook/storybook-static` build.
  `shots.mjs` uses Playwright's default headless Chromium and pauses infinite web
  animations at t=0; it does not open a window. `kit-diff.py --no-build --only
  spinner,loader,dialog` was attempted and stopped at the missing-build check,
  before launching Chromium. No new similarity scores can be claimed.

## Remaining boundaries

- Platform AccessKit attachment is future work; this provides the shared tree hook.
- Select/menu roving works after focus reaches the group. Their opening autofocus
  and popup keyboard dismissal are not yet unified with the modal API.
- Context-menu, tabs and avatar gallery scenes remain; indeterminate progress is
  still static. Existing demo PNGs have not been overwritten.
- Native occlusion and physical idle wakeups require a non-Studio windowed run.

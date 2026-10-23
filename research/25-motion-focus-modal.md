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
  themed panel and spring-faded scrim. Upstream's rewritten window shell already
  gates hidden/occluded redraws; the rebase retains that implementation.
- Pointer, keyboard and requested input focus share `set_focus`. A newly focused
  input wins over the old field; normalization remeasures when model focus changes.
  Blur cancels preedit, breaks the undo typing group and requests a native IME
  reset. Focused text editing consumes keys before unhandled keys reach the app.
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
- Initially 13 runtime tests passed, including document order/overrides, disabled controls,
  radio roving and activation, nested modal trap/restoration, outside policy and
  offscreen loop scheduling; existing layout, presence, text-flight and virtual-row
  tests also passed. After rebasing onto `a9ca7f7`, all **17 runtime tests** and
  **3 loop tests** passed. This includes the modality correction and a new regression
  for programmatic input handover, composition cancellation, text-key priority,
  unhandled function-key bubbling and background-click blur. Kit-gallery, settings
  and opencode release builds also passed on mini.
- Tests/builds use HiveMind only, with `RUSTC_WRAPPER=`. Two initial shell timeouts
  interrupted queue/cold compile; the next cold release build exposed helper-method
  privacy (fixed). A subsequent spill from mini to Pro rebuilt dependencies and
  hit HiveMind's 3,600 s limit during gallery-test compilation. Narrowed checks on
  the warmed mini then completed successfully. No Studio window or screen capture.
  A later pre-rebase Pro build passed both focus tests but timed out while building
  demos. The final rebased job spilled from Pro to mini and completed: runtime test
  compile 24m54s, loop test compile 1m34s, demo compile 12m59s.
- Headless `motion-focus-modal` script: 720 frames at 120 Hz, keyboard open/Tab
  cycle/Escape, Space toggle of switch/checkbox, segmented arrow selection, pointer
  reopen and outside dismissal. `motion-lint analyze ... --check`: **0 candidates,
  0 hitches**. A 12-tile strip was inspected. The final tree is idle with the opener
  restored; switch/checkbox and second segment selected. CPU p50 .071 ms, p95
  .352 ms in this single diagnostic run (not a benchmark claim).
- Repeated the replay with the rebased binaries: **720 frames, 0 motion-lint
  candidates, 0 hitches**. The final tree is idle, with `opener` focused and zero
  visible rings following pointer dismissal. Inspected `strip-rebased.png`; trace
  is isolated at `perf/autoresearch/motion-focus/traces/rebased/trace.jsonl` because
  motion-lint deduplicates traces sharing the same launch name within a directory.
- All **nine existing-demo snapshots are byte-identical** to the captured baseline:
  home, session bottom/light/top, streaming, stream end, send, settings dark/light.
  `regression-comparison.json` also records zero changed pixels for each.
- The headless proof job requested `--kind render --worker mini --no-sync`, but
  HiveMind unexpectedly spilled it to Studio. It opened no native window and did
  not capture the screen or play audio. Future render commands need an explicit
  in-job hostname guard as well as the worker hint to enforce the host boundary.
- The local OpenCode checkout has no `packages/storybook/storybook-static` build.
  `shots.mjs` uses Playwright's default headless Chromium and pauses infinite web
  animations at t=0; it does not open a window. `kit-diff.py --no-build --only
  spinner,loader,dialog` was attempted and stopped at the missing-build check,
  before launching Chromium. No new similarity scores can be claimed.

## Remaining boundaries

- Independent Opus review remains required before push; no agent-spawn tool is
  available in this session. The parent has a coordination note in the worktree.
  Final fetch/rebase/reverification is still required before `HEAD:main` if main
  advances after the reviewed head.
- `.cycle()` now uses `LoopSpec` and the shared AnyHost deadline; see the dev
  integration below for its wall-clock and group-opacity compatibility checks.
- Platform AccessKit attachment is future work; this provides the shared tree hook.
- Select/menu roving works after focus reaches the group. Their opening autofocus
  and popup keyboard dismissal are not yet unified with the modal API.
- Context-menu, tabs and avatar gallery scenes remain; indeterminate progress is
  still static. Existing demo PNGs have not been overwritten.
- Native occlusion and physical idle wakeups require a non-Studio windowed run.

## Combobox and popup integration

Rebased onto hex-gaps' `9c4e087` stack, then `f4d00b0`. The requested-focus
normalization now uses `set_focus` on the newly measured tree and remeasures once
when model focus changes. This preserves non-input focus and avoids calling an
edit accessor after its row was deleted. Fading-out inputs retain no edit callbacks.

`on_input_key` runs before built-in focus navigation, text editing and app `on_key`;
only unhandled keys bubble to the app. `El::focus_return(trigger_key)` restores
popover, combobox and menu triggers on removal, with keyboard/pointer modality
preserved. Combobox close cancels composition, and modal dismissal cancels it before
the close callback can remove the model. Secondary clicks cannot bubble out of a modal.

The first integration run passed 22 runtime tests and exposed the deleted-row
accessor regression. After fixing it, HiveMind on mini passed the deleted-row test,
all 3 focus tests, both secondary-input tests, all 5 combobox tests, the context-menu
test and the new popover-focus test. An interrupted rerun was cancelled by a server
restart; no worker job remained before resuming it.

After rebasing onto `9864df3` (the newer compact-window/accessory host changes),
the final targeted run passed all 3 focus tests, 5 combobox tests, context-menu
and popover-focus tests on mini. No native windows were opened. The earlier
snapshot/motion-lint evidence above predates this integration and is not claimed
as a new visual run on this head.

## Independent-review fixes

- Wired OpenCode's picker and composer `on_input_key` handlers so picker navigation
  and toolbar Tab cycling precede text editing and Rune traversal. App navigation
  gets first refusal when there is no focused element. Its dismissal surface is
  transparent rather than zero-opacity, keeping it hittable. Settings and the kit
  gallery have no global key handler to migrate.
- Modal membership follows anchored portal ownership and root layers above the
  modal. Outside dismissal uses the allowed hit target. The Dialog+Select test
  chooses an option outside the panel and keeps the dialog open.
- Window deactivation queues preedit cancellation and resets native IME while
  retaining committed text and field focus; overlay hosts also cancel preedit.
- Focused virtual inputs retain fresh view accessors while unrealized, accept edits
  offscreen, and clear model focus when another control takes focus.
- Delayed loops sleep until the shared AnyHost deadline. Idle catch-up preserves
  the deadline through intermediate input, with visibility and time-scale gating.

On `111f999`, HiveMind mini passed 26 runtime tests, 4 loop tests, 5 combobox tests,
context-menu, popover-focus, Dialog+Select, and all 4 OpenCode picker tests. Built
OpenCode, settings and kit-gallery in release mode. The Dialog test's initial
right-only outside assertion was corrected to accept any outside edge; the
broader picker test exposed and verified the transparent-dismissal fix.

Headless mini captures matched **19 current-main stills with zero changed pixels**:
five OpenCode scenes, settings dark/light, and twelve kit comparison Rune columns.
Existing goldens were never overwritten. No Studio rendering, windows or audio.

Final rebase: `c6bf061`, including NativeSurface. Resolved overlapping additions
by retaining compositor marks, native-surface APIs and video timing alongside
focus/loop work. Reverified on mini: **26 runtime + 2 native-surface unit + 8 kit
integration + 4 OpenCode picker tests passed**; release builds of all three demos
passed. Repeated the 19-still comparison with these binaries: zero changed pixels.
The four loop-unit tests passed before this rebase; their sources did not change.
The renderer hold note was deleted after confirming NativeSurface on main.
Nothing has been pushed; the parent must arrange the short delta review.

## Dev landing and cycle unification

The parent reviewed the fixes clean and authorized landing after desktop-fidelity.
Rebased onto `origin/dev` at `133729e`, retaining window chrome, hover groups,
equal-column grids, standalone Select triggers and fitted segmented controls.
The overlapping window-focus match arms are combined so chrome placement cannot
mask IME cancellation. No changes beyond conflict resolution and cycle unification.

`El::cycle(Cycle)` is sugar over `El::looping(LoopSpec::cycle(...))`. The shared
track supports Cycle's linear stops, absolute wall-clock phase, reduced-motion
`still`, and requested fps. Runtime's loop deadline schedules both delayed starts
and paced cycles; Host no longer owns a separate ambient timer. Zero inherited
group opacity, offscreen geometry and hidden surfaces suppress loop wakeups.

Verification used one HiveMind job at a time, `HIVEMIND_CAS_SYNC=0`, and SHA-256
checks of synchronized source (plus capture scripts and golden inputs). Passed
27 runtime tests, 5 loop tests, 8 kit integration tests and 4 OpenCode picker tests.
All nine workspace examples built in release mode. The first combined job hit
HiveMind's one-hour cap after runtime/loop tests; remaining checks resumed from
the cache, and the final builds have no unreachable-focus-handler warning.

Headless golden checks on mini:
- Seven OpenCode, two settings, and twelve kit component stills: pixel-identical.
- Both kit indexes differ from old committed PNGs because of the already-reviewed
  added scenes; they exactly match the rebased, reviewed pre-sugar stack.
- Four lab stills differ from committed PNGs under default capture timing. A fresh
  `133729e` build produces exactly the same four images as this branch.
- Background, live-values, shapes, overlay controls and video-tiles: pixel-identical
  against that fresh dev build (these examples have no committed still goldens).

No golden files changed. Builds and captures opened no native windows or audio.

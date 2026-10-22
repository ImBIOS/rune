# 19. Rune performance and quality loop

2026-10-22. Starting revision `685db9d`. Synthetic fixtures only. Builds/tests use
HiveMind on Pro; release executables copied from its per-repo Cargo target and
timed offscreen on Studio (M5 Max). No window or live-server access.

## Method

`python3 scripts/bench-ui.py --out perf/autoresearch/<experiment>` generates a
2,000-message fixture and runs each workload once warm, then seven measured
times, at 120 Hz virtual time and 2× scale. Results include every run, median,
and median absolute deviation. CPU includes update/build/encode/submit, excludes
readback. The maximum includes first frame. These are CPU timings, not actual
display presentation intervals. Driver caches are warm across fresh processes;
GPU init plus first-frame CPU is **not** cold process-to-pixels latency.

`python3 scripts/snap-ui.py perf/autoresearch/<experiment>/snaps` pins nine
states. Before/after PNG bytes must agree for performance-only edits. Five
existing OpenCode/settings goldens match these exact commands; the existing
streaming and settings-dark images use different states/times. They are not
overwritten. Captures and raw timing logs are ignored local evidence.

## Baseline

All entries ms; medians of seven runs.

| Workload | CPU p50 | CPU p95 | max / first | GPU init |
|---|---:|---:|---:|---:|
| Stream | .237 | .450 | 3.786 / 3.79 | 10.9 |
| Scroll | .233 | .289 | 3.637 / 3.64 | 10.5 |
| Switch sessions | .227 | .428 | 2.572 / 2.50 | 10.6 |
| Settings interactions | .122 | .210 | 1.263 / 1.26 | 10.2 |
| 2,000-message stream | .646 | 11.251 | 70.900 / 70.90 | 9.9 |
| 2,000-message scroll | .464 | .587 | 72.624 / 72.62 | 10.0 |

At rest the shell uses `ControlFlow::Wait`, requesting another redraw only while
`Runtime::animating` is true or on an external event. Headless `--frames` forces
frames even at rest; its “at-rest CPU” must not be reported as idle work in the
window. No ten-second native idle/power measurement was run on Studio.

## Experiment 1: reuse text cluster scratch (kept)

- Hypothesis: revealing words allocates a temporary cluster table for every
  visible text block on every presentation frame.
- Change: retain that table in `TextSystem`, clearing it per line.
- Counting allocator initially found 181 allocations / 30 streaming frames:
  180 cluster-table allocations and one instance-buffer capacity growth. The
  final test warms the full visible working set before restarting a reveal;
  it asserts **0 allocations / 30 frames**, including growth, scroll, reveal and
  instance building. Commit/view/layout and wgpu driver allocations are outside
  this gate and are not claimed to be zero.
- Stream CPU p50/p95: .237/.450 → .237/.451 ms; settings .122/.210 → .121/.213.
  Timing is unchanged within noise; the exact allocation reduction is the win.
- Checks: targeted allocator test passes; release builds pass; all nine
  before/after snapshots are byte-identical. Existing goldens untouched.
- Independent model review unavailable: this subagent's tool catalog has no
  subagent-spawn tool. Self-reviewed bounds, scratch reuse and disjoint borrows.

## Experiment 2: retain Taffy's layout cache (kept)

- Hypothesis: rebuilding the Taffy tree on every token throws away unchanged
  paragraph measurements and constraint caches in long transcripts.
- Change: keep layout nodes by keyed path, dirty only changed styles/children or
  text measurement inputs. Updating a text node's index after reorder does not
  invalidate its measurement. Remove unused layout nodes after the tree diff.
- 2,000-message streaming CPU p95 **11.251 → 6.904 ms** (39% lower), p50
  .646 → .644 ms, mean 1.570 → 1.179 ms. Ordinary streaming p95 .450 → .343 ms.
  Settings p95 .210 → .207 ms. Long-scroll p50 .464 → .468 ms (noise).
- First frame is unchanged: long stream 70.90 → 70.24 ms. This optimizes
  subsequent commits, not initial layout or view construction.
- Verification: 40 generated edits compare retained layout with a fresh runtime,
  including reorder/remove/reinsert, text↔box changes, padding, width and scale.
  Runtime presence/growth tests and zero-allocation presentation gate pass.
  Nine before/after PNGs remain byte-identical.
- Measurement correction: the first build command hit its shell timeout and
  left the previous executable in place. Discarded that timing directory
  (`retained-layout`); rebuilt to completion and measured `retained-layout-verified`.

## Experiment 3: constant-time scene sibling links (kept)

- Hypothesis: inserting 2,000 timeline siblings scans from the first child for
  every insertion, making scene mount quadratic.
- Change: index the last child and previous sibling. Appends and unlinks are
  constant-time; node slots and traversal order stay the same.
- Long-stream first CPU frame **70.24 → 63.88 ms**, long-scroll **73.02 →
  63.72 ms**, seven-run medians. Stream p95 6.904 → 6.846 ms (unchanged).
  Ordinary stream first frame 3.77 → 3.85 ms (noise).
- Checks: 600 generated insert/remove/reuse operations compare root and nested
  sibling traversal with a vector reference. Nine PNGs remain byte-identical.
- The ≤50 ms cold first-pixel target remains unmet; first viewport still shapes
  all messages. Virtualization/estimated heights is a larger remaining change.

## Experiment 4: incremental release compilation (reverted)

- Hypothesis: reusing optimized leaf-crate work makes one-line edits much faster.
- `hivemind run --kind cargo -- python3 scripts/bench-build.py`: one warmup and
  seven actual comment edits in `examples/opencode/src/main.rs`, restored in a
  `finally` block. Build medians on Pro (background priority): **24.96 s → 3.23 s**
  with `CARGO_PROFILE_RELEASE_INCREMENTAL=true`; MAD 1.56 → .51 s.
- However the changed code generation regresses long-stream CPU p95
  **6.846 → 7.295 ms**, MAD .048 → .067 ms (a repeatable 6.6% regression).
  Ordinary stream .356 → .364 ms; nine snapshots byte-identical.
- Reverted the release profile change. Keep the benchmark for future compile
  experiments. An opt-in development profile could accept this tradeoff, but
  the production frame budget takes priority. Release deps already have no
  debug info; the research-17 debug-info suggestion applies to dev builds.
- Release rebuild after scene/runtime changes: 72 s on Pro. A clean dependency
  build was not measured. Do not confuse it with these warm leaf rebuilds.

## Quality instrumentation and measured pass

- `--trace` now includes scene-edge sampling error, spatial spring retarget
  velocity error, frame CPU and animation state. Diagnostics are off otherwise.
  `scripts/trace-quality.py` summarizes research-15 metrics without pretending
  virtual timestamps are display presentation times.
- At 120 Hz, stream/switch/send max scene position error is **.0078125 device px**
  (half the 1/64 grid); scroll is 0. Largest retarget velocity discontinuity is
  **.00003052 pt/s** across 18 streaming lanes; switch 4 lanes .00002480 pt/s;
  send 1 lane .00001216 pt/s. Settings has no spatial spring retarget in its script.
- CPU excess over the 8.33 ms budget, excluding startup: 0 ms/s in these five
  recorded small-fixture traces. Virtual hitch ratio is .0267 ms/s from rounding
  JSON timestamps to .001 ms; cadence sigma .0000566 periods. Neither is physical
  hitch evidence. Growth/scroll are excluded from the analytic scene-edge metric.
- motion-lint: **0 candidates** in stream (1440 frames), switch (1080), scroll
  (480), send (720), settings (360). The current CLI requires a directory:
  `bun ~/code/open-source/motion-lint/cli/motion-lint.ts analyze <trace-dir> --check`.
  Streaming excuses 9 intended height changes; session switches excuse intentional
  settle cuts. Stream/send/scroll/settings end idle; switch's 9 s recording ends
  during a reveal, so that is not an idle measurement.
- A 300-frame jitter test adds up to 12 ms to 120 Hz intervals and interrupts
  nested spring tracks every seven samples. Edge error stays below 1/64 px and
  inherited velocity discontinuity below .001 pt/s.

## Quality pass: partial-line growth and turn completion

- Reuse Parley's final-line metrics to hold back up to .52/.82 line heights while
  text streams (research/18 §7). Keep the soft edge while the partial line is idle,
  allow shrink on a viewport rewrap even if a token arrives in that frame, and
  restore full measured height on completion. No extra shaping or allocation.
- Sub-line growth alone caused a turn-end reversal at 8.45 s: the timeline moved
  8.9 pt one way and 3.2 pt back as Working disappeared. A persistent 28 pt tail
  with a .34 s Working/copy crossfade removes that reversal. This uses Rune's
  existing tail size rather than Desktop's 82 pt reservation or its large scale
  and blur effects. Both controls now share the same vertical alignment.
- Copy uses Desktop's `outline-copy` path from `origin/dev`, and copies only the
  selected assistant message's text parts (Markdown, separated by blank lines).
  Reasoning and tool inputs are excluded. The callback resolves text on press;
  view rebuilds don't assemble the clipboard string. Empty replies have no action.
- `Tx::copy` routes through the shell's existing clipboard sink. A targeted test
  checks Unicode content, action/keyboard ordering in both directions, and no
  repeat after consumption. Native clipboard access was not exercised.
- Reduced-motion tracing exposed full-opacity row insertions and animated gap
  closure. New rows now land geometrically and fade for .15 s; removed rows finish
  their fade before their gap lands. A runtime test covers both paths and idle.
  Traces explicitly mark reduced-motion size/position cuts, without blanket
  opacity/transition intent. Motion-lint reports 0 remaining candidates for stream
  (1440 frames), send (720) and reduced-motion stream (1440). Intent exclusions
  include 13 streaming height changes and the deliberate reduced-motion cuts;
  the separate presence test checks that fades really occur.
- Latest seven-run medians: long-stream p95 **6.878 ms**, first CPU frame
  **63.43 ms**; stream p95 **.346 ms**; settings p95 **.219 ms**. The quality pass
  retains the earlier performance win. Presentation remains **0 allocations / 30
  warmed frames**. Maximum analytic scene-edge error remains .0078125 device px.
- Inspected dark/light session, streaming, send-handoff and completed-stream PNGs.
  Home, session-top and both settings snapshots are byte-identical to baseline.
  Updated `demo/opencode/session-bottom.png` and `session-light.png` intentionally:
  the new copy tail reserves 28 pt and shifts bottom-pinned content upward. Other
  demo images/videos retain their historical capture states.
- Checks ran through HiveMind on Pro: 11 core unit tests, then the new reduced
  motion case, the allocation gate, and both release examples. Independent model
  review is still unavailable in this tool catalog. Self-review found and fixed
  clipboard ordering and simultaneous resize/token handling before pushing.

## Research 18 guarantee status

1. Time-based springs and per-second growth caps: covered by 30/60/120 Hz tests.
2. Continuous spring retargeting: tested with jitter and measured in traces.
3. Synchronous measurement including last-line fill: implemented in this pass.
4. Enforced measure/mutate commit API: implemented by separate capabilities and an
   owned measured result (experiment 9).
5. Fractional scroll and wrap-once landing widths: implemented. Growth targets wait
   for 120 ms of stable width; streaming shrink is allowed for 400 ms. Reading
   history lands measured heights with its keyed scroll correction (experiment 12).
6. Leaving-node retention: covered by exit/revival tests and the stable tail.
7. Reduced motion: .15 s fades, including newly mounted growth rows, are tested.
8. Settle on session open: tested at first-frame bottom. Native window resume and
   background pacing still need end-to-end coverage.

## Next

1. Migrate composer pickers onto the landed rune-kit, then use themes/opencode for
   their palette/icons. Preserve stable filtered identities and keyboard/IME behavior.
2. Horizontal code scrolling, tool expansion/details and remaining hover/press fidelity.
3. Lazy row construction for long transcripts; repeat paired release timing headlessly
   on Studio, not contended workers.
4. Native idle/wakeups, display presentation and truly cold OS/driver startup remain
   unmeasured. The earlier <50 ms result was fresh-process headless on Studio.
5. Extend picker fidelity beyond the bounded fixture catalog: large-list keyboard
   scroll-into-view, provider management and model tooltips.

## Experiment 5: virtual transcript rows (kept)

- Hypothesis: offscreen Taffy nodes, text shaping, scene mounts and row bookkeeping
  dominate the 2,000-message first frame and streaming commits.
- Added `.virtualize(estimate)` for keyed growth rows directly inside a scroll
  viewport. Only the overscanned window enters layout/text/scene. Unvisited rows
  retain estimated space; measured corrections preserve the first visible keyed
  row, and pinned scrolling uses the exact measured bottom. Paging culls scene
  state immediately and remounts historical rows without replaying their reveal.
  This still constructs the app's offscreen element descriptions; lazy view
  construction is a separate future experiment.
- First windowed version: long-stream p95 **6.878 → 2.171 ms**, first CPU frame
  **63.43 → 7.36 ms**. The motion gate caught a .02 pt tail twitch from float
  cancellation at large content coordinates. Rebased the scene's row coordinates
  around the mounted window and kept scroll extent/offset accumulation in f64.
- Final seven-run medians (`virtual-rebased`): long-stream p95 **2.210 ms**, first
  frame **7.60 ms**, mean **.345 ms**, p50 **.125 ms**. Long-scroll p95 **.320 ms**,
  first **7.43 ms**. Ordinary stream p95 **.371 ms** versus .346 before the feature
  (about 25 µs for bookkeeping). Retain for the 68% long-stream p95 reduction and
  88% first-frame reduction against the previous quality pass.
- Differential test compares visible keyed rows against a full 600-row variable-
  height layout through 135 fractional up/down scroll steps, prepend and return
  to bottom. It checks culling, shaping reduction and released row state. The
  test found a settle retry loop; measurement now honors the requested pin state.
  Degenerate zero-height runs have a bounded full-measurement fallback.
- The return test also found that an instant layout request didn't cancel a
  spring if its target was unchanged. Fixed `Scene::set_frame`; a targeted scene
  test now lands that interrupted animation exactly.
- Seven fixture traces pass motion-lint: stream, send, switch, scroll, reduced,
  long-stream and long-scroll. Traces now identify actual wheel/fling movement
  with the standard scrolling flag, so deliberate 20,000 pt scripted wheel input
  isn't mistaken for a spring fault. Automatic follow remains unflagged. Invisible
  virtual extent rectangles are excluded from tracing; actual row surfaces are
  still traced. The tail jitter is fixed, not excluded.
- All nine PNGs are byte-identical to the preceding quality pass. No golden
  changes. Runtime tests, the new scene test and the presentation allocator gate
  pass through HiveMind; the allocator test now exercises a virtual growth stack.
- Independent model review is unavailable in this tool catalog. Self-reviewed
  measurement retries, culling versus model removal, stable keys and scroll math.

## Experiment 6: fresh process to completed pixels (measurement)

- Added `--first-pixels`, an explicitly headless mode that flushes a stdout marker
  after the first GPU readback completes. The external stopwatch starts before
  process creation and stops on that marker. It includes loader, fixture parsing,
  fonts, GPU init, layout, submission and readback, and excludes PNG encoding and
  process teardown. This avoids treating CPU submission as completed pixels.
- `scripts/bench-startup.py` launches a fresh process for each run, with one warmup
  and seven measured runs. OS filesystem and GPU driver caches remain warm; this
  is process-cold, not a reboot/cold-cache or physical-display measurement.
- Studio medians (MAD), milliseconds: settings **25.657 (.529)**; OpenCode home
  **30.366 (.330)**; session **31.237 (.501)**; 2,000-message session **36.819
  (1.016)**. All measured runs were under 50 ms; long-session range 34.769–38.813.
- Session components: fixture/state .292 ms, runtime setup .035 ms, GPU init
  9.768 ms, first-frame CPU 3.800 ms, readback 12.757 ms. The long fixture adds
  about 1.74 ms parsing and 3.61 ms frame CPU. Component medians need not sum to
  the median total.
- The current headless path already meets PLAN §6b's 50 ms target under this
  definition, so no speculative GPU-init rewrite was made. Native presentation,
  other machines and cold OS/driver caches remain unmeasured.
- Both release examples built through HiveMind. Enabling the first-pixels marker
  preserves the exact session/settings snapshot bytes. The prior seven motion
  gates cover the unchanged frame behavior; only the opt-in first readback waits.

## Experiment 7: composer undo/redo (kept)

- Added Cmd/Ctrl-Z and Cmd/Ctrl-Shift-Z to `TextEdit`. Continuous typing forms a
  group; navigation, pointer focus, paste, cut, newline and IME commits separate
  edits. Undo restores Unicode byte offsets and selection direction. Uncommitted
  IME preedit is cancelled first; new edits discard the redo branch.
- History is immutable/shared in view snapshots, copy-on-write on editing, and
  limited to 128 transactions / 1 MiB of retained text. A single oversized edit
  drops history. External text replacement invalidates stale history. Submitting
  uses `clear()` to discard the sent draft's history while retaining focus.
- Four editor tests pass through HiveMind, covering typing, Unicode, selection,
  backwards/forwards deletion, cut, paste normalization, IME, branching, bounds,
  snapshot sharing and clearing. Release settings/OpenCode builds pass.
- The fixture `undo-redo.json` checks keyboard routing through the runtime: at 1.75 s
  the prompt is empty after undo, at 1.9 s redo restores it, and after send a later
  undo leaves it empty. Its trace and all seven prior motion cases pass with zero
  candidates. All nine existing snapshots remain byte-identical.
- Initial timing overlapped snapshot capture, so it was discarded as a benchmark.
  Isolated seven-run medians: long-stream p95 **2.169 ms**, first **7.60 ms**;
  long-scroll p95 **.324 ms**, first **7.49 ms**; ordinary stream p95 **.356 ms**.
  No meaningful performance change from virtualization's 2.210 / 7.60 ms.
- Desktop's composer delegates text editing to its browser editor; its source
  does not define a custom undo stack. This implements the standard shortcuts and
  transactions, not a claim of identical browser undo grouping heuristics.
- Independent review remains unavailable (tool discovery exposes model search,
  no spawn tool). Self-review covered history ownership, limits and submission.

## Experiment 8: measured send flight (kept after two corrections)

- Added `Tx::fly_text(source_key, target_key)` and wired both fixture submit paths.
  Before committing, Rune captures the visible composer glyphs using their text
  bounds, rather than the input control's empty space. After each frame's growth
  and scroll build it measures the bubble's live destination. Source glyphs and
  the destination wrap crossfade while flying; glyph atlas sampling supports
  uniform scale. Reduced motion uses a 150 ms crossfade, and settle cancels flight.
- Re-read Desktop `send-flight.ts` at **39eb86d**. It interpolates source to the
  live destination with one growth-spring progress, rather than spring-following
  that moving destination. Rejected an initial independent rectangle follower:
  it preserved velocity on retarget but kept lagging behind subsequent replies.
  The retained version uses the same 0.32 s / zero-bounce progress as Desktop,
  with time-based evaluation rather than Desktop's per-frame dt cap. Re-reading
  the destination does not restart the progress or its velocity. This is not a
  separate guarantee of continuity across arbitrary discontinuous endpoint jumps;
  a settle request deliberately lands the whole transition.
- Rejected destination-row clipping during flight: a growing bubble's mask hid
  the copy before it arrived. The travelling overlay is window-clipped, and the
  normal bubble clip resumes on landing. A glyph-visibility assertion and fixture
  trace check now verify that the source/destination handoff never goes blank.
- Land sub-quantum size residuals before snapping both edges, avoiding a 1/64 px
  height twitch while the top edge moves. Both source and destination overlays
  are represented in traces with their actual alpha and visible rectangles.
- Three focused flight tests cover common-time 30/120 Hz samples, live target
  updates without progress restart, reduced motion, and a runtime send while
  an earlier row grows and moves the destination. The warmed virtual-presentation
  allocator test still reports zero allocations. Builds/tests used HiveMind.
- All nine pre-existing snapshots are byte-identical, including the settled send.
  Nine motion cases pass with zero candidates: the seven existing cases, reduced
  send, and overlapping sends with different wraps/Unicode. `send-overlap.json`
  exercises a second send while the first row is still arriving.
- Seven-run medians: long-stream p95 **2.198 ms**, first **7.53 ms**; long-scroll
  p95 **.315 ms**, first **7.56 ms**. A separate seven-run before/after fixture send
  comparison gives p95 **.359 → .362 ms**, mean **.223 → .226 ms**.
- Headless before/after video and frame strip rendered on Pro through HiveMind;
  inspected the strip through takeoff, travel and landing. The video is slowed 2×,
  before left / after right. Published at
  `https://studio.taila8e7c7.ts.net:8444/raw/2026-10-22-rune-send-flight/send-before-after-2x.mp4`.
  Native display presentation remains untested. Independent model review remains
  unavailable; self-reviewed the shared progress, shader footprint, clip ownership
  and snapshot handoff.

## Experiment 9: enforced measurement capability (kept)

- Moved view/layout/virtual-row measurement behind a separate `Measurement` type.
  Its borrows include only immutable app state, layout/text caches, virtual row
  geometry and bookkeeping. Scene, growth/reveal tracks and draw instances are
  absent from that capability, so measurement code cannot mutate presentation.
- Measurement retries complete before returning an owned `Measured` value. The
  mutation phase consumes it; an element tree or an unfinished measurement pass
  is not a valid input. Deferred reduced-motion policy updates to mutation too:
  previously measurement called `Scene::set_reduced_motion` before Taffy finished.
- A regression test changes both row size and reduced-motion policy, completes
  measurement, and verifies that scene geometry, active tracks, displayed row
  height and motion policy remain untouched until the measured value is applied.
  All nine runtime tests and release builds pass through HiveMind.
- All nine snapshots remain byte-identical; all nine motion cases pass with zero
  candidates. This is a commit API guarantee, not a ban on render-side cached
  glyph lookup or reading the current presentation for send-flight destinations.
- Initial seven-run long-stream p95 was **2.868 ms** / first **8.40 ms**, above the
  preceding quiet-run 2.198 / 7.53 ms. An alternating seven-pair comparison against
  the saved pre-flight binary found **2.909 → 2.914 ms** p95 and **.475 → .473 ms**
  mean under the same conditions; first frames **8.21 → 8.89 ms**. The old binary
  also slowed, so the p95 increase is shared environmental variation, not evidence
  of a phase-boundary regression. The comparison includes the intervening flight
  feature (inactive in this stream case), rather than claiming an exact parent A/B.
  Every measured median remains below the requested 4 ms p95 target.
- Independent review is unavailable. Self-review checked the capability's field
  list, retry lifetime, deferred policy change and single-consumption boundary.

## Experiment 10: copy/Working handoff (kept)

- The hollow box was the SVG copy icon overprinting Working, not a missing font
  glyph. Both occupied the same reserved tail footprint. Added an element-local
  opacity curve and staged 180 ms fades: outgoing first, incoming after 180 ms.
  First paint and settle still land immediately; reduced motion retains the fade.
- A focused runtime test covers both handoff directions and reduced motion, asserting
  the two labels never simultaneously exceed .001 opacity. It and release builds
  passed through HiveMind. Self-review covered opacity override scope and initial
  mount behavior; independent review tooling remains unavailable.
- All nine fixture motion cases pass with zero candidates on Pro through HiveMind.
  Eight snapshots are byte-identical. The reviewed send snapshot removes the copy
  rectangles over the W. Its exact SHA-256 is
  `8b5786467337eb28efbb4384264ad45540fb3189d2c7874c59a774bf03efaeae`.
  The gate accepts only that reviewed digest, not arbitrary changes to send.png.
- Added `scripts/check-ui.py` to run the nine snapshot and motion gates, including
  optional fixture scripts and exact-digest reviewed replacements. A first worker
  lint attempt lacked the lint configuration; copied it and completed all gates.
  No performance win is claimed for this visual correction.

## Experiment 11: fixture composer pickers (kept)

- Read Desktop at `4617210822`: `providers/models/select-dialog.tsx:495-719`
  and `composer/editor/editor.tsx:844-918`. Implemented top-start, 6 pt gutter
  menus, 284 pt model width, provider headings, case-insensitive model/provider
  search, selected checks, pointer-active rows, wraparound arrows and Enter.
  Escape restores the composer; outside dismissal does not steal focus. Tab reaches
  all three controls. IME Enter cannot accidentally select or send. Cmd-' opens
  models; Cmd-. and Shift-Cmd-D cycle agents and effort as in Desktop commands.
- The catalog is synthetic fixture data (four models, two agents). Selections are
  session/draft-local, clear incompatible effort on model change, and populate the
  fixture's next reply metadata. This does not implement provider management,
  billing, model tooltips or a live catalog. Native accessibility remains pending.
- Added root-level measured anchoring, pointer-entry callbacks and opt-in animated
  dependent layout for search inputs. Fixed focus transfer away from an exiting
  input and blocked clicks through exiting ancestors. Menu entrance/exit uses
  .25/.20 s zero-bounce springs and .96 scale; reduced motion removes travel.
- Rejected the first filtering implementation: instant editor transactions snapped
  the menu height, and independently disappearing rows overprinted neighbours.
  Also rejected borrowing timeline growth stacks for filtering: changing masks and
  container movement reversed rows. The retained approach keeps keyed slots,
  springs their heights/positions together, fades filtered slots in 150 ms and
  softly clips each slot. Persistent text stays sharp.
- Interrupted reopen exposed a runtime bug: Scene insertion began a default .4 s
  layout before the menu's .25 s transaction, making its children move on different
  curves. Revive now preserves the old frame target until the transaction retargets
  it. Unrelated direct input also no longer cuts an already-running exit.
- Inspection now reports inherited opacity and parent motion, and the exact drawn
  grid rectangles. Sub-quantum dimension residuals land before two-edge snapping,
  removing 1/64 px terminal size twitches. Motion-lint thresholds were unchanged.
- Four focused picker tests and ten runtime tests pass through HiveMind. All nine
  goldens are byte-identical to experiment 10. Nine existing motion cases plus
  normal/reduced picker scripts pass with zero candidates. Reviewed model/filter
  stills and captured a headless 2×-slowed menu clip on Pro:
  `https://studio.taila8e7c7.ts.net:8444/raw/2026-10-22-rune-pickers/pickers-2x.mp4`.
- Seven alternating release pairs on Pro: long-stream p95 **35.008 → 34.785 ms**,
  mean **6.817 → 6.562 ms**, first CPU frame **85.60 → 92.89 ms**. Both binaries
  were far slower than earlier Studio measurements; individual first frames ranged
  from 34 to 194 ms. A worker probe showed concurrent Xcode/Cargo/browser jobs and
  load average 21.02. These noisy worker numbers establish no performance win and
  do not revalidate the earlier <4 ms absolute target. Quiet-worker calibration is
  still needed. Evidence: `perf/autoresearch/pickers/paired/results.json`.
- Independent model review tooling remains unavailable. Self-review covered input
  routing, provider identity, fixture-only initialization, phase isolation, inherited
  trace state, exiting hit targets and the interrupted-reopen regression.

## Experiment 12: stable-width hold (kept after resumed verification)

- Prepared growth-stack width tracking: measure at the declared landing width,
  retain height targets for 120 ms after the last width change, then release on
  the fixed growth substeps. A 400 ms window permits streaming shrink; reduced
  motion and settle bypass the hold. The shared clock stays awake during the hold.
- Added a proposed headless runtime test for 30/120 Hz agreement, no remeasurement
  during presentation, reduced motion and settle, with optional motion traces.
  **Neither compilation nor this test completed**, so these remain hypotheses.
- HiveMind jobs timed out before reaching project compilation: 5 minutes on mini,
  then 20 minutes on Pro. Process inspection found other compiler jobs waiting in
  `sccache` for over ten minutes. A per-job `RUSTC_WRAPPER=` override advanced through
  dependencies but still timed out after ten minutes. No shared cache daemon was
  restarted and no other job was interrupted.
- Saved the exact unverified diff to `perf/autoresearch/width-pending.patch` and
  removed it from production sources. Main retains the verified picker build.
  Resume by inspecting/applying that patch, using HiveMind with a healthy compiler
  cache or the per-job override, then run the targeted test, width motion traces,
  virtualization/streaming resize regressions and the snapshot/motion gates before
   committing. All three timed-out job groups have ended.

### Resumed verification

- After the cache restart, the saved patch compiled and its initial test passed.
  Expanded coverage to restart the hold while it is active, allow streaming shrink
  only within the 400 ms window, and preserve a reading anchor across a rewrap.
- Rejected the original behavior while reading virtualized history: measurement
  corrected scrolling to new heights while growth still displayed old heights,
  moving the anchor **113.5 pt**. Unpinned virtual stacks now land the measured
  heights with that correction. Following stacks retain the hold and growth easing.
- Normal and reduced-motion width traces at 30/120 Hz each pass motion-lint with
  zero candidates. The test checks equal sampled curves, no presentation-time
  reshaping/recommit, active-clock retention, restart, and settle. All 11 runtime
  tests and four fixture picker tests pass after integrating upstream through
  `8b37ced`. Builds/tests ran through HiveMind; all OpenCode runs used fixtures.
- Nine goldens are byte-identical and all eleven fixture motion cases pass on mini.
  No lint thresholds or golden digests changed. Removed the superseded pending
  patch; the implementation and its regressions are now in the runtime.
- Removed unnecessary substep deadline arithmetic when no heights are pending.
  An exact-parent seven-pair Studio comparison (both on rune-kit base `ad80ae1`)
  gives long-stream p95 **2.539 → 2.480 ms**, MAD **.139 → .061 ms**; mean
  **.376 → .368 ms**, first CPU frame **8.44 → 8.81 ms**, MAD **.27 → .42 ms**.
  This is performance-neutral within noise, retained for the width/anchor behavior.
  Evidence: `perf/autoresearch/width/paired-exact/results.json`.
- Final upstream-integrated Studio seven-run medians: long-stream p95 **2.971 ms**,
  first CPU frame **8.76 ms**; long-scroll p95 **.443 ms**, first **9.61 ms**.
  The <4 ms long-stream target is revalidated. Native presentation remains unmeasured.
- The earlier picker-binary comparison included rune-kit's renderer changes and
  cannot isolate this patch. Pro's target cache also disappeared between jobs,
  forcing a dependency rebuild that hit its timeout. A later HiveMind job spilled
  to Studio; reused that release cache for the remaining tests/builds. Snapshot
  rendering stayed on mini, and every benchmark ran headlessly on Studio.
- Independent model review remains unavailable (no spawn tool). Self-review checked
  pending-row removal, fixed-substep release, reduced motion, settle, the reading
  anchor, and the empty-pending fast path. Slack coordination was read throughout;
  the available slack-control CLI is read-only.
- The repository switched to a fresh public root during landing. Preserved the old
  local history as `width-before-public-root` and transplanted only this three-file
  change onto `1f6fbd0`; no force push or old-history merge into the public root.

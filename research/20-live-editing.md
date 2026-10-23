# Live Rust views retain the running Rune app

Desktop integration, 2026-10-22. This follows the [Subsecond spike](08-spike-hot-patch.md).
The goal is to let an agent edit a Rust view and inspect new pixels without rebuilding
app state, resetting scroll, or restarting an animation.

Both examples passed ten successive GPU patches with exact state/scroll/spring
continuity. **The one-second target is not met:** final shared-worker medians were
**8.195 s for Settings** and **21.169 s for OpenCode** to GPU pixels.
Those timing runs used the main-branch base `6d87e63`, before its history was squashed
and the newer app host and renderer landed. Post-rebase verification is recorded below.

## Run the guarded launcher

```sh
scripts/rune-dev settings -- --snap-on-patch perf/live-edit/settings.png --hot-control perf/live-edit/control.json
scripts/rune-dev opencode -- --scene session --snap-on-patch perf/live-edit/opencode.png
```

On Studio, the launcher relays source edits to Pro and runs the compiler and app in
one HiveMind lease there. Each completion marker is forwarded only after its PNG and
JSON tree have arrived back in the checkout. Omit `--snap-on-patch` for a worker window.
OpenCode hot builds require a fixture and reject live-server and send flags.

Install Dioxus CLI **0.7.10** at `~/.cargo/bin/dx` on the worker. `RUNE_DX` overrides
that path. The unrelated Deno `dx` executable does not work. The launcher clears
the inherited compiler wrapper because the patch compiler needs incremental artifacts.

## Stable ownership preserves state across new view code

The optional `rune/hot` feature pins `subsecond` and `dioxus-devtools` to `0.7.10`.
Feature-off builds omit the dependencies, patch dispatch, connection, generation checks
and extra runtime fields.

The host owns the app value, runtime, scene, springs, scroll offsets and GPU resources.
Hot builds erase the app state behind thin typed adapters so the large runtime and
window shell are compiled in the stable Rune library. The view adapter downcasts the
state and calls `subsecond::HotFn::current(spec.view).call((state, cx))` explicitly.
This avoids the pointer-sized closure problem found in the spike.

After a patch callback, the runtime commits a new keyed tree **before consuming queued
input**. That replaces event-handler trait objects while preserving keyed nodes and
their tracks. Press, hover, drag, text editing and submission receive the app value,
not the erased host wrapper. Context changes such as animation speed propagate back
through the typed adapter.

The persistent headless loop uses the regular runtime and renderer. It reads the GPU
output, writes a fast lossless PNG and a tree, then flushes `rune-hot-pixels`. The marker
includes generation, process and state identities, scroll offsets, and spring positions
and velocities. Paused or idle loops wait instead of rebuilding frames while the patch
compiler is working.

## The guard rejects incompatible edits before the compiler sees them

`scripts/rune-dev` watches the original source and publishes accepted edits into a
private compiler mirror. `tools/rune-dev-guard` parses Rust with `syn` and compares
declarations after removing eligible function bodies.

| Edit | Result |
|---|---|
| Settings view/helper body; OpenCode view/palette body | Publish and patch |
| State field, field order or type, even at the same size | Restart needed |
| Function signature, import, constant, impl method or `main` | Restart needed |
| Const-fn body or function with a non-allowlisted attribute | Restart needed |
| Nested declaration or unknown macro contract | Restart needed |
| Framework, dependency, asset, added or removed source file | Restart needed |
| Invalid Rust syntax | Keep the last accepted source until fixed |

Known expression macros are parsed recursively so a `static` or type declaration
cannot hide inside `vec!` or `format!`. Unparsed macro syntax stays part of the contract.
The source guard is conservative; it is not a Rust ABI verifier or a state migration
system. Restarting discards the running state.

Keep persistent values in `AppSpec::init`'s result. Tip-crate statics and thread locals
can reset on any patch. Cached view-owned trait objects and background callbacks do
not acquire new behavior automatically. Loaded patch libraries remain resident.
Physical iOS devices are unsupported by this backend; simulator support was not tested.

## Measure pixels rather than compiler completion

Each hot benchmark makes ten label edits in one process. It first changes app state,
pauses virtual time mid-animation, and captures nonzero spring velocities. Every edit
must execute the new Rust view and change the PNG while preserving PID, state address,
the affected toggle/project subtree, scroll offsets, and exact unedited spring samples.
Finally it advances time to verify the same animation continues, then tries an
incompatible edit and verifies that the compiler mirror never receives it.

Settings has no scroll viewport. OpenCode exercises scrolling in its fixture session.
Pausing isolates patch continuity from normal time advancement; it does not establish
foreground display smoothness. GPU readback is the pixel endpoint, not physical display
presentation. PNG acknowledgement includes encoding and tree publication.

The rebuild comparison makes ten edits with `cargo build --release`, then starts a new
fixture-only process for each snapshot. That measures the existing release workflow,
not a same-profile compiler microbenchmark. Relaunch reconstructs initial state.
Both benchmarks exclude cold builds; worker timings exclude Studio transport.

## Verification and timing record

The initial integration completed ten GPU patches per example before the final
idle-loop and exact-velocity instrumentation changes. Median save-to-GPU times were
**2.924 s for Settings** and **5.607 s for OpenCode**. Those runs retained app state,
moving rectangles, and OpenCode's scroll offset of **1094.5 points**. They did not meet
the approximately one-second goal and are not the final instrumented benchmark.

After rebasing onto main's fixture pickers and Linux shell changes, all three syntax
guard tests passed. The typed-host test passed for press, hover, text editing,
submission and context propagation.

The final instrumented runs each passed all ten edits, animation continuation, and
incompatible-edit rejection before mirror publication:

| Example | Save→GPU median | Save→PNG/tree median | GPU p95 / maximum | Samples |
|---|---:|---:|---:|---:|
| Settings | 8.195 s | 8.908 s | 13.778 s | 10 |
| OpenCode fixture | 21.169 s | 23.523 s | 45.293 s | 10 |

With ten samples, nearest-rank p95 is the maximum. The table retains the OpenCode
summary.
Settings retained a spring velocity of −143.820 points/s; the sampled OpenCode
project-row springs retained −892.798 points/s. Explicitly stepping the same process
after the final patch advanced the original animations.

The ten-sample release rebuild/relaunch comparisons completed:

| Example | Incremental build median | Save→relaunch PNG median | PNG p95 / maximum |
|---|---:|---:|---:|
| Settings | 39.876 s | 40.631 s | 73.776 s |
| OpenCode fixture | 174.596 s | 176.653 s | 242.789 s |

The PNG endpoint was 4.56× faster for hot Settings and 7.51× faster for hot OpenCode
in these sequential runs. The processes were retained only by the hot workflow.
These are different compiler profiles and worker-load windows, not an isolated
estimate of hot-patching's intrinsic speedup.

The Studio relay passed an end-to-end edit in **4.848 s**, from local save to the
locally fetched PNG/tree. PID and state identity were retained. This is one transport
check, not a ten-sample latency estimate; its worker-load window differs from the table.

The relay creates an empty Git root in its staging directory for HiveMind's local
admission, and supplies a source manifest instead of transferring Git history. Leases
use `HIVEMIND_STRICT_PIN=1`; a busy Pro must not move the renderer onto Studio.
The launcher and benchmark also reject worker-mode execution on Studio.

All **nine feature-off PNGs are byte-identical** to main at `f4d00b0`: both Settings
themes and OpenCode home, bottom, top, light, streaming, stream-end and send.
The normal dependency tree contains neither `subsecond` nor `dioxus-devtools`.
The first attempt produced stale benchmark output after source relay; the final check
used a no-CAS sync with verified source hashes and touched the edited source before
building each side. Both sides used `scripts/snap-ui.py OUT BIN_DIR` with release builds.

The second rebase integrates main's new `App` host, `AppSpec::min_size`, live-value
waker and pending transactions, image-atlas uploads, and the theme workspace.
The launcher covers the two `rune::run` examples; custom `App` lifetimes and multiwindow
factories are not part of this integration.

The previous rebase used **`f4d00b0`** (scheduled overlay wakes). It also routes new
secondary-press, input-key and input-change callbacks through the typed app state,
preserves key diagnostics, and wakes idle headless sessions for timer deadlines.
After `hivemind sync --worker pro --no-cas`, all twelve
recorded source hashes matched the worker.
Targeted hot-feature checks passed:

- Typed host: press, hover, secondary press, input change/key, text editing, submission
  and context propagation.
- Both runtime timer tests: exact/coalesced deadlines and handler transaction merging.
- One GPU patch per example, with exact state/scroll/spring preservation, continued
  animation, and rejection of the incompatible edit before publication.

The last checks are compatibility evidence, not a replacement for the ten-sample
timing runs.
These shared-worker runs do not isolate the performance effect of the idle-loop change
from compiler contention or the intervening main-branch changes.

The worker was Pro (Apple M2 Max), macOS 27.0.1, Rust 1.99.0, Dioxus 0.7.10, under
HiveMind's utility scheduling and nice priority 10. Other jobs shared the worker.
An external Cargo cache disappeared during an earlier build; subsequent runs keep
their reusable cache in the active source tree at `target/live-edit-build`. The first
post-rebase hot-profile unit-test build took 43m26s, and the enclosing one-hour lease
expired before the first hot timing sample. That cold-build incident is excluded from
edit latency and prompted separate benchmark leases.

## Integration onto dev (2026-10-23)

Rebased the four implementation/evidence commits onto fetched `origin/dev`
**`b6783ae7a72613b2e51813d09009e7a0b7ff2fbe`**. The patch-boundary commit now borrows
`Tx`, preserving upstream's mid-frame commits before text input. Typed host adapters
also cover the new focus/modal, physical-input, link, pointer, layout, window-focus
and paste callbacks. Context conversion carries the host reduced-motion preference,
caret/layout values and their read tracking back to the stable runtime.

OpenCode's ordinary build retains its `App` chrome configuration. Its hot build routes
the same spec through `rune::run` and remains fixture-only. New runtime submodules,
secure-field handling, optional transitions, rolling advances and surface mirroring
retain upstream behavior. The regenerated lockfile adds the optional patch dependencies
without upgrading or downgrading upstream packages.

All build/test jobs ran sequentially on Pro through HiveMind with
`HIVEMIND_CAS_SYNC=0 HIVEMIND_STRICT_PIN=1`:

- Three hot-host tests passed: existing editing/handler coverage, new focus/physical/link/
  window callback routing, and caret/layout read tracking with the OS motion preference.
- Both `timed_wakes_` tests and all three standalone syntax-guard tests passed.
- `cargo test -p rune -p rune-kit --features rune-kit/markdown` passed in one full run.
  It retained the three manual Rune test ignores, five ignored doc examples and two
  GUI-session skips. The linker reported the large `__eh_frame` warning.
- `cargo check --workspace --all-targets` passed.
- `cargo check -p settings -p opencode --features settings/hot,opencode/hot --all-targets`
  passed.

The two new tests initially assumed the wrong modal focus and root layout behavior;
their corrected expectations passed when rerun individually. No new latency measurements
or feature-off PNG comparisons were taken during this integration. The ten-edit numbers
above remain measurements on `6d87e63`; the nine golden comparisons and one-patch checks
remain evidence on `f4d00b0`, not `b6783ae`. The one-second target remains unmet.

## Review fixes on dev d3358d7 (2026-10-23)

The guard now retains `const fn` bodies, including those used indirectly through a
constant array length. It also retains bodies with any attribute outside `doc`, `inline`,
`cold` and `allow`: a procedural attribute can turn a function body into persistent
declarations. Both cases have regression coverage.

The typed view adapter catches unwinding panics from the patch call and invokes the
original startup function pointer with the retained state and a fresh context. Its test
injects a panicking replacement into the same recovery boundary, confirms original
handlers still mutate the same state allocation, and then accepts a healthy replacement.
This does not catch handler-closure panics, a panic in the original fallback, or undo
interior-mutability side effects. The README records those limits and the restart
requirement for helpers reached only through startup `poll`/`on_key`/`setup` pointers.

Malformed control taps/scrolls now log and skip. Hot-only CLI fields are crate-private.
The hot OpenCode initializer no longer constructs `Server`, so fixture startup does not
read its password file. Source watching shares one loop and caches bytes by size and
nanosecond timestamps plus file identity; unchanged scans read no source/asset bytes.
Tests compare cached snapshots with full reads across edits, atomic replacement, restored
mtime, additions/deletions and a read race. The relay uses checksum-based transfer for
changed files so a same-size save with preserved mtime still reaches the worker.

On base `d3358d7236946be58561e2d1584e2f2b69e07255`, all 15 targeted tests passed once
through HiveMind on Pro: five guard tests, five hot-host tests, two timer tests and three
Python watcher tests. Normal Settings/OpenCode all-target checks passed too. The hot
example check first lost its worker connection, then stalled in the cache path; it passed
with `RUSTC_WRAPPER=` while retaining HiveMind admission. That check reports five dead-code
warnings for OpenCode's now-unreachable live-loading helpers. No passing tests were rerun.
The read-only Claude review of these fixes reported no blockers.

No new GPU timings or PNG
golden comparisons were taken in this follow-up.

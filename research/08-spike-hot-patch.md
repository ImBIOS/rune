# Standalone Rust hot-patching with Subsecond

Hands-on spike, 2026-10-21. [Source and reproduction instructions](../spikes/hot-patch/README.md). All Rust execution took place on a remote M2 Max build Mac. No windows were opened.

## Summary

- **Yes, a standalone Rust program can hot-patch without a Dioxus UI.** A terminal loop used `dioxus_devtools::connect_subsecond()` and `subsecond::call(...)`; the patched function returned a string.
- Ten successive body edits preserved the running process, counter, Vec allocation and contents, and main-owned struct instance. Save→first new-function output was **989 ms median / 1,150 ms p95**, with a 712–1,150 ms range.
- This did **not** meet Rune's approximately 300 ms target. Even save→patch-applied, before waiting for the next tick, was **826 ms median / 1,100 ms p95**.
- A conventional build of the final spike took **71.33 s artifact-cold** and **1.846 s for one incremental body edit**. The registry was warm for that cold-artifact build.
- The exact simple expression `subsecond::call(|| render(&state))` silently kept running old code in 0.7.10. A two-reference capture worked; `HotFn::current(render).call((&state,))` also worked and is the better integration candidate.
- Generic function bodies, a closure body, and a statically dispatched trait implementation patched successfully. A cached `Box<dyn Trait>` retained its old method. A signature change inside a stable closure patched; changing the explicit `HotFn` boundary signature left old behavior running.
- **State preservation is conditional.** Adding a struct field was accepted without migration or rejection. Tip-crate atomic statics and thread locals reset on patches in this experiment, including a separately tested exported static. The static result contradicts the broad documentation claim.
- Subsecond 0.7.10 explicitly documents **physical iOS devices as unsupported for Rust hot-patching**. Simulator support is documented but was not measured. Recommendation: an optional desktop development backend, not Rune's sole hot-reload foundation.

## 1. Environment and integration

| Item | Actual setup |
|---|---|
| Worker | M2 Max build worker, Apple M2 Max, 12 cores / 96 GiB |
| OS | macOS 27.0.1, build 26A434 |
| Compiler | `rustc 1.99.0 (b940084d7 2026-09-28)`, stable aarch64 Apple Darwin |
| CLI | `dioxus 0.7.10 (57d6794)` |
| Crates | `subsecond = =0.7.10`, `dioxus-devtools = =0.7.10`, lockfile retained |
| Build settings | `opt-level=0`, `debug=0`, incremental enabled, two Cargo jobs |
| Execution | Remote shell on a background-priority job runner |

The pre-existing `dx` on `PATH` was Deno's launcher. I installed the latest available Dioxus release with `cargo-binstall dioxus-cli --no-confirm` on the worker. It installed `~/.cargo/bin/dx`, which the driver addresses explicitly. The inherited sccache wrapper rejected incremental compilation, so the driver clears `RUSTC_WRAPPER` for its subprocesses. [Recorded environment](../spikes/hot-patch/evidence/environment.json).

The binary has a counter, `Vec<u64>`, and nested `Panel` struct owned by `main`. Each nominal 120 Hz tick increments the counter and first Vec entry, calls `render`, and prints a timestamp, PID, state/Vec addresses, and output. It creates no `VirtualDom` or Dioxus renderer. `dioxus-devtools` does bring Dioxus core/signals dependencies transitively; this proves independence from the UI runtime, not a dependency graph free of Dioxus crates.

The devtools adapter supplies a WebSocket connection with the PID, build ID, and ASLR reference, then passes matching jump tables to `subsecond::apply_patch`. The runtime loads the patch dylib and updates its jump table. It retains loaded libraries and jump tables rather than unloading previous code. Sources: [devtools 0.7.10, `connect_subsecond` and `connect_at`](https://github.com/DioxusLabs/dioxus/blob/v0.7.10/packages/devtools/src/lib.rs#L69-L120), [Subsecond `apply_patch`](https://github.com/DioxusLabs/dioxus/blob/v0.7.10/packages/subsecond/subsecond/src/lib.rs#L498-L555), and [jump-table storage](https://github.com/DioxusLabs/dioxus/blob/v0.7.10/packages/subsecond/subsecond/src/lib.rs#L270-L321).

The measured closure needs a second captured reference because of the bug discussed below. A separate successful probe used this simpler API:

```rust
dioxus_devtools::connect_subsecond();
// main owns state; each iteration calls the latest render implementation.
let output = subsecond::HotFn::current(render).call((&state,));
```

## 2. Measured edit latency and builds

The driver writes `src/main.rs`, closes it, records Python `time.time_ns()`, and waits for the first frame whose revision matches the edit. Rust records its epoch timestamp immediately after the patched call returns. Both clocks are on the worker. These numbers include filesystem watching, compilation/linking, patch delivery/loading, and the next actual call. They exclude syncing the source to the worker. The script makes ten revision edits in one process, with at least 30 observed frames between them. [Raw samples](../spikes/hot-patch/evidence/benchmark.json).

| Edit | Save→new output, ms | Save→patch callback, ms |
|---:|---:|---:|
| 1 | 1,150.117 | 1,099.745 |
| 2 | 712.420 | 663.000 |
| 3 | 998.195 | 807.863 |
| 4 | 867.826 | 712.570 |
| 5 | 753.253 | 751.811 |
| 6 | 1,005.250 | 834.011 |
| 7 | 851.558 | 817.549 |
| 8 | 979.820 | 860.620 |
| 9 | 1,036.368 | 915.992 |
| 10 | 1,030.275 | 938.728 |
| **Median** | **989.008** | **825.780** |
| **p95, nearest rank** | **1,150.117** | **1,099.745** |

With ten observations, nearest-rank p95 is the maximum. This is one short run on a shared worker, not a population estimate. The CLI's own ten reported hot-patch durations were 962, 523, 665, 575, 617, 695, 678, 723, 773, and 732 ms; their median is 686.5 ms. CLI duration alone omits part of the experienced delay.

### Scheduling qualification

The job runner launches jobs with background/utility scheduling and `nice 10`. The sleep-based 120 Hz loop was visibly batched: among 1,453 adjacent frame intervals, median was 0.004 ms, p95 16.917 ms, and maximum 207.965 ms. It catches up missed deadlines rather than presenting evenly spaced frames. Patch callback→first new output was 105.374 ms median and 190.332 ms maximum.

These are actual background-worker measurements. They do not establish 120 Hz rendering quality or foreground-app latency. The callback timings still show that removing frame-wakeup delay alone would not get this run under 300 ms. A foreground-priority measurement would need a separate controlled run.

### Comparison with normal builds

| Operation | Wall time | Conditions |
|---|---:|---|
| Conventional artifact-cold build, final source | 71.333 s | Empty `target/normal-final`; dependencies already downloaded |
| Conventional incremental rebuild, final source | 1.846 s | One render constant changed; same target directory |
| Initial conventional cold build | 79.600 s | Earlier minimal source; included dependency resolution/download work |
| Initial conventional incremental rebuild | 1.672 s | Earlier minimal source |
| Initial `dx` hotpatch-enabled build | CLI reported 133.88 s | Separate DX artifact/profile cache; initial single-capture version |

The final normal-build pair is the closer comparison. Both conventional builds exited successfully. The median live patch was about 1.87× faster than that single conventional incremental build, while also retaining the tested state. Conventional build timing stops at build completion and does not include app relaunch/state reconstruction. [Final build results](../spikes/hot-patch/evidence/builds.json), [initial build results](../spikes/hot-patch/evidence/initial-builds.json).

## 3. What preserved state, what broke

The ten body edits all retained PID `12294`, Vec address `4313468912`, and state address `6169897736`. At every first patched tick, the counter advanced, `vec0 == counter + 73`, and panel width remained 800. This establishes preservation of these existing main-owned values, not arbitrary Rust state.

Each following probe began with a fresh process. The driver saved the source edit and observed the running output. [Probe results](../spikes/hot-patch/evidence/probes.json), [extra probes](../spikes/hot-patch/evidence/extras.json), [explicit boundary-signature probe](../spikes/hot-patch/evidence/extra-explicit_hotfn_signature.json).

| Edit | Observed behavior |
|---|---|
| Generic `generic<T: Display>` body, `g0`→`g1` | New monomorphized behavior appeared in 811 ms; main-owned state preserved. This tests an existing `u64` instantiation, not all generic changes. |
| Closure body inside `render`, `value+10`→`value+20` | New result appeared in 974 ms; state preserved. |
| Trait implementation, direct `state.panel.label()` | `t1` appeared in 1,051 ms; state preserved. |
| Trait implementation with a `Box<dyn Label>` cached in main-owned state | Revision changed in 838 ms, but the trait call still returned `t0:42x24`. The existing object's vtable continued to call old code. |
| `render(&state)`→`render(&state, tone)`, with callsite changed inside the two-capture closure | `rev=99` appeared in 1,351 ms. The outer `FnMut() -> String` boundary stayed compatible, allowing its body and inner call to change together. |
| Same signature change when `HotFn::current(render)` was called directly from the original main loop | Patch callback fired, but old `rev=0` continued through the five-second observation. No automatic restart or panic was observed. The host's old call boundary was not replaced. |
| Append `extra: u64` to `AppState`; change its initializer in main | Patch accepted in 1,449 ms. Patched code reported size 56 instead of 48, while PID, state address, and existing contents remained. Main's new initializer did not run. No migration or incompatible-layout rejection occurred. |
| Atomic static initializer, 1000→9000 | First patched output was `static=9000`, rather than the next old counter value. Appeared in 1,123 ms. |
| Same static test after adding `#[unsafe(no_mangle)] pub` | Still reset to 9000; appeared in 1,104 ms. Exporting its symbol did not preserve it in this configuration. |
| Thread-local initializer, 2000→9000 | First patched output was `tls=9000`; appeared in 872 ms. |

Even the ten ordinary revision edits reset the unchanged tip-crate static and TLS to 1000 and 2000 on each first patched frame. The [0.7.10 docs](https://docs.rs/subsecond/0.7.10/subsecond/#globals-statics-and-thread-locals) warn about tip-crate TLS, but claim globals are tracked and initializer edits are not observed. **The static probes did not match that claim.** This result is specific to the tested macOS tip-crate static; dependency statics were not tested.

The struct probe deliberately did not explicitly read `extra`, to avoid turning an out-of-bounds read into a supposed success. Crossing that changed Rust type is already outside the supported safety contract. It demonstrates acceptance of an incompatible patch, not that adding fields is safe. The upstream [layout limitation](https://docs.rs/subsecond/0.7.10/subsecond/#struct-layout-and-alignment) explicitly leaves re-instancing to framework authors.

### Pointer-sized capture bug

The requested minimal `subsecond::call(|| render(&state))` compiled and ran normally, but a revision edit never appeared during the initial 30-second observation. A second isolated reproduction with a patch callback confirmed that a jump table was applied, yet old output continued for five seconds. [Initial failure](../spikes/hot-patch/evidence/pointer-capture-failure.json), [instrumented reproduction](../spikes/hot-patch/evidence/extras.json).

The source explains this behavior. [`HotFn::try_call`](https://github.com/DioxusLabs/dioxus/blob/v0.7.10/packages/subsecond/subsecond/src/lib.rs#L411-L440) chooses function-pointer dispatch whenever the closure's size equals a function pointer's size. One captured reference satisfies that test. [`call_as_ptr`](https://github.com/DioxusLabs/dioxus/blob/v0.7.10/packages/subsecond/subsecond/src/lib.rs#L900-L949) then searches the jump table with those captured bytes, and falls back to the original callable when there is no match. The observed lack of a diagnostic is consistent with that fallback.

Adding a second captured reference makes the measured closure follow the other dispatch path. The separate explicit-argument `HotFn` probe changed behavior in 918 ms with state preserved. Use that API with a stable signature rather than relying on the artificial second capture. Its signature-change probe also shows why the stable-signature restriction matters.

## 4. iOS and GPU scope

### Simulator

Not executed. The worker had Xcode 27.1, no booted simulators, and only the `aarch64-apple-darwin` Rust target installed. More importantly, the release's [`start_ios_sim`](https://github.com/DioxusLabs/dioxus/blob/v0.7.10/packages/cli/src/build/apple.rs#L85-L145) boots a simulator and calls `open::that_detached` on `Simulator.app`. A straightforward `dx serve --ios` would violate this spike's no-visible-windows constraint. A headless simulator launcher would be a separate integration task. No simulator latency or state-preservation claim is made here.

### Physical iPhone, researched only

The [Subsecond 0.7.10 platform documentation](https://docs.rs/subsecond/0.7.10/subsecond/#platform-support) explicitly excludes devices because of code-signing requirements. The [release architecture note](https://github.com/DioxusLabs/dioxus/blob/v0.7.10/notes/architecture/07-HOTRELOAD.md) also lists iOS Simulator support and device hot-patching as unsupported.

Ordinary device deployment needs the `aarch64-apple-ios` target, a signed development app with matching bundle ID/provisioning profile, a development identity, Developer Mode/device trust, and transport to the device. The CLI's [Apple signing path](https://github.com/DioxusLabs/dioxus/blob/v0.7.10/packages/cli/src/build/apple.rs#L282-L363) embeds a provisioning profile and signs the bundle; its generated development entitlements include `get-task-allow`. The [device-serving discussion](https://github.com/DioxusLabs/dioxus/discussions/3545) reports ordinary `dx serve --ios --device` working in 2026. That does not establish Rust dylib hot-patching.

Subsecond additionally needs each newly generated patch to reach the running app, load as executable code under iOS's signing restrictions, and be mapped to that process's ASLR addresses. I found no supported turnkey device path in this release. A proposed debugger-assisted or signed-patch loader remains unverified engineering work; a simulator result would not remove this blocker.

### GPU

The optional wgpu/Metal extension was not attempted. This spike preserves CPU heap allocations and state, not a verified live GPU device/queue/texture. Owning GPU resources outside the patched code is a plausible design, but cached callbacks/vtables and changing Rust wrapper layouts would still need attention.

## Steal

- Use explicit patch boundaries around a small UI-logic function, with state owned by a stable host.
- Keep the compiler/linker cache warm and compile the smallest changing crate. Subsecond documents that only the executable's tip crate patches; `mod` files in that crate fit better than a changing library dependency. [Workspace limitation](https://docs.rs/subsecond/0.7.10/subsecond/#workspace-support).
- Record save time, patch-applied time, and first new frame separately. Verify state with identities and content invariants rather than a CLI success message.
- Reuse the ASLR/PID/build-ID protocol if implementing a smaller Rune-specific transport. The existing adapter is sufficient for an initial experiment.

## Avoid

- Promising approximately 300 ms from the name “Subsecond” or its compiler timer alone.
- Treating arbitrary captured closures, changed type layouts, tip-crate statics, or persistent trait-object vtables as transparently reloadable.
- Assuming a successful Rust build or patch callback means the application is executing new code.
- Designing the connected-iPhone workflow around an unverified device patch loader.

## Recommendation for Rune

Keep Subsecond as an optional **desktop development backend** worth evaluating with a realistic Rune renderer. Put long-lived state and GPU ownership in a stable host, expose explicit patch functions with stable argument/return shapes, and rebuild or migrate state on incompatible schema changes. Replace cached callbacks or trait objects when their implementation changes. Any such migration and invalidation machinery belongs to Rune; this spike did not implement it.

Reject Subsecond 0.7.10 as the **sole Mac-and-iPhone hot-reload layer** for now. The measured latency misses the target, physical devices remain unsupported, and several ordinary Rust patterns silently keep old behavior or lose state. Also reject “wrap everything in `subsecond::call`” as an integration strategy, and do not change the entire renderer to Dioxus just to obtain its compiler tooling: the standalone integration already works.

Normal rebuild/relaunch is the known fallback for incompatible Rust changes, but it does not satisfy the requested state-preserving iteration loop. A runtime-editable UI/layout description could cover frequent visual edits without recompilation; that is a candidate for a separate measurement, not a result of this spike.

## Open questions

1. How much faster is the same end-to-end test under a real foreground app's scheduling policy, and with Rune-sized dependencies?
2. Can the pointer-sized capture and tip-static behavior be fixed upstream or constrained reliably in an adapter?
3. How should Rune detect incompatible schema changes before applying a patch and migrate state across them?
4. What is the practical memory growth over hundreds of patches, given retained libraries and jump tables?
5. Can a supported, repeatable physical-iPhone patch loader meet the target without restarting the app?
6. Do real wgpu/Metal resources survive patches once cached callbacks, vtables, and renderer-thread coordination are included?

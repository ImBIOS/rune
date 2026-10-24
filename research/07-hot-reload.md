# Hot reload: keep the native host, reload the view and small handlers

Research date: 2026-10-21. This report supports the choice of Rune's reload architecture. It is source research, not a completed benchmark; the practical spike is separate.

## Summary

- Aim for under 300 ms from saved edit to presented frame for tokens, view structure, and small interpreted handlers on both Mac and a connected iPhone. Keep rendering, text, layout, animation evaluation, and durable app state native.
- Use one reloadable view representation on both platforms. A small declarative language or restricted expression interpreter avoids native compilation and executable-memory changes for the edits designers make most often.
- Add Subsecond for Mac and simulator Rust implementation edits. Its current source explicitly excludes physical iOS devices. Its documentation also understates current workspace support, so use the inspected implementation rather than the introductory limitations alone.
- Native function replacement does not migrate Rust state. Struct layout, closures, futures, trait-object vtables, thread-local storage, and destructor ownership all need an explicit compatibility boundary.
- Flutter demonstrates that device hot reload and JIT remain possible with debugger cooperation. Current Dart/Flutter code contains special executable-page handling and version-specific LLDB workarounds. Attaching a debugger once is not a general solution for a Rust dylib loader.
- InjectionNext advertises device support, but a detailed July 2026 iPhone failure and an October maintainer response leave modern device loading unresolved. HotReloadKit deliberately supports only Mac and simulator. Neither establishes a dependable physical-iPhone Rust patch path.
- wasm3, WAMR's interpreter, and wasmi can execute replacement logic without native JIT. They still incur module loading, interpretation, and, for Rust guests, Rust compilation. A current iPhone benchmark supports keeping expensive UI work outside the interpreter.
- Build state preservation around explicit node/state IDs and schemas. Preserve compatible state; migrate declared changes; reset only incompatible subtrees; restart with a snapshot for native-core changes. Apply each generation atomically and acknowledge its first presented frame.
- No source reviewed establishes a general sub-300 ms native-Rust or physical-device edit-to-pixels guarantee. The budgets below are proposed engineering targets, not measurements.

## 1. Measure the whole edit-to-frame path

For a save-triggered reload, measure:

```text
file save → detection/batching → parse or compile → link/package
          → device transfer → validate/load → commit generation
          → reconcile/layout → GPU work → presented frame
```

An application receipt after `dlopen`, or a CLI message after compilation, stops too early. Record generation IDs at every stage. For interactive controls, use the input event rather than a file save as the start. At 120 Hz a frame interval is 8.33 ms; at 60 Hz it is 16.67 ms. Compilation can happen in the background, but final application must also fit the active frame workload.

### What the available numbers actually establish

| System | Number | Evidence class and limitation |
|---|---|---|
| Subsecond/ThinLink | Incremental build below 500 ms | Author claim in [runtime docs, lines 168–183](https://github.com/DioxusLabs/dioxus/blob/7a817390a69294f614b3055aae2dacdf25c050a2/packages/subsecond/subsecond/src/lib.rs#L168-L183). No device-to-presented-frame distribution. |
| Subsecond symbol cache | Docsite patch time reduced from 3 s to 1.1 s | Author-reported measurement in [source comment, lines 63–68](https://github.com/DioxusLabs/dioxus/blob/7a817390a69294f614b3055aae2dacdf25c050a2/packages/cli/src/build/patch.rs#L63-L68). Hardware and sampling unspecified. Shows real workloads can exceed 300 ms. |
| HotReloadKit | Roughly 100–300 ms for safe edits | [README](https://github.com/kitlangton/hot-reload-kit/blob/410708f0a204618d7ef3afc923f3e5790bcc80d6/README.md) claim, not a published latency dataset. Runtime receipt follows invalidation, not confirmed display. |
| Flutter | Example log says 978 ms | [Official hot-reload guide](https://docs.flutter.dev/tools/hot-reload). Illustrative output, not a benchmark or SLA. |
| React Native Fast Refresh | “Within a second or two” | [Official guide](https://reactnative.dev/docs/fast-refresh), qualitative product expectation rather than a sub-300 ms result. |
| `hot-lib-reloader` | 500 ms default debounce; suggests 20–50 ms | [README](https://github.com/rksm/hot-lib-reloader-rs/blob/master/README.md). Configured waiting time, before compilation/loading/rendering costs. |
| Slint live preview | 50 ms rebuild debounce | [Implementation](https://github.com/slint-ui/slint/blob/6a6fe8c874809d8c35a656809ccb33582276989d/internal/live-preview/lib.rs). Its separate macOS 200 ms [watcher rescan delay](https://github.com/slint-ui/slint/blob/6a6fe8c874809d8c35a656809ccb33582276989d/internal/live-preview/file_watcher.rs#L98-L108) repairs missed events after watch changes; it is not a universal 200 ms edit delay. |
| Makepad current scripting reload | No trustworthy end-to-end number found | Source establishes a no-Rust-compile path for script edits, not a measured latency. |

The Subsecond CLI's completion log also precedes a rendered-frame acknowledgement ([builder source](https://github.com/DioxusLabs/dioxus/blob/7a817390a69294f614b3055aae2dacdf25c050a2/packages/cli/src/build/builder.rs#L883-L895)). Do not compare these figures as if they all measured the same endpoint.

## 2. Subsecond removes much linking work, but leaves Rust compatibility to Rune

Inspected Dioxus revision: `7a817390a69294f614b3055aae2dacdf25c050a2` (2026-10-17). Published Subsecond documentation currently exposes version 0.7.10; the implementation findings here refer to the pinned repository revision.

### ThinLink reuses the running program's symbols

The baseline build records compiler/linker arguments and symbol information. For a patch, the CLI recompiles changed workspace crates and the tip crate, then links fresh code against symbols already resident in the running process. It generates assembly stubs for otherwise undefined symbols, using an address supplied by the running application to account for address-space randomization. The patch therefore avoids relinking unchanged dependency archives into a whole new application.

The runtime loads the patch image, adjusts the address map, retains the image, and publishes a new jump table. Calls through `subsecond::call`/`HotFn` reach updated implementations. Framework integration still decides when to invalidate/rebuild views and how to handle retained state. See [workspace/link flow](https://github.com/DioxusLabs/dioxus/blob/7a817390a69294f614b3055aae2dacdf25c050a2/packages/cli/src/build/link.rs#L135-L265), [symbol mapping](https://github.com/DioxusLabs/dioxus/blob/7a817390a69294f614b3055aae2dacdf25c050a2/packages/cli/src/build/patch.rs#L424-L463), and [runtime loading](https://github.com/DioxusLabs/dioxus/blob/7a817390a69294f614b3055aae2dacdf25c050a2/packages/subsecond/subsecond/src/lib.rs#L498-L546).

Two documentation discrepancies matter:

1. **Workspace support exists in the inspected implementation.** The runtime's introductory docs still say only the tip crate patches. `compile_workspace_hotpatch` now replays changed workspace crates in dependency order and links their updated rlibs. The old [workspace issue #4160](https://github.com/DioxusLabs/dioxus/issues/4160) was closed through #5291 in February 2026. This does not prove every workspace configuration works, but “main.rs only” is stale guidance.
2. **Do not assume exact function-body diffing.** ThinLink's docs advertise object-file diffing. The inspected path collects fresh `.rcgu.o` files plus updated workspace rlibs, and the patch map matches same-name symbols. That is not evidence that only bytewise-changed function bodies enter each patch. Measure patch size and invalidation scope rather than assuming ideal granularity.

### Unchanged state is only safe when its representation remains compatible

The [runtime's own limitations](https://github.com/DioxusLabs/dioxus/blob/7a817390a69294f614b3055aae2dacdf25c050a2/packages/subsecond/subsecond/src/lib.rs#L77-L122) include:

- Struct size/alignment/layout changes need framework-managed re-instancing. Calling new code on an old layout can crash.
- Tracked globals persist, renamed globals are new, and changed initializers are ignored. Newly introduced globals do not have their destructors run.
- Patched-crate thread-locals reset. Current [TLS patch handling](https://github.com/DioxusLabs/dioxus/blob/7a817390a69294f614b3055aae2dacdf25c050a2/packages/cli/src/build/patch.rs#L1192-L1234) still makes per-patch copies; initializer fixes do not make them persistent host state.
- Function pointer identity is not a reliable fine-grained change detector across generations.

`subsecond::call` provides a reload/unwind boundary, not a proof that arbitrary existing Rust values match new code. The inspected `try_call` path casts callable pointers; it does not provide comprehensive schema/layout validation. A changed async function also changes its compiler-generated future type. Existing futures, captured closures, and trait objects can retain old implementation pointers even if a top-level function was replaced.

**Rune use:** keep the scheduler, runtime/Tokio ownership, state store, GPU objects, and thread-locals in the stable host. Patch small view-builder/reducer entry points. Reject incompatible schemas before invoking replacement code. Use generation-scoped tasks and callbacks, with quiescence or cancellation at reload. Keep old images resident until safe reclamation is demonstrable; bound memory with a snapshot/restart policy.

### Platform labels need qualification

The source lists macOS, Linux, Windows, Android, wasm32, and iOS arm64, then explicitly says **physical iOS devices are unsupported due to code signing; use the simulator** ([lines 193–208](https://github.com/DioxusLabs/dioxus/blob/7a817390a69294f614b3055aae2dacdf25c050a2/packages/subsecond/subsecond/src/lib.rs#L193-L208)). This is project-declared support, not a matrix independently tested here. For Rune, count Mac/simulator native patching as available to investigate and physical iPhone native patching as unavailable through stock Subsecond.

## 3. A stable-ABI dylib is simpler to reason about, but has the same device barrier

[`hot-lib-reloader`](https://github.com/rksm/hot-lib-reloader-rs) watches a dynamic library, uses unique shadow copies to avoid loader caching/locking problems, loads exported symbols through `libloading`, and coordinates replacement. It does not eliminate Cargo/rustc compilation. Its examples using `#[unsafe(no_mangle)]` do not automatically establish a stable C ABI; unmangled names and ABI stability are different properties.

Its [documented limitations](https://github.com/rksm/hot-lib-reloader-rs/blob/master/README.md#know-the-limitations) cover signature/layout mismatches, generic exports, globals, and changing `TypeId`s. The README also describes a tracing-callsite crash, illustrating how third-party global registries can retain references into unloaded images.

For a Rune-owned boundary, use an explicit `extern "C"` entry point returning a versioned `#[repr(C)]` function table. Pass opaque host handles, fixed-width scalars, or explicitly owned byte buffers. Keep allocation/freeing paired with the owning side. Do not pass `Vec`, `String`, Rust trait objects, references with hidden lifetimes, or panics across the boundary. Validate ABI version and schema fingerprints before installation.

Host-owned state can survive every library generation. Library-owned state needs a serialization/migration call into the old generation before retiring it. Every registered callback/task must either retain its owning generation or be unregistered and drained before unload. A C ABI does not solve those lifetime problems by itself.

**Tradeoff:** this is a useful Mac fallback if Subsecond integration proves too coupled to Dioxus tooling. It requires more explicit API design and still cannot load arbitrary downloaded native code on ordinary physical iOS. Reject it as the shared Mac/iPhone reload foundation.

## 4. Makepad and Slint demonstrate a more portable view boundary

### Makepad reparses scripts and reapplies them to a live native app

Inspected Makepad revision: `44ecb822b48106fa76bde4dfd309e5a8a7ff1932`. Current Splash documentation uses `script_mod!`; older `live_design!` descriptions refer to the archived system ([current scripting guide](https://github.com/makepad/makepad/blob/44ecb822b48106fa76bde4dfd309e5a8a7ff1932/docs/agents/splash.md)).

The reload path receives changed file contents, extracts script blocks from Rust source, matches them to compiled sites, checks block counts and embedded Rust `#(...)` placeholder counts, then tokenizes/parses the replacement scripts. It stages a new override map and publishes it only after validation of the batch. See [live_reload.rs, lines 186–366](https://github.com/makepad/makepad/blob/44ecb822b48106fa76bde4dfd309e5a8a7ff1932/platform/src/live_reload.rs#L186-L366).

The app then reapplies scripts to the existing application through `ScriptApply` ([app_main.rs](https://github.com/makepad/makepad/blob/44ecb822b48106fa76bde4dfd309e5a8a7ff1932/platform/src/app_main.rs#L326-L350)). `StudioToApp::LiveChange { file_name, content }` supplies a transport-shaped entry point ([cx_shared.rs](https://github.com/makepad/makepad/blob/44ecb822b48106fa76bde4dfd309e5a8a7ff1932/platform/src/os/cx_shared.rs)). This is a concrete design for sending source as data rather than loading a new native library.

Preservation follows the existing object's apply behavior; it is not universal migration of arbitrary Rust fields. Native Rust code and changed embedded Rust values still require compilation. Shader cache invalidation is a separate concern from applying style properties. Source inspection establishes these mechanisms, but does not establish current end-to-end iPhone transport reliability or a latency distribution.

**Steal:** a live language embedded near native code, stable binding sites, preflight validation, batched application, and native hot paths. Prefer explicit IDs over line/column-derived identity where source movement should preserve state.

### Slint reconstructs a component and restores explicitly retained values

Inspected Slint revision: `6a6fe8c874809d8c35a656809ccb33582276989d`. The live-preview implementation watches `.slint` dependencies and resources, compiles on a worker using the interpreter, and retains the last working component on compilation failure. On success it creates a new component using the existing window ([live_component.rs, lines 118–145](https://github.com/slint-ui/slint/blob/6a6fe8c874809d8c35a656809ccb33582276989d/internal/live-preview/live_component.rs#L118-L145)).

It restores the properties and callbacks held in its explicit registration maps ([lines 174–199](https://github.com/slint-ui/slint/blob/6a6fe8c874809d8c35a656809ccb33582276989d/internal/live-preview/live_component.rs#L174-L199)). That is narrower than preserving every current internal widget value, focus target, scroll offset, or running animation. Values changed only inside the old instance are not automatically all represented by those maps.

**Steal:** worker compilation, diagnostics, an existing-window handoff, and last-good retention. **Improve for Rune:** define persistent state separately from the generated view tree, so state survival does not depend on incidental property registration. The inspected preview facility is not proof of a turnkey physical-iPhone live-preview product; Rune would supply the device host and transport.

## 5. Flutter and React preserve state because their runtimes know its identity

### Flutter controls both executable code and object metadata

Flutter's debug hot reload sends changed Dart program material to the VM, updates code, and rebuilds the widget tree while preserving compatible state. `main()` and `initState()` do not rerun; static/global initialization is generally retained. Native Swift/Objective-C/Kotlin/Java changes require a restart/rebuild. Enum/class changes and changes to generic type parameter counts are among documented reload restrictions ([official guide](https://docs.flutter.dev/tools/hot-reload)).

The [Dart VM reload design](https://dart.googlesource.com/sdk/+/42ec219908a0ce38b0f16593302f96cad6f38e9b/docs/Hot-reload.md) explains replacement of method dictionaries, retention of existing field values, and invalidation of inline caches and optimized assumptions. This works because the VM owns object metadata and code dispatch. Rust's unmanaged existing allocations do not come with that capability.

Flutter supports physical and virtual targets in debug mode. Its device JIT/debugger machinery is substantial, as detailed below. The reusable idea is the division between persistent state and replaceable behavior, not the assumption that a native Rust function patch inherits Dart's migration semantics.

### React Fast Refresh uses component families and hook signatures

The [refresh runtime](https://github.com/facebook/react/blob/main/packages/react-refresh/src/ReactFreshRuntime.js) associates old/new component implementations with stable families. It compares component kinds and hook signatures, including custom-hook dependencies, then separates families that can update from families that must remount.

Function-component hook state can survive compatible edits; class component state is not preserved. Changed hook structure can force a reset. Effects, memoized values, and callbacks refresh even with otherwise unchanged dependency arrays, and module export shape affects how far refresh propagates ([React Native guide](https://reactnative.dev/docs/fast-refresh)).

**Rune lesson:** give every reloadable component a stable family ID and a state-schema signature. Distinguish “recompute this view” from “recreate this state.” Rerun subscriptions with cleanup rules instead of preserving stale closure pointers. React's “second or two” wording is not evidence for the target latency.

## 6. Swift tooling is useful precedent, with a sharp simulator/device split

### HotReloadKit has a narrow compatibility planner worth copying

Inspected revision: `410708f0a204618d7ef3afc923f3e5790bcc80d6`, plus local `README.md` and `docs/architecture.md`.

HotReloadKit parses baseline and edited Swift with SwiftSyntax, replaces allowed function/computed-property bodies with empty bodies, and compares the remaining structure. Compatible edits produce `@_dynamicReplacement` thunks with source-scoped private imports. It compiles those thunks with `swiftc -emit-object`, links a Mach-O bundle against the baseline executable using `clang -bundle -bundle_loader`, publishes an atomic manifest, loads the bundle with `dlopen`, invalidates affected observers, and writes a receipt ([architecture](https://github.com/kitlangton/hot-reload-kit/blob/410708f0a204618d7ef3afc923f3e5790bcc80d6/docs/architecture.md)).

Stored-property, signature, and new-type changes fall back to rebuilding. Opaque return types need additional restrictions; TCA reducer edits are constrained to suitable closure bodies. SwiftUI state survives when the existing identity remains. Recreating a subtree through `.rebuildsOnHotReload()` resets its local state, focus, and scroll, while externally owned models survive ([README](https://github.com/kitlangton/hot-reload-kit/blob/410708f0a204618d7ef3afc923f3e5790bcc80d6/README.md)).

The runtime is intentionally inert on physical devices and in release builds. Its filesystem manifest depends on the shared host/simulator filesystem; adding a socket would solve transport, but not native device loading. Loaded Swift images remain resident; the documented 50-generation restart policy bounds accumulation. Its claimed 100–300 ms is promising for a narrow edit class, not a measured device guarantee.

### Modern SwiftUI previews are not simply third-party dynamic replacement

Apple's current [build-product layout documentation](https://developer.apple.com/documentation/xcode/understanding-build-product-layout-changes) describes the debug executable stub, application `.debug.dylib`, and `__preview.dylib`. Older descriptions focused on generated dynamic-replacement thunks. HotReloadKit deliberately uses its own compatible build layout and replacement mechanism; its README's relationship to previews should not be read as a complete description of current Xcode internals.

Apple's supported preview workflow, including on-device previews, does not establish that an ordinary development app may receive a newly built dylib into its data container and `dlopen` it. Use previews as precedent for edit classification and tooling, not evidence for a portable Rust device loader. No comparable current edit-to-presented-frame dataset was found.

### InjectionNext's actual loader is more informative than its support label

InjectionIII and its successor InjectionNext rebuild changed Swift/Objective-C source and replace behavior using injection/runtime mechanisms. InjectionNext advertises device support. At inspected revision `b39c9f15cf8d8b99978297cb76514a7e0280ec8c`, the host signs generated dylibs using a selected Apple Development identity ([NextCompiler.swift, lines 392–411](https://github.com/johnno1962/InjectionNext/blob/b39c9f15cf8d8b99978297cb76514a7e0280ec8c/App/InjectionNext/NextCompiler.swift#L392-L411)); the app receives bytes, writes a temporary file, and loads/patches it ([runtime, lines 349–354](https://github.com/johnno1962/InjectionNext/blob/b39c9f15cf8d8b99978297cb76514a7e0280ec8c/Sources/InjectionNext/InjectionNext.swift#L349-L354)).

Counterevidence is specific and recent. [Issue #81, July 2026](https://github.com/johnno1962/InjectionNext/issues/81#issuecomment-5106386779) reports an iPhone 15 on iOS 26.4.2 with Xcode 26.3 rejecting the dylib with `code signature invalid … (errno=1)`, despite a matching signing identity and corrected load commands. An entitlement hypothesis did not resolve the report. On [2026-10-06 the maintainer wrote](https://github.com/johnno1962/InjectionNext/issues/81#issuecomment-6006741081), “Been unable to test this on an iOS 26 phone thus far.”

The thread also [quotes Apple DTS](https://github.com/johnno1962/InjectionNext/issues/81#issuecomment-5106411624) distinguishing OS/embedded libraries from downloaded libraries in the data container. The original [Apple forum thread](https://developer.apple.com/forums/thread/773210) was blocked by security verification during this research, so that quotation is secondary evidence. The thread does not prove every native injection technique is impossible on every iOS version. It does rule out treating “sign the dylib with the same identity” as an established solution.

## 7. Physical iPhone reload must separate data loading from native executable loading

Apple's [runtime process security documentation](https://support.apple.com/guide/security/runtime-process-security-sec15bfe098e/web) explains signed entitlements and execute-never protections. A development signing identity, Developer Mode, and `get-task-allow` are not interchangeable with permission to execute arbitrary downloaded native pages. `get-task-allow` enables debugger attachment; macOS's hardened-runtime JIT entitlement is not a general iOS entitlement an ordinary app can add.

Historically, debugger attachment and `CS_DEBUGGED` relaxed relevant checks. See the [XNU code-signing flags](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/kern/cs_blobs.h) and [technical investigation of iOS 18.4 changes](https://gist.github.com/osy/8940e5ae5f24646b808f58d197883ca5). With newer Trusted Execution Monitor restrictions, executable pages may require debugger-assisted handling rather than merely a one-time attachment.

Current working source shows the distinction:

- Dart's [virtual_memory_posix.cc](https://github.com/dart-lang/sdk/blob/main/runtime/vm/virtual_memory_posix.cc) creates executable mappings/aliases and calls `NOTIFY_DEBUGGER_ABOUT_RX_PAGES` for debugger-assisted preparation.
- Flutter's [ios/lldb.dart](https://github.com/flutter/flutter/blob/master/packages/flutter_tools/lib/src/ios/lldb.dart) installs a Python callback that touches those pages via LLDB `WriteMemory`, including the `IHELPED!` marker. The inspected implementation also contains an iOS 27 manual-continue workaround.
- [UTM's corresponding change](https://github.com/utmapp/UTM/commit/07c79d17c5c9cc3136d50b00867bdeb56ca070d2) provides another concrete debugger-assisted JIT precedent.

Therefore, both “iOS JIT is impossible” and “attach LLDB and normal dylib loading works” are too broad. A native Rust experiment could investigate debugger-assisted executable pages, but would additionally need Mach-O/object relocation, symbol resolution, unwind metadata, TLS handling, architecture-specific details, and patch lifetime management. That is a new loader/runtime project, not an entitlement flag for Subsecond.

| Route | Mac | iOS simulator | Physical iPhone |
|---|---|---|---|
| Tokens, assets, parsed view descriptions | Ordinary data reload | Ordinary data reload | Ordinary data reload; implement device transport |
| Native Subsecond patch | Project-supported | Project-supported | Explicitly unsupported in current source |
| HotReloadKit | Supported Debug path | Supported Debug path | Runtime deliberately inert |
| InjectionNext dylib transfer | Native injection path | Native injection path | Advertised, but recent signing/load failure unresolved |
| Dart/Flutter debug hot reload | Supported framework workflow | Supported framework workflow | Supported framework workflow with debugger machinery |
| Interpreter embedded in signed host | Feasible | Feasible | Avoids native executable mapping for replacement bytecode |
| Custom debugger-assisted Rust loader | Unnecessary for baseline | Unnecessary for baseline | Experimental, no Rune implementation or latency proof |

For development, transfer ordinary data to an already installed native host. A release build can compile out the reload transport and ship bundled descriptions or a native lowering of the same representation. App Store distribution policy is a separate question from whether a developer-installed interpreter can run; do not describe an interpreter choice as automatic approval for arbitrary downloaded production features.

## 8. Interpreted Wasm can carry Rust logic, but should not carry the renderer

An interpreter treats incoming Wasm bytes as data and executes them through code already in the signed host. Configure interpreter-only builds explicitly: WAMR also offers AOT/JIT modes, and a native-code AOT artifact does not gain the portability of interpreted bytecode merely by coming from a Wasm toolchain.

| Runtime | Mechanism and fit | Main question for Rune |
|---|---|---|
| [wasm3](https://github.com/wasm3/wasm3) | C interpreter, internal execution representation without native JIT. Strong current scalar/call-heavy benchmark results. | Does the chosen Rust Wasm target emit only supported features? Measure load time and the host-call boundary. |
| [WAMR](https://github.com/bytecodealliance/wasm-micro-runtime) fast interpreter | Configurable C runtime with interpreter, AOT, and JIT options. Use interpreter only. | Select features carefully; published benchmark patches must not be mistaken for upstream functionality. |
| [wasmi](https://github.com/wasmi-labs/wasmi) | Rust interpreter with validation, embedding APIs, and fuel metering. Version 2.0 arrived 2026-09-01 with accumulator IR and revised dispatch. | Benchmark current v2, rather than using historical rankings. Its [changelog](https://github.com/wasmi-labs/wasmi/blob/main/CHANGELOG.md) claims 2.2× geomean improvement over v1; that is not an iPhone comparison. |

The strongest current physical-device evidence found is a [2026-09-22 interpreter comparison](https://github.com/rebeckerspecialties/wasm-benchmark/blob/main/docs/runtime-comparison-2026-09-22.md), with raw data and N=10 runs on an iPhone XS Max/A12, iOS 18.7.10, primarily E-core execution. Across 17 mutually supported cases, geometric means of CPU time relative to the fastest runtime per case were wasm3 **1.04**, WAMR **1.38**, and Pulley **1.68**. Those are relative interpreter scores, not slowdowns versus native, and wasmi was absent. The WAMR build carried patches.

That benchmark's full femtovg Wasm guest achieved only **7.0 fps** for scene 0 on wasm3/A12. Pulley's load-time translation of the roughly 1 MB guest took about **2.7 s**. These workloads do not predict tiny-handler reload times, but they directly contradict assuming that a full interpreted Rust UI stack will comfortably maintain 120 Hz or load below 300 ms.

Recommended boundary:

- Native host owns layout, text shaping, rasterization, springs/gesture integration, resource caches, and authoritative state.
- Guest computes small event reductions and view descriptions when dependencies change. It does not run a whole per-frame renderer.
- Host calls are batched through a compact command/result buffer. Avoid crossing the interpreter boundary for every property of every node.
- Stable numeric handles identify native state and resources. Guest memory is generation-local unless an explicitly versioned serialization contract migrates it.
- Fuel/time limits contain runaway handlers. A failed candidate generation leaves the previous one active.

Rust-to-Wasm still invokes rustc. A small isolated crate may compile quickly, but no sub-300 ms result was established here. For the primary design loop, a parsed view/expression language is the stronger latency bet. Wasm is an optional logic tier when Rust reuse is worth the compilation and embedding cost. Reusing an old guest's linear-memory bytes with a changed Rust allocator/data layout is not state migration.

## 9. Tokens, assets, and shaders should each have a direct reload path

### Reuse knobs' typed remote-control model

Local sources inspected: the knobs library's `README.md`, especially `createRemoteControls`, and `docs/migration.md`. Remote descriptors carry typed values such as spring, color, point, easing, gradient, and curve. `onSet` handles individual updates; `onSetBatch`/`setMany` batches related edits; `null` resets a value. Incoming `.sync` updates avoid echoing back and creating undo entries.

The requested tuning files from a private iOS client were absent from the current checkout. They were inspected through Git revision `67fbf2b6`: `scripts/tune-server.ts` and `scripts/tune-page/index.ts` in the private iOS client. That historical implementation uses `/phone` and `/ui` WebSockets, capability negotiation, atomic batch validation, and the older `setValues` name. State broadcasts are coalesced for 16 ms. The page's `echo: 5000` setting suppresses stale echoes; it is not a five-second outbound delay. Do not assume those historical paths or protocol names describe the present checkout.

For Rune, connect each typed descriptor to a stable host property ID. Validate a whole edit transaction before applying it on the UI thread. Preserve spring position and velocity when changing targets or parameters. Stream compact deltas while dragging; persist the final chosen values to source/configuration with an explicit policy so a live value is not mistaken for a saved default.

### Assets can swap by stable resource handle

Proposed asset pipeline: content hash → transfer missing bytes → decode/shape/upload off the render thread where possible → replace the resource generation at a frame boundary. Keep the old resource on decode failure and while GPU work references it. A font change must invalidate affected text/layout caches; it is more expensive than changing a color. Large texture uploads and font work get separate latency reporting rather than being hidden in the token budget.

### Shader compilation is available without arbitrary CPU JIT

Metal exposes runtime shader-library compilation through [`MTLDevice.makeLibrary(source:options:)`](https://developer.apple.com/documentation/metal/mtldevice/makelibrary(source:options:)). For a portable WGSL front end, [naga](https://github.com/gfx-rs/wgpu/tree/trunk/naga) can validate and translate to backend shader code. Neither API implies a universal shader-to-pixels latency.

Proposed pipeline: parse/validate → compile shader and pipeline asynchronously → validate resource/binding compatibility → stage the new pipeline → swap at a frame boundary. Retain old pipeline/resources until prior GPU submissions complete. Binding-layout changes need a compatible resource rebuild or a rejected patch. On any error, retain the last working pipeline and report source diagnostics. Cached small shaders may fit 300 ms; cold or complex pipeline compilation needs measurement and must not block ordinary view/token edits.

## 10. State preservation needs an explicit contract

The state host should retain application entities/signals, navigation, keyed list identity, text selection/composition, focus, scroll anchors, gesture ownership, and animation position/velocity independently of replaceable view code. Stable identity must be explicit enough to survive inserting a sibling or moving a source block. A source line number or Rust `TypeId` alone is insufficient.

### Classify edits before publishing them

| Edit | Proposed behavior |
|---|---|
| Color, spacing, text, spring target/parameters | Update values, invalidate affected dependencies, retain state and motion continuity. |
| View structure with the same explicit node/state IDs | Reconcile nodes; preserve compatible state, focus, and scroll anchors. |
| Handler/body change with the same state and call schema | Install new behavior at a safe boundary; retained state remains authoritative. |
| Added optional state field with a declared default | Run a transactional schema migration. Existing fields keep their values. |
| Renamed field with an explicit migration | Migrate by stable field ID or declared mapping, not coincidental position. |
| Incompatible component state or changed hook-like signature | Reset only that component/subtree and report the reason. |
| Native renderer/layout ABI, allocator, or platform-shell change | Snapshot supported state, rebuild/relaunch, restore; replay if required. |

Schemas must capture semantic compatibility, not just `size_of`/alignment. Two equally sized fields can change meaning or order. State outside the stable host needs an explicit export/import contract; absence of a migration means reset or restart, not a best-effort pointer cast.

### Commit one generation, with no mixed old/new behavior

Proposed transaction:

1. Receive generation N with base-generation ID, source hashes, required host capabilities, ABI/schema fingerprints, and all changed resources.
2. Parse/compile and validate in staging. Reject missing dependencies, stale bases, incompatible bindings, or invalid migration plans without mutating the live generation.
3. At a safe event/frame boundary, finish the current reducer. Drain or cancel generation-owned work. Take the migration snapshot from the current state, not an earlier background copy that would lose intervening user input.
4. Apply migrations and install view/handler/resource references as one commit. Route future events through stable handler IDs. Drop late async results from retired generations unless their host-owned operation explicitly survives reload.
5. Reconcile affected nodes, render, and acknowledge N on its first presented frame. Release retired resources only when CPU callbacks and GPU submissions no longer reference them.

Old native images may need to stay loaded because destructors/vtables/code pointers remain reachable. Interpreted generations are easier to reclaim if host handles and generation ownership are enforced. Validation can be atomic even when parsing allocates staging objects; execution-time traps still need an explicit rollback/reset policy rather than a claim that every script has been proven safe.

### Snapshot/replay is a fallback, not a substitute for live identity

Record semantic inputs, external I/O responses, timer delivery, a controlled clock, and random seeds. Rebuild from a snapshot and replay events to restore a useful development state after native changes. Network mutations and other external side effects must be suppressed or substituted during replay, with stable operation IDs to avoid duplicate sends.

Replay needs the same deterministic scheduler/input model used for headless agent verification. It will not automatically recreate arbitrary OS handles, an in-progress IME session, or undocumented platform state. Expose which state was preserved, migrated, reset, or reconstructed in the inspector.

## 11. Steal and avoid

### Steal

- Subsecond's baseline-symbol reuse and jump-table entry points for desktop native edits.
- HotReloadKit's conservative compatibility planner, atomic publication, and runtime receipt, extended through actual presentation.
- Makepad's source-as-data update path and preflight of a complete script batch.
- Slint's worker compilation, diagnostics, and last-good retention.
- React's stable component families and compatibility signatures.
- Flutter's persistent-state/replaced-behavior model, with explicit restart-only edit classes.
- Knobs' typed descriptors, batch validation, remote synchronization, and echo suppression.

### Avoid

- Calling simulator support “iOS device hot reload.”
- Claiming <300 ms from a compiler timer, animation GIF, or unqualified README number.
- Depending on arbitrary data-container dylib loading for the main iPhone workflow.
- Patching arbitrary Rust allocations or async futures across type-layout changes.
- Using reloadable-crate TLS/globals as the durable state host.
- Unloading libraries while callbacks, tracing registries, vtables, or destructors still reference them.
- Putting text/layout/rendering or spring integration into an interpreter merely to make everything reloadable.
- Rebuilding the entire view state on every edit, or replaying external side effects to reconstruct it.
- Applying half of a multi-file save or reporting success before the changed generation is visible.

## 12. Recommended Rune layers and latency budgets

Use a native runtime with one persistent state store and one keyed view representation. Expose four reload classes through the same development connection:

1. **Live values:** typed tokens, animation parameters, and resource references. This should be the quickest path on every platform.
2. **Live views:** a declarative tree plus bounded expressions/handlers that can be parsed without invoking Rust. Keep IDs, schemas, diagnostics, and dependency tracking shared with the native runtime. The same representation can be bundled or lowered for production.
3. **Replaceable logic:** small interpreted Wasm modules when Rust reuse is needed; Subsecond as the Mac/simulator native acceleration path. Both use the same host-state and handler-ID contract where practical.
4. **Native rebuild:** renderer/platform/core changes rebuild with snapshot restoration and deterministic replay support.

Do not make an elaborate language a prerequisite for the first spike. Start with a minimal typed tree and a tiny handler/expression vocabulary that can change real behavior. Let the device result determine whether to expand that language or add Wasm.

### Proposed warm-path budgets, not measurements

All figures below are milliseconds. Assumptions: app already running in the foreground; established connection; small payload (tokens or view/handler delta up to roughly 32 KiB); no large asset upload; warm caches; no new native build. These are allocations to validate, not percentiles inferred from other frameworks.

| Stage | Tokens, Mac / iPhone | View data + small interpreted expressions, Mac / iPhone |
|---|---:|---:|
| Detect/settle save or collect input | 10 / 10 | 20 / 20 |
| Parse, validate, package | 2 / 2 | 25 / 25 |
| Established transport | 2 / 15 | 2 / 20 |
| Receiver decode/validation/staging | 2 / 5 | 15 / 30 |
| Wait for safe boundary and commit | 10 / 10 | 10 / 10 |
| Reconcile/layout/render/present | 17 / 25 | 25 / 35 |
| Scheduling/variance reserve | 15 / 25 | 40 / 60 |
| **Allocated total** | **58 / 92** | **137 / 200** |

The render stage can span multiple display intervals; these numbers do not authorize a single 25–35 ms UI-thread stall. Background staging and incremental work must preserve the normal 8.33 ms frame budget. View updates have 100 ms of headroom under the 300 ms target, which may be consumed by realistic trees, transport variance, or cache misses.

For small Rust-to-Wasm edits, a separate **aspirational iPhone allocation** is 20 ms detect + 100 ms compile/package + 20 ms transfer + 40 ms load/validate + 10 ms commit + 35 ms render/present + 50 ms reserve = **275 ms**. The 100 ms Rust compilation and 40 ms module-load allocations are unproven and may fail. If they do, retain the fast interpreted view path rather than moving its target to match Rust compile time.

For Mac Subsecond, write the total as **rustc + patch link + approximately 50–100 ms delivery/application/presentation overhead**, with that overhead also a proposed allowance. A 300 ms total would leave only about 200–250 ms for compilation/linking. The reviewed evidence does not establish that allowance for realistic Rune crates. Treat native patching as a convenience tier with measured per-edit results, not the foundation of the latency promise.

### Device transport must be measured independently

Use a long-lived bidirectional connection with generation IDs, hashes, capability negotiation, and last-applied/last-presented acknowledgements. Test local-network and a deliberately configured USB-forwarded route separately. A plugged-in phone may still be sending traffic over Wi-Fi; log the actual route. USB pairing/forwarding setup and cold connection time are separate from warm edit latency. Remote build-worker-to-Mac artifact transfer, when used, must be counted as an extra stage.

Use a host monotonic clock for save-to-returned-present-ack time. That includes acknowledgement return transit and is a conservative observable bound. Device timestamps partition receive-to-present without cross-machine clock assumptions. For more precise one-way attribution, estimate clock offset and uncertainty explicitly. A submitted command buffer is not a presented drawable; use presentation timing where available and label any proxy.

### Rejected foundations

| Alternative | Why reject it as the primary architecture |
|---|---|
| Subsecond everywhere | Current physical iPhone exclusion, Rust compile variability, and no automatic arbitrary-state migration. |
| One stable-ABI native dylib everywhere | Cleaner boundary, same device executable-loading problem. |
| Build/sign/download a dylib into the iPhone container | Recent concrete failure; no dependable supported recipe established. |
| Custom debugger-assisted native Rust JIT first | High loader/runtime cost and OS-version coupling before proving the basic design loop. Keep as a later experiment. |
| Entire UI stack in interpreted Wasm | Conflicts with the 120 Hz priority; current full-renderer benchmark and load-time evidence are poor. |
| Rust-to-Wasm for every spacing/text edit | Retains compiler latency for edits that can be parsed directly. |
| Restart and replay for every edit | Loses immediate interaction continuity and makes external side-effect reconstruction part of the common path. |
| Adopt Flutter or a WebView solely for hot reload | Gives up the requested ownership of Rune's native rendering/layout/state stack. Reuse the state model instead. |

## 13. Validate the physical-iPhone path first

The first spike should run a development-signed native host on a physical iPhone, launched **without an attached debugger**, with the same minimal view representation on Mac. Keep native state, rendering, and animation alive while a remote edit changes a token, a keyed subtree, and a small event handler. A counter whose increment changes from one to two proves changed behavior rather than only remote styling.

The spike should answer these questions in order:

1. Can token, tree, and handler changes travel to the phone as data and appear within 300 ms, measured through presentation? Record hardware, OS/toolchain, payload sizes, transport route, warm/cold status, and each pipeline stage. Collect at least 30 warm edits per class, reporting median, p95, maximum, and failures; this is proposed spike work, not work performed in this report.
2. Does the same state survive? Include a nonzero counter, text input/selection, a scrolled keyed list, navigation, and an active spring. Check that position/velocity continue across a parameter or subtree edit.
3. Does malformed input leave the last good generation visible? Does an incompatible state-schema change reset only its intended subtree? Can two rapid edits supersede an older staged generation without mixing resources?
4. Does the interpreter remain outside the frame-critical path? Measure handler execution and reload-commit work against the existing native frame budget. Add Wasm only after the simpler path establishes transport and state behavior.
5. Separately, how fast are small Mac Subsecond edits in a realistically partitioned workspace? Include body-only, changed workspace dependency, closure capture, added state field, and patched-crate TLS cases. Record rejection/reset behavior, not just successful reloads.

Only then consider a separate modern-iPhone native-loader experiment with debugger assistance. Its acceptance criterion is repeatable success across the actual device/OS/toolchain combination, including reconnect and multiple generations, not one successful `dlopen`.

## Open questions

- How much application behavior must be live for the first consumers? That determines whether a small expression language is sufficient before introducing Wasm.
- Can a shared typed view representation lower to native production code without dev/release semantic drift? Which constructs must be rejected or interpreted in both modes?
- Which state schemas can be generated mechanically, and which require explicit migrations? How should agents receive precise reset/restart diagnostics?
- What are actual p95 transport and presentation latencies on Kit's connected iPhone over USB forwarding versus local Wi-Fi?
- Which current interpreter wins on Rune-sized modules when compilation, instantiation, host calls, and memory reclamation are included? Include wasmi v2, absent from the cited device comparison.
- Does the chosen renderer expose reliable presented-frame timestamps on every target, or must some targets report a clearly labeled submission/completion proxy?
- How much resident memory accumulates across Subsecond/Swift-style native generations, and when should snapshot/restart occur?
- Can shader/layout changes preserve a moving interface without a visible discontinuity, and which binding or geometry changes require an explicit transition/reset?

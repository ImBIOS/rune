# 17. Performance architecture: cutting-edge tricks for Rune

Scope: UI-engine performance architecture, incremental computation, dirty tracking, render-thread handoff, GPU upload,
Rust compile speed, hot patching, PGO/BOLT. Written 2026-10-21 for tenet 7 (agents maintain the code, so fast-path +
reference-path and differential testing make "ugly but fast" safe).

**Provenance.** Marked **[fetched]** = I read the page this session. Marked **[memory]** = from my own knowledge, link
given but not re-read; verify before relying on numbers. Estimates are labelled.

## Summary

- Rune's biggest structural win is already in the plan: animation on the render thread with analytic springs. The
  cleverest extension is to **evaluate springs on the GPU** from a track table, with draw instances referencing
  property-tree node ids, so an animating frame costs ~one uniform write on the CPU (idea 1).
- Slate's invalidation measurement shows the cost model: a 10k-widget UI paying full price "to do nothing" took
  34.29 ms/frame; with caching it took 0.35 ms [fetched]. Rune's "zero work at rest" needs per-node dirty classes, not
  just a global "something changed" flag (idea 2).
- Cheapest big compile win on macOS, measured: `[profile.dev.package."*"] debug = false` cut a clean nushell debug
  build 22-30%; `split-debuginfo = "packed"` adds ~16 s per edit [fetched]. Linking is not the bottleneck on macOS (idea 10).
- Cranelift is **not ready on macOS** (no unwinding on macOS, arm64-macOS ABI mismatches with LLVM as of the June 2025
  report [fetched]); skip for Rune dev builds on Apple.
- Deterministic channel replay (PLAN §6) doubles as a perfect PGO training corpus (idea 12).
- Don't adopt Salsa/SwiftUI-AttributeGraph-style general dependency graphs; take only **early cutoff** (equal recompute
  → no propagation) and **constraint-based memoization** (comemo) at layout/text boundaries.

---

## Ideas

### 1. GPU-evaluated spring tracks + property trees (Chromium `cc` shape)  — verdict: **spike (S-new, high value)**

- **Source.** Chromium compositor property trees, the transform/clip/effect/scroll trees separate from layers, so
  compositor-thread animation only mutates a tree node
  ([docs](https://chromium.googlesource.com/chromium/src/+/main/docs/how_cc_works.md), [memory]; older but the design
  still stands). Flutter/Impeller and Core Animation's render server do the same split. Analytic spring segments are
  Psychopomp's model (PLAN §4.9).
- **What it achieves.** In Chromium, a transform animation never re-runs paint or layerization. Number: none I can cite.
- **How Rune uses it.** Scene build emits instances that carry `(transform_node, clip_node, effect_node)` u16/u32
  indices instead of baked positions. Tracks live in a GPU buffer as SoA `{t0, x0, v0, target, omega, zeta}`; a vertex
  shader (or a tiny compute pre-pass per node level) evaluates `x(t)` closed-form for the frame's target timestamp. The
  CPU per-frame cost while springs run becomes: write one `time` uniform, encode the same command buffer (or reuse an
  Indirect Command Buffer). Retargeting writes one 24-byte segment. Hit-testing/culling still needs CPU-side values:
  keep the CPU evaluator as the **reference path** and differential-test it against GPU readback in scenes
  (floating-point: use `f32` with identical formula; allow 1/64 px tolerance, which matches the plan's edge snapping).
- **Cost/risk.** Layout-edge animation changes node rects, so text/glyph positions that depend on them need the
  property-tree indirection to cover size as well as translation (an affine can't express an edge-only resize of a
  rounded rect with unscaled corners; those need per-instance `rect = lerp(edges)` evaluated in-shader). Hit-testing at
  presented positions needs the CPU evaluator anyway (it's cheap: a few flops per animating node).
- **Verdict.** Spike on the first scene with 1,000 animating cards: compare CPU instance-buffer rewrite vs GPU evaluate.
  Expected (estimate) win is CPU time per animating frame dropping from O(animating instances) to O(1).

### 2. Per-node dirty classes with a "volatile" escape hatch (Slate invalidation)  — verdict: **adopt now**

- **Source.** Unreal Slate [Invalidation docs](https://dev.epicgames.com/documentation/unreal-engine/invalidation-in-slate-and-umg-for-unreal-engine?lang=en-US)
  [fetched]: four classes (Volatile/Visibility → Paint → Layout → Child) in increasing cost, each implying the later
  steps, dirty list processed once per frame; Volatile widgets bypass paint caching but keep layout caching. Independent
  measurement ([ta-dah.tech, Aug 2026](https://www.ta-dah.tech/blog/retainer-box-vs-invalidation-box-what-10000-updating-widgets-taught-me-about-umg-performance/)
  [fetched]): 10,000 widgets, one updating: **34.29 ms → 0.35 ms** (Retainer) / **0.39 ms** (Invalidation Box);
  `SWidget::Paint` count 4,487 → 29. Gotcha they hit: a Retainer set to render on a fixed phase *re-rendered every
  frame* and ran at 50.9 ms, worse than no cache. Retainer gives about no extra CPU win over invalidation, costs a
  render target per retainer (Epic: "try Invalidation Boxes first").
- **How Rune uses it.** Dirty bits per node, SoA bitset: `PAINT`, `LAYOUT`, `CHILDREN`, plus `PRESENCE`/`VOLATILE`.
  Tree diff sets the minimal class (colour change = PAINT only, no Taffy call; key add/remove = CHILDREN). Bits
  propagate up as "subtree has dirty descendants" so traversals prune. Property tracks never set bits (they're
  render-thread data). Cached subtree textures (retainers) only for provably static, expensive-to-draw subtrees
  (blur, big vector art) with an exact invalidation rule; never on a phase timer.
- **Perf gate.** Count `nodes_visited_per_frame` and `nodes_painted` in the exact-counters harness; at rest both must
  be 0 and the display link stopped.
- **Cost/risk.** Missed invalidation = stale pixels. Mitigation (tenet 7): reference path re-runs everything every
  frame in test mode and diffs pixels against the incremental path ("paranoid mode", Slate itself ships a
  `Slate.InvalidationDebugging` equivalent).
- **Verdict.** Adopt. It's the cheap 100× for scrolling lists and big static chrome.

### 3. Early cutoff and "durability" from Salsa/Adapton/Incremental; skip the general graph  — verdict: **adopt the cutoff, skip the engine**

- **Sources.** [Salsa](https://salsa-rs.github.io/salsa/) (backdating: if a query recomputes to an equal value, its
  dependents are not invalidated; durability levels skip re-validating stable inputs), [Adapton](https://docs.rs/adapton)
  (demand-driven incremental, paper "Adapton: Composable, Demand-Driven Incremental Computation", PLDI 2014),
  [Jane Street Incremental](https://github.com/janestreet/incremental) (cutoff functions on nodes; heap by height),
  SwiftUI's AttributeGraph (not public; known via reverse engineering write-ups, [memory]). All [memory].
- **What it achieves.** Salsa's rust-analyzer reuses work across edits; no UI-frame numbers.
- **How Rune uses it.** Three specific uses, none a general graph:
  1. After Taffy runs, compare each node's new rect to the old; only changed rects emit layout-edge commands to the
     render thread (cutoff). Likewise text: new shaped-run hash equal → no re-upload.
  2. View-function inputs: `cx.read(model)` records a version number; if every read version is unchanged and props
     hash equal, skip re-running the view and reuse its `El` subtree (React Compiler does exactly this by
     auto-memoizing per-component values, [docs](https://react.dev/learn/react-compiler), [memory]).
  3. Durability: tag models as `static-ish` (theme, fonts, locale) vs `hot` (cursor, hover); a theme change is the only
     thing that revalidates the world.
- **Why not the full engine.** PLAN §4.8 already rejected signals because state must survive hot patches; a graph keyed
  by code identity breaks across patches. Version numbers on typed models don't.
- **Verdict.** Adopt cutoff + version-stamp memoization; skip Salsa/Adapton as dependencies.

### 4. Constraint-based memoization (comemo) for layout and text  — verdict: **spike**

- **Source.** [comemo](https://github.com/typst/comemo) (Typst): memoize pure functions whose arguments are *tracked*
  types; the cache entry records only which methods of the tracked argument were called and their results, so a changed
  argument that doesn't change *those answers* still hits. [memory]
- **How Rune uses it.** Layout of a subtree depends on `(available space, style, children)`. Taffy already caches per
  node with a small fixed set of slots keyed by available-space (known behaviour, [memory]); comemo-style tracking would
  go further for text: shaping depends on `(text, font, size, features)` but wrapping depends only on *width bucket*;
  record "line breaks stayed identical for any width in [w1, w2]" so container resizes during a spring don't re-wrap
  (the plan's two-tier cache does a cruder version of this). Valuable for **layout animation** where width changes
  continuously but the target layout is computed only at target (so it matters less); useful for resizable windows.
- **Cost/risk.** Tracked-call bookkeeping costs more than recomputing small nodes. Only worth it above a node-size or
  text-length threshold.
- **Verdict.** Spike, only for paragraph wrap intervals. Cheap to try because Parley exposes break opportunities.

### 5. Lock-free handoff: SPSC byte ring for commands, triple buffer for "latest state", left-right for read-mostly trees  — verdict: **adopt now**

- **Sources.** [triple_buffer](https://docs.rs/triple_buffer) (non-blocking, wait-free latest-value handoff where the
  producer may overwrite; reader always sees a complete snapshot) [fetched via search page]; [left-right](https://github.com/jonhoo/left-right)
  (Jon Gjengset: two copies of a structure, writers apply an op log to the idle copy, readers never block) [memory];
  [rtrb](https://github.com/mgeier/rtrb)/ringbuf for SPSC rings [memory]; seqlocks for tiny POD [memory].
- **How Rune uses it.** The command channel is semantically two things; use two primitives:
  - *Ordered, lossless*: tree diffs, spring targets, text runs → one SPSC ring of **length-prefixed POD records**
    (bump-encoded, no per-command allocation, trivially serializable for the USB/replay path: the ring's bytes *are*
    the replay log).
  - *Latest-wins*: gesture/scroll offsets, viewport, presented-frame id back to the app → triple buffer / atomics.
  - *Read-mostly shared structure*: hit-test data (presented rects) that the **main thread** queries for input while
    the render thread updates → left-right with a per-frame "swap" at the end of scene build. Alternative: render thread
    answers hit tests via a message (adds a hop); left-right avoids the hop.
- **Numbers.** None I can cite for UI; triple buffering's whole point is that reader/writer collisions cost no waiting
  vs mutexes (crate docs).
- **Risk.** Exact memory-ordering bugs. Mitigation: loom tests for each primitive (agents write these well), or use
  the published crates and skip custom code.
- **Verdict.** Adopt, using published crates first; hand-roll the byte ring only when the allocator counter says so.

### 6. Struct-of-arrays subtree-contiguous tree + SIMD hit-test/cull  — verdict: **adopt layout; spike SIMD**

- **Sources.** [Clay](https://github.com/nicbarker/clay) (flat arrays, arena, two-pass flex layout, "microsecond layout"
  claim from its README; author's walk-through video is the best explainer) [memory/search snippets]; Dear ImGui (ID
  stack + immediate draw lists with a single vertex/index buffer per window) [memory]; Xilem/Masonry's per-pass dirty
  flags (`needs_layout`, `needs_paint`) [memory].
- **How Rune uses it.** Store nodes in DFS pre-order with `subtree_end[i]`. A subtree is a contiguous index range, so:
  dirty propagation = range ops; clipping/culling = skip `i..subtree_end[i]`; "is this subtree animating" = one flag;
  scene build is a linear scan. Rects as four `f32` columns (`x0,y0,x1,y1`) so a hit test or visibility test over 4 or 8
  nodes is one NEON/AVX compare + movemask (`std::simd` is still unstable; `wide` or hand-`core::arch::aarch64` works).
  Keep a `Vec<Node>` (AoS) reference implementation behind the same trait for differential tests (tenet 7).
- **Numbers.** None measured. **Estimate**: hit-testing 10k nodes goes from ~10-20 µs scalar to ~3-5 µs with NEON; at
  120 Hz neither matters unless pointer-move storms run every frame, so SIMD is low priority.
- **Cost/risk.** DFS ordering is awkward on keyed insertion mid-tree (renumbering). Mitigate with generational ids plus a
  per-frame compaction; the tree is rebuilt from views anyway.
- **Verdict.** Adopt DFS-contiguous SoA now; SIMD hit-test later, behind a counter that proves it matters.

### 7. Per-frame bump arena + counting allocator as a test gate  — verdict: **adopt now**

- **Sources.** [bumpalo](https://github.com/fitzgen/bumpalo) [memory]; a counting `GlobalAlloc` (e.g.
  [stats_alloc](https://docs.rs/stats_alloc), or ~30 lines hand-written) [memory]; PLAN tenet 7 already mandates it.
- **How Rune uses it.** Scene build, diff scratch and El construction go in an arena that resets per frame; long-lived
  data (tracks, atlas, node SoA) are pre-sized `Vec`s with capacity reuse. A test asserts `allocations == 0` between
  `begin_frame` and `present` for a fixed catalog of scenes, **at rest and while animating**. Note `El` trees built by
  views each rebuild will allocate unless `El` children are arena slices; plan for `&'frame [El]` not `Vec<El>`, a
  signature decision that is expensive to retrofit.
- **Verdict.** Adopt now and make the arena lifetime part of the `El` type from day one.

### 8. Apple Silicon unified-memory upload: write instance data in place  — verdict: **spike (after wgpu baseline)**

- **Sources.** Apple [Metal Best Practices: Resource Options](https://developer.apple.com/library/archive/documentation/3DDrawing/Conceptual/MTLBestPracticesGuide/ResourceOptions.html)
  (unified memory: CPU and GPU share system memory; `Shared` is the natural mode; triple-buffer dynamic data with a
  semaphore) [search page]; `newBufferWithBytesNoCopy` needs page-aligned, page-multiple-length memory (and gotchas
  on StackOverflow when contents read as zero) [search page].
- **How Rune uses it.** Per-frame instance buffer = a ring of 3 persistently mapped `Shared` buffers; scene build writes
  `UberQuad` instances **straight into the mapped slice** (no intermediate `Vec`, no memcpy). Atlas uploads: write into a
  `Shared` staging region and `blit`/replaceRegion only dirty rects; glyph atlas on Apple can be `Shared` texture
  directly. The shaped-text path can emit instances directly from cached glyph runs.
- **wgpu caveat.** wgpu's `queue.write_buffer` copies through an internal staging belt; to avoid that use
  `mapped_at_creation` or `Buffer::map_async` rings, which on Metal map to the same unified allocation, but verify that
  claim by reading wgpu-hal's Metal buffer code before relying on it [unverified].
- **Cost/risk.** Honest answer: the memcpy of a few hundred KB of instances is ~tens of µs on M-series (**estimate**), so
  this is a polish item not a bottleneck. Prioritize it only if frame traces show `encode` dominated by copies.
- **Verdict.** Spike with raw Metal in the §4.2 later backend; don't contort the wgpu path.

### 9. Damage tracking on a TBDR GPU  — verdict: **later**

- **Source.** Apple GPUs are tile-based; load/store actions control bandwidth [memory]. `CAMetalLayer` presents the whole
  drawable and rotates 2-3 of them, so partial redraw requires "buffer age" bookkeeping (EGL `EXT_buffer_age`-style) to
  re-copy or re-draw regions that were stale in this drawable [memory].
- **Opportunity.** Idle-ish frames (caret blink, a tiny spinner) draw a scissored rect. PLAN §4.3 already lists dirty-rect
  scissoring. Win is **power**, not frame time. A caret blink at 2 Hz is "zero ticks" anyway if you present only on change.
- **Verdict.** Later. First measure whether a full pass at rest-with-small-change costs notable energy (Instruments
  energy log) before building damage-history machinery.

### 10. Rust compile-speed levers, measured  — verdict: **adopt the cheap ones now**

| Lever | Result | Source | Verdict |
|---|---|---|---|
| `[profile.dev.package."*"] debug = false` | clean build −22% / −30% (nushell, 799 units, M-series, Rust 1.99); `target/` 4.3→2.3 GB | [ImportStatic, Oct 2026](https://importstatic.com/rust/rust-debug-build-cargo-profile) [fetched] | **adopt** |
| leave `split-debuginfo` at macOS default (`unpacked`) | `packed` took edit→build from ~2 s to ~18 s | same [fetched] | **adopt** (never set `packed` in dev) |
| `debug = false` everywhere | clean −16/−20% but rebuild-after-edit *slower* (3.1-3.7 s vs 1.6-2.8 s, extra `strip=debuginfo` pass, likely cause unproven) | same | avoid |
| Linker choice on macOS | final link only ~2-4 s of a 112-216 s clean build; ~2 s incremental | same [fetched] | linker is not the bottleneck on Mac |
| mold | 4.9× lld, 1.9× wild at median; M1 Ultra (Linux) debug links 1.5-10× lld; README lists `macho/` in repo and says it's now written in Rust with an ASPLOS 2027 paper | [mold README](https://github.com/rui314/mold), [paper](https://arxiv.org/abs/2608.23228) [fetched] | watch: its Mach-O status unverified; check before assuming |
| wild | Linux-focused ("a very fast linker for Linux") | [wild](https://github.com/wild-linker/wild) [search snippet] | no use on macOS |
| Cranelift backend (`rustc_codegen_cranelift`) | reported 20-30% less clean-build time on some projects [reddit anecdote]; **no unwinding on macOS, several arm64-macOS ABI incompatibilities with LLVM** (June 2025 report) | [bjorn3 report](https://bjorn3.github.io/2025/06/30/progress-report-june-2025.html) [fetched] | **skip on Apple**; maybe on Linux CI |
| Parallel front end (`-Zthreads=N`) | claimed 20-30% on nightly; stabilization is a 2026 goal; known deadlock/ICE/incremental issues being fixed, no robust test suite yet | [Rust goals 2026](https://goals.rust-lang.org/2026/parallel-front-end.html) [fetched] | spike on nightly in a throwaway profile, not for the main loop |
| rustc itself | ~4.6% mean wall-time gain over two months of compiler work | cited in ImportStatic from Nethercote's Sept 2026 roundup [fetched, secondhand] | free over time |

**Rune-specific levers (mostly my recommendations, not measured):**
- **Crate split along the hot-patch boundary:** `rune-core` (arena, tree, tracks, protocol; changes rarely) /
  `rune-render` / app view crates. Subsecond patches the app crate only; keeping the patched crate tiny is the main
  determinant of patch time (estimate: patch time ≈ incremental compile + thin link of that crate).
- **Dev profile:** `opt-level = 1` for the rune crates (springs/layout at `opt-level = 0` are too slow to feel right at
  120 Hz) and `opt-level = 3` for Taffy/Parley/skrifa/wgpu via `[profile.dev.package.taffy]` etc. This is the well-known
  Bevy recipe [memory]; heavy deps compile once and stay cached.
- **Type-erased views and no generics tower** (already tenet 7); generics → `-Zshare-generics` helps incremental
  [memory].
- **`cargo check`-speed inner loop for agents** is likelier to be dominated by rust-analyzer-like analysis than linking;
  run `cargo check` with `-Zthreads` only on a spike machine.
- The numbers above come from a 12-core Apple-silicon Mac, so
  larger machines should do better. **Do a one-hour measurement of the real Rune workspace on a build Mac** before
  committing to any of this beyond the two profile lines.

### 11. Hot patching: shrink what's patched  — verdict: **spike already in plan; tighten the design**

- **Source.** [subsecond](https://docs.rs/subsecond): detours calls through a jump table; patches are separate objects
  built by intercepting the link step and driving `rustc` manually; "works across macOS, Windows, Linux, iOS, Android"
  per the author ([HN](https://news.ycombinator.com/item?id=44369642), [search snippet]); requires an ASLR reference
  address from the running process.
- **How Rune uses it.** The plan's state design (typed models outside views, keyed by stable ids) is what makes patching
  safe, since struct layout changes break old state. Add: (1) models are the only state that crosses a patch, and a
  **layout hash of each model type** is stored with it; a mismatch triggers a reset of that model only, with an
  inspector message, rather than UB; (2) keep the render thread's code out of the patch set (it's in `rune-render`
  and not hot), so animation continues during a patch; (3) patch granularity = view functions, handlers.
- **Cost/risk.** Layout-hash gating is the difference between "usually works" and "agents trust it". Known from subsecond
  docs: changing struct layouts requires a rebuild.
- **Verdict.** Already planned; add the layout-hash guard and render-thread isolation as requirements.

### 12. PGO from replayed command streams; BOLT later  — verdict: **later (post-M3)**

- **Sources.** [cargo-pgo](https://github.com/Kobzol/cargo-pgo) and [Kobzol's write-up](https://kobzol.github.io/rust/cargo/2023/07/28/rust-cargo-pgo.html)
  (PGO + BOLT; the tool's README claims up to ~15% speedups [search snippet]; Rust's own compiler ships PGO/BOLT-optimised
  builds). Mach-O BOLT support was not mature when I last looked [memory, unverified]; PGO works with LLVM on macOS.
- **The agent-specific trick.** Rune records the command channel for deterministic replay (PLAN §6). A set of replayed
  sessions (scroll a long list, spring storms, typing, window resize) is a **reproducible, repo-checked PGO training
  corpus**, which most apps lack. CI can produce a profile and rebuild with `-Cprofile-use` automatically, and perf
  counters (instructions) gate regressions precisely.
- **Cost/risk.** Profile staleness and build time (two-stage builds); PGO varies in effect on already-branch-light
  SoA code. Estimate 3-10% on scene-build/layout paths; I would not expect it to change a 120 Hz verdict.
- **Verdict.** Later. Do it for shipped release builds once the scene catalog stabilises.

### 13. Text: cache before you SIMD  — verdict: **skip SIMD; adopt cache design**

- **Sources.** [simdutf](https://github.com/simdutf/simdutf) (GB/s UTF-8↔UTF-16 validation/transcoding; used by Node,
  Bun, ghostty) [search page]; Ash Vardanian's [UTF-8 SIMD search](https://ashvardanian.com/posts/search-utf8/) [search
  page]. Both target bulk text.
- **Assessment.** UI strings are short; the cost is shaping and rasterizing, not validation or transcoding. The one place
  bulk text matters is the editor (rope + sum tree, PLAN §4.6), where Zed's tree already gives O(log n) offsets. An ASCII
  fast-path (all bytes < 0x80 → skip bidi/script itemization, use cached advance table) is worth more than SIMD and is
  ~20 lines.
- **Do instead.** Glyph-run cache keyed by `(font id, size, features, text hash)`; wrap-interval caching (idea 4);
  shape off the main thread for paragraphs above N glyphs (already in plan).
- **Verdict.** Skip SIMD text; adopt ASCII fast path + caches.

---

## Steal

- Slate's four dirty classes and the **Volatile** escape hatch; avoid time-phased retainers.
- Chromium property trees: instances reference tree nodes, so animation touches nodes not geometry.
- Salsa/Incremental **early cutoff**; React Compiler's memoize-by-input-versions idea; comemo's recorded-constraint hits.
- Clay's flat arena + contiguous subtrees; ImGui's single shared vertex/index buffer per frame.
- Triple buffer for latest-wins, byte-encoded SPSC ring for ordered commands (the ring is also the replay log).
- Counting allocator as a unit test; "frame arena in the `El` lifetime".
- `debug = false` for dependencies; `unpacked` split-debuginfo on macOS.

## Avoid

- A general reactive dependency graph (Salsa/Adapton/AttributeGraph): conflicts with hot-patch state survival; use
  version stamps.
- Retainer/offscreen caching by timer ("render on phase"): the one measured foot-gun (50.9 ms, worse than uncached).
- Cranelift on Apple until macOS unwinding and arm64 ABI land.
- `split-debuginfo = "packed"` in dev profiles (+16 s per edit).
- SIMD for text validation/transcoding; SIMD hit-testing before a counter justifies it.
- Hand-written lock-free structures when published crates work; BOLT on Mach-O.

## Recommendation for Rune (ranked)

1. **Property-tree scene + GPU-evaluated spring tracks** (idea 1): the one idea that can make "animation costs the CPU
   nothing" literally true; spike early, with a CPU reference evaluator.
2. **Dirty classes + early cutoff + exact counters** (ideas 2, 3): gates "zero work at rest" with numbers.
3. **Frame arena in `El`'s type + counting allocator** (idea 7): a signature decision that is expensive to change later;
   decide before the view API ships.
4. **Command channel = byte SPSC ring + triple buffer, loom-tested** (idea 5), doubling as the replay log.
5. **Two-line Cargo profile + measured compile baseline** (idea 10), then hot-patch hardening (idea 11).

**Rejected alternatives:** Salsa/SwiftUI-style graph (state vs patches); Slate retainers by phase (measured regression);
Cranelift on Apple (unwinding, ABI); SIMD text (wrong bottleneck); damage-history tracking now (power only, unmeasured).

## Open questions

- Does wgpu's Metal backend expose a path to write instance data in place without a staging copy? (read wgpu-hal.)
- Can edge-animated rounded rects and their text be expressed as property-tree nodes, or does idea 1 need per-instance
  edge tracks only? (Prototype in the spring scene catalog.)
- Is mold's Mach-O linker production-ready on macOS 27 (the README lists `macho/`)? Only matters if link time shows up
  in the real Rune workspace, where the nushell data says it won't.
- Real Rune compile numbers: baseline `cargo check`, dev build, and patch time with the crate split.
- Is a layout-hash guard on models enough for agent-trusted hot patching, or do we need a full state-migration hook?
- Measure energy (not frame time) of full-pass-at-rest-with-small-change to decide whether idea 9 is worth building.

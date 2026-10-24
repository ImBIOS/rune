# 04 — Language, GPU API, 2D Renderer Design & Input-to-Photon Latency

## 1. Executive Summary

Rune’s foundational bet is **Rust + a thin custom GPU HAL (~1,800 LOC per native backend) over raw Metal (macOS/iOS) and `wgpu` (Linux/Windows/WebGPU), paired with a hybrid Core Animation compositor host (`NSGlassEffectView`/`UIVisualEffectView` + `CAMetalLayer`) and an uber-primitive instanced SDF + Sparse-Strips/Slug renderer**.

1. **Language — Rust (with thin Swift/ObjC2 platform shims and Dioxus `subsecond` hot-patching):** Rust remains the only systems language in 2026 that simultaneously gives zero-GC deterministic frame times, production-grade text/layout/accessibility crates (`taffy`, `parley`, `harfrust`, `fontations`, `swash`, `accesskit`), fearless multi-threaded background work, and top-tier LLM/agent synthesis reliability (compiler diagnostics + strict type/borrow checking catch hallucinations before runtime). Its historical weakness—1–3 s incremental UI iteration—is solved in 2025–2026 by **ThinLink + jump-table binary hot-patching (`subsecond`)** (<200–500 ms stateful UI patches) plus `-Zthreads=8`, `-Zshare-generics=y`, and `mold`/`wild`/Apple `ld_prime`.
2. **GPU API — Custom Thin HAL (`RuneGpu`) with First-Class Raw Metal on Apple + `wgpu` 28+ Elsewhere:** Neither pure `wgpu` everywhere nor four hand-written native backends is optimal. On macOS/iOS, raw Metal (`objc2-metal`) combined with a shallow `CALayer` host tree is mandatory for zero-copy `IOSurface` video/camera surfaces, `NSGlassEffectView` liquid glass backdrops, `CATransaction`-synchronized window resizing (`presentsWithTransaction`), programmable tile/framebuffer fetch, and EDR headroom. On Linux, Windows, and Web, Zed’s February 2026 migration from `blade-graphics` to `wgpu` ([zed#46758](https://github.com/zed-industries/zed/pull/46758), cutting median CPU draw time from 301 µs to 236 µs) proves `wgpu` is now fast enough and eliminates thousands of lines of Vulkan/D3D12 driver-quirk maintenance while giving WebGPU for free.
3. **2D Renderer — Single-Pass Uber-SDF Primitives + Public-Domain `Slug` (Large/Transformed Text & Icons) + `vello_gpu` Sparse Strips (Complex Vector Art):**
   - **95% of UI** (rounded boxes, per-corner radii, elliptical inner borders, analytic 4-sample `erf` Gaussian drop/inner shadows, bevel/specular surface materials, Oklab gradients with Jimenez IGN triangular dithering, and **analytic 4-edge soft-clip masks**) renders in a **single instanced uber-shader pass** with zero geometry tessellation and zero intermediate passes.
   - **Text** uses a dual-path pipeline: crisp hinted grayscale/subpixel raster atlas (`R8Unorm`/`RGBA8Unorm`) for body/code text (`≤ 32 px` axis-aligned), switching seamlessly to **Eric Lengyel’s Slug algorithm** (dedicated to the public domain in March 2026, [EricLengyel/Slug](https://github.com/EricLengyel/Slug), and already proven in Makepad’s `draw_text.rs`) for zoomable canvas text, large headings, rotated/sheared labels, and monochrome vector icons.
   - **Arbitrary vector paths** (charts, complex SVGs, strokes) use Linebender’s 2026 **`vello_gpu` Sparse Strips** pipeline (4×4 pixel blocks after SIMD CPU flattening) rather than heavyweight full-GPU compute `vello` or CPU triangle tessellation (`lyon`).
4. **Latency — Event-Driven Unthrottled Input Fast-Path + `CADisplayLink` Animation Pacing (< 6 ms p50 on 120 Hz ProMotion):** Waiting for a display link callback before processing input or acquiring a drawable adds 16–49 ms of pipeline lead (measured at 41.5 ms / 5 refreshes in windowed `CAMetalDisplayLink` on 120 Hz macOS). Rune decouples discrete input from `CADisplayLink`: keystrokes, clicks, and trackpad scrolls immediately wake the UI thread, update state, encode a <0.4 ms Metal pass into a `maximumDrawableCount = 2` ring, and present on the very next scanout (~4–8 ms input-to-photon on 120 Hz ProMotion), engaging `CADisplayLink` (`preferredFrameRateRange`) only while springs or timeline animations are in flight.

---

## 2. Detailed Findings by Topic

### Topic 1: Programming Language Evaluation (2026 Landscape)

#### Comparative Matrix

| Dimension | Rust (2024 Ed. / 1.86+) | Swift 6.3 / 6.4 | Zig 0.15 / 0.16-dev | C++23 / C++26 | Odin / Jai / Mojo |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **Runtime Perf & Tail Latency** | **A+** (No GC, no pervasive ARC atomic traffic, SIMD via `wide`/`std::simd`) | **B+** (ARC retain/release traffic & existential boxing unless carefully annotated) | **A+** (Manual allocators, `@Vector` SIMD, zero hidden control flow) | **A+** (Zero-cost abstractions, mature intrinsics) | **A** (Odin/Jai manual memory; Mojo focused on AI kernels) |
| **Platform Reach (macOS, iOS, Win, Linux, Android, Web)** | **A+** (Tier-1 native everywhere + `wasm32-unknown-unknown` <1 MB) | **B** (macOS/iOS A+; Android SDK added in 6.3; Wasm 6.2+ still bulky; Linux/Win GUI immature) | **A** (Cross-compiles everywhere; mobile/Web bindings require manual C glue) | **A** (Everywhere via CMake/Emscripten, painful packaging) | **C** (Odin/Jai weak on iOS/Android/Web; Mojo has no GUI targets) |
| **UI Ecosystem (Text, Layout, A11y)** | **A+** (`taffy`, `parley`, `harfrust`, `fontations`, `swash`, `accesskit`, `kurbo`) | **C** (Apple-only `CoreText`/`NSAccessibility`; zero cross-platform equivalents) | **C** (Wraps C libraries: `harfbuzz`, `freetype`, `stb_truetype`; no `accesskit`) | **A-** (`HarfBuzz`, `Skia`, `DirectWrite`, `Yoga`, `ICU` — heavy C++ deps) | **D** (Must bind C libraries from scratch) |
| **Incremental Compile Speed** | **B** (0.8–2.5 s cold-ish crate rebuild; **<0.4 s** with `subsecond` ThinLink) | **B-** (Swift Build 6.4 fast for small modules, slow on heavy generics/macros) | **A+** on Linux x86_64 (<150 ms via `Elf2`); **C+** on macOS arm64 (Mach-O incremental WIP) | **C** (Header/module bloat; 2–10 s incremental link) | **A+** (Odin ~0.3 s; Jai <0.2 s full builds) |
| **Hot-Reload Friendliness** | **A-** (`subsecond` jump-table binary patching + dylib state preservation) | **A-** (`@_dynamicReplacement` + Mach-O `-bundle_loader`, ~100–300 ms on macOS) | **B+** (Clean C ABI `dlopen` state reload) | **B** (Live++ on Windows; fragile `dlopen` on macOS) | **A** (Odin/Jai trivial DLL reload) |
| **Agent-Writability (LLM Synthesis)** | **A+** (Exhaustive `rustc`/Clippy errors guide autonomous self-repair; huge corpus) | **B+** (Swift 6 strict concurrency region errors frequently trip LLMs) | **C+** (0.14→0.15→0.16 stdlib/build.zig churn causes stale LLM syntax) | **B** (Template SFINAE errors and UB escape compiler checks) | **D** (Small training corpus; Jai closed beta) |
| **Memory & Thread Safety** | **A+** (`Send`/`Sync` compile-time race freedom; `unsafe` quarantined to FFI) | **A** (Swift 6 data-race safety + ARC, but Reference Cycles leak) | **C** (Spatial checks in Debug, manual lifetime/thread discipline) | **D** (Use-after-free, iterator invalidation, data races) | **C** (Manual memory management) |

#### Deep Dive on the Contenders

1. **Rust’s Compile-Time & Hot-Reload Reality in 2026:**
   - **Compiler & Linker Gains:** The [2025 Rust Compiler Performance Survey](https://blog.rust-lang.org/2025/09/10/rust-compiler-performance-survey-2025-results) identified single-crate incremental rebuilds and `cargo check`/`cargo build` cache splitting as top pain points. In 2026, enabling the parallel frontend (`-Zthreads=8`), `-Zshare-generics=y`, `-Cdebuginfo=line-tables-only`, and fast linkers (`ld_prime` on macOS, `mold` or incremental [`wild`](https://github.com/davidlattimore/wild) on Linux) cuts workspace incremental rebuilds by 30–45%.
   - **Cranelift (`rustc_codegen_cranelift`):** Per the [February 2026 Rust Project Goal](https://goals.rust-lang.org/2026/improve-cg_clif-performance.html), on a 16-core machine building Zed incrementally, Cranelift is currently **12% faster than LLVM** with `-Cdebug-info=line-tables-only` (and 10–15% faster across the `rustc` benchmark suite), with an active 2026 roadmap for an in-memory machine-code caching daemon targeting a 2× backend speedup.
   - **Subsecond (`subsecond` 0.7) Binary Hot-Patching:** Built by Dioxus ([`docs.rs/subsecond/0.7.10`](https://docs.rs/subsecond/0.7.10/subsecond/)), `subsecond` introduces **ThinLinking** (automatic dynamic linking of workspace dependencies + object-file diffing + runtime jump tables). When a developer or agent edits a UI view function, only the modified functions are recompiled into a tiny patch object and redirected via jump tables without restarting the process or losing app state, shrinking Rust incremental UI iteration to **< 500 ms** (typically 150–350 ms for leaf view edits).
   - **Architecture Discipline for Compile Times:** GPUI’s slow compile times in Zed come primarily from monomorphizing massive view trees (`Div::child(impl IntoElement)`) and putting 100k+ LOC inside single monolithic crates. Keeping Rune’s element tree type-erased at container boundaries (`SmallVec<[AnyElement; 4]>`) and splitting widget crates cleanly keeps standard incremental `cargo build` under 1.2 s even without hot-patching.

2. **Zig (0.15 / 0.16-dev in 2026):**
   - Ghostty proved Zig + Metal + AppKit/SwiftUI (`libghostty`) can ship a wonderful macOS/Linux terminal.
   - **Why Zig loses for Rune:** First, Zig’s flagship incremental compilation (`-fincremental` + in-place binary patching) works on `x86_64-linux` (`Elf2`), but on `aarch64-macos` (our primary target) the self-hosted `aarch64` backend and Mach-O incremental in-place linker remain incomplete in 2026 ([ziglang/zig#21165](https://github.com/ziglang/zig/issues/21165)). Second, Zig would force Rune to abandon the entire pure-Rust Linebender/AccessKit stack (`taffy`, `parley`, `harfrust`, `fontations`, `accesskit`) and revert to C FFI over `HarfBuzz`/`FreeType` plus writing a cross-platform accessibility tree from scratch. Third, LLMs routinely hallucinate Zig `build.zig` and `std.Io` APIs due to breaking changes between 0.13, 0.14, 0.15, and 0.16.

3. **Swift (Swift 6.3 / 6.4 in 2026):**
   - Swift has made real cross-platform strides: [Swift 6.3](https://www.swift.org/blog/swift-6.3-released/) shipped an official Android SDK, [Swift 6.4](https://www.swift.org/blog/) defaulted to the unified `Swift Build` engine, and `JavaScriptKit` optimized Wasm bridges. Kit’s own `hot-reload-kit` demonstrates 100–300 ms hot-reloading on macOS via `@_dynamicReplacement` and `-bundle_loader`.
   - **Why Swift loses as core language (but wins as Apple host glue):** Outside Apple platforms, Swift has no cross-platform GUI/text/accessibility ecosystem; Wasm binaries carry heavy runtime weight unless restricted to Embedded Swift; and pervasive ARC reference counting across dense retained/immediate UI trees introduces atomic retain/release traffic and accidental closure reference cycles. However, using ~500 lines of Swift/Objective-C via `objc2` for macOS/iOS system integration (e.g., `NSGlassEffectView`, menu bars, `RPSystemBroadcastPickerView`, share sheets, as done in `launcher/native/LauncherView.swift`) is effortless and strongly recommended.

4. **C++23/26, Odin, Jai, Mojo:**
   - **C++** lacks memory safety, suffers from header/build fragmentation, and makes fearless multi-threaded UI state scheduling hazardous.
   - **Odin** and **Jai** compile fast and excel at standalone games, but have near-zero accessibility, international text shaping, or mobile/web platform infrastructure, and tiny LLM training corpora.
   - **Mojo** is specialized for heterogeneous AI/tensor compute and has no desktop/mobile windowing or UI ecosystem.

---

### Topic 2: GPU API & Compositor Integration

#### The Contenders Analyzed

1. **Raw Metal (`objc2-metal`) on macOS & iOS:**
   - **Evidence from GPUI, Scrawl, and Makepad:**
     - GPUI’s original macOS renderer (`kitlangton/gpui/src/platform/mac/metal_renderer.rs:1-850`) is **850 lines of Rust**.
     - Kit’s production recording pill renderer in Scrawl (`scrawl/src/native/pill/mod.rs:1-420` + `pill.metal:1-369`) is **~790 lines total**, directly driving a 3-pass separable Gaussian blur + SDF capsule pipeline on a `CAMetalLayer` over an `NSGlassEffectView`.
     - Makepad’s direct Metal backend (`makepad/platform/src/os/apple/metal.rs:1-1100`) gives direct `IOSurface` cross-process texture sharing and exact `MTLCommandBuffer` GPU start/end timestamps (`GPUStartTime`/`GPUEndTime`, lines 318–326).
   - **Capabilities `wgpu` hides or complicates on Apple:**
     - **Programmable Framebuffer Fetch / Tile Memory (`[[color(0)]]`, `[[imageblock_data]]`, `[[raster_order_group(0)]]`)**: Apple Silicon is a Tile-Based Deferred Renderer (TBDR). Rive’s Metal shaders (`rive-runtime/renderer/src/shaders/metal.glsl:301-450`) read and write 4 pixel-local planes (`colorBuffer`, `clipBuffer`, `scratchColorBuffer`, `coverageCountBuffer`) directly in on-chip tile SRAM (`[[color(IDX)]]`) at zero system memory bandwidth cost. `wgpu` does not expose multi-plane tile memory / programmable blending on Metal.
     - **Zero-Copy Video & Camera (`CVMetalTextureCache` / `IOSurface`)**: GPUI’s `surface_fragment` (`shaders.metal:874-890`) samples bi-planar `Y` (`R8Unorm`) and `CbCr` (`RG8Unorm`) `IOSurface` planes directly from `CVPixelBuffer` hardware decoders without CPU/GPU copies.
     - **Live Resize Synchronization (`presentsWithTransaction`)**: Both GPUI (`zed/crates/gpui_macos/src/window.rs:3202,3328`) and Scrawl (`scrawl/src/native/pill/mod.rs:403-417`) toggle `layer.setPresentsWithTransaction(true)` inside `CATransaction` during window resize/frame changes so Metal content never lags behind the window border.
     - **Precompiled `.metallib` Instant Startup**: `xcrun metal` compiles `.metal` at `build.rs` time into a binary `.metallib`. `newLibraryWithData` loads in **< 0.5 ms** at startup, versus 15–60 ms for runtime WGSL→Naga→MSL string translation + `newLibraryWithSource`.

2. **`wgpu` (v28+ in 2026) vs `blade-graphics`:**
   - Historically, `wgpu` had heavy state-tracking and validation overhead, prompting Zed to adopt Kvark’s lighter `blade-graphics` for Linux/Windows.
   - **Crucial 2026 Update:** In February 2026, Zed merged [PR #46758 ("Migrate from blade-graphics to wgpu")](https://github.com/zed-industries/zed/pull/46758) (`zed/crates/gpui_wgpu/src/wgpu_renderer.rs`). Benchmarks in that PR showed **`wgpu` reduced median CPU draw time from 301.29 µs to 235.67 µs (-21.8%)** compared to `blade-graphics`, while adding `wgpu::Features::DUAL_SOURCE_BLENDING` for subpixel text AA (`wgpu_renderer.rs:781-795`), pipeline disk caching (`PipelineCache`), Vulkan/Wayland stability fixes, and a WebGPU/WebGL2 target.
   - **Tradeoffs:** `wgpu` + `naga` adds ~2.5–3.8 MB to stripped binary size and translates WGSL to backend shaders. That is an unacceptable tax on Apple-specific capabilities if used as the *only* backend on macOS/iOS, but an **overwhelming win** for Linux (Vulkan), Windows (D3D12/Vulkan), Android, and Web (WebGPU).

3. **SDL3 `SDL_gpu`, Google `Dawn`, and Flutter `Impeller`:**
   - **`SDL_gpu` (shipped in SDL 3.2+):** Clean C API, ~400 KB binary footprint, good for games, but lacks `wgpu`’s Rust safety, storage buffer ergonomics, dual-source blending portable setup, and WebGPU browser integration.
   - **Dawn:** Powers Chromium’s WebGPU; C++ build (CMake/GN + `tint` + `spirv-tools`) adds 8–15 MB to binaries and heavy C++ build times.
   - **Flutter Impeller:** Proves the value of **100% AOT-compiled shaders** (`impellerc`) to eliminate runtime PSO compilation hitching—a lesson Rune must steal via build-time `.metallib` / SPIR-V / DXIL / WGSL validation.

4. **Hybrid Core Animation (`CALayer` Tree + Custom Metal) vs Pure Single-Drawable:**
   - **How macOS/iOS actually works:** The WindowServer compositor is already a hardware-accelerated `CALayer` tree evaluator with direct display-controller scanout planes.
   - **What `CALayer` does better than custom shaders:**
     - **System Materials & Liquid Glass (`NSGlassEffectView` / `NSVisualEffectView`):** Private WindowServer backdrop filters blur and refract windows *behind* the app at zero app-GPU cost. Both Zed (`gpui_macos/src/window.rs:301,3810`) and Scrawl (`scrawl/src/native/pill/mod.rs:348-380`) place an `NSVisualEffectView` / `NSGlassEffectView` behind a transparent `CAMetalLayer`.
     - **Out-of-process HDR Video & WebViews (`AVSampleBufferDisplayLayer`, `WKWebView`):** Composited directly by WindowServer hardware planes without waking the UI render pass.
   - **What `CALayer` does poorly (Avoid 1-Layer-Per-Widget):** Allocating hundreds of `CALayer`s for normal buttons/rows causes Core Animation IPC serialization overhead, layout-to-GPU desynchronization, and breaks cross-platform behavior.
   - **The Sweet Spot ("Shallow Compositor Host"):** 1 root `NSGlassEffectView`/`NSVisualEffectView` + 1 primary `CAMetalLayer` for all 2D UI + optional sibling native `CALayer`s only for hardware video/camera/WebView cutouts.

---

### Topic 3: 2D UI Renderer Design

#### 3.1 Instanced SDF Primitives & Analytic Surface Shading
- **GPUI’s Baseline (`kitlangton/gpui/src/platform/mac/shaders.metal:102-545`):**
  - Draws `Shadow`, `Quad`, and `Underline` as instanced 4-vertex unit quads reading per-instance structs from storage/constant buffers (`[[buffer(QuadInputIndex_Quads)]]`).
  - `quad_fragment` evaluates an exact outer rounded-box SDF (`quad_sdf_impl`, line 1065) and an inner border SDF (`quarter_ellipse_sdf`, line 440) that handles non-uniform border widths (`top != left`), plus perimeter-parameterized dashed borders (`dash_alpha`, line 421).
  - **Limitation in GPUI:** Because `Shadow`, `Quad`, and `Border` are separate primitive batches in `scene.rs` (`zed/crates/gpui/src/scene.rs:88-115`), a button with a drop shadow, background fill, inner highlight, and border emits multiple draw instances or requires batch-breaking z-order sorts.
- **Makepad’s Analytic `Material` Breakthrough (`makepad/draw/src/shader/surface.rs:27-165`):**
  - Evaluates **bevel profile & normal (`m_normal`)**, **wrapped Lambert diffuse + Blinn-Phong specular (`m_lit`)**, **edge ambient occlusion (`m_ao`)**, **rim lighting (`m_rim`)**, **sky-box gloss (`m_gloss`)**, and **4-sample `erf` Gaussian inner/outer shadow (`box_cov`)** purely as a function of the signed distance `d` inside a *single* fragment pass!
- **Analytic Drop & Inner Shadows:**
  - Both GPUI (`shaders.metal:525-542,1087-1104`) and Makepad (`sdf.rs:10-76`) use Evan Wallace’s separable formulation: exact closed-form 1D Gaussian integral along X via a polynomial `erf()` approximation (`blur_along_x`), integrated across Y with a 4-step midpoint quadrature clamped to `[-3σ, +3σ]`.
  - **Crucial detail to steal from GPUI (`shaders.metal:541,1170-1177`):** 8-bit `BGRA8Unorm` framebuffers visibly band in the `[0.0, 0.02]` alpha tail of soft shadows and Oklab gradients. GPUI adds **Jimenez Interleaved Gradient Noise (IGN) with a 2-tap triangular PDF (`±1/255` amplitude)** modulated by `smoothstep(0.0, 0.02, alpha) * smoothstep(1.0, 0.98, alpha)`, completely eliminating shadow and gradient mach banding at 3 ALU instructions per pixel.
- **Rune’s Uber-Primitive Upgrade:**
  - Combine Rounded/Squircle (G2 continuous $L^p$ superellipse option) Box + Border + Inner Shadow/Bevel/Rim + Outer Shadow into a single 128-byte `UberQuad` instance struct so an entire styled card renders in **1 instance** with **1 SDF evaluation**.

#### 3.2 Vector Paths & Curves: The 2026 Showdown

| Technique | Representative Engine | How It Works | CPU Cost | GPU Cost & Memory | Verdict for Rune |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **CPU Triangle Tessellation** | `lyon`, Iced fallback, GPUI legacy | Ear-clipping / sweep-line on CPU → indexed triangles | **High** (0.5–4 ms for complex paths; poor for animated morphs) | Low fragment, high vertex count, requires 4× MSAA | **Avoid** (CPU bottleneck on dynamic vector UI) |
| **Loop-Blinn + 4× MSAA Intermediate** | GPUI (`shaders.metal:737-838`) | CPU triangulates hull; fragment shader solves $u^2 - v$ into full-viewport 4× MSAA texture, then blits | Medium (CPU earcut hull) | **High** (allocates full-window 4× MSAA texture + resolve + blit pass) | **Avoid** (Massive memory & bandwidth waste for small icons) |
| **Full Compute Pipeline** | `vello` (classic compute) | 9+ compute stages (pathtag, bbox, flatten, bin, tile, backdrop, coarse, fine) | Very Low | **High baseline** (~0.4–0.8 ms dispatch overhead even for 1 path; complex shaders) | **Avoid** (Linebender moved it to `research/` in 2026) |
| **Sparse Strips (Hybrid CPU+GPU)** | `vello_gpu` / `vello_hybrid` (`vello/ARCHITECTURE.md`) | SIMD CPU flattening + tiling into 4×4 pixel alpha strips; simple instanced GPU raster pass | Low (~50–200 µs via NEON/AVX) | **Very Low** (Simple vertex/fragment shader, zero compute dispatch overhead) | **Steal for general vector art / charts / complex SVGs** |
| **Direct GPU Quadratic Bézier Winding** | **Slug** ([EricLengyel/Slug](https://github.com/EricLengyel/Slug), `makepad/.../draw_text.rs:42-350`) | Curves stored once in `curve_texture` + horizontal/vertical `band_texture`; fragment shader raycasts exact roots | **Zero per frame** (static em-space upload once per glyph/icon) | **Low** (Exact analytical AA at any scale/rotation/perspective; zero atlas invalidation) | **Steal for large/transformed text & vector icons!** |
| **PLS / Atomic Analytical Coverage** | Rive (`draw_raster_order_path.frag`) | Cubic tessellation in vertex shader + exact winding accumulation in tile memory (`[[color(n)]]`) | Low | Ultra-fast for 10,000-path animations; requires tile-memory/ROV shader variants | Optional plugin for embedded `.riv` files |

- **Why the March 2026 Slug Public-Domain Dedication Changes Everything:**
  - On March 17, 2026, Eric Lengyel dedicated US Patent `#10,373,352` (Slug GPU Bézier rendering) to the public domain ([`github.com/EricLengyel/Slug`](https://github.com/EricLengyel/Slug)).
  - Makepad immediately integrated Slug in `makepad/draw/src/text/slug_atlas.rs:1-200` and `draw_text.rs:41-350`.
  - In Slug, a glyph or SVG icon’s quadratic Bézier curves (cubics split once via `CUBIC_TO_QUAD_TOLERANCE = 0.05`) are uploaded **once** in em-space into an append-only `RGBA32F`/`RGBA16F` `curve_texture` and indexed into horizontal and vertical spatial bands (`band_texture`). The vertex shader dilates the bounding quad by 0.5 px in screen space (`slug_dilate`), and the fragment shader scans only the curves intersecting the pixel’s horizontal/vertical band (`scan_horizontal_list`, `scan_vertical_list`), using Lengyel’s bit-shift root eligibility code `(u32(11892) >> shift) & u32(257)` (`draw_text.rs:237`) to avoid numerical edge dropouts.
  - **Result:** Infinite zoom, rotation, 3D perspective, and scale animations on text and UI icons with **zero CPU re-rasterization and zero glyph-atlas thrashing**.

#### 3.3 Text Rendering: Hybrid Raster Atlas + Slug
- **Why not Slug for *all* text?** At body sizes (`11–18 px` on 2× Retina = `22–36` device px), an `R8Unorm` texture sample costs 1 texture fetch (~2 GPU cycles) and preserves platform-specific stem darkening / optical hinting (`CoreText` on macOS, `DirectWrite` on Windows, `fontations`+`swash` autohinting on Linux). Slug requires 4–10 texel fetches + quadratic root math per pixel.
- **Subpixel vs Grayscale AA on Retina (2026):**
  - On macOS, Apple removed subpixel LCD layout from CoreGraphics in Mojave, and all modern Macs/iPhones ship ≥2× Retina panels (many OLED or mini-LED with non-RGB-stripe subpixel layouts on iPad Pro M4 / iPhone). However, CoreText still applies **font smoothing / stem darkening** (`CGContextSetAllowsFontSmoothing`).
  - As proven in `scrawl/src/native/pill/atlas.rs:133-201`, rasterizing glyphs with CoreText into an `R8Unorm` shelf-packed atlas (with 2×2 subpixel bin positioning for fractional X offsets) uses 4× less VRAM than `RGBA8` and blends in a single pass.
  - For Windows/Linux 1× external monitors, `wgpu::Features::DUAL_SOURCE_BLENDING` (as implemented in `zed/crates/gpui_wgpu/src/wgpu_renderer.rs:781-795`) enables proper per-channel RGB subpixel alpha blending in a single draw call.
- **Rune’s Hybrid Text Rule:**
  - **Axis-aligned text `≤ 32 pt` with static scale:** Shelf-packed `R8Unorm` (macOS/iOS Retina) or `RGBA8` dual-source subpixel (1× Windows/Linux) glyph atlas, keyed by `(FontId, GlyphId, FontSizeFixed, SubpixelBinX)`.
  - **Text `> 32 pt`, any rotated/skewed/3D-transformed text, or text undergoing continuous zoom/scale animation:** Rendered directly via **Slug** (`SlugAtlas`), eliminating the infamous "blurry scaled atlas" or "100 MB atlas eviction storm during pinch-to-zoom".

#### 3.4 Blur, Backdrop Filters & Peak-Alpha Legibility
- **Window-Level Backdrop Blur (Zero App GPU Cost):** Delegated to `NSGlassEffectView` (macOS 26+) / `NSVisualEffectView` (macOS 14–15) / `UIVisualEffectView` (iOS) / `DwmEnableBlurBehindWindow` / `Mica` (Windows).
- **In-App Element Backdrop Blur (`backdrop-filter: blur()`):**
  - **Dual Kawase Pyramid** for large radii (`σ > 6 px`): Downsample 2× per pass (4-tap bilinear offset `(±1, ±1)`) for $N$ levels (typically 2–3 levels down to 1/4 or 1/8 resolution), then upsample 2× per pass (8-tap tent filter) restricted strictly to the dirty/clipped bounding rect of the blurring element. At 1/4 resolution, blurring a 400×300 popover takes **< 60 µs** on Apple M-series GPUs.
  - **Separable 1D Gaussian + Peak-Alpha Boost** for animated morphs & small blurs (`σ ≤ 6 px`): Steal directly from Kit’s `scrawl/src/native/pill/pill.metal:208-360`. When blurring UI elements (like morphing pill labels or crossfading icons), standard Gaussian blur reduces peak alpha and makes text look washed out. Scrawl’s `composite_fragment` (`pill.metal:326-349`) computes `boost = 1.0 + smoothstep(0.0, 0.35, b) * 2.5` and `pow(a, 1.0 / boost)`, keeping blurred text crisp and luminous throughout the transition.

#### 3.5 Cheap Soft-Edge Gradient Masks at Every Clip Edge
- **The Problem:** Traditional UI engines clip scroll views and containers using either hardware `glScissor`/`setScissorRect` (hard pixel edge, axis-aligned only), hardware `[[clip_distance]]` planes (GPUI `shaders.metal:56,80`, hard edge), or an offscreen mask texture (expensive render pass switch). When content scrolls under a header or a pill morphs width, hard clips look abrupt; developers want a 6–16 px soft alpha fade at the top/bottom/left/right clip edge without allocating an offscreen layer.
- **The Zero-Pass Solution (Analytic Soft-Clip Mask + Room Gating):**
  1. **Per-Fragment Analytic Rounded-Clip & 4-Edge Feather:** Every draw instance carries a `clip_index: u32` indexing into a compact `ClipStackBuffer` on the GPU. Each `ClipNode` (32 bytes) stores:
     - `bounds: float4` (`origin.xy, size.zw`)
     - `corner_radii: float4` (`tl, tr, br, bl`)
     - `edge_feather: half4` (`top, right, bottom, left` feather distances in scaled pixels; `0.0` = crisp 1px antialiased SDF clip; `> 0.0` = smooth gradient mask fade over `edge_feather` px!).
     In the fragment shader, evaluating the clip costs ~8 ALU instructions:
     ```metal
     // Signed distance to the rounded clip rect (negative inside, positive outside)
     float clip_sdf = quad_sdf(frag_pos, clip.bounds, clip.corner_radii);
     float hard_aa = saturate(0.5 - clip_sdf);
     // Directional 4-edge soft gradient fade inside the clip rect
     float4 dist_to_edges = float4(
         frag_pos.y - clip.bounds.origin.y,                          // top
         (clip.bounds.origin.x + clip.bounds.size.x) - frag_pos.x,   // right
         (clip.bounds.origin.y + clip.bounds.size.y) - frag_pos.y,   // bottom
         frag_pos.x - clip.bounds.origin.x                           // left
     );
     float4 edge_alpha = saturate(dist_to_edges / max(float4(clip.edge_feather), float4(1e-4)));
     // Hermite smoothstep across each feathered edge
     edge_alpha = edge_alpha * edge_alpha * (3.0 - 2.0 * edge_alpha);
     float clip_mask = hard_aa * edge_alpha.x * edge_alpha.y * edge_alpha.z * edge_alpha.w;
     color *= clip_mask; // Premultiplied alpha modulation
     ```
  2. **Per-Element SDF Room Gating (Steal from `scrawl/src/native/pill/model.rs:146-184`):** When an item sits inside a morphing container (e.g., a pill or collapsing sidebar), evaluating `box_clearance` (the container’s analytic SDF clearance around the child’s bounding box) on the CPU or vertex shader produces a continuous `presence = smoothstep((room + feather) / feather)` multiplier that drives both opacity and subtle scale/blur before the child ever touches the border.
  3. **Result:** Every scroll view, overflow container, rounded card, and morphing pill gets arbitrary rounded-corner clipping **and** independent top/right/bottom/left soft gradient fade masks with **zero offscreen textures, zero stencil buffers, and zero render-pass breaks**.

#### 3.6 Layers, Offscreen Caching & Group Opacity
- **When Offscreen Passes Are Needed:** Only for (a) true group opacity (`0.0 < opacity < 1.0` on a container with *overlapping* children, where per-element alpha would show internal seams), (b) container-wide `backdrop-filter` / `filter: blur()`, or (c) explicitly cached complex subtrees (`will_change` / `cache_layer`).
- **Tile-Local Group Opacity Shortcut on Apple Silicon:** When a group with `opacity < 1.0` fits within a single pass and does not require blur, Metal programmable blending (`[[color(0)]]` scratch plane in tile memory, like Rive’s `scratchColorBuffer` in `draw_raster_order_path.frag:10`) can composite the group in on-chip tile memory and blend it onto the main color plane without ever allocating or bandwidth-flushing an offscreen texture!
- **Transient Texture Pool:** For effects that do cross pixel neighborhoods (blur), maintain a power-of-two / bucketed `MTLTexture` / `wgpu::Texture` pool with frame-age eviction (evict after 120 idle frames).

#### 3.7 Damage Tracking, Partial Present & 120 Hz Power Cost
- **The 120 Hz Power Reality:**
  - Running a full-window 4K/5K Metal render pass at 120 Hz consumes 4–9 W of package power even if only a 2×18 px text cursor is blinking.
  - At complete rest (zero dirty views, zero active springs), Rune **must draw 0 frames/sec** (0% CPU, 0% GPU).
- **Partial Present & Damage Tracking on Metal vs D3D12/Vulkan:**
  - Metal’s `CAMetalLayer` does **not** expose an equivalent to `EGL_KHR_swap_buffers_with_damage` or `IDXGISwapChain1::Present1(DirtyRects)`—when you present a `CAMetalDrawable`, WindowServer treats the drawable as a full surface, and because `nextDrawable` cycles through a ring of 2–3 buffers (`maximumDrawableCount`), rendering only a dirty scissor rect directly into `nextDrawable()` leaves stale content from 2–3 frames ago outside the scissor rect!
  - **How to Make Damage Tracking Actually Work on Metal:**
    1. **Persistent Backing Texture + Scissored Dirty Render:** Render the UI scene into a single persistent `MTLTexture` (`StorageModePrivate`, `BGRA8Unorm` / `RGBA16Float`) using `MTLLoadActionLoad` and `setScissorRect(dirty_bbox)` so Apple Silicon’s TBDR GPU only shades tiles overlapping `dirty_bbox`. Then encode a single hardware blit (`MTLBlitCommandEncoder::copyFromTexture`) or scissored copy into the `CAMetalDrawable`. On Apple Silicon unified memory, a tile-aligned blit of a 5K buffer takes ~80 µs while saving 95%+ of fragment shader execution on partial updates (like typing or cursor blink).
    2. **ProMotion Dynamic Frame Rate Scaling (`preferredFrameRateRange`):** Use `CADisplayLink`’s `CAFrameRateRangeMake(min, max, preferred)` ([Apple ProMotion Docs](https://developer.apple.com/documentation/quartzcore/optimizing-iphone-and-ipad-apps-to-support-promotion-displays)):
       - Interactive scroll, drag, or fast spring velocity: `CAFrameRateRangeMake(80, 120, 120)` (8.33 ms).
       - Settling spring tail (`velocity < threshold`) or subtle spinner/pulse: `CAFrameRateRangeMake(30, 60, 60)` or `24–48 Hz`.
       - Cursor blink / static timer: **Display link paused**, scheduled via timer only on blink transitions.

#### 3.8 Wide Color (Display P3), EDR/HDR, Linear Blending & Dithering
- **Color Pipeline:**
  - Authoring colors in sRGB, Display P3, or **Oklab / Oklch** (perceptually uniform lightness/chroma).
  - Gradients interpolate in **Oklab** (like GPUI `shaders.metal:950-988,1225`) or linear Display P3, with Jimenez IGN triangular dithering (`±1/255`).
  - **Surface Format:** Default `MTLPixelFormatBGRA8Unorm` with `CGColorSpaceCreateWithName(kCGColorSpaceDisplayP3)` on wide-gamut displays (50% less memory bandwidth than `RGBA16Float`), upgrading dynamically to `MTLPixelFormatRGBA16Float` with `wantsExtendedDynamicRangeContent = YES` only when an HDR/EDR surface or specular HDR highlight (> 1.0 headroom) is mounted in the scene.

---

### Topic 4: Input-to-Photon Latency Architecture

#### 4.1 Real-World Latency Measurements (macOS & iOS, 2025–2026)

| System / Benchmark | Display Rate | Measured Input-to-Photon (p50 / p99) | Architectural Cause |
| :--- | :--- | :--- | :--- |
| **Hardware + OS Compositor Floor** (Windowed macOS ProMotion) | 120 Hz (8.33 ms) | **~4.2–8.5 ms** | USB/SPI keyboard scan (~1 ms) + AppKit event delivery (~0.5 ms) + GPU render (<0.5 ms) + WindowServer next scanout (0–8.33 ms) |
| **Blackbird Terminal** ([`vtebench-2026-04-20.md`](https://github.com/conjfrnk/blackbird/blob/main/docs/benchmarks/vtebench-2026-04-20.md)) | 120 Hz ProMotion | **p50 ≤ 6 ms, p99 ≤ 20 ms** (CI-gated) | Direct event-triggered render without waiting for display-link lead |
| **Alacritty 0.17** ([`komsit37/ghostty-input-latency`](https://github.com/komsit37/ghostty-input-latency), Jul 2026) | 60 Hz (16.67 ms) | **p50 17.9 ms** (~4 ms above 14 ms capture floor) | Immediate redraw on PTY/key event; `maximumDrawableCount = 2` |
| **Ghostty 1.3.2** ([`komsit37/ghostty-input-latency`](https://github.com/komsit37/ghostty-input-latency), Jul 2026) | 60 Hz (16.67 ms) | **p50 58.6 ms** (~40 ms slower than Alacritty) | Queued frame scheduling / multi-refresh display-link pipeline lead |
| **`CAMetalDisplayLink` Windowed** ([`rrva/metal-display-link-pacing`](https://github.com/rrva/metal-display-link-pacing), 2026) | 60 Hz / 120 Hz | **66.5 ms (60 Hz) / 41.5 ms (120 Hz)** callback → screen | Windowed `CAMetalDisplayLink` targets **3 refreshes (49.8 ms at 60 Hz)** or **5 refreshes (41.5 ms at 120 Hz)** ahead of its callback! |
| **Zed (`gpui_macos`)** | 120 Hz ProMotion | **~12–20 ms** | `CVDisplayLink` io-thread → GCD `DispatchSource` → main thread `step()` (`display_link.rs:122-160`), adds 1 refresh vs direct input draw |

#### 4.2 Why `CAMetalDisplayLink` and `CVDisplayLink` Add Latency if Misused
1. **The `CAMetalDisplayLink` Trap:** Apple introduced `CAMetalDisplayLink` in macOS 14 / iOS 17 with `preferredFrameLatency` (accepting only `1.0` or `2.0`). However, Apple’s documentation notes: *"The final latency may be bigger if the system needs more time, such as for windowed modes on macOS."* Independent 2026 empirical measurements ([`rrva/metal-display-link-pacing`](https://github.com/rrva/metal-display-link-pacing)) prove that in **windowed mode on macOS**, `CAMetalDisplayLink` schedules its callback **5 refreshes (41.5 ms) ahead of scanout on 120 Hz panels** and **3 refreshes (49.8 ms) ahead on 60 Hz panels**. That guarantees smooth 120 fps throughput for heavy 3D games, but adds **33–50 ms of input lag** to a UI framework if input waits for `CAMetalDisplayLinkUpdate`!
2. **The `CVDisplayLink` Trap:** As documented in Zed’s `zed/crates/gpui_macos/src/display_link.rs:1-52`, `CVDisplayLink` runs on a separate high-priority CoreVideo I/O thread, has broken teardown semantics (`CVDisplayLinkStop` races with in-flight callbacks, causing segfaults in `CVHWTime::reset`), lacks `preferredFrameRateRange` for ProMotion power saving, and requires cross-thread signaling (`dispatch_source_merge_data`) to wake the main runloop.

#### 4.3 Rune’s Zero-Wait Input-to-Photon Pipeline (< 6 ms p50 on 120 Hz)
To achieve sub-frame (~4–6 ms p50) input-to-photon latency while maintaining butter-smooth 120 Hz animations:
1. **Two Distinct Frame Triggers (Input-Immediate vs DisplayLink-Paced):**
   - **Path A — Discrete Input Fast-Path (KeyDown, MouseDown, Trackpad Scroll phase, Direct Manipulation):** When `NSEvent` / `UIEvent` arrives on the main runloop and no frame has been presented in the current VSync interval (`now - last_present_time > 0.75 * refresh_period`), **do not wait for the next `CADisplayLink` tick**. Coalesce any pending events currently in the runloop queue, run incremental layout/paint (< 0.4 ms), acquire `layer.nextDrawable()`, encode and `commit()` immediately. Because a UI render pass takes ~0.2–0.4 ms on Apple Silicon, the drawable finishes rendering well before the compositor’s next VSync deadline and scans out on the **immediately upcoming refresh** (0–8.33 ms away, **p50 ~4.8 ms**).
   - **Path B — Continuous Animation Pacing (`CADisplayLink` on Main `NSRunLoop`):** When springs, physics scroll momentum, or shaders are actively animating, `CADisplayLink` (macOS 14+ / iOS) drives frame ticks on `NSRunLoopCommonModes` using `targetTimestamp` to step analytical springs (`model.rs` exact closed-form `spring(duration, bounce)`) to the exact display presentation time, eliminating dt-jitter.
2. **Double Buffering (`maximumDrawableCount = 2`) with Completion Pacing:**
   - Triple buffering (`maximumDrawableCount = 3`) allows the CPU to run 2 full frames ahead of the display, adding 8.3–16.6 ms of latency during continuous trackpad dragging.
   - Set `CAMetalLayer.maximumDrawableCount = 2` (or bound in-flight frames to 1 via an `MTLSharedEvent` / `dispatch_semaphore_t` signaled in `commandBuffer.addCompletedHandler`), ensuring `nextDrawable()` never queues stale frames.
3. **Live Window Resize Without Jitter (`presentsWithTransaction`):**
   - Keep `presentsWithTransaction = false` during normal operation (so asynchronous commits never block the main thread).
   - Inside `setFrameSize:` / `displayLayer:` (`zed/crates/gpui_macos/src/window.rs:3327-3340`, `scrawl/src/native/pill/mod.rs:403-417`), temporarily set `presentsWithTransaction = true`, call `commandBuffer.commit()`, `commandBuffer.waitUntilScheduled()`, and `drawable.present()`, then restore `presentsWithTransaction = false`.

---

## 3. What to Steal vs. What to Avoid

### What to Steal
1. **Zed (`gpui_macos` & `gpui_wgpu`):**
   - **Oklab gradient interpolation + Jimenez IGN 2-tap triangular dithering** (`±1/255`) in `kitlangton/gpui/src/platform/mac/shaders.metal:950-988,1170-1177` to eliminate 8-bit banding in shadows and gradients.
   - **Quarter-ellipse inner border SDF** and **perimeter-parameterized dashed borders** (`shaders.metal:194-449`).
   - **`wgpu` backend architecture for Linux/Windows/Web** with storage buffers + dual-source subpixel blending (`zed/crates/gpui_wgpu/src/wgpu_renderer.rs:781-795`).
   - **Zero-copy bi-planar `Y`/`CbCr` `IOSurface` video rendering** (`surface_fragment` in `shaders.metal:874-890`).
2. **Kit’s Scrawl (`scrawl/src/native/pill/`):**
   - **Peak-alpha boost during Gaussian blur** (`boost = 1.0 + smoothstep(0.0, 0.35, b) * 2.5`, `scrawl/src/native/pill/pill.metal:326-349`) so morphing/blurring text stays crisp and legible.
   - **Analytic SDF `box_clearance` room gating** (`scrawl/src/native/pill/model.rs:146-184`) to smoothly fade/scale child elements as container bounds morph.
   - **Hybrid `NSGlassEffectView` + `CAMetalLayer` host** (`scrawl/src/native/pill/mod.rs:348-380`).
3. **Makepad (`makepad/draw/src/shader/`):**
   - **Public-domain Slug GPU Bézier text & icon renderer** (`makepad/draw/src/shader/draw_text.rs:41-350` and `slug_atlas.rs:1-200`).
   - **Single-pass analytic SDF `Material` shading** (bevel, wrapped Lambert diffuse, Blinn-Phong specular, rim light, inner shadow coverage, `makepad/draw/src/shader/surface.rs:27-165`).
4. **Linebender (`vello_gpu` / `parley` / `harfrust` / `fontations`):**
   - **Sparse Strips (`vello_gpu`)** 4×4 pixel alpha tile strips (`vello/vello_gpu/src/render/wgpu/mod.rs`) for complex multi-contour vector paths and strokes.
5. **Rive (`rive-runtime/renderer/src/shaders/`):**
   - **Apple Silicon programmable framebuffer fetch (`[[color(IDX)]]`)** (`rive-runtime/renderer/src/shaders/metal.glsl:396-450`) for zero-bandwidth tile-local group opacity and blending.
6. **Dioxus (`subsecond`):**
   - **ThinLink + jump-table hot-patching** ([`docs.rs/subsecond`](https://docs.rs/subsecond/0.7.10/subsecond/)) for <400 ms stateful Rust UI hot-reloading.

### What to Avoid
1. **Avoid GPUI’s 4× MSAA Full-Window Intermediate Texture for Paths** (`kitlangton/gpui/src/platform/mac/metal_renderer.rs:195-235`): Allocating a full-window 4× MSAA texture and running a separate resolve + sprite-blit pass just to draw a few SVG icons wastes tens of megabytes of VRAM and breaks batching. Use **Slug** for icons/curves and **`vello_gpu` Sparse Strips** for complex vector art.
2. **Avoid `CVDisplayLink` on macOS 14+** (`zed/crates/gpui_macos/src/display_link.rs:1-52`): Deprecated, lacks ProMotion `preferredFrameRateRange`, and has unsafe thread teardown races. Use `CADisplayLink` on the main `NSRunLoop` combined with an immediate input-triggered draw fast-path.
3. **Avoid Windowed `CAMetalDisplayLink` for Latency-Critical Input**: Its 3–5 refresh lead (41–50 ms on macOS windowed mode) makes typing and dragging feel sluggish.
4. **Avoid Full Compute `vello`**: Moved to `research/` by Linebender in 2026 because its 9-stage compute pipeline has too high a baseline dispatch cost for low-latency 120 Hz UI frames.
5. **Avoid Maintaining Custom Vulkan & D3D12 Backends (`blade-graphics` / `gpui_windows`)**: Zed’s 2026 migration to `wgpu` proved that `wgpu` is faster on Linux/Windows while saving thousands of lines of driver-workaround code.

---

## 4. Decisive Recommendation & Renderer Architecture Sketch

### Decisive Stack Choice
- **Core Language:** **Rust (2024 Edition)** with thin `objc2` / Swift 6 host shims on Apple platforms and `subsecond` jump-table hot-patching in dev builds.
- **GPU Layer (`rune-gpu`):** A ~400-line internal trait (`GpuDevice`, `RenderPass`, `BufferSlice`, `TextureHandle`) with **two backends**:
  1. **`rune-metal` (macOS & iOS):** Direct `objc2-metal` (~1,600 LOC) with build-time `.metallib`, `IOSurface` zero-copy textures, programmable framebuffer fetch (`[[color(0..1)]]`), and `CAMetalLayer` + `NSGlassEffectView` host integration.
  2. **`rune-wgpu` (Linux, Windows, Android, WebGPU):** `wgpu` 28+ (~1,200 LOC) sharing the exact same scene buffer layout.

### Threading & Latency Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│ MAIN / UI THREAD (NSRunLoop / UIEventLoop)                              │
│  1. Input Fast-Path (NSEvent/UIEvent) OR CADisplayLink(targetTimestamp) │
│  2. Step analytical springs (exact closed-form at targetTimestamp)      │
│  3. Reactive view diff -> Taffy incremental layout -> SceneBuilder      │
│  4. Pack flat GPU instance buffers (UberQuad, GlyphRun, Slug, Strip)    │
└───────────────────────────────┬─────────────────────────────────────────┘
                                │ Lock-free triple-buffered SceneArena
                                ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ GPU SUBMISSION (Inline on Main Thread <250 µs, or Dedicated Render Q)   │
│  • Acquire persistent backing texture + dirty scissor bbox              │
│  • Upload dirty glyph/Slug/strip pages via UMA mapped ring buffer       │
│  • Encode RenderPass -> Blit to CAMetalDrawable -> commit()             │
└─────────────────────────────────────────────────────────────────────────┘
                                ▲
┌───────────────────────────────┴─────────────────────────────────────────┐
│ BACKGROUND WORKER POOL (Rayon / Async Executor)                         │
│  • Async image decoding -> staging ring buffer                          │
│  • Large text paragraph shaping (Parley + HarfRust)                     │
│  • Heavy SVG / chart flattening into vello_gpu Sparse Strips            │
└─────────────────────────────────────────────────────────────────────────┘
```

### Frame Graph (Typical Frame = 1 Render Pass + 1 Scissored Blit)

```
[Pass 0: Atlas Upload (Blit Encoder, only when new glyphs/icons/strips added)]
   └── Append dirty R8/RGBA8 glyph tiles, Slug curve/band texels, or Sparse Strip alphas

[Pass 1: Backdrop Blur Pyramid (Only if scene contains active in-app BackdropBlur)]
   └── Scissored Dual-Kawase 2x downsample (2 levels) -> 2x tent upsample

[Pass 2: Main UI Uber-Pass (Single RenderCommandEncoder, Scissored to Dirty Union)]
   ├── DrawBatch::UberQuad     (Rounded/Squircle box, border, outer/inner erf shadow,
   │                            analytic surface material, Oklab gradient + IGN dither)
   ├── DrawBatch::AtlasSprite  (Hinted body text R8/dual-source RGB + raster images)
   ├── DrawBatch::SlugVector   (Large/rotated/zooming text + crisp SVG icons via Slug)
   ├── DrawBatch::SparseStrips (Complex multi-color vector paths via vello_gpu 4x4 strips)
   └── DrawBatch::NativeSurface(Zero-copy YCbCr / RGBA IOSurface video & camera frames)
   * Every primitive evaluates `ClipStackBuffer[instance.clip_id]` in-shader for
     rounded-rect clipping + 4-edge analytic soft-gradient masks (zero mask passes!).
   * On Apple Silicon, non-blurred group opacity uses tile-local `[[color(1)]]` scratch
     plane without breaking Pass 2!

[Pass 3: Present]
   └── Copy/present persistent backing texture to `CAMetalDrawable` (`maximumDrawableCount = 2`)
```

### Core GPU Primitive Set (6 Primitives Total)

| Primitive | Instance Size | Responsibilities |
| :--- | :--- | :--- |
| `ClipNode` (GPU Buffer Table) | 32 B | `bounds`, `corner_radii`, `edge_feather (top, right, bottom, left)`, `parent_clip_id`, `transform_id`. Indexed by all primitives. |
| `UberQuad` | 128 B | Fill (Solid / Linear/Radial/Angular Oklab Gradient / Noise), per-corner radii + squircle exponent, border widths/style/color, analytic 4-sample `erf` outer/inner shadow, optional Makepad-style `Material` (bevel/rim/gloss). |
| `AtlasSprite` | 64 B | Monochrome (`R8Unorm`), Subpixel (`RGBA8` dual-source), or Polychrome (`RGBA8`) atlas tile + tint + rounded corner radii + transformation ref. |
| `SlugInstance` | 64 B | Em-space quad bounds, `curve_offset`, `curve_count`, `band_offset`, `band_count`, fill rule, color/gradient ref. |
| `SparseStripInstance` | 32 B | `vello_gpu` 4×4 pixel strip header (`x, y, width, alpha_offset, paint_id`) for arbitrary stroked/filled vector art. |
| `BackdropEffect` | 64 B | Kawase blur radius, saturation/contrast matrix, peak-alpha legibility boost (`pill.metal`), tint, and rounded-rect mask. |

---

## 5. Open Questions for Prototyping

1. **Uber-Shader Branching vs. Specialized PSOs on Low-End Integrated GPUs:** On Apple M1–M4, evaluating the unified `UberQuad` shader with uniform early-out branches (`if (quad.shadow_sigma == 0 && quad.bevel == 0)`) has near-zero divergence cost within 32-thread SIMD-groups. On older Intel/Qualcomm integrated GPUs via `wgpu`, should `UberQuad` split into `SimpleQuad` (64 B, flat/rounded + border) and `RichQuad` (128 B, shadow + gradient + material) pipelines?
2. **Slug Crossover Threshold on 2× vs 3× Retina:** At what exact pixel size (`24 px` vs `32 px` vs `48 px`) does Slug’s fragment raycasting match CoreText’s stem-darkened optical weight on macOS Retina displays without needing synthetic stem dilation in the Slug vertex shader?
3. **Persistent Texture Blit vs. Direct Drawable Rendering Threshold:** When >60% of the window area is dirty (e.g., full-window scrolling), rendering directly into `CAMetalDrawable` saves the 80 µs blit copy. What dirty-area ratio (e.g., `dirty_area / window_area > 0.35`) is the optimal hysteresis threshold to switch between persistent-texture partial redraw and direct-drawable full redraw?

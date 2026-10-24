# 02 — Rust UI Ecosystem & Shared Foundation Survey (2025–2026)

## Executive Summary

- **The Rust UI ecosystem has split cleanly into a shared low-level foundation and diverging high-level paradigms.** Across otherwise competing frameworks, text, layout, geometry, and accessibility have converged onto the same crates: `taffy` (layout), `parley` + `fontique` + `harfrust` + `skrifa` (text discovery, shaping, metrics/outlines), `kurbo` + `peniko` + `color` (2D geometry and styling), and `accesskit` (platform accessibility). Even `egui` 0.37 migrated `epaint` onto `fontique` + `harfrust` + `skrifa` + `vello_cpu` (`egui/crates/epaint/Cargo.toml:88-100`).
- **Linebender pivoted Vello from compute-only to Sparse Strips (`vello_cpu` + `vello_gpu`).** In 2026, the original compute-shader renderer (`vello` 0.8.0, 26k+ LOC of WGSL/Rust) was moved into `research/vello_research/` (`vello/ARCHITECTURE.md:5-31`). The active production path is **Sparse Strips** (`vello_common` 0.3, `vello_cpu` 0.3, and `vello_gpu` 0.3—renamed from `vello_hybrid` on 2026-10-02 in `vello/vello_gpu/CHANGELOG.md:9-19`), which tiles paths into 4×N strip spans on the CPU using `fearless_simd` and rasterizes/composites via either multithreaded SIMD CPU pipelines or universal vertex/fragment GPU shaders (including WebGL2 and tile-based mobile GPUs where compute shaders hit driver bugs and memory limits).
- **Makepad 2.0 (`dev` branch, 2026) is the most relevant prior art for Rune's live-tuning and agent-driven thesis.** Makepad replaced its legacy `live_design!` macro with **Splash (`script_mod!`)**, a reactive bytecode VM (`makepad-script`, `makepad/platform/script/Cargo.toml:1-12`) whose blocks are hot-extracted directly from `.rs` source files on disk without recompiling (`makepad/platform/src/live_reload.rs:144-179`). It also shipped a built-in HTTP `--remote` agent control protocol (`/snap`, `/g`, `/gseq`, `/click`, `if_user_seq` in `makepad/platform/src/remote.rs:1-42`, `makepad/docs/agents/app-remote.md:1-120`), a live visual `/tweak` inspector that writes layout edits back to Rust source lines (`makepad/widgets/families/tweaker/src/tweaker.rs:1-50`), a GPU **Slug** quadratic-Bezier text renderer for crisp large glyphs (`makepad/draw/src/text/slug_atlas.rs:1-45`), and inertia/spring scroll physics (`makepad/widgets/src/scroll_motion.rs:1-40`).
- **GPUI v0.2 and `gpui-component` (`gpui-kit` 0.8) prove that a custom 2D UI Uber-shader beats generic vector renderers for desktop apps, but raw GPUI lacks ergonomic primitives.** GPUI renders a small fixed set of UI primitives (`Quad`, `Shadow`, `Path`, `Underline`, `MonochromeSprite`, `PolychromeSprite`) in a single batchable GPU pass (`zed/crates/gpui/src/scene.rs:1-120`) and added `subsecond` hot-reloading (`zed/crates/gpui/Cargo.toml:50-56`). Longbridge's `gpui-kit` (`gpui-base` + `gpui-component` + `gpui-shell`) supplies the missing 75+ accessible controls, headless UI integration tests, and tested AI coding recipes (`gpui-component/README.md:13-51`, `gpui-component/docs/ARCHITECTURE.md:18-48`).
- **Dioxus 0.7 / Blitz 0.3 and Subsecond changed the Rust iteration speed curve.** `subsecond` binary hot-patching is now embedded across Dioxus (`dioxus/packages/devtools/Cargo.toml:19`), GPUI (`zed/crates/gpui/Cargo.toml:55`), and Freya (`freya/crates/freya-core/Cargo.toml:62`), enabling sub-second Rust view function patching without losing state. Meanwhile, `dioxus-native` renders HTML/CSS natively via Servo's `stylo` + `taffy` + `parley` + `anyrender` (`vello_hybrid`/`vello_cpu`/`skia`) without a WebView (`dioxus/packages/native/Cargo.toml:13-88`).
- **Iced 0.14 / 0.15-dev matured its runtime and developer tooling (`comet`, `iced_test`, `lilt` animations, `cargo-hot`), while Slint 1.19 added an embedded MCP server (`SLINT_MCP_PORT`).** However, Iced still lacks merged upstream `accesskit` support in `iced` master (`iced/Cargo.toml:25-94`), and Slint's `.slint` DSL splits application logic across two languages under a dual GPLv3/commercial license (`slint/Cargo.toml:83`).
- **Animation, audio, and multi-sensory polish remain third-class citizens across almost every Rust UI crate.** Only Makepad (`Animator` + shader-driven state tracks + `scroll_motion.rs`), Iced (`iced_core::animation::Animation` backed by `lilt` 0.8), GPUI (frame-interpolated `AnimationElement`), and Floem (reactive keyframe/spring transitions) provide built-in animation primitives—and none treat interruptible 2D/3D spring velocities, layout-transition FLIP morphing, or procedural UI audio synthesis as core primitives.
- **Decisive Recommendation for Rune:** Do **not** build a font parser, shaper, bidi/line-breaker, CSS Flexbox/Grid solver, or OS accessibility bridge. Depend on **`taffy`**, **`parley`** (which brings **`fontique`**, **`harfrust`**, **`skrifa`**, **`read-fonts`**, and **`icu4x`**), **`kurbo`**, **`peniko`**, **`color`**, and **`accesskit`**. Use **`vello_cpu` / `vello_gpu`** only for complex arbitrary SVG/vector path layers (`AnyRender`-style escape hatch), while building Rune's **primary UI compositor and custom SDF/box-shadow/material/spring-animated quad pipeline directly on `wgpu`** (the GPUI / Makepad model) alongside a **Splash/Knobs-style hot-tunable parameter + agent inspection layer**.

---

## 1. The Linebender Stack (`Xilem`, `Masonry`, `Vello`, `Parley`, `Fontations`, `HarfRust`, `AccessKit`, `Kurbo`, `Peniko`)

In Q1 2026, Linebender core authors Raph Levien and Daniel McNab joined Canva while continuing open-source stewardship of the Linebender stack. By October 2026, Linebender's modular crates have become the de facto lower-half standard of the Rust UI ecosystem.

### 1.1 `Xilem` (0.4.0) & `Masonry` (0.4.0)
- **Architecture & State Model:** Linebender separates its reactive view tree (`xilem_core` / `xilem_masonry`) from its retained widget tree (`masonry_core` / `masonry`) (`xilem/ARCHITECTURE.md:1-60`).
  - **`xilem_core::View`** (`xilem/xilem_core/src/view.rs:53-103`) is a lightweight, statically typed, short-lived description parameterized by `(State, Action, Context)` with two associated types—`type Element: ViewElement` and `type ViewState`—and four core methods: `build`, `rebuild(prev, view_state, ctx, element, app_state)`, `teardown`, and `message`. Because widget sequences are typed as nested tuples implementing `ViewSequence`, tree diffing is statically monomorphized with zero heap allocation or dynamic dispatch on unchanged subtrees (`memoize`).
  - **`masonry_core::core::Widget`** (`xilem/masonry_core/src/core/widget.rs:140-260`) is the retained, mutable object layer stored inside `WidgetPod`s with generational `WidgetId(NonZeroU64)` handles (`widget.rs:42`). Widgets receive explicit lifecycle passes: `on_pointer_event`, `on_text_event`, `on_access_event`, `on_anim_frame`, `update`, `measure`, `layout`, `compose`, `paint`, and `accessibility` (`widget.rs:115-123`).
- **Renderer Approach:** In 2026 (`v0.4.0`), Masonry decoupled itself from hardcoding `vello` by introducing the `imaging` painter abstraction (`xilem/Cargo.toml:98-103`, `masonry_core/src/core/widget.rs:20`), allowing Masonry windows to render via `imaging_vello`, `imaging_vello_hybrid`, `imaging_vello_cpu`, or `imaging_skia`.
- **Layout:** Masonry 0.4 replaced its legacy Druid box constraints with an explicit `measure` + `layout` + `compose` pass (`widget.rs:121`). Separate `compose` passes allow scroll/transform updates to reposition children without re-running layout.
- **Accessibility & Testing:** First-class `accesskit` 0.24 integration (`xilem/Cargo.toml:90`). Every widget implements `accessibility(&mut self, ctx: &mut AccessCtx, props: &PropertiesRef, node: &mut accesskit::Node)` (`widget.rs:122`). `masonry_testing` provides a headless test harness that drives pointer/text/accessibility events and snapshots rendered output.
- **Weaknesses:** Deeply nested tuple types in `Xilem` can produce multi-screen compiler errors if `ViewMarker` / `Action` types mismatch, and `Xilem` does not yet expose declarative spring/layout-transition animations (`on_anim_frame` in `widget.rs:206-211` is manual per-frame nanosecond delta stepping).

### 1.2 `Vello` (0.8.0 Research vs. Sparse Strips `vello_cpu` 0.3.0 & `vello_gpu` 0.3.0)
The most consequential architectural shift in Rust 2D graphics during 2025–2026 occurred inside `linebender/vello`:
- **Why Linebender pivoted from Compute Vello to Sparse Strips:**
  - The original **`vello`** crate (`v0.8.0`, now relocated to `research/vello_research/` per `vello/ARCHITECTURE.md:5-31`) is a full GPU compute-shader pipeline (`pathtag_reduce`, `bbox_clear`, `flatten`, `tile_alloc`, `coarse`, `fine` in `wgpu` compute shaders).
  - While compute Vello excels on high-end desktop GPUs for massive vector scenes (e.g., Paris map SVG with 100k+ paths), it suffered severe production blockers for general UI and mobile/web deployment (`vello/vello_gpu/README.md:84-90`, `vello/vello_gpu/CHANGELOG.md:105-145`):
    1. **Mobile GPU driver bugs & tile-memory limits:** Android Adreno and ARM Mali GPUs (e.g., Mali-G52) exhibited driver compilation failures, workgroup shared-memory exhaustion, and pipeline stalls on complex compute prefix-scans.
    2. **No WebGL2 / Downlevel support:** Compute shaders require full WebGPU/Vulkan/Metal/DX12 compute queues, ruling out WebGL2 browsers and older GLES devices.
    3. **Unbounded GPU buffer pre-allocation:** Complex paths could overflow fixed transient bump buffers mid-frame.
    4. **Excessive shader compilation startup latency & binary footprint.**
- **The Sparse Strips Architecture (`vello_common` 0.3, `vello_cpu` 0.3, `vello_gpu` 0.3):**
  - Developed from Laurenz Stampfl's 2025 ETH Zürich thesis (*High-performance 2D graphics rendering on the CPU using sparse strips*, cited in `vello/vello_cpu/README.md:169`), Sparse Strips splits rendering into:
    1. **CPU Path Flattening & 4×N Strip Tiling (`vello_common`):** Flattens `kurbo::BezPath` curves and packs anti-aliased alpha coverage into compact 4-pixel-tall horizontal strip spans (`vello/vello_common/Cargo.toml:1-31`).
    2. **`vello_cpu` (0.3.0):** Rasterizes and composites those strips entirely on the CPU using portable explicit SIMD via `fearless_simd` 1.0 (`NEON` on aarch64, `AVX2`/`SSE4.2` on x86_64, `wasm32` SIMD128) with two selectable pipelines: `u8_pipeline` (`RenderMode::OptimizeSpeed`) and `f32_pipeline` (`RenderMode::OptimizeQuality`), plus optional multithreaded tiling (`vello/vello_cpu/README.md:113-136`).
    3. **`vello_gpu` (0.3.0, formerly `vello_hybrid` until 2026-10-02):** Performs path flattening and strip generation on the CPU, uploads the tiny strip buffer to the GPU, and performs strip rasterization, image sampling, gradients, clip masks, andblurred rounded rects in standard **vertex + fragment shaders** over `wgpu` 29 or `webgl2` (`vello/vello_gpu/README.md:29-66`, `vello/vello_gpu/CHANGELOG.md:9-48`).
    4. **`glifo` (0.4.0):** Shared glyph run preparation and atlas caching crate sitting atop `vello_common` and `skrifa` (`vello/glifo/Cargo.toml:1-35`).
- **Performance & Status (Oct 2026):**
  - `vello_cpu` (`u8_pipeline` + `fearless_simd`) achieves sub-millisecond to low-single-digit-millisecond frame times for typical UI scenes on Apple Silicon and x86_64 (`https://laurenzv.github.io/vello_chart/`, cited in `vello/vello_cpu/README.md:159-164`), making it fast enough that `egui` 0.37 adopted `vello_cpu` as its built-in glyph/color-emoji rasterizer (`egui/crates/epaint/Cargo.toml:41,100`).
  - `vello_gpu` 0.3.0 explicitly validates against Adreno, Mali-G52 (`max_textures_per_shader_stage = 16`), Apple Silicon Metal, Vulkan, DX12, and WebGL2 (`vello/vello_gpu/CHANGELOG.md:36-46, 118-137`), though complex filter graphs and non-isolated blend modes are still unimplemented (`vello/vello_gpu/README.md:94-96`).

### 1.3 `Parley` (0.12.0), `Fontations` (`skrifa` 0.49 / `read-fonts` 0.46), and `HarfRust` (0.14.0)
- **The Pure-Rust Text Stack:**
  - **`fontations` (`read-fonts` 0.46 + `skrifa` 0.49):** Google Fonts' memory-safe replacement for FreeType, shipped in production Chrome and Android (`fontations/Cargo.toml:25-73`). Provides zero-copy OpenType table parsing, variable font variation axis interpolation, TrueType/CFF hinting (`autohint`), glyph metrics, outline extraction (`kurbo`), and `COLR`v0/v1 color glyph painting.
  - **`harfrust` (0.14.0):** Official HarfBuzz team port of the complete HarfBuzz OpenType shaping engine to safe Rust built directly on `read-fonts` (`harfrust/Cargo.toml:5-27`). Replaces `rustybuzz` with upstream parity and active Google Fonts backing.
  - **`fontique` (0.12.0):** System font enumeration, font fallback per script/locale, and generic family resolution (`parley/Cargo.toml:45`).
  - **`parley` (0.12.0):** Rich text layout engine combining `fontique`, `harfrust`, `skrifa`, and `icu4x` (`icu_properties`, `icu_segmenter`, `unicode-bidi`) for bidirectional text reordering, script segmentation, dictionary/UAX#14 line breaking (`complex-scripts` feature), inline boxes, and cursor/selection hit-testing (`parley/parley/Cargo.toml:16-51`).
  - **Important 2026 Split (`parley` vs `parley_engine`):** In 0.12, Parley split out `parley_engine` and `parlance` (`parley/parley/Cargo.toml:38-39`) so frameworks that want to manage their own style caching can drive the layout engine without Parley's high-level `LayoutContext` wrapper.

### 1.4 `Kurbo` (0.13.1), `Peniko` (0.6.1), `Color` (0.3.3), and `AccessKit` (0.25.1)
- **`kurbo` (0.13.1):** `#![forbid(unsafe_code)]`, `no_std`-capable 2D curve and geometry library (`Point`, `Vec2`, `Size`, `Rect`, `RoundedRect`, `Affine`, `BezPath`, cubic/quadratic flattening, stroke expansion, dash patterns, tight bounding boxes) (`kurbo/Cargo.toml:10-36`).
- **`peniko` (0.6.1) & `color` (0.3.3):** Shared vocabulary types (`Brush`, `Gradient`, `ImageBrush`, `BlendMode`, `Fill`, `FontData`, `Blob` via `linebender_resource_handle`) plus CSS Color 4 color spaces (`Oklab`, `Oklch`, `DisplayP3`, `Srgb`, `PremulRgba8` interpolation) (`peniko/peniko/Cargo.toml:34-38`).
- **`accesskit` (0.25.1):** Cross-platform accessibility tree diffing (`TreeUpdate`, `Node`, `Role`, `ActionRequest`) with native adapters for **macOS (`NSAccessibility`)**, **iOS (`UIAccessibility`)**, **Windows (`UIAutomation`)**, **Linux (`AT-SPI` via D-Bus)**, and **Android (`AccessibilityNodeProvider`)** (`accesskit/Cargo.toml:3-14`, `accesskit/accesskit/Cargo.toml:1-30`).

---

## 2. Makepad (v1.0 crates.io / v2.0 `dev` branch, 2026)

Makepad (`makepad/makepad`, created by Rik Arends) is a full-stack GPU-rendered UI and application environment with zero dependencies on `wgpu`, `winit`, or `taffy`. Its 2025–2026 evolution on the `dev` branch (`v2.0.0`) contains the most advanced live-coding, shader-styling, and AI-agent inspection infrastructure in the Rust ecosystem.

- **From `live_design!` to Splash (`script_mod!`) & `makepad-script`:**
  - In Makepad 1.0 (May 2025), UI layout and shaders were declared inside `live_design! { ... }` macros. In Makepad 2.0 (2026 `dev`), `live_design!` was completely replaced by **Splash** via `script_mod! { ... }` backed by `makepad-script` (`makepad/AGENTS.md:27-88`, `makepad/platform/script/Cargo.toml:1-12`).
  - `makepad-script` is a custom, minimal-dependency dynamically typed scripting language and bytecode VM with prototype inheritance (`+WidgetBase { ... }`), reactive property propagation, closures, and embedded shader code (`makepad/platform/script/src/vm.rs`).
- **True Hot Reload Without Recompiling (`live_reload.rs`):**
  - Why does Makepad embed `script_mod!` inside `.rs` files? Because at runtime (`MAKEPAD=lines cargo run` or Makepad Studio), `makepad_platform::live_reload::extract_script_mod_from_file` (`makepad/platform/src/live_reload.rs:144-179`) watches `.rs` files on disk, tokenizes the file to extract the token stream inside `script_mod! { ... }` without invoking `rustc`, re-evaluates the script module in the `makepad-script` VM, and patches widget properties, layouts, and GPU shaders in single-digit milliseconds while preserving Rust struct state.
- **Shader DSL & Rendering Pipeline (`makepad-draw`):**
  - Every widget's visual primitives (`DrawQuad`, `DrawColor`, `DrawText`, custom SDF shapes) are written in Makepad's Rust-embedded shader language (`draw_bg: { pixel: fn() -> vec4 { ... } }`), which cross-compiles at runtime to **Metal MSL**, **DirectX 11 HLSL**, **OpenGL/GLES GLSL**, and **WebGL2** (`makepad/platform/src/os/`).
  - Instead of CPU-tessellating vector paths for standard UI controls, Makepad draws controls as instanced quads with analytical SDF math in the fragment shader—making per-pixel animations, procedural materials, glow, and morphing effectively free on the GPU.
- **Text Stack (GPU Slug + MSDF + Raster Atlas):**
  - In 2026, Makepad uses a hybrid text engine (`makepad/draw/src/text/`):
    - **`slug_atlas.rs` (`makepad/draw/src/text/slug_atlas.rs:1-45`):** Implements Eric Lengyel's **Slug** algorithm, packing quadratic Bézier curves and horizontal/vertical spatial bands into GPU textures and evaluating exact winding numbers per pixel in the fragment shader. Used for large/scaled text where bitmap atlases blur or blow out VRAM.
    - **Raster/SDF Glyph Atlas + `rustybuzz` (`makepad/draw/Cargo.toml:14`):** Used for small hinted body text.
- **Layout & Animation:**
  - Custom single-pass turtle/flow layout engine (`Walk`, `Layout`, `Flow::Right`, `Flow::Down`, `Flow::Overlay`, `Size::fill()`, `Size::fit()`) in `makepad-draw`—very fast ($O(N)$ single pass), though less expressive than CSS Grid/Flexbox alignment edge cases.
  - Built-in `Animator` state machine (`animator: { hover = { default: off, on = { from: { all: Forward { duration: 0.15 } }, apply: { draw_bg: { hover: 1.0 } } } } }`) that interpolates shader uniforms and layout properties directly, plus `ScrollMotion` (`makepad/widgets/src/scroll_motion.rs:1-40`) for flick inertia and spring bounce-back.
- **First-Class Agent Inspection, Frame Tracing & Live Tweaker (2026):**
  - **`--remote` HTTP Control Protocol (`makepad/platform/src/remote.rs:1-42`, `makepad/docs/agents/app-remote.md:1-120`):** Running any Makepad app with `--remote 127.0.0.1:8123` (and optionally `MAKEPAD_HIDE_WINDOWS=1` for headless verification) exposes:
    - `GET /snap` → compact semantic widget hierarchy + bounding boxes + `user_seq` counter.
    - `GET /g` and `GET /gseq?frames=10&interval_ms=33` → single or multi-frame PNG screenshots of active windows without waking the OS screen.
    - `POST /click`, `/scroll`, `/type`, `/key`, `/Eval` with `if_user_seq` optimistic concurrency guards so agents never click stale coordinates.
  - **`/tweak` Live Tweaker (`makepad/widgets/families/tweaker/src/tweaker.rs:1-50`, `makepad/docs/agents/tweaker.md:1-90`):** Holding `Alt/Option` (or calling `GET/POST /tweak`) inspects every live `Walk`/`Layout`/`DrawQuad` value alongside its exact `file:line:col` origin and writes interactive slider drags directly back to the `.rs` file on disk.
  - **Frame & Present Tracing (`makepad/platform/src/frame_trace.rs`, `present_trace.rs`):** Per-frame timing instrumentation for debugging hitches.
- **Platforms:** Custom zero-`winit` OS backends in `makepad/platform/src/os/`: **macOS / iOS / tvOS / visionOS** (`apple/`), **Windows** (`windows/`), **Linux X11/Wayland/Direct DRM** (`linux/`), **Android** (`android/`), **WebAssembly/WebGL2** (`web/`), and **Meta Quest OpenXR** (`linux/openxr*.rs`).
- **Compile Times & Binary Size:** Because Makepad avoids heavy proc-macros (`syn`/`quote`), `wgpu`, and `winit`, a clean debug build of `makepad-example-simple` compiles ~120 crates (vs 400+ for `wgpu`+`winit` stacks) in ~15–25s on Apple Silicon, with stripped release binaries around **4–7 MB** (measured/documented across Makepad builds).
- **Weaknesses:** Poor external documentation (`makepad/AGENTS.md` is actually better written for AI agents than human docs on crates.io), no `accesskit` integration (screen readers cannot read Makepad windows; verified in `blog.wybxc.cc` Aug 2026 survey), and IME preedit composition overlay positioning is incomplete.

---

## 3. Major Rust UI Frameworks (2025–2026)

### 3.1 Slint (`v1.19.0`)
- **Approach & DSL:** Uses a dedicated QML-like `.slint` declarative language (`slint/Cargo.toml:1-87`). `.slint` files can either be ahead-of-time compiled to Rust/C++ code (`slint-build` / `slint!` macro) with static vtables (`helper_crates/vtable`) and fine-grained two-way property bindings (`Property<T>`), or dynamically loaded via `slint-interpreter` (`internal/interpreter`) for live preview (`SLINT_LIVE_PREVIEW=1`).
- **Renderers:** Pluggable across four backends (`slint/api/rs/slint/Cargo.toml:210-239`):
  1. **`renderer-femtovg`** (default on desktop, OpenGL or `wgpu` via `renderer-femtovg-wgpu`).
  2. **`renderer-skia`** (Metal, Vulkan, D3D, OpenGL, or CPU).
  3. **`renderer-software`** (`#![no_std]` capable dirty-rect scanline renderer supporting `RGB565` and `Gray8` for MCUs with <300 KB RAM).
  4. **`renderer-vello` / `i-slint-renderer-anyrender`** (`slint/Cargo.toml:101`, `slint/api/rs/slint/Cargo.toml:231-238`, experimental Vello/AnyRender backend).
- **Text, Layout, Animation & Accessibility:**
  - **Text:** Migrated in 1.18–1.19 to `parley` + `fontique` 0.12 + `skrifa` 0.44 + `read-fonts` 0.41 + `rustybuzz` 0.20 + `icu_*` (`slint/Cargo.toml:133, 148-172`).
  - **Layout:** Built-in `HorizontalLayout`, `VerticalLayout`, `GridLayout`, and Flexbox-like stretch constraints compiled directly into property dependency graphs.
  - **Animation:** Declarative `animate prop { duration: 250ms; easing: ease-in-out-back; }` and `states [ ... ]` transitions in `.slint`.
  - **Accessibility & Agent Tooling:** Full `accesskit` integration (`accessibility` feature enabled by default in `slint/api/rs/slint/Cargo.toml:28, 85`), working out-of-the-box with macOS VoiceOver and IME. Notably, **Slint 1.19 added an embedded MCP server (`mcp` feature, `SLINT_MCP_PORT`)** and system-testing server (`slint/api/rs/slint/Cargo.toml:136-152`) so AI agents can inspect and drive a running Slint UI over Streamable HTTP.
- **Platforms:** Desktop (macOS, Windows, Linux X11/Wayland/KMS-DRM), Android (`backend-android-activity-06`), iOS, WebAssembly, and bare-metal MCUs (ESP32, STM32, RP2040).
- **Licensing:** Triple-licensed (`GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0`, `slint/Cargo.toml:83`)—restrictive for an MIT/Apache-2.0 open-source framework like Rune.

### 3.2 Dioxus (`v0.7.x`) & Blitz (`v0.3.0-beta.4`) + `Subsecond`
- **Architecture:** Dioxus 0.7 offers two native desktop paths:
  1. **`dioxus-desktop`:** Renders `rsx!` VirtualDOM inside the OS WebView (`wry`/`tao`).
  2. **`dioxus-native` (`dioxus/packages/native/Cargo.toml:1-99`):** Renders Dioxus `rsx!` HTML/CSS components natively on the GPU without a WebView via **Blitz** (`blitz-dom`, `blitz-paint`, `blitz-shell`).
- **How Blitz Works (`blitz/packages/blitz-dom/Cargo.toml:46-84`):**
  - Combines **Servo's `stylo`** (`style`, `selectors`, `cssparser`) for parallel CSS cascade/selector matching with **`stylo_taffy` + `taffy` 0.15** (Flexbox, CSS Grid, Block, Float, `calc()`) for layout, **`parley` + `skrifa` + `icu4x`** for inline/rich text layout, **`accesskit`** for accessibility, and **`anyrender`** (`anyrender_vello_hybrid` by default in `dioxus/packages/native/Cargo.toml:13, 37-45`, with `vello`, `vello_cpu`, and `skia` options) for painting.
- **State Model:** Copyable arena-allocated reactive signals (`Signal<T>`, `Memo<T>`, `Resource<T>`) backed by `generational-box` (`dioxus/packages/signals/`), paired with structural `rsx!` template diffing (`dioxus-core`).
- **`Subsecond` Hot-Patching (`dioxus/packages/devtools/Cargo.toml:19`):**
  - Dioxus 0.7 introduced `subsecond`, a Rust binary hot-patching runtime and linker workflow (`dx serve --hot-patch`). When a Rust function changes, `dx` compiles a tiny dynamic object containing only the modified symbols and patches a jump table in the running process—allowing arbitrary Rust view code, closures, and new widgets to hot-reload in <500ms without restarting the app or losing signal state.
- **Weaknesses:** Carrying `stylo` + full HTML DOM semantics adds substantial compile-time weight (~500+ crates) and forces desktop UI into web CSS quirks rather than purpose-built native motion/interaction primitives.

### 3.3 Iced (`v0.14.0` / `v0.15.0-dev`)
- **State Model & Runtime:** Strictly follows the Elm Architecture (`State`, `Message`, `update(&mut State, Message) -> Task<Message>`, `view(&State) -> Element<Message>`) (`iced/Cargo.toml:1-198`). In `0.14.0` (`iced/CHANGELOG.md:9-48`), Iced introduced **reactive rendering** (redrawing and re-laying out only when visual outputs change) and **`Task`** (replacing `Command` with composable async streams via `sipper`).
- **Renderer & Text:**
  - Dual renderer architecture (`iced/Cargo.toml:25-31`): **`iced_wgpu`** (on `wgpu` 30, using instanced quads, `cryoglyph` glyph atlas, and `lyon` tessellation for vector canvas) with automatic fallback to **`iced_tiny_skia`** (`tiny-skia` 0.12 software rasterizer + `softbuffer`).
  - Text shaping and layout use **`cosmic-text`** (`iced/Cargo.toml:206`), which wraps `swash`, `rustybuzz`, and `fontdb`.
- **Layout & Animation:**
  - Custom Flexbox-inspired `Layout` and `Node` solver in `iced_core::layout` (does not use `taffy`).
  - Added first-class `iced_core::animation::Animation` in 0.14 (`iced/Cargo.toml:215`), backed by the `lilt` 0.8 spring/easing transition crate, paired with `window::frames()` for smooth vsync-driven transitions.
- **2025–2026 Tooling (`comet`, `iced_test`, `cargo-hot`):**
  - **`comet` & Time-Travel Debugging (`iced/Cargo.toml:46-47`):** Because all state mutations flow through `Message`, Iced 0.14 shipped `comet`, an external visual debugger with live time-travel scrubbing across message history and performance metrics (`F12`).
  - **`iced_test` & `iced_tester` (`iced/Cargo.toml:50, 193-194`):** Headless `Simulator` that queries widgets by text/ID (`iced_selector`), synthesizes clicks/keys, and compares visual snapshots across `wgpu` and `tiny-skia`.
  - **`cargo-hot` (`iced/Cargo.toml:48-49, 205`):** Experimental hot-reloading of `view` functions.
- **Ecosystem Adoption (System76 COSMIC):** System76's `libcosmic` fork of Iced powers the production COSMIC Linux desktop environment (and added `accesskit` + `taffy` in its fork), but upstream `iced` 0.15-dev still lacks built-in `accesskit` integration (`blog.wybxc.cc` survey, Aug 2026).

### 3.4 GPUI (`v0.2.2` in Zed) & `gpui-component` / `gpui-kit` (`v0.8.0`)
- **GPUI Architecture (`zed/crates/gpui/`):**
  - Hybrid **Entity + Immediate-Element + Retained-Fiber** architecture. Application state lives in reference-counted `Entity<T>` handles owned by the `App` context (`cx.new(...)`, `cx.notify()`). Views implement `Render::render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement`.
  - **Renderer (`zed/crates/gpui/src/scene.rs`):** Instead of general 2D path tessellation, GPUI sorts primitives by draw order/stacking context into a flat GPU `Scene` of `Quad` (with analytic border/corner-radius/background), `Shadow` (analytic box shadow), `Underline`, `MonochromeSprite` (text glyphs/icons), `PolychromeSprite` (images/emoji), and `Path` (rare custom shapes via `lyon` / screen-space MSAA). On macOS it renders via direct Metal (`objc2-metal` / `blade-graphics` on Linux/Windows/WebGPU), effortlessly hitting 120 FPS in Zed.
  - **Layout & Text:** Uses **`taffy`** (`zed/crates/gpui/Cargo.toml:107-115`) for Flexbox/Grid layout and native platform text APIs (**CoreText** on macOS, **DirectWrite** on Windows, ** cosmic-text / font-kit** on Linux) for OS-authentic glyph rasterization and subpixel AA.
  - **2026 Additions:** Added WebAssembly target support (`wasm32-unknown-unknown`), `accesskit` workspace dependency (`zed/crates/gpui/Cargo.toml:62`), `leak-detection` (`Cargo.toml:38`), and `subsecond` `hot-reload` (`Cargo.toml:50-56`).
- **Why `gpui-kit` / `gpui-component` (`v0.8.0`) Matters (`gpui-component/README.md:1-146`, `docs/ARCHITECTURE.md:1-150`):**
  - Raw GPUI ships zero high-level widgets (not even a `TextInput`). Longbridge built **`gpui-kit`** (powering Longbridge Pro desktop) with a clean three-layer split:
    1. **`gpui-base`:** Unstyled behavioral primitives (focus traps, popup collision `Positioner`, `InputState`, `EditorState`, `VirtualList`, `TreeState`, `DockArea`, and `AccessKit` role/action wiring).
    2. **`gpui-component`:** 75+ styled Shadcn-inspired desktop controls (`DataTable`, 200k-line Tree-sitter `CodeEditor`, `Select`, `Dialog`, `Sheet`, `Calendar`, `Toast`).
    3. **`gpui-shell`:** Capability-gated QuickJS (`quickjs-jit`) extension runtime for JS plugins.
  - Notably, `gpui-kit` also ships **`examples/ai_recipes/`** (`gpui-component/examples/ai_recipes/README.md:1-40`)—a verified suite of executable consumer recipes and `script/check-ai` drift gates specifically designed so AI coding agents produce compiling, non-leaking GPUI code on the first attempt.

### 3.5 `egui` (`v0.37.0`)
- **Architecture:** Pure immediate-mode GUI (`ui.button("Click").clicked()`). Every frame rebuilds a flat mesh of textured triangles (`epaint::Mesh` with a single 1D feathering/gradient texture + font atlas) so the entire UI renders in a handful of draw calls on `egui_wgpu` (default in `eframe` since 0.34) or `egui_glow`.
- **2026 Text Overhaul (`egui/crates/epaint/Cargo.toml:38-42, 88-100`):** In `0.37.0`, `epaint` replaced its legacy `ab_glyph` rasterizer with **`fontique` + `harfrust` + `skrifa` + `vello_cpu`**, gaining proper OpenType shaping, system font fallback (`egui_system_font_provider`), and `COLR`/`sbix`/`CBDT` color emoji rendering!
- **Accessibility & Testing:** Strong `accesskit` 0.24 support and **`egui_kittest`** (`egui/Cargo.toml:25, 111`), which lets tests query and drive widgets semantically through their AccessKit nodes (`harness.get_by_label("Submit").click()`).
- **Why `egui` is Great for Tools and Bad for High-Craft Consumer Feel:**
  - *Great for tools:* Zero state synchronization boilerplate, trivial embedding in game/3D viewports (`rerun.io`), fast compile times.
  - *Bad for feel:* Immediate-mode single-pass layout requires knowing child sizes *after* placing parents (causing one-frame layout lag/flicker on dynamic content resized first frame), lacks retained per-element velocity state for interruptible spring physics or FLIP layout morphing, and forces custom look-and-feel into procedural `Painter` math rather than composable style/animation trees.

### 3.6 `Floem` (`v0.2.0`), `Freya` (`v0.5.0-beta.12`), `Vizia` (`v0.4.0`), and `Cushy` (`v0.4.0`) / `Kludgine` (`v0.11.0`)
- **Floem (`v0.2.0`, `floem/Cargo.toml:1-150`):**
  - Fine-grained reactive signals (`RwSignal<T>` is `Copy`, backed by arena IDs in `floem_reactive`) where views are constructed once and closures surgical-patch styles/text without virtual DOM diffing.
  - Uses **`taffy` 0.9.2** for Flexbox/Grid layout, **`parley` 0.7 + `fontique` 0.7** for text, **`ui-events` 0.3**, and J.R. Moulton's new **`understory`** crates (`understory_box_tree`, `understory_event_state`, `understory_focus`, `understory_virtual_list` in `floem/Cargo.toml:117-121`) for spatial indexing and focus management.
  - Pluggable renderer trait (`floem_renderer`) with backends for `vger` (GPU SDF shaders), `vello`, `skia`, and `tiny_skia` fallback. Includes built-in reactive keyframe and spring animations (`.animation(...)`) and a rich style transition system, but in 2026 its IME and AccessKit wiring on macOS remain broken (`blog.wybxc.cc` survey).
- **Freya (`v0.5.0-beta.12`, `freya/Cargo.toml`, `freya/crates/freya-core/Cargo.toml:1-96`):**
  - **Major 2025–2026 Pivot:** Freya 0.4/0.5 dropped Dioxus's VirtualDOM/RSX runtime entirely and built its own **`freya-core`** reactivity (`generational-box` signals + `pathgraph` tree + `ragnarok` event dispatch) with a clean builder API instead of HTML tags.
  - Uses **`torin`** (`freya/crates/torin/`, custom Flexbox-like layout engine with caching), **`freya-skia-safe`** (`skia-safe` 0.93 for rendering and `skia::textlayout::Paragraph` for text), **`accesskit` 0.21**, **`freya-testing`** headless harness, and **`subsecond` 0.7.4** for hot-reloading.
- **Vizia (`v0.4.0`, `vizia/ARCHITECTURE.md:1-90`, `vizia/Cargo.toml:50-111`):**
  - ECS-inspired retained tree (`Entity` IDs indexing sparse-set component stores for styles/layout caches) with fine-grained signals (`vizia_reactive`), CSS stylesheet parsing (`cssparser` 0.37 + `selectors` 0.39) with CSS property animations, **`morphorm` 0.9** (custom single-pass stretch layout), **`skia-safe` 0.99** (with Skia `textlayout`), **`accesskit` 0.24**, and `vizia_baseview` for VST/CLAP audio plugin windows alongside `vizia_winit`.
- **Cushy (`v0.4.0`) & Kludgine (`v0.11.0`) (`cushy/Cargo.toml:1-66`, `kludgine/Cargo.toml:1-49`):**
  - Built by Khonsu Labs around a single ergonomic reactive primitive (`Dynamic<T>`), rendered on `wgpu` 24 via `kludgine` (`lyon_tessellation` + `cosmic-text` 0.12 + `etagere` atlas packer). Clean widget composition and built-in `Dynamic` easing animations, but development has been largely dormant since late 2024 and lacks `accesskit` screen-reader support.

### 3.7 `Taffy` (`v0.15.0`)
- **Status & Capabilities (`taffy/Cargo.toml:1-91`):** Maintained under `DioxusLabs/taffy` by Nico Burns, Alice Cecile, and Jonathan Kelley. `taffy` 0.15 implements **CSS Flexbox** (including CSS Flexbox Level 2 `flex-wrap: balance`), **CSS Grid**, **CSS Block**, **CSS Float (`float_layout`)**, and **`calc()`** expressions.
- **Architecture:** Exposes both a ready-to-use arena (`TaffyTree` backed by `slotmap`) and low-level traits (`TraversePartialTree`, `LayoutFlexboxContainer`, `LayoutGridContainer`) so frameworks can store layout styles directly inside their own widget tree or ECS without duplicating nodes, using a custom measure callback (`measure_function`) to invoke `parley` for inline text measurement.
- **Who Uses It:** **Zed (`gpui`)**, **Bevy (`bevy_ui`)**, **Dioxus Native / Blitz (`stylo_taffy`)**, **Floem**, **Lapce**, **System76 `libcosmic`**, **Rosin**, and **Tessera**.

### 3.8 Notable 2025–2026 Entrants (`Blinc`, `Rosin`, `Tessera`, `Ply`)
- **Blinc (`project-blinc/Blinc`, early 2026):** Reactive `wgpu`-based cross-platform UI framework targeting desktop, iOS, and Android with stateful components; early alpha with incomplete CJK font fallback, manual reactive event wiring on inputs, and no `accesskit` integration (`blog.wybxc.cc` survey, Aug 2026).
- **Rosin (`docs.rs/rosin`, 2026):** Combines top-level centralized state with fine-grained reactive lenses, CSS stylesheet styling, `vello` rendering (exposing raw `vello` canvas callbacks per node via `.on_canvas(...)`), working IME, and `accesskit` integration.
- **Tessera (`tessera-ui` 2.5.0, 2025–2026):** Declarative immediate-mode UI framework using proc-macro positional memoization (`#[tessera]`) and a pluggable `wgpu` shader pipeline (e.g., frosted glass/fluid shaders) targeting Material Design 3; still suffers from platform `wgpu` surface bugs on macOS (`blog.wybxc.cc` survey).
- **Ply (`plyx.iz.rs`, v1.0 in March 2026):** Clay-inspired immediate-mode builder UI library focused on minimal boilerplate and autocompletion-friendly layout builders, with optional `accesskit` setup.

---

## 4. Ecosystem Comparison Matrix (Oct 2026)

| Framework (Version) | Renderer Approach | Text Stack | Layout Engine | State Model | Animation Support | Accessibility (`accesskit`) | Platforms (Desktop / Mobile / Web) | Hot Reload / Live Tooling | Clean Debug Compile / Stripped Release Size |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **Xilem / Masonry** (`0.4.0`) | `imaging` trait (`vello`, `vello_gpu`, `vello_cpu`, `skia`) | `parley` 0.12 (`fontique` + `harfrust` + `skrifa` + `icu4x`) | Masonry custom `measure` / `layout` / `compose` | Static typed `View` tree diffing (`ViewSequence`) | Manual `on_anim_frame(ns)` per widget | **Yes** (`accesskit` 0.24, built-in + `masonry_testing`) | Win, Mac, Linux, Android (`xilem_web` separate DOM crate) | None built-in (`subsecond` compatible) | ~45–60s (est.) / ~8–12 MB (est.) |
| **Makepad** (`1.0` / `2.0-dev`) | Custom GPU instanced SDF shaders (Metal, DX11, GL, WebGL2) | GPU **Slug** curve atlas (`slug_atlas.rs`) + SDF/raster + `rustybuzz` | Custom 1-pass Turtle `Walk` / `Flow` | `script_mod!` (Splash VM) + Rust `Widget` structs | Shader-driven `Animator` tracks + `ScrollMotion` springs | **No** | Win, Mac, Linux, **iOS, Android, WebGL2, visionOS, Quest** | **Best-in-class:** zero-recompile `script_mod!` reload + `/tweak` + `--remote` HTTP API | **~15–25s** / **~4–7 MB** (measured/repo) |
| **Slint** (`1.19.0`) | `femtovg` (GL/wgpu), `skia`, `software` (`no_std`), `anyrender`/`vello` | `parley` + `fontique` 0.12 + `skrifa` 0.44 + `rustybuzz` | Built-in compiled constraint/grid/flex solver | `.slint` two-way reactive `Property<T>` bindings | Declarative `animate` & `states` in `.slint` | **Yes** (default on desktop, works with VoiceOver) | Win, Mac, Linux (KMS), **Android, iOS, WASM, MCUs** | `SLINT_LIVE_PREVIEW` interpreter + VS Code preview + **`SLINT_MCP_PORT`** | ~35–50s / **~2–5 MB** (`software`/`femtovg`) |
| **Dioxus Native / Blitz** (`0.7` / `0.3-b4`) | `anyrender` (`vello_hybrid` default, `vello`, `vello_cpu`, `skia`) | `parley` + `fontique` + `skrifa` + `icu4x` | Servo `stylo` + `stylo_taffy` + **`taffy` 0.15** | Copyable arena `Signal<T>` + `rsx!` template diffing | CSS transitions (partial in Blitz) or frame signals | **Yes** (`accesskit` in `blitz-dom`) | Win, Mac, Linux, iOS/Android (experimental), WASM | **`subsecond`** binary hot-patching + RSX template hot reload (`dx serve`) | ~60–90s (`stylo` heavy) / ~15–25 MB (est.) |
| **Iced** (`0.14` / `0.15-dev`) | `iced_wgpu` (`wgpu` 30 quads + `lyon`) + `tiny-skia` CPU fallback | `cosmic-text` (`swash` + `rustybuzz` + `fontdb`) + `cryoglyph` | Custom `iced_core::layout` Flexbox solver | Elm Architecture (`State`, `Message`, `Task`) | `Animation<T>` (`lilt` 0.8 springs/easings) | **No** upstream (present in System76 `libcosmic` fork) | Win, Mac, Linux, WASM (limited mobile) | **`comet`** time-travel debugger + `cargo-hot` + `iced_test` | ~35–50s / ~8–14 MB (est.) |
| **GPUI + `gpui-kit`** (`0.2.2` / `0.8.0`) | Custom Uber-shader (`Quad`, `Shadow`, `Sprite`, `Path`) on Metal/Blade/WebGPU | Native OS (**CoreText**, **DirectWrite**) + `font-kit` | **`taffy`** (Flexbox + CSS Grid) | Retained `Entity<T>` models + immediate `Element` fiber | `AnimationElement` + `gpui-base` motion helpers | **Yes** (`accesskit` wired in `gpui-base`) | Mac, Win, Linux, **WASM** (minimal mobile) | **`subsecond`** (`hot-reload` feature) + `gpui-shell` QuickJS + Inspector | ~45–70s / ~15–30 MB (`gpui-shell` 78 MB stripped) |
| **egui** (`0.37.0`) | Immediate-mode 2D triangle mesh (`egui_wgpu` / `egui_glow`) | `fontique` + `harfrust` + `skrifa` + **`vello_cpu`** | Custom 1-pass immediate-mode cursor layout | Immediate mode + `Context` ID memory store | `ctx.animate_bool` / `animate_value_with_time` | **Yes** (`accesskit` 0.24 + **`egui_kittest`**) | Win, Mac, Linux, **WASM**, Android/iOS (via `winit`) | External only (`subsecond` / `hot-lib-reloader`) | **~20–30s** / **~5–9 MB** |
| **Floem** (`0.2.0`) | `vger` (GPU SDF), `vello`, `skia`, `tiny_skia` | `parley` 0.7 + `fontique` 0.7 + `swash` | **`taffy` 0.9.2** + `understory_box_tree` | Fine-grained `Copy` `RwSignal<T>` (no VDOM) | Built-in reactive keyframe & spring `.animation()` | Partial / broken on macOS in 0.2 | Win, Mac, Linux, WASM | Inspector overlay only | ~40–55s / ~10–15 MB (est.) |
| **Freya** (`0.5.0-beta.12`) | `freya-skia-safe` (`skia-safe` 0.93 Metal/GL/Vulkan/D3D) | Skia `textlayout::Paragraph` | Custom **`torin`** Flexbox layout engine | `freya-core` `generational-box` signals + builder views | `freya-animation` hook-based easings/springs | Wired (`accesskit` 0.21), macOS screen reader issues | Win, Mac, Linux | **`subsecond` 0.7.4** + `freya-devtools` + `freya-testing` | ~30–45s (with prebuilt Skia bin) / ~18–28 MB |
| **Vizia** (`0.4.0`) | `skia-safe` 0.99 (Metal, Vulkan, D3D, GL) | Skia `textlayout` + `unicode-bidi` | **`morphorm` 0.9** (1-pass stretch layout) | ECS sparse-sets + `vizia_reactive` signals | CSS stylesheet animations (`vizia_style`) | Wired (`accesskit` 0.24), crashes on edit signal | Win, Mac, Linux (`winit` + **`baseview`** audio plugins) | CSS stylesheet reload at runtime | ~35–50s (prebuilt Skia) / ~18–28 MB |
| **Cushy / Kludgine** (`0.4` / `0.11`) | `kludgine` on `wgpu` 24 (`lyon_tessellation`) | `cosmic-text` 0.12 | Custom `Widget` constraint layout (`figures` units) | Reactive `Dynamic<T>` values & bindings | `Dynamic` value transitions (`easing-function`) | **No** | Win, Mac, Linux, WASM | None | ~35–45s / ~9–13 MB (est.) |

---

## 5. What Rune Should Steal vs. What Rune Should Avoid

### What to Steal
1. **Steal GPUI & Makepad's "UI Uber-Shader + SDF Instanced Quad" primary renderer, NOT general path tessellation for Chrome.**
   - 98% of UI frames consist of rounded rectangles with borders, gradients, inner/outer analytic shadows, clip masks, textured sprites, and glyph runs (`zed/crates/gpui/src/scene.rs`). Rendering these as instanced SDF quads in a single uber-shader pass gives 120–240 Hz performance, zero CPU tessellation cost during spring animations, and effortless shader effects (backdrop blur, noise, specular borders, liquid morphing).
2. **Steal Makepad's `--remote` Agent Protocol and `/tweak` Source Write-Back (`makepad/platform/src/remote.rs`, `makepad/docs/agents/tweaker.md`).**
   - Every Rune application should compile with an optional embedded local agent/inspector server (like Makepad's `--remote` and Slint's `SLINT_MCP_PORT`) that supports:
     - Headless off-screen window rendering (`RUNE_HEADLESS=1`, never waking the user's display).
     - Semantic + geometric snapshot (`GET /snap`) with monotonic interaction sequence guards (`if_user_seq`).
     - Multi-frame motion capture (`GET /gseq?frames=N&interval_ms=16`) and per-frame motion trace export (`frame_trace.rs`) so agents can verify spring curves and catch 1-frame layout pops deterministically.
     - Live knob/style tweaking (`#[knob]` / `script_mod!`) that writes tuned constants back to the exact `file!():line!():column!()` in `.rs` source.
3. **Steal `egui_kittest` + `gpui-base`'s AccessKit-driven headless testing and AI Recipes (`egui/crates/kittest`, `gpui-component/examples/ai_recipes/README.md`).**
   - Wiring `accesskit` into unstyled behavioral primitives from day one gives both real OS screen-reader accessibility and a zero-flake semantic query tree (`get_by_role`, `get_by_label`) for automated tests. Pair this with `gpui-kit`-style `check-ai` canonical consumer recipes.
4. **Steal Makepad's GPU Slug text (`makepad/draw/src/text/slug_atlas.rs`) for scale/transform-animated typography, paired with a hinted raster atlas (`skrifa` + `vello_cpu`/`glifo`) for body text.**
   - Traditional bitmap glyph atlases shimmer or blur when text scales continuously during a spring transition, zoomable canvas pan, or 3D card tilt. Using hinted raster atlases for static 1x body text and Slug GPU curve evaluation (or high-res SDF) for transformed/large display text solves both crispness and kinetic typography.
5. **Steal Floem's `Copy` Arena Signals (`RwSignal<T>`) + Xilem/Masonry's `measure` / `layout` / `compose` separation (`masonry_core/src/core/widget.rs:121`).**
   - Separating `layout` (expensive Taffy box calculation) from `compose` (cheap transform/scroll/spring-offset matrix update) means spring animations and scroll physics never trigger layout recalculation unless box dimensions actually depend on them.
6. **Steal `Subsecond` integration (`zed/crates/gpui/Cargo.toml:55`) + zero-recompile style/knob hot reload (`makepad/platform/src/live_reload.rs`).**
   - Combine zero-recompile file watching for literal style/spring/shader blocks with `subsecond` binary jump-table patching for structural Rust code changes.

### What to Avoid
1. **Avoid C++ FFI rendering dependencies (`skia-safe`, Qt, GTK, WebView).**
   - `skia-safe` (used by Freya and Vizia) bloats binary size by 15–25 MB, complicates cross-compilation to iOS/Android/WASM, and hides the GPU command encoder so you cannot seamlessly interleave custom `wgpu` shaders or 3D layers with UI widgets.
2. **Avoid building a custom DSL that replaces Rust for control flow and business logic (`.slint`, legacy Makepad `live_design!`, Ribir's `@ { pipe!(...) }` macros).**
   - Custom DSLs break standard `rust-analyzer` refactoring, split state across two languages, and confuse coding agents when syntax drifts from training data (as seen in the Aug 2026 survey on Ribir and Makepad). Keep view trees in normal Rust syntax; use lightweight macros or `#[knob]` attributes only for live-reloadable literals/shaders whose values can be parsed out of `.rs` files without recompiling.
3. **Avoid Compute-only Vello (`vello` 0.8) as the primary UI renderer.**
   - Even Linebender moved compute `vello` to `research/vello_research/` (`vello/ARCHITECTURE.md:27-31`) because compute pipelines struggle on mobile tile GPUs, WebGL2, and memory-constrained devices.
4. **Avoid full HTML/CSS engines (`stylo` in Blitz) for native app UI.**
   - Pulling in Servo's `stylo` adds hundreds of crates and browser layout baggage without giving you interruptible springs, FLIP layout morphing, or custom GPU materials. `taffy` alone gives 100% of the Flexbox/Grid ergonomics needed at a tiny fraction of the compile cost.
5. **Avoid immediate-mode one-pass layout (`egui`, `ply`).**
   - Immediate-mode layout cannot size a parent container to dynamic children without either a two-pass measure or a one-frame visual pop—fatal for Kit's motion standards.

---

## 6. Concrete Crate Dependency vs. Custom-Build Recommendation for Rune

### Crates Rune Should Depend On Directly
| Layer | Recommended Crate(s) | Version (Oct 2026) | Why Depend Instead of Build |
| :--- | :--- | :--- | :--- |
| **2D Geometry & Curves** | `kurbo` | `0.13.1` | `#![forbid(unsafe_code)]`, `no_std`, shared by `skrifa`, `parley`, `vello_common`, and `accesskit`. Zero reason to reinvent `Point`, `Rect`, `RoundedRect`, `Affine`, or `BezPath`. |
| **Color & Brush Types** | `color`, `peniko` | `0.3.3`, `0.6.1` | First-class CSS Color 4 (`Oklch`, `Oklab`, `DisplayP3`, `Srgb`) perceptual color interpolation and shared `Brush`/`Gradient`/`FontData` handles. |
| **Box Layout (Flexbox & Grid)** | `taffy` | `0.15.0` | Battle-tested in Zed, Bevy, Dioxus, and Floem; minimal dependencies (`slotmap`, `smallvec`, `grid`, `flexbox`, `block_layout`); trait-based API lets Rune run layout directly over its own retained node arena. |
| **Font Parsing, Hinting & Shaping** | `parley` (or `parley_engine` + `fontique` + `harfrust` + `skrifa` + `read-fonts`) | `parley` `0.12.0`, `skrifa` `0.49`, `harfrust` `0.14` | Google Fonts (`skrifa`/`harfrust`) + Linebender (`fontique`/`parley`) is the undisputed pure-Rust standard in 2026. Handles variable fonts, autohinting, OpenType shaping, bidi, UAX#14/CJK line breaking, and cursor hit-testing. |
| **Vector / Emoji Raster Escape Hatch** | `vello_cpu` (+ optional `vello_gpu` / `glifo`) | `0.3.0` (`glifo` `0.4.0`) | Use `vello_cpu` (just like `egui` 0.37 does in `epaint`) for color emoji (`COLR`), complex SVG icons, and CPU snapshot testing, and optionally `vello_gpu` for complex canvas vector layers. |
| **Accessibility** | `accesskit` (+ platform adapters) | `0.25.1` | Non-negotiable for macOS VoiceOver, Windows UIA, Linux AT-SPI, iOS, and Android, and doubles as the semantic backbone for headless UI testing and agent introspection. |
| **Input Event Vocabulary** | `ui-events` | `0.3.0` | W3C-aligned `PointerEvent`, `KeyboardEvent`, and modifier state types (`kurbo`-integrated), already shared by Masonry and Floem. |
| **Spatial Indexing (Optional)** | `understory_box_tree` / `etagere` | `0.1` / `0.2.8` | Proven R-tree / bounding-box hit-testing (`understory`) and shelf texture atlas packing (`etagere`, used by GPUI, Iced, and Kludgine). |
| **Hot-Patching Hook (Optional)** | `subsecond` | `0.7.10` | Proven in Dioxus, GPUI, and Freya for sub-second symbol hot-patching of Rust view functions. |

### Subsystems Rune Should Build Itself (and Why)
1. **Retained Reactivity + Explicit 4-Phase Frame Pipeline (`Update → Measure/Layout → Animate/Compose → Paint`):**
   - *Why build:* None of Xilem, Iced, Floem, or GPUI treat **FLIP layout transitions** (automatically animating a node's bounds with a spring when Taffy moves it) and **interruptible velocity-preserving springs** as first-class steps between `Layout` and `Compose`. Rune needs a custom retained node graph where Taffy computes target layout boxes and the `Compose` phase applies active spring/gesture deltas before generating GPU instances.
2. **Custom `wgpu` UI Uber-Shader + Analytic Material/Shadow/SDF Pass (+ GPU Slug/SDF Text):**
   - *Why build:* General 2D vector renderers (`vello_gpu`, `femtovg`, `tiny-skia`) do not natively expose per-quad analytic squircle corners, multi-layered elevation shadows, backdrop blur/refraction, or custom WGSL material shaders per widget. Building a compact ~2,500-LOC `wgpu` instanced primitive compositor (drawing on GPUI's `Scene` and Makepad's `DrawQuad` + `SlugAtlas`) gives Rune complete control over visual craft and 120Hz frame pacing.
3. **First-Class Multi-Sensory & Motion Engine (Springs, Gestures, Procedural Audio/Haptics):**
   - *Why build:* No existing Rust UI framework links pointer velocity, spring physics, and sub-millisecond procedural UI audio/tactile feedback into a unified interaction primitive.
4. **Built-in Live Tuning (`knobs`) + Headless Agent Remote Protocol (`/snap`, `/gseq`, `/tweak`, Motion Trace):**
   - *Why build:* Makepad proved the architecture (`remote.rs`, `tweaker.rs`, `frame_trace.rs`, `live_reload.rs`), and Rune should build a clean, pure-Rust version natively integrated with Kit's `knobs` and Scrawl's motion-trace format from day one—allowing both humans and coding agents to tune springs/styles live without recompiling and verify motion traces headlessly.

### Rejected Alternatives (and Why)
- **Forking or building directly on `Xilem` / `Masonry`:** Masonry's `Widget` trait and `imaging` abstraction are well-engineered, but Xilem's deeply nested static tuple view types create hostile compiler diagnostics for humans and agents, and Masonry's rendering model assumes a traditional 2D vector painter (`vello`) rather than a shader-first instanced compositor with FLIP layout springs and backdrop materials.
- **Building on `GPUI` or `gpui-kit`:** GPUI's macOS backend is tightly coupled to Zed's internal workspace crates (`gpui-pre` snapshots require exact `=0.3.9` pins per `gpui-component/Cargo.toml:62-71`), lacks iOS/Android support, and relies on platform-specific graphics abstractions (`blade-graphics` / raw `objc2-metal`) rather than a clean cross-platform `wgpu` pipeline.
- **Building on `Makepad`:** Makepad's custom `script_mod!` VM, custom turtle layout (no Flexbox/CSS Grid), and lack of `accesskit` or standard Rust ecosystem types (`kurbo`/`taffy`/`parley`) make it an island ecosystem rather than a composable Rust library.
- **Building on `Iced` or `Slint`:** Iced's Elm Architecture forces all component animation/gesture state through top-level messages (or opaque internal widget state) and lacks `accesskit`; Slint's `.slint` DSL and GPLv3/commercial licensing are incompatible with a permissively licensed Rust-native library.

---

## 7. Open Questions for Rune Architecture

1. **`parley` High-Level API vs. `parley_engine` Direct Integration:** Should Rune use `parley::LayoutContext` directly, or depend on the newer lower-level `parley_engine` + `fontique` + `skrifa` crates (introduced in Parley 0.12, `parley/parley/Cargo.toml:37-39`) to avoid intermediate style buffer allocations during high-frequency text layout?
2. **Text Rendering Dual-Path Threshold:** At what pixel size or transform state should Rune switch between a hinted `skrifa` + `vello_cpu` raster glyph atlas (crispest at 10–18px static UI text) and a Makepad-style GPU Slug / MSDF curve evaluator (artifact-free during spring scale/rotation/3D transforms)?
3. **Windowing Layer (`winit` 0.30/0.31 vs. Custom Platform Shells on macOS/iOS):** Is `winit` sufficient for Rune's frame-pacing (`CADisplayLink` / `CAMetalDisplayLink` 120Hz ProMotion sync, trackpad gesture phases, and transparent/vibrancy window chrome), or should Rune follow GPUI and Makepad in owning a thin native `objc2` macOS/iOS window shell while keeping `winit` as a fallback for Linux/Windows/Web?
4. **Zero-Recompile Tuning Syntax vs. `Subsecond`:** Should Rune parse `#[knob]` / style macro literals directly from `.rs` source files on disk (like `makepad_platform::live_reload::extract_script_mod_from_file` in `makepad/platform/src/live_reload.rs:144`) for sub-5ms zero-recompile parameter reloads, and pair that with optional `subsecond` binary patching when structural Rust code changes?

# 05 — Text, IME, Accessibility, and Layout

*Research date: 2026-10-21*

## Executive Summary

- **Pure-Rust text shaping and font parsing are production-ready in 2026 (`harfrust` + `skrifa` + `fontique`).** Google Fonts' `harfrust` (`v0.14.0`, Oct 2026) has reached HarfBuzz 14.5.1 parity, uses zero-copy `fontations`/`read-fonts` tables directly (`ShaperData` + `ShaperInstance`), and runs 10–15% faster than `rustybuzz` while supporting variable-font variation coordinates, AAT (`morx`/`kerx`/`trak`), and shape plans. Pair it with `skrifa` (`v0.42.0`) for TrueType/CFF hinting + COLRv1 and `fontique` (`v0.8.0`) for font enumeration and script/locale fallback.
- **Keep OS font metrics on Apple (`CTFont`) while using `harfrust` + `parley` for cross-platform shaping and line breaking.** On macOS and iOS, system UI fonts (`SF Pro`, `SF Compact`, `.AppleSystemUIFont`) rely on optical-size (`opsz`) tracking tables (`trak`) and OS-specific ascent/descent rules. Use `parley` (`v0.12.0`, built on `parley_engine` + `fontique` + `harfrust` + `skrifa` + `icu4x`) for bidirectional paragraph layout, mixed inline runs, and cursor/selection geometry, augmented with GPUI's two-tier unwrapped-vs-wrapped `LineLayoutCache` (`zed/crates/gpui/src/text_system/line_layout.rs:523-698`) so container resizes only recompute wrap boundaries without re-shaping.
- **Luminance-quantized glyph dilation solves light-on-dark thin text on Apple displays without subpixel LCD fringes.** Subpixel LCD rendering is incompatible with transparent layers and GPU transforms. Instead, steal Kit's GPUI fork (`kitlangton/gpui`) luminance glyph dilation (`kitlangton/gpui/src/platform/mac/text_system.rs:42-81, 470-564`, commit `2582af8ad0`): quantize text color relative luminance ($0.2126R + 0.7152G + 0.0722B$) into 5 cache buckets (`0..=4`), pad `raster_bounds` by `1px`, and rasterize with `set_should_smooth_fonts(true)` at that gray fill level when `AppleFontSmoothing != 0`, paired with linear-space (`sRGB` framebuffer) alpha blending.
- **Never re-rasterize hinted glyphs on every frame of a spring transform.** Vello's `glifo` (`vello/glifo/src/glyph.rs:1702-1731`) folds uniform transform scale into `draw_font_size` and hashes `size.to_bits()`, thrashing the glyph atlas and causing stem-hinting pop on every frame of a 120 Hz spring animation. During active transforms, lock glyphs to unhinted/linear-metrics outlines rasterized at the target (or octave-stepped) scale and sample bilinearly, or promote large/hero animated text (`> 28px` or explicitly tagged) to Rive-style per-glyph vector paths (`rive-runtime/src/text/text.cpp:544-592`).
- **Use a unified `SumTree`-backed rope + display map for all text editing, from single-line inputs to multi-megabyte buffers.** Zed's concurrent B+ tree `SumTree` (`zed/crates/sum_tree/src/sum_tree.rs`, `TREE_BASE = 6`) maintains UTF-8 bytes, UTF-16 code units (required by Apple/Windows/Web IME), UTF-32 chars, line breaks, and soft-wrap display rows in $O(\log N)$ time with zero-copy `Arc` snapshots.
- **Implement native platform IME protocols directly on desktop and iOS, with a sliding-window `<textarea>` + `EditContext` hybrid on Web.** Direct `NSTextInputClient` (macOS), `UITextInput` + `UIEditMenuInteraction` subclass (iOS, stealing GPUI's deferred `reconcileTextInput` in `gpui_ios/src/ios/text_input.rs:197-318`), `ITfThreadMgr`/`ImmGetCompositionStringW` (Windows), and `zwp_text_input_v3` (Wayland) avoid hidden-native-widget desynchronization while preserving marked text, candidate window anchoring, Dictation, and Apple Writing Tools.
- **AccessKit (`v0.24` / 2026) is the clear winner for cross-platform accessibility and doubles as Rune's agent-inspector tree.** AccessKit now covers macOS (`accesskit_macos`), Windows (`accesskit_windows`), Linux AT-SPI (`accesskit_unix`), Android (`accesskit_android`), and iOS (`accesskit_ios`, `accesskit/adapters/ios/src/node.rs`). Building a single retained `accesskit::TreeUpdate` per frame gives VoiceOver/TalkBack/NVDA and AI testing/automation agents (`cua-driver`) one canonical semantic tree with roles, bounds, values, and deterministic `ActionRequest` dispatch.
- **Adopt a two-path layout architecture: `Taffy` (`v0.9.x`) for general Flexbox/CSS Grid UI + a `SumTree`-indexed height cache for virtualized lists + first-class layout interpolation hooks.** `Taffy` is heavily optimized (1.6 ms for 1,000 nodes vs Yoga's 1.5 ms on M1 Pro; 59 ms vs 84 ms on 100k Twitter feed nodes) and matches CSS mental models that LLMs write fluently, whereas `Pretext` + `SumTree` prefix-sum heights eliminate the 106.4 ms SwiftUI `sizeThatFits` bottleneck (a private iOS layout benchmark), dropping 500-paragraph resize passes to 2.6 ms.

---

## 1. Text Shaping & Fonts

### 1.1 `harfrust` vs `rustybuzz` vs OS Shapers (CoreText / DirectWrite)

| Dimension | `harfrust` (`v0.14.0`, Oct 2026) | `rustybuzz` (`v0.20.x`) | CoreText (`CTLine`/`CTTypesetter`) | DirectWrite (`IDWriteTextAnalyzer`) |
|---|---|---|---|---|
| **HarfBuzz parity** | HarfBuzz 14.5.1 (`harfrust/CHANGELOG.md:9`) | Trails HarfBuzz ~6–12 months | Apple AAT + OpenType engine | Microsoft OpenType engine |
| **Font table backend** | Native `read-fonts` (`fontations`), zero-copy via `ShaperData` | Bundled `ttf-parser` | `CGFont` / `CTFont` | `IDWriteFontFace` |
| **Performance** | ~10–15% faster than `rustybuzz`; caches reusable `ShaperData` & `ShaperInstance` | Fast CPU, re-parses tables per face | Fast, but Objective-C/CF allocation overhead per run | Fast, COM call overhead |
| **Apple `trak` / AAT (`morx`, `kerx`)** | Supported (`harfrust` ports HarfBuzz AAT tables) | Partial AAT support | Reference implementation (including optical size `opsz` tracking) | No AAT `morx` |
| **Cross-platform determinism** | 100% identical glyph IDs & advances across all OSes | 100% identical across all OSes | Apple-only | Windows-only |

**Source findings:**
- **`harfrust` has superseded `rustybuzz` across the Linebender and Servo ecosystems.** Originally started by Simon Cozens and Behdad Esfahbod (`harfrust/CHANGELOG.md:201-215`), `harfrust` is maintained directly under the HarfBuzz organization with Behdad Esfahbod and Chad Brokaw actively porting HarfBuzz algorithmic improvements (`harfrust/CHANGELOG.md:7-55`).
- **Pre-parsed table caching (`ShaperData`) and variation instance caching (`ShaperInstance`):** In `harfrust/src/hb/ot_shaper.rs` and `parley/parley/src/shape.rs:146-172`, `ShaperData::new(&font_ref)` pre-validates font tables once, while `ShaperInstance::from_variations(&font_ref, coords)` caches variation-axis coordinate lookups and feature stores. Shaping a run reuses a thread-local `harfrust::UnicodeBuffer` (`parley/parley/src/shape.rs:424-449`) with zero per-run heap allocations.
- **CoreText vs `harfrust` tradeoff on Apple:** GPUI currently calls `CTTypesetterCreateWithAttributedString` and `CTLineGetGlyphRuns` directly on macOS and iOS (`zed/crates/gpui_apple/src/text_system.rs:477-568`). While CoreText automatically applies Apple's undocumented optical tracking for `.AppleSystemUIFont` (`SF Pro`), allocating `CFAttributedString` and `CFDictionary` per text run costs measurable CPU time: in a private iOS layout benchmark, cold `CTFramesetter` measurement across 500 chat paragraphs takes **17.8 ms** (35.5 µs/msg), and SwiftUI `Text.sizeThatFits` takes **106.4 ms** (212.9 µs/msg).

### 1.2 Font Loading, Parsing, Hinting, and Fallback (`fontations`/`skrifa`, `swash`, `fontique`)

- **`fontations` (`read-fonts` + `skrifa` v0.42.0) vs `swash`:**
  - Google's `fontations` workspace (`skrifa` + `read-fonts`) is now the standard Rust font stack used by Chrome (replacing FreeType in Chromium), Parley, Vello (`glifo`), and Bevy.
  - `skrifa` (`fontations/skrifa/src/outline/mod.rs:1-148`) provides a complete TrueType bytecode interpreter, CFF/CFF2 hint engine, and a FreeType-derived Latin/CJK **autohinter** (`HintingOptions { engine: Engine::AutoFallback, target: Target::Smooth { mode: SmoothMode::Lcd | Normal, symmetric_rendering: true, preserve_linear_metrics: false } }`).
  - `swash` is effectively unmaintained upstream; Parley removed `swash` entirely in `v0.4.0` (`parley/CHANGELOG.md:191-215`) in favor of `harfrust` + `skrifa`.
- **`fontique` (`v0.8.0`) for system font discovery and fallback:**
  - `fontique` (maintained inside `parley/fontique`) handles cross-platform system font collection (`CoreText` on macOS/iOS, `DirectWrite` on Windows, `fontconfig` on Linux, `Android` XML/NDK font configuration) plus app-registered memory fonts (`register_fonts`).
  - During shaping (`parley/parley/src/shape.rs:212-345`), `fontique` partitions text by `icu_properties::CodePointMapData::<Script>`, queries the family list first, and only falls back to `font_ctx.collection.fallback_families(FallbackKey::new(script, locale))` for `.notdef` (`glyph_id == 0`) sub-clusters, preserving variable font variation coordinates across fallback faces.

### 1.3 Variable Fonts, OpenType Features, and Emoji

- **Variable fonts (`wght`, `opsz`, `wdth`, `slnt`, custom axes):** `skrifa::MetadataProvider::axes()` resolves user axis tags into normalized `F2Dot14` design-space coordinates shared identically between `harfrust::ShaperInstance` (for `GPOS`/`GSUB` feature variations and `HVAR`/`MVAR` advances) and `skrifa::outline::OutlineGlyphCollection` (for `gvar`/`CFF2` interpolated glyph outlines).
- **OpenType features:** `harfrust::Feature` supports arbitrary 4-byte OpenType tags (`liga`, `calt`, `dlig`, `tnum`, `onum`, `zero`, `ss01`–`ss20`, `cv01`–`cv99`, `kern`) scoped by byte range (`parley/parley/src/shape.rs:178-206`).
- **Emoji (`COLRv0`/`COLRv1`, `sbix`, `CBDT`/`CBLC`):**
  - Apple Color Emoji uses **`sbix`** (embedded PNG strips per strike ppem).
  - Google Noto Color Emoji (Android/Linux/Web) uses **`COLRv1`** (vector paint graphs with linear/radial/sweep gradients, blend modes, and `VARC` variable composites) or **`CBDT`/`CBLC`** (embedded PNG bitmaps).
  - Windows Segoe UI Emoji uses **`COLRv0`/`COLRv1`**.
  - Vello's `glifo` (`vello/glifo/src/glyph.rs:289-424` and `vello/glifo/src/colr.rs`) inspects glyph tables in priority order: `skrifa::color::ColorGlyphCollection` (`COLRv0`/`COLRv1`) -> `sbix`/`CBDT` bitmap strikes (`image::load_from_memory_with_format(..., ImageFormat::Png)`) -> standard monochrome outline.

---

## 2. Text Layout

### 2.1 `parley` vs `cosmic-text` vs GPUI's Text System

| Engine | Shaping / Bidi / Line-Break Stack | Strengths | Weaknesses |
|---|---|---|---|
| **`parley` (`v0.12.0`, Oct 2026)** | `harfrust` + `skrifa` + `fontique` + `icu4x` (`icu_segmenter`, `parley_bidi`) | Clean split between `parley_engine` (core layout) and `fontique`; supports inline boxes (`InlineBox`), text-indent (`IndentOptions`), cluster/cursor geometry, `Alignment::Justify`, zero allocations on warm `LayoutContext`. | Does not include a built-in cross-frame unwrapped-vs-wrapped line cache out of the box (expects host UI to cache `Layout`). |
| **`cosmic-text`** | `rustybuzz` + `swash` + `fontdb` + `unicode-bidi` | Battle-tested in System76 COSMIC; includes a basic shape cache (`ShapeLine` vs `LayoutLine`). | Stuck on legacy `swash`/`rustybuzz`; weaker inline-box and CSS-aligned typography model than `parley`. |
| **GPUI (`gpui` + `gpui_apple` / `gpui_wgpu`)** | Platform `LineWrapper` + `layout_line` (`CoreText` on Apple, `DirectWrite` on Windows, `cosmic-text` on Linux/Web) | Best-in-class two-tier `LineLayoutCache` (`line_layout.rs:523-698`) with frame-epoch eviction and bulk `reuse_layouts`. | Fragmented per-OS shaping leads to subtle cross-platform line-break and fallback differences; manual boundary wrapper (`LineWrapper::wrap_line`) rather than full UAX #14. |

### 2.2 Bidirectional Text, Mixed Inline Runs, and Line Breaking

- **Bidirectional layout (UAX #9):** `parley` uses `parley_bidi` (`parley/CHANGELOG.md:27`) to resolve embedding levels per paragraph and reorders visual runs per wrapped line *after* line breaking (as required by UAX #9 Section 3.4), ensuring hit-testing (`Cursor::from_point`) and selection rectangles (`Selection::geometry_with`) remain accurate across mixed RTL/LTR/numeric runs.
- **Line breaking (UAX #14) & hyphenation:** `parley` uses `icu_segmenter::LineSegmenter` for locale-aware UAX #14 line break opportunities (handling CJK kinsoku shori, Thai dictionary breaking, and emergency `WordBreakStrength::BreakAll` / `KeepAll`).
- **Inline boxes (pills, badges, icons, inline code):** `parley::LayoutBuilder::push_inline_box(InlineBox { id, index, width, height })` (`parley/parley/src/layout.rs`) participates directly in line breaking, baseline alignment, and cursor navigation as an atomic cluster—essential for chat UIs with `@mention` pills, file chips, and inline status badges.

### 2.3 Truncation, Ellipsis, and SwiftUI/TextKit 2 Soft Fade Masks

- **Why ellipsis (`…`) layout is expensive when naively implemented:** Shaping an ellipsis string and re-shaping the truncated line tail on every resize requires mutating the glyph stream. GPUI (`zed/crates/gpui/src/text_system/line_wrapper.rs`) measures the ellipsis width once per font/size and truncates at cluster boundaries, while SwiftUI/TextKit 2 (`NSTextLayoutManager`) supports both glyph-replacement ellipsis and visual fade masks.
- **Soft fade masks instead of `…`:** In modern streaming chat and card UIs (and animated label containers), hard `…` glyph substitution causes jarring character popping as width animates or tokens stream in. Rendering the full shaped line clipped to the container rect with a **horizontal alpha gradient mask** over the trailing `16–24px` (in linear alpha space, direction-aware for LTR/RTL) requires **zero re-shaping during width animations**, never pops glyphs, and signals overflow cleanly. Rune should support both `Overflow::Ellipsis` (cached ellipsis cluster substitution on `max_lines`) and `Overflow::FadeMask { tail_px: f32 }` (shader-evaluated edge ramp in the text quad primitive).

### 2.4 Caching Shaped Lines Across Frames (Zed's `LineLayoutCache` & `Pretext`)

- **Zed's two-tier `LineLayoutCache` (`zed/crates/gpui/src/text_system/line_layout.rs:523-698`):**
  1. **Unwrapped tier (`lines`):** Keyed by `(SharedString, font_size: Pixels, SmallVec<[FontRun; 1]>, force_width: Option<Pixels>)`. Stores `Arc<LineLayout>` containing shaped glyph positions and UTF-8 indices.
  2. **Wrapped tier (`wrapped_lines`):** Keyed by `(SharedString, font_size, runs, wrap_width: Option<Pixels>)`. When `wrap_width` changes (e.g., dragging a split pane or rotating a phone), `layout_wrapped_line` (`line_layout.rs:639-644`) hits the cached `unwrapped_layout` and only runs `compute_wrap_boundaries` over pre-shaped advances.
  3. **Two-frame epoch eviction + bulk reuse:** `LineLayoutCache` holds `current_frame` and `previous_frame` hash maps (`FxHashMap`). Views that do not re-layout call `reuse_layouts(range: Range<LineLayoutIndex>)` (`line_layout.rs:523-566`), which moves used entries from `previous_frame` to `current_frame` by index slice without even hashing keys. At `finish_frame()` (`line_layout.rs:586-600`), the maps swap and stale entries from `previous_frame` are dropped.
- **`Pretext` validation (a private iOS layout benchmark):**
  - Across 500 mixed markdown/code chat messages on iOS, re-measuring height on viewport width change took **106.44 ms** with SwiftUI `sizeThatFits`, **17.75 ms** with cold `CTFramesetter`, **2.60 ms** with cached `CTFramesetter` frames, and **0.23 ms** with Pretext's pre-segmented width table (a **462x** speedup over SwiftUI `sizeThatFits`, though Pretext's naive whitespace Collapse had a 29% height mismatch against TextKit 2's hyphenation/kerning). Using **exact glyph-run advances** from the unwrapped `LineLayout` cache gives Pretext-class resize speed (**< 1 ms for 500 paragraphs**) with **0% height drift**.

---

## 3. Glyph Rendering

### 3.1 Glyph Atlas (Grayscale vs Subpixel AA) vs GPU Vector Outlines

| Technique | Crispness at 10–16pt (2x Retina / 1x LoDPI) | Memory & Bandwidth | Arbitrary 2D/3D / Spring Scale Cost | Verdict for Rune |
|---|---|---|---|---|
| **Grayscale Mask Atlas + Luminance Dilation (`kitlangton/gpui`)** | Excellent on 2x Retina and 1x with `skrifa` hinting + 5-level luminance dilation | Low (`R8Unorm` 1 byte/px; `RGBA8` 4 bytes/px for emoji) | Fast at fixed scale; thrashes atlas if re-rasterized per scale frame | **Primary UI text path** |
| **Subpixel LCD Atlas (`RGB` ClearType/FreeType LCD)** | Sharpest on 1x non-Retina horizontal RGB panels | 3–4x atlas memory; requires dual-source blending (`blend_src1`) | Breaks under rotation, scale, non-RGB subpixel layouts (OLED), or unpremultiplied compositing | **Avoid** (dead on macOS since Mojave, broken on OLED/mobile/rotated layers) |
| **SDF / MSDF Atlas** | Poor at 11–14pt UI sizes (rounded corners, stem weight distortion on CJK/kanji) | Medium (offline or slow generator per glyph) | Smooth infinite zoom | **Avoid for UI text** |
| **Slug (Lengyel GPU Bezier evaluation)** | Exact unhinted outlines at any zoom | Compact curve/band textures | Zero atlas thrashing under zoom/3D; higher fragment shader ALU per covered pixel; no stem hinting at 11–13pt | Optional hero/3D/canvas text path (patent entered public domain / open-sourced in 2026, or via `glifo`/Vello compute) |
| **Vello / `glifo` CPU-cached Atlas (`vello/glifo/src/glyph.rs`)** | Good (`skrifa` hinted outlines rasterized via `vello_common::kurbo` strip generator into shelf-packed `AtlasPage`) | Low (`PlainMask` + `Color` atlases) | Currently absorbs transform `t_d` into `draw_font_size` (`glyph.rs:1702-1731`), thrashing cache on scale animations | Steal `glifo`'s `skrifa` + `COLRv1` + `sbix` pipeline, **fix** its transform-scale key behavior |
| **Rive Direct Vector Path (`rive-runtime/src/text/text.cpp:544-592`)** | Exact unhinted vector curves tessellated/rasterized by GPU path renderer | Zero glyph atlas | Native per-glyph `TextModifierGroup` affine transforms + variable-font axis keyframing at 120 Hz | **Use for hero / animated / path-warped text (`TextRenderMode::Vector`)** |

### 3.2 macOS Font Smoothing, Stem Darkening, and Kit's GPUI fork (`kitlangton/gpui`) Luminance Glyph Dilation

**Why text in naive GPU renderers looks "too thin" or "washed out" on macOS:**
1. **Gamma blending error:** Blending a linear coverage mask $\alpha$ directly in non-linear sRGB space computes $C_{\text{out}} = \alpha C_{\text{fg}} + (1 - \alpha) C_{\text{bg}}$ in sRGB, which darkens mid-tones and makes light text on a dark background lose ~20–30% of its perceived stroke weight compared to dark text on a light background.
2. **CoreGraphics `AppleFontSmoothing` behavior in alpha-only contexts:** When rasterizing glyphs into a single-channel `kCGImageAlphaOnly` bitmap (`gpui_apple/src/text_system.rs:429-434`), calling `CGContextSetShouldSmoothFonts(cx, true)` tells CoreGraphics to apply **stem darkening (synthetic bolding/dilation)**—but CoreGraphics modulates the dilation strength by the *luminance of the active fill color* in the `CGContext`! If a GPU renderer rasterizes glyphs once into an alpha-only atlas with a fixed white (`1.0`) or black (`0.0`) fill color, either dark-on-light text looks overly heavy or light-on-dark text looks spindly.

**How Kit's GPUI fork (`kitlangton/gpui`) Luminance Glyph Dilation works (`kitlangton/gpui/src/platform/mac/text_system.rs`, commit `2582af8ad0`):**
- **5-bucket relative luminance quantization (`text_system.rs:42-69`):**
  ```rust
  pub(crate) const GLYPH_DILATION_LEVELS: u8 = 5;

  pub(crate) fn glyph_dilation_for_color(color: Hsla) -> u8 {
      if !font_smoothing_enabled() {
          return 0;
      }
      let rgba = color.to_rgb();
      let luminance = 0.2126 * rgba.r + 0.7152 * rgba.g + 0.0722 * rgba.b;
      (luminance * (GLYPH_DILATION_LEVELS - 1) as f32)
          .round()
          .clamp(0.0, (GLYPH_DILATION_LEVELS - 1) as f32) as u8
  }
  ```
- **System default detection (`text_system.rs:71-81`):** Reads macOS `NSUserDefaults::standardUserDefaults().integerForKey("AppleFontSmoothing")` once via `OnceLock`; enabled when `!= 0` (matching macOS default `2`).
- **Cache key & bounds padding (`text_system.rs:470-499`):** `RenderGlyphParams` includes `dilation: u8` (`0..=4`). When `font_smoothing_enabled()` is true, `raster_bounds` adds `1px` padding on all four sides (`origin - (1, 1)`, `size + (2, 2)`) so dilated glyph stems never clip at the atlas tile boundary.
- **Rasterization (`text_system.rs:537-564`):** Inside `rasterize_glyph`, when `params.dilation > 0`:
  ```rust
  let luminance = params.dilation as f64 / (GLYPH_DILATION_LEVELS - 1) as f64;
  cx.set_should_smooth_fonts(true);
  cx.set_gray_fill_color(luminance, 1.0);
  ```
  This feeds CoreGraphics the exact 25%-stepped gray luminance (`0.0, 0.25, 0.50, 0.75, 1.0`), producing authentic macOS stem weight across both light and dark themes while capping atlas growth to at most the active foreground luminance buckets (typically 2 buckets per theme: primary text + muted text).
- **Cross-platform equivalent on `skrifa`/FreeType (`Windows`/`Linux`/`Android`):** When using `skrifa` + CPU/GPU outline rasterization off Apple, apply FreeType/Adobe CFF **stem darkening** in outline space (`darken_x = 0.025 * ppem`, modulated by foreground-vs-background luminance contrast) or a linear-space coverage gamma adjustment ($\alpha' = \alpha^{1.0 / \gamma_{\text{contrast}}}$) in the glyph fragment shader.

### 3.3 Why SDF / MSDF Fails for Small UI Text

- **Single-channel SDF** cannot preserve sharp corners (a single distance value rounds every corner when reconstructed at scales different from the raster grid), making `11–14pt` sans-serif stems (`I`, `l`, `m`, `n`) look mushy or blob-like.
- **Multi-channel SDF (MSDF)** preserves sharp 2-edge corners by encoding signed distances to two edge segments in R/G/B channels and taking `median(r, g, b)`. However:
  1. Complex glyphs with >2 converging edges per texel (common in `11–14pt` CJK ideographs, `@`, `%`, `&`, and serif joins) suffer from **false edge artifacts (spikes and dropouts)** unless the MSDF texel resolution is high (`32x32` to `64x64` per glyph).
  2.Generating an MSDF on the CPU on demand for newly encountered CJK/Unicode glyphs is **10–30x slower** than scanline coverage rasterization.
  3. MSDF operates on unhinted outlines, losing pixel-grid stem alignment at `10–15pt` on 1x/2x displays.

### 3.4 Subpixel Positioning (`4` Horizontal × `1` Vertical Phases)

- Without subpixel positioning, snapping glyph origins to integer device pixels causes uneven inter-glyph kerning ("dancing letters" when slowly scrolling or adjusting tracking).
- Both GPUI (`zed/crates/gpui/src/text_system.rs`, `SUBPIXEL_VARIANTS_X = 4, SUBPIXEL_VARIANTS_Y = 1` on Retina) and Vello's `glifo` (`vello/glifo/src/glyph.rs:437-465`, `SubpixelAxis::Four` = `0.0, 0.25, 0.5, 0.75`) quantize the fractional device-pixel X coordinate into **4 horizontal subpixel phases** (`u8` `0..=3`) and **1 vertical phase** (since horizontal text baselines align vertically to rows).
- **Important optimization from `glifo/src/glyph.rs:444-465`:** Above a font size threshold (`OPTICAL_SIZE_THRESHOLD` ~ `24–32px` device pixels), subpixel phase quantization is reduced to `SubpixelAxis::Two` or `One` (integer pixel snapping), preventing atlas bloat on large headings where a `0.25px` shift is imperceptible.

### 3.5 Animated Text Under Spring Transforms (Why Text Jitters & How Apple / Figma / Rive Solve It)

**The bug in naive atlas renderers (including Vello `glifo`):**
- In `vello/glifo/src/glyph.rs:1702-1731` (`GlyphScaleProperties::new`), when a draw transform has uniform or axis-aligned scale (`t_a`, `t_d`), `glifo` folds `t_d` directly into `draw_font_size = font_size * t_d` and keys the glyph cache by `size: draw_font_size.to_bits()` (`u32`, `glyph.rs:480-523`).
- During a 120 Hz spring scale animation (e.g., `scale: 0.92 -> 1.02 -> 1.00`), `draw_font_size` takes 60+ distinct `f32` bit patterns in 500 ms. Worse, if hinting or integer advance rounding is enabled, stems jump between pixel columns ("swimming / jittering letters") and the glyph atlas thrashes on every frame.

**How production engines solve animated text:**
1. **Apple CoreAnimation / SwiftUI (`CATextLayer`, `.drawingGroup()`, `NSStringDrawingOptions`):**
   - For standard layer transforms (`scaleEffect`, modal spring entrance), CoreAnimation caches the text layer at its **target (`1.0x`) contentsScale** (or highest octave scale) and applies the spring transform in the GPU compositor with bilinear texture filtering + linear metric advances (`CTFontCreateWithFontDescriptor` with `kCTFontLinearMetricsAttribute` / subpixel positioning).
   - For SwiftUI `matchedGeometryEffect` / variable font transitions, Apple interpolates between two pre-rasterized scale snapshots or switches to unhinted subpixel-positioned outlines (`preserve_linear_metrics: true`).
2. **Figma (WebGL / WebGPU canvas renderer):**
   - Figma renders text via cached glyph textures while stationary at a discrete zoom level, and during continuous zoom/spring animations either samples the nearest higher power-of-two mip octave from the glyph atlas with linear metrics or renders vector glyph meshes on the GPU.
3. **Rive (`rive-runtime/src/text/text.cpp:544-592`, `text_modifier_group.cpp`):**
   - Rive never uses a bitmap glyph atlas for scene text. Every shaped glyph outline is extracted as an unhinted `RawPath` in normalized em-space (`rive-runtime/src/text/text.cpp:544-592`) and rendered directly by Rive's GPU vector renderer (PLS / atomic feathering) under per-glyph `Mat2D` transforms (`TextModifierGroup::transformGlyph` supporting per-character opacity, rotation, scale, position, and variable font axis keyframing at 120 Hz with zero atlas re-rasterization).

**Rune's hybrid rule for animated text:**
- **Static & scrolling UI text (`<= 28pt`, no active scale/rotation animation):** Hinted (`skrifa` `preserve_linear_metrics: false` on 1x, unhinted/light-hinted `preserve_linear_metrics: true` on $\ge 2\text{x}$ Retina) + 4x horizontal subpixel phase + 5-level luminance dilation in the glyph atlas.
- **Text inside an active scale/rotation spring transform:** Automatically switch the text primitive to **transform-invariant atlas sampling**: rasterize once at `ceil_octave(max_expected_scale) * font_size` with `preserve_linear_metrics: true` and fractional GPU quad positions (do **not** snap glyph quads to integer device pixels while transforming!), sampling with bilinear filtering + mild sharpening.
- **Hero / kinetic typography (`> 28pt`, or explicit `.textRenderMode(.vector)` / per-glyph `TextModifier`):** Bypass the bitmap atlas and emit unhinted `skrifa` outlines directly into Rune's GPU vector path pipeline (Vello/Slug-style curve evaluation), enabling Rive-class per-character spring choreography and variable-font weight/width animations with zero atlas traffic.

---

## 4. Text Editing (Single-Line Input through Rich Code/Prose Editor)

### 4.1 Selection, Caret, Bidirectional Cursor Movement, and Affinity

- **Selection representation:** Every selection is an ordered or directional pair `Selection { start: usize, end: usize, reversed: bool, goal_column: Option<f32> }` in UTF-8 byte offsets, accompanied by `Affinity::{Upstream, Downstream}` (`parley/parley/src/cursor.rs`).
- **Why `Affinity` is mandatory:** At a soft line wrap boundary, byte offset $k$ corresponds to **two distinct screen positions**: the end of line $i$ (`Affinity::Upstream`) and the start of line $i+1$ (`Affinity::Downstream`). Likewise, at an LTR/RTL bidi boundary, a single byte offset maps to two split-caret visual X positions. `parley::Cursor` encodes `(index: usize, affinity: Affinity)` directly and computes visual geometry via `Cursor::geometry(&layout, size)`.
- **Grapheme and word navigation (UAX #29):**
  - Left/Right arrow movement steps by **extended grapheme clusters** (`icu_segmenter::GraphemeClusterSegmenter` or `unicode-segmentation`), so multi-codepoint emoji (family ZWJ sequences `👨‍👩‍👧‍👦`, skin-tone modifiers, flag pairs) and combining marks move and delete (`Backspace` vs `Delete`) correctly.
  - Option+Left/Right (macOS) / Ctrl+Left/Right (Windows/Linux) and double-click word selection use UAX #29 word boundaries customized with character-class tables (`Alphanumeric`, `Punctuation`, `Whitespace`, plus configurable `word_characters` like `_` or `$` for code inputs).
  - Triple-click selects the logical paragraph (or visual line with modifier), dragging extends selection by word (double-click-drag) or line (triple-click-drag) by anchoring the initial word/line range (`gpui_ios/src/ios/text_input.rs` / `zed/crates/editor`).

### 4.2 Buffer Data Structures: Ropes vs Zed's `SumTree`

| Data Structure | Memory / Node | Concurrent Snapshot | Multi-Coordinate Indexing (UTF-8, UTF-16, Lines, Display Wrap) | Used By |
|---|---|---|---|---|
| **`String` / Gap Buffer** | Contiguous `Vec<u8>` | Requires full clone | $O(N)$ scan for UTF-16 or line numbers | Naive single-line inputs |
| **`ropey`** | B-tree of UTF-8 chunks | Clone-on-write (`Arc`) | UTF-8, chars, lines; UTF-16 conversion added but no custom summary dimensions | Helix, cosmic-text |
| **Zed `SumTree` + `Rope` (`zed/crates/sum_tree/src/sum_tree.rs`, `zed/crates/rope`)** | `Arc`-backed B+ tree (`TREE_BASE = 6`, up to 12 children/node) | $O(1)$ lock-free `Clone` | Arbitrary `Summary` trait + multiple simultaneous `Dimension` types (`usize` bytes, `Point`, `PointUtf16`, `OffsetUtf16`, `Chars`) | Zed (`Buffer`, `DisplayMap`, `WrapMap`, `FoldMap`, `BlockMap`) |

**Why Rune should steal Zed's `SumTree` + `Rope` pattern (`zed/crates/sum_tree/src/sum_tree.rs:1-140`):**
- Every node in `SumTree<T>` stores an aggregated `T::Summary`. For `Rope::Chunk`, `TextSummary` tracks `len: usize` (UTF-8), `len_utf16: OffsetUtf16`, `lines: Point` (row/col), `first_line_chars`, `last_line_chars`, and `longest_row`.
- Converting between **UTF-8 byte offsets** (used by Rust/`harfrust`/`parley`) and **UTF-16 code-unit offsets** (mandated by macOS `NSRange`, iOS `UITextPosition`, Windows `ImmGetCompositionStringW`, and Web `EditContext`/`HTMLTextAreaElement`) is a single $O(\log N)$ B+ tree traversal (`rope.offset_to_offset_utf16(byte_offset)`), eliminating the #1 source of UTF-16 surrogate-pair crashes in emoji-heavy inputs!
- Even better, the **exact same `SumTree` crate** powers both text buffers and **virtualized list prefix-sum heights** (Section 7).

### 4.3 Undo/Redo Grouping, `DefaultKeyBinding.dict`, and System Text Services

- **Undo/Redo transaction grouping:** Group consecutive character insertions within a `300 ms` typing window (broken by whitespace/newline, paste, cursor jump, or backspace transitions) into a single undo transaction storing `(transaction_id, timestamp, selection_before, selection_after, Vec<Edit>)`.
- **macOS `DefaultKeyBinding.dict` bug in GPUI and how to fix it:**
  - In `zed/crates/gpui_macos/src/window.rs:3513-3531`, GPUI implements `doCommandBySelector:` as:
    ```rust
    // We ignore which selector it asks us to do because the user may have
    // bound the shortcut to something else.
    extern "C" fn do_command_by_selector(this: &Object, _: Sel, _: Sel) { ... }
    ```
    Because GPUI drops the `selector: Sel` argument and simply replays `keystroke_for_do_command` into its own keymap, any custom user binding in `~/Library/KeyBindings/DefaultKeyBinding.dict` (such as mapping a key chord to `deleteWordBackward:`, `moveToBeginningOfParagraph:`, `yank:`, `setMark:`, `swapWithMark:`, or a multi-selector array) is ignored!
  - **Fix for Rune:** First check if the application keymap explicitly binds the raw keystroke. If not (or if `NSTextInputContext` synthesized one or more `NSStandardKeyBindingResponding` selectors from `DefaultKeyBinding.dict`), dispatch the actual `Sel` (`insertNewline:`, `deleteBackward:`, `deleteWordBackward:`, `moveWordForward:`, `moveToBeginningOfLine:`, `transpose:`, `yank:`, etc.) to the focused text input handler.
- **Spellcheck, Autocorrect, Dictation, and Apple Writing Tools (macOS 15+ / iOS 18+):**
  - **Spellcheck & Autocorrect:** On macOS, query `NSSpellChecker.sharedSpellChecker` asynchronously on background worker threads per edited paragraph (`checkSpellingOfString:startingAt:language:wrap:inSpellDocumentWithTag:wordCount:`) and render wavy/dotted underlines as a text decoration layer; on iOS, `UITextInputTraits.spellCheckingType` and `autocorrectionType` are driven natively by `UITextInteraction`.
  - **Dictation:** On both macOS (`NSTextInputClient`) and iOS (`UITextInput`), system Dictation streams tentative transcriptions via `setMarkedText:selectedRange:replacementRange:` and commits via `insertText:replacementRange:` (plus `insertDictationResult:` on iOS).
  - **Apple Writing Tools (macOS 15+ / iOS 18+):** Conforming to `NSWritingToolsCoordinatorDelegate` (macOS) or `UIWritingToolsCoordinatorDelegate` / `UITextInput` (iOS) allows Apple Intelligence Writing Tools to read context ranges, animate proofreading/rewrite highlights, and apply replacements with built-in undo support.

---

## 5. IME (Input Method Editors) Per Platform

### 5.1 Architecture Matrix Across All 6 Platforms

| Platform | Native Protocol / API | Coordinate Unit | Composition / Marked Text | Candidate Window Positioning | Context / Recon Strategy |
|---|---|---|---|---|---|
| **macOS** | `NSTextInputClient` on `NSView` + `NSTextInputContext` (`gpui_macos/src/window.rs:236-298, 2700-2780, 3368-3532`) | UTF-16 `NSRange` | `setMarkedText:selectedRange:replacementRange:`; `unmarkText` | `firstRectForCharacterRange:actualRange:` (screen coordinates, bottom-left origin!) | Synchronous query into `InputHandler` during `handleEvent:` |
| **iOS** | Custom `UIView <UITextInput, UIKeyInput>` + `UITextInteraction` + `UIEditMenuInteraction` (`gpui_ios/src/ios/text_input.rs:197-1150`; `FlutterTextInputPlugin.mm:180-1100`) | Abstract `UITextPosition` wrapping UTF-16 index | `setMarkedText:selectedRange:`; `unmarkText` | `firstRectForRange:` + `caretRectForPosition:` + `selectionRectsForRange:` | **Deferred coalesced `reconcileTextInput`** (`dispatch_async` to main queue) to prevent UIKit feedback loops |
| **Android** | `View.onCreateInputConnection` -> `BaseInputConnection` + `InputMethodManager` | UTF-16 `Editable` / offsets | `setComposingText`, `setComposingRegion`, `finishComposingText` | `CursorAnchorInfo.Builder` (`setInsertionMarkerLocation`, `addCharacterBounds`) | State push via `imm.updateSelection` + `imm.updateCursorAnchorInfo` |
| **Web (2026)** | Primary: **Sliding-Window Hidden `<textarea>`** (`gpui_web/src/ime_mirror.rs:1-450`, `CONTEXT_CHARS = 512`) + progressive enhancement to **`EditContext` API** on Chromium/Edge | UTF-16 DOM offsets | `compositionstart`, `compositionupdate`, `compositionend`, `beforeinput` (`insertFromComposition`) | CSS positioning of `<textarea>` at `bounds_for_range` (`ime_mirror.rs:412-448`) or `EditContext.updateCharacterBounds` | Sliding 1,024-char window synced after every frame |
| **Windows** | `WM_IME_STARTCOMPOSITION`, `WM_IME_COMPOSITION` (`ImmGetCompositionStringW` with `GCS_COMPSTR`, `GCS_COMPATTR`, `GCS_CURSORPOS`, `GCS_RESULTSTR`) + TSF (`ITfThreadMgr` / `ITfContext`) (`gpui_windows/src/events.rs:654-790, 1710-1760`) | UTF-16 (`WCHAR` slice) | Clause-attributed composition (`ATTR_TARGET_CONVERTED`, `ATTR_CONVERTED`, `ATTR_INPUT`) | `ImmSetCompositionWindow` + `ImmSetCandidateWindow` (`CFS_CANDIDATEPOS`) | Per-window `ImmAssociateContextEx` enabled only when text input focused |
| **Linux (Wayland / X11)** | Wayland: `zwp_text_input_v3` (`gpui_linux/src/linux/wayland/client.rs:2278-2354`); X11: XIM (`XIC`) | UTF-8 byte offsets | `Event::PreeditString { text, cursor_begin, cursor_end }` buffered until `Event::Done { serial }` | `set_cursor_rectangle(x, y, width, height)` + `commit()` with serial tracking | Transactional `PreeditString` + `CommitString` applied atomically on `Done` |

### 5.2 Deep-Dive Findings & Subtle Platform Traps

1. **macOS Key Routing Order (`gpui_macos/src/window.rs:2700-2748`):**
   - If you run app keybindings *before* `[inputContext handleEvent: native_event]`, multi-keystroke bindings (like Vim `jj` or chord shortcuts) intercept keys meant for Japanese/Chinese/Korean IME (turning `ji` into `jい` instead of `じ`) or swallow dead keys (`Option+e` -> `é`).
   - Conversely, if you send *all* keys to `inputContext` first when no text input is focused, macOS beeps or eats shortcuts.
   - **Rule:** Send key events to `inputContext` first if and only if: (a) `hasMarkedText()` is true (`is_composing`), OR (b) a printable key without `Ctrl`/`Cmd`/`Fn` is pressed while an IME input source is active and the focused view accepts text input (`is_ime_printable_key`), OR (c) it is a dead key (`key_char.is_none()`).
2. **iOS Direct `UITextInput` vs Hidden `UITextField` (`gpui_ios/src/ios/text_input.rs` & `FlutterTextInputPlugin.mm`):**
   - **Why a hidden `UITextField` / `UITextView` fails:** A hidden native text field has its own internal TextKit layout and gesture recognizers; its selection handles, loupe magnifier, Scribble (Apple Pencil), and floating trackpad cursor (holding Spacebar on iOS keyboard) cannot match custom-rendered GPU text lines.
   - **How GPUI and Flutter solve iOS `UITextInput` directly:**
     - Create a dedicated `UIView` subclass (`GPUITextInputView` in `gpui_ios/src/ios/text_input.rs:197-318`, `FlutterTextInputView` in `FlutterTextInputPlugin.mm:400-1100`) that conforms to `UITextInput` and `UIKeyInput`.
     - Attach a `UITextInteraction::editableInteraction()` (`text_input.rs:285-295`) and `UIEditMenuInteraction` so UIKit renders the **native iOS selection handles, double-tap/long-press loupe, floating keyboard trackpad cursor (`beginFloatingCursorAtPoint:` / `updateFloatingCursorAtPoint:` / `endFloatingCursor`), Scribble, and Cut/Copy/Paste/Autofill callout bar** directly over the GPU canvas using geometry returned by `firstRectForRange:`, `caretRectForPosition:`, and `selectionRectsForRange:`!
     - **Critical anti-reentrancy trick (`gpui_ios/src/ios/text_input.rs:212-238, 1080-1145`):** Calling `[inputDelegate textWillChange:]` / `[inputDelegate selectionDidChange:]` synchronously inside a UIKit callback causes UIKit's `UITextInteraction` and autocorrect engine to re-enter `UITextInput` methods with stale state. GPUI sets an `in_input_callback: BOOL` ivar and schedules a single coalesced `reconcileTextInput` via `dispatch_async_f(dispatch_get_main_queue(), ...)` to notify `inputDelegate` (`selectionWillChange`/`selectionDidChange`/`textWillChange`/`textDidChange`) only when the model state actually diverged from what UIKit already knows.
     - **Flutter's iOS 17+ `UITextInteraction` workaround (`FlutterTextInputPlugin.mm:260-285`):** On iOS 17+, `UITextInteraction` requires an view in the responder chain when keyboard dictation / trackpad mode activates; keeping `GPUITextInputView` in the view hierarchy (with zero visual sublayers) satisfies UIKit's responder checks.
3. **Web IME in 2026: Hidden Sliding-Window `<textarea>` + `EditContext` (`gpui_web/src/ime_mirror.rs:1-450`):**
   - **State of `EditContext` in 2026:** W3C `EditContext` is shipping in Chromium/Edge (`Chrome 121+`), giving direct `textupdate`, `characterboundsupdate`, and `textformatupdate` (Japanese clause underline thickness/style) without DOM mutation side-effects. However, **Safari (WebKit) and Firefox still lack complete `EditContext` support on mobile/desktop in 2026**.
   - **Why GPUI's `ImeMirror` (`gpui_web/src/ime_mirror.rs:1-130`) works everywhere:** Instead of mirroring an entire 100 KB document into a hidden `<textarea>` (which freezes mobile Safari), `ImeMirror` mirrors only a **sliding window of `CONTEXT_CHARS = 512` characters before and after the selection** (`ime_mirror.rs:23`), tracks `window_start_utf16` and `composition_start_utf16`, and positions the `1px` transparent `<textarea>` directly at the caret's `bounds_for_range` (`ime_mirror.rs:412-448`) so the OS candidate popup and macOS emoji picker (`Ctrl+Cmd+Space`) anchor to the exact caret pixel.
4. **Windows Clause Underline Attributes (`gpui_windows/src/events.rs:713-785, 1738-1760`):**
   - Japanese and Chinese IMEs on Windows split an active composition into multiple clauses, where one clause is the **active conversion target** (`ATTR_TARGET_CONVERTED` / `ATTR_TARGET_NOTCONVERTED`, rendered with a thick or highlighted underline) and sibling clauses are `ATTR_CONVERTED` (thin/dotted underline). Querying `ImmGetCompositionStringW(ctx, GCS_COMPATTR, ...)` alongside `GCS_CURSORPOS` is required so users can see which clause Left/Right arrow keys are reconverting.
5. **Linux Wayland `zwp_text_input_v3` Serial & Vim-Mode Quirks (`gpui_linux/src/linux/wayland/client.rs:2278-2354`):**
   - Wayland `zwp_text_input_v3` sends `PreeditString` and `CommitString` as pending state followed by `Event::Done { serial }`. If `Done` arrives with `ime_pre_edit == None` while previously composing, the client must clear marked text (`client.rs:2346-2350`).
   - Also note `client.rs:2306-2320`: IBus intercepts single ASCII keys like `a` or `b` and sends them as `CommitString("a")` instead of `wl_keyboard::Key`, which breaks modal keybindings unless single-byte ASCII commits when not composing are routed through `KeyDown`.

---

## 6. Accessibility (a11y) & Agent Inspection

### 6.1 AccessKit in 2026 (`accesskit` v0.24+)

Inspection of `accesskit/` confirms AccessKit now ships first-party platform adapters across **all five native targets**:
- **macOS (`accesskit_macos`):** Maps `accesskit::Node` to `NSAccessibilityElement` subclasses (`accesskit/adapters/macos/src/node.rs:36-210`), posting `NSAccessibilityFocusedUIElementChangedNotification`, `NSAccessibilityValueChangedNotification`, and `NSAccessibilityLayoutChangedNotification`.
- **iOS (`accesskit_ios`):** Implemented in `accesskit/adapters/ios/src/node.rs:1-515` (`v0.4.0`), mapping `accesskit::Node` to `UIAccessibilityElement` with `UIAccessibilityTraits` (`Button`, `Header`, `Adjustable`, `UpdatesFrequently`, `Selected`, `NotEnabled`), `accessibilityFrame` (converted via `UIAccessibilityConvertFrameToScreenCoordinates`), `accessibilityActivate`, `accessibilityIncrement`/`Decrement`, and `accessibilityScroll:`.
- **Android (`accesskit_android`):** Implemented in `accesskit/adapters/android/` (`v0.6.0`), bridging to Android's `AccessibilityNodeProvider` (`AccessibilityNodeInfo` virtual view hierarchy) for TalkBack.
- **Windows (`accesskit_windows`):** Implements UI Automation (`IRawElementProviderSimple`, `ITextProvider`, `IValueProvider`, `IScrollItemProvider`) for Narrator, NVDA, and JAWS.
- **Linux (`accesskit_unix`):** Implements AT-SPI2 over D-Bus (`zbus`) for Orca.

### 6.2 Roles, Actions, Live Regions, Focus, and Text Navigation

- **Core primitives (`accesskit/accesskit/src/lib.rs`):**
  - `NodeId(NonZeroU64)`: Stable 64-bit identifier per semantic node.
  - `Role` (`lib.rs:62-284`): 180+ WAI-ARIA / native roles (`Button`, `TextInput`, `MultilineTextInput`, `Switch`, `Slider`, `List`, `ListItem`, `Dialog`, `Tab`, `TabPanel`, `ScrollView`, `TextRun`, etc.).
  - `Action` (`lib.rs:289-350`): `Click`, `Focus`, `Blur`, `Increment`, `Decrement`, `ReplaceSelectedText`, `SetValue`, `SetTextSelection`, `ScrollIntoView`, `ScrollToPoint`, `SetScrollOffset`, `Expand`, `Collapse`, `ShowContextMenu`, `CustomAction`.
  - `Live` (`lib.rs:584`): `Off`, `Polite`, `Assertive` (maps to `UIAccessibilityPostNotification(UIAccessibilityAnnouncementNotification, ...)` on iOS and `NSAccessibilityAnnouncementRequestedNotification` on macOS).
  - **Character-level text navigation (`Role::TextRun`):** For multi-line text inputs and selectable text, AccessKit represents each wrapped visual line as a child `Role::TextRun` node carrying `character_lengths: Box<[u8]>`, `character_positions: Box<[f32]>`, `character_widths: Box<[f32]>`, and `word_starts: Box<[u8]>`. AccessKit transforms this directly into macOS `accessibilityFrameForRange:`, Windows `ITextRangeProvider`, and iOS text selection navigation without platform-specific text glue!

### 6.3 How Flutter Maps `SemanticsNode` and What iOS VoiceOver Needs

- In Flutter (`flutter/engine/src/flutter/shell/platform/darwin/ios/framework/Source/SemanticsObject.h` & `SemanticsObject.mm`), every `SemanticsNode` becomes a `SemanticsObject : UIAccessibilityElement` (or `SemanticsObjectContainer` for children).
- **Critical iOS VoiceOver requirements that custom GPU frameworks miss:**
  1. **Two-finger Z-scrub ("Escape" gesture):** iOS VoiceOver users scrub a "Z" with two fingers to dismiss modals, pop navigation stacks, or close sheets. `accesskit_ios` (`accesskit/adapters/ios/src/node.rs`) and Rune's root container must implement `accessibilityPerformEscape` and map it to a dismiss/back action on the active modal or navigation route.
  2. **Modal sibling masking (`accessibilityViewIsModal`):** When a dialog, sheet, or popover is open, setting `Node::set_modal(true)` (which maps to `accessibilityViewIsModal = YES` on iOS) prevents VoiceOver swipe navigation from cycling through background views behind the scrim.
  3. **Scroll-on-swipe (`accessibilityScroll:` & `Action::ScrollIntoView`):** When VoiceOver swipes past the last visible row of a virtualized list, the list must keep a **1-screen buffer of offscreen semantic nodes** above and below the viewport and handle `Action::ScrollIntoView` + `accessibilityScroll:` by advancing the virtual scroll offset and posting `UIAccessibilityLayoutChangedNotification`.
  4. **Rotor Custom Actions (`UIAccessibilityCustomAction`):** Swipe actions on list rows (e.g., Archive, Delete, Pin) must register as `CustomAction` on the row's `accesskit::Node` so VoiceOver users can trigger them via the vertical swipe rotor.

### 6.4 Should the Accessibility Tree Double as the Agent-Inspector Tree?

**Yes — unequivocally.**
- Kit's existing automation tools (`cua-driver`, macOS Accessibility inspector, agent proof scripts) already operate by snapshotting the accessibility tree, binding stable element tokens, reading bounds/labels/values, and invoking semantic actions (`Click`, `SetValue`, `ScrollIntoView`).
- By making `accesskit::TreeUpdate` a **first-class frame output** (built incrementally for dirty subtrees or serialized on demand over a local Unix socket / WebSocket / MCP debug endpoint even when OS screen readers are inactive), Rune gets:
  1. **Zero-cost agent drivability on every platform** (including headless Linux CI, iOS simulators, Android emulators, and Web) via one JSON/binary `TreeUpdate` schema.
  2. **Built-in a11y enforcement:** If an AI agent cannot see or click a button in an agent proof script, a blind VoiceOver user cannot use it either.
  3. **Lightweight extension:** Add an optional side-table `InspectorMetadata` keyed by `accesskit::NodeId` for agent-only diagnostics (component source `file:line`, test ID, computed style/spring state, layout constraints) without polluting the OS accessibility bridge.

---

## 7. Layout

### 7.1 Comparative Evaluation of Layout Models

| Dimension | **Taffy (`v0.9.x`, Flexbox + CSS Grid)** | **SwiftUI Proposal / Size Negotiation** | **Flutter Constraints-Down, Sizes-Up (`BoxConstraints`)** | **Clay (Nic Barker's C micro-layout)** | **Morphorm (Subform / Vizir units)** |
|---|---|---|---|---|---|
| **Mental Model** | CSS Flexbox & CSS Grid (`Style` struct) | Parent proposes `(w?, h?)` -> Child picks size -> Parent places child | Parent passes `(min_w..max_w, min_h..max_h)` -> Child returns `Size` | Single-header immediate-mode `Sizing::Fixed`, `Grow`, `Percent`, `Fit` | Constraint units (`Pixels`, `Percentage`, `Stretch`, `Auto`) |
| **Passes per Tree** | 1–3 passes (Flex intrinsic measure + flex resolve; cached by `(known_dimensions, available_space)`) | 1–3 proposals (`nil` ideal, `0` min, `.infinity` max, concrete width) | Strictly $O(N)$ single pass (except `IntrinsicWidth`/`Height`) | Strictly 3 linear array passes (BFS/DFS over flat arena) | 2 passes (upward intrinsic, downward stretch) |
| **Agent-Writability (LLM accuracy)** | **Highest:** Every LLM has millions of Tailwind/CSS Flexbox/Grid examples in pretraining | High (SwiftUI modifiers), but `.frame(maxWidth: .infinity)` + `layoutPriority` traps are common | Medium-High (`Expanded`/`Flexible` inside unconstrained `ScrollView` panics at runtime!) | Good for simple rows/columns; no CSS Grid or baseline wrap | Low training corpus; unfamiliar `Units::Stretch` rules |
| **1M-Row Virtualized List Perf** | Must NOT instantiate 1M Taffy nodes; pair with virtual viewport + `SumTree` height index | `LazyVStack` struggles with variable heights (`sizeThatFits` = 106.4 ms / 500 rows) | `SliverList` is fast, requires custom sliver protocol | Arena-based (~15 µs for visible ~100 nodes), no built-in virtualization | Requires external virtualization |
| **Layout Animation (FLIP / Shared Element)** | Outputs static `Layout { location, size }`; requires framework-level previous-rect diffing | First-class (`matchedGeometryEffect`, `Layout` protocol animatable properties) | Manual (`AnimatedSize`, `Hero` overlay widgets) | Immediate-mode rect output; easy to interpolate `(x, y, w, h)` per ID | Static rect output |

### 7.2 Measured & Cited Benchmark Numbers

1. **Taffy (`v0.9.2`) vs Yoga (`v3.2.1`) on Apple M1 Pro (`taffy/README.md:94-114`, Criterion benchmarks):**
   - **1,000 nodes (deep hierarchy):** Taffy **1.60 ms** vs Yoga **1.50 ms** (~1.6 µs/node).
   - **10,000 nodes (deep hierarchy):** Taffy **17.5 ms** vs Yoga **16.2 ms**.
   - **10,000 nodes (super-deep 12-level hierarchy):** Taffy **24.6 ms** vs Yoga **36.2 ms** (**Taffy is 32% faster** due to 9-slot `Cache` in `taffy/src/tree/cache.rs` avoiding exponential re-measurement).
   - **100,000 nodes (wide 2-level hierarchy):** Taffy **98.9 ms** vs Yoga **191.5 ms** (**Taffy is 48% faster**).
   - **Twitter feed benchmark (`yoga/taffy` real-world cards):**
     - 1,500 nodes: Taffy **626 µs** vs Yoga **950 µs** (**1.52x faster**).
     - 15,000 nodes: Taffy **6.40 ms** vs Yoga **11.17 ms** (**1.75x faster**).
     - 100,000 nodes: Taffy **59.17 ms** vs Yoga **84.42 ms** (**1.43x faster**).
2. **GPUI's Taffy Integration (`zed/crates/gpui/src/taffy.rs:1-315`):**
   - GPUI wraps `TaffyTree<NodeContext>` in `TaffyLayoutEngine`, using `FxHashMap<GlobalElementId, NodeId>` and a 2-frame mark-and-sweep (`computed_layouts: FxHashSet<NodeId>`) so unchanged subtrees keep their Taffy nodes and measurement cache across frames instead of rebuilding the Taffy arena from scratch each frame.
3. **Text Measurement Inside Layout (a private iOS layout benchmark):**
   - In any real UI, **80–90% of layout wall-clock time is spent inside the leaf `measure_function` (text shaping & line wrapping), not in the Flexbox solver itself.**
   - Measuring 500 chat paragraphs on iOS:
     - SwiftUI `sizeThatFits`: **106.44 ms** (212.88 µs/item) — blows nine 120 Hz frames (8.33 ms budget).
     - Cold CoreText `CTFramesetter`: **17.75 ms** (35.51 µs/item).
     - Reused `CTFramesetter` height: **2.60 ms** (5.19 µs/item).
     - Pre-computed width-break table (`Pretext`): **0.23 ms** (0.45 µs/item).

### 7.3 Solving the 3 Hard Layout Cases in Rune

1. **Agent-Writability:**
   - Expose **Tailwind/Flexbox/Grid-aligned builder methods** backed by `taffy` (`flex()`, `flex_col()`, `items_center()`, `justify_between()`, `gap(px(8.))`, `grid_cols(3)`) **plus** ergonomic SwiftUI-style shorthand (`hstack()`, `vstack()`, `zstack()`, `spacer()`) that compile down to the exact same `taffy::Style`. Agents never hit Flutter's dreaded *"RenderFlex children have non-zero flex but incoming height constraints are unbounded"* crash because Taffy gracefully resolves `flex-grow` inside auto-sized containers to content size.
2. **1M-Row Variable-Height Virtualized Lists:**
   - Never insert offscreen rows into `TaffyTree`.
   - Store the 1M-row list state in a **`SumTree<ListItemSummary>`** (`zed/crates/sum_tree/src/sum_tree.rs`) where each item summary stores `(count: usize, height: f32, is_measured: bool)`:
     - Unmeasured items initialize with a fast estimated height (or `LineLayoutCache` unwrapped width / container wrap width estimate).
     - Mapping a scroll pixel offset $y \in [0, 50,000,000]$ to the first visible row index is a **$O(\log N)$ `SumTree` seek (~200 nanoseconds for 1M items)**.
     - Only the visible viewport rows + overscan buffer (~20–40 rows) are laid out via `Taffy` each frame (< 0.15 ms). When a row's measured height differs from its estimate while scrolling upward, anchor scroll position relative to the first visible item's top edge so the viewport never jumps.
3. **Layout Animation (Spring-Driven Bounds & Shared-Element `layout_id`):**
   - Why layout animation is painful in raw Flexbox: Taffy outputs discrete `(location, size)` rectangles per frame.
   - **Rune's solution:** Attach an optional `.layout_id(id)` / `.animate_layout(Spring)` to any element. After `Taffy` computes the target layout rect $R_{\text{target}} = (x, y, w, h)$ in window space, Rune's layout-animation pass compares $R_{\text{target}}$ against the previous frame's rendered rect $R_{\text{prev}}$ for that `ElementId`:
     - **Position delta ($\Delta x, \Delta y$):** Drive a 2D spring offset on the element's GPU transform matrix (zero re-layout cost!).
     - **Size delta ($\Delta w, \Delta h$):** Drive a 2D spring on the element's rendered clip/background bounds while keeping text children anchored (or using `Overflow::FadeMask`) so text never re-wraps on every intermediate frame of a container resize spring!

---

## 8. What to Steal / What to Avoid

### What to Steal

1. **Kit's GPUI fork (`kitlangton/gpui`) 5-level Luminance Glyph Dilation (`kitlangton/gpui/src/platform/mac/text_system.rs:42-81, 470-564`, commit `2582af8ad0`):** Quantize text foreground luminance into 5 buckets (`0..=4`), pad glyph bounds by `1px`, and rasterize with `AppleFontSmoothing` / stem darkening scaled to that luminance so light-on-dark and dark-on-light text have optically matched weight.
2. **GPUI's Two-Tier `LineLayoutCache` (`zed/crates/gpui/src/text_system/line_layout.rs:523-698`):** Separate cached unwrapped `LineLayout` (shaping) from `WrappedLineLayout` (wrap boundaries) with two-frame epoch swapping and index-range `reuse_layouts`. Resizing a window or split pane only recomputes line breaks over cached glyph advances, never re-running HarfRust/CoreText shaping.
3. **Zed's `SumTree` (`zed/crates/sum_tree/src/sum_tree.rs`):** Use one battle-tested concurrent B+ tree for both text ropes (simultaneous UTF-8, UTF-16, line/col, and soft-wrap coordinates in $O(\log N)$) and 1M-row virtualized list height indexing.
4. **Parley `v0.12.0` (`parley` + `fontique` + `harfrust` + `skrifa` + `icu4x`):** Use Parley's shaping/bidi/segmentation pipeline (`parley/parley/src/shape.rs`, `layout.rs`, `cursor.rs`) for UAX #9 bidi reordering, UAX #14 line breaking, `InlineBox` layout, and `Affinity`-aware cursor/selection geometry.
5. **Vello `glifo`'s `COLRv1` + `sbix` + `skrifa` Glyph Extractor (`vello/glifo/src/glyph.rs:289-465`, `colr.rs`):** Steal `glifo`'s priority waterfall (`COLRv0/v1` vector paint trees -> `sbix`/`CBDT` PNG bitmaps -> `skrifa` hinted outlines) and size-adaptive subpixel quantization (`SubpixelAxis::Four` at body sizes, `One` at large sizes).
6. **GPUI iOS's `GPUITextInputView` + Deferred `reconcileTextInput` (`zed/crates/gpui_ios/src/ios/text_input.rs:197-318, 1080-1145`) & Flutter's `UIEditMenuInteraction` / Floating Cursor (`FlutterTextInputPlugin.mm:400-950`):** Direct `UITextInput` conformance with `UITextInteraction` and coalesced main-queue delegate reconciliation gives native iOS selection handles, loupe, Scribble, Dictation, and Spacebar trackpad cursor with zero reentrancy bugs.
7. **GPUI Web's 512-Char Sliding-Window `ImeMirror` (`zed/crates/gpui_web/src/ime_mirror.rs:1-130, 412-448`):** Bound hidden `<textarea>` synchronization to $\pm 512$ characters around the selection and position it at the exact caret rect, adding Chromium `EditContext` as a fast path when detected.
8. **AccessKit (`v0.24+`) Across All 5 Platforms (`accesskit/adapters/{macos,ios,android,windows,unix}`):** Emit one `accesskit::TreeUpdate` that simultaneously powers VoiceOver (macOS/iOS), TalkBack (Android), UIA (Windows), AT-SPI (Linux), and Rune's AI agent inspector.

### What to Avoid

1. **Avoid absorbing continuous spring scale transforms into the glyph atlas cache key (`vello/glifo/src/glyph.rs:1702-1731`):** Hashing `draw_font_size.to_bits()` during a 120 Hz scale animation rasterizes 60+ transient font sizes per glyph, thrashing the atlas and causing stem-hinting jitter.
2. **Avoid ignoring the `Sel` argument in macOS `doCommandBySelector:` (`zed/crates/gpui_macos/src/window.rs:3515`):** Dropping the Cocoa selector breaks custom `~/Library/KeyBindings/DefaultKeyBinding.dict` bindings and standard Cocoa text movement/kill-ring commands.
3. **Avoid SDF / MSDF for body UI text (`10–16pt`):** MSDF rounds or spikes complex CJK/symbol corners, lacks stem hinting, and has slow CPU generation for dynamic Unicode fallback.
4. **Avoid subpixel LCD (`RGB`) glyph atlases:** Incompatible with layer opacity, rotation, scale springs, OLED subpixel layouts, and modern macOS/mobile compositors.
5. **Avoid hidden native `UITextField` / `NSTextView` overlays for mobile/desktop IME:** They desynchronize selection handles, break custom line heights/inline pills, and fight custom scroll physics.
6. **Avoid per-OS text shapers as the sole layout engine:** Relying exclusively on CoreText on Apple and DirectWrite on Windows causes cross-platform line-wrap differences and `CFAttributedString` allocation overhead (17.8 ms / 500 paragraphs cold vs < 1 ms cached `harfrust` + `LineLayoutCache`).
7. **Avoid putting offscreen virtualized list items into `TaffyTree` or re-running text shaping during width animations:** Use `SumTree` height prefix-sums for offscreen rows and `Overflow::FadeMask` or cached unwrapped glyph advances during animated width transitions.

---

## 9. Decisive Recommendations for Rune

### 9.1 Architectural Decisions & Rejected Alternatives

| Sub-Area | Rune Decision | Rejected Alternatives & Why |
|---|---|---|
| **1. Text Shaping & Fonts** | **`harfrust` (`0.14`) + `skrifa` (`0.42`) + `fontique` (`0.8`)**, with a thin platform bridge on Apple to read `.AppleSystemUIFont` (`SF Pro`) `trak` optical tracking tables. | **Rejected `rustybuzz` + `swash`:** `swash` is unmaintained; `rustybuzz` is 10–15% slower and duplicates `ttf-parser` tables.<br>**Rejected CoreText/DirectWrite-only shaping:** Allocates OS objects per run and produces divergent line wraps across platforms. |
| **2. Text Layout** | **`parley` (`0.12`)** wrapped in **GPUI's two-tier `LineLayoutCache`** (unwrapped shaped runs separated from wrapped line boundaries) + first-class **`Overflow::FadeMask`** shader clip alongside `Overflow::Ellipsis`. | **Rejected `cosmic-text`:** Tied to `rustybuzz`/`swash`, lacks `InlineBox` layout.<br>**Rejected naive `Pretext` whitespace width tables:** 29% height mismatch on hyphenated/kerned prose (`opencode-ios-pretext-layout`); caching exact `LineLayout` glyph advances achieves the same sub-millisecond resize speed with 0% drift. |
| **3. Glyph Rendering** | **Two-tier hybrid:**<br>• **Tier 1 (UI Text `<= 28pt`):** Grayscale `R8Unorm` (`RGBA8` for `COLRv1`/`sbix` emoji) shelf-packed atlas with `skrifa` hinting, 4x X-subpixel phases, and **5-level luminance glyph dilation (`kitlangton/gpui`)**.<br>• **Tier 2 (Animated / Hero Text):** Transform-invariant linear-metrics octave sampling for scaled UI containers, and **direct GPU vector paths (`skrifa` outlines -> Vello/Slug)** for hero/per-character animated text (`> 28pt` or `.textRenderMode(.vector)`). | **Rejected SDF/MSDF:** Artifacts on CJK and `11–14pt` UI text.<br>**Rejected LCD subpixel AA:** Breaks transparency and transforms.<br>**Rejected per-frame `draw_font_size` atlas rasterization (`glifo` default):** Thrashes atlas on spring scales. |
| **4. Text Editing** | **Single unified `TextBuffer` built on Zed's `SumTree` + `Rope`**, supporting both `<Input>` and multi-MB `<Editor>`, UAX #29 grapheme/word movement (`icu_segmenter`), `Affinity`-aware bidi carets, 300 ms undo coalescing, and macOS `DefaultKeyBinding.dict` selector dispatch. | **Rejected separate `String` for `<Input>` and `ropey` for `<Editor>`:** Maintaining two cursor/IME/UTF-16/undo stacks doubles surface area for surrogate-pair and IME bugs. |
| **5. Platform IME** | **Direct native protocols per OS:** `NSTextInputClient` (macOS), `UITextInput` + `UITextInteraction` + deferred `reconcileTextInput` (iOS), `InputConnection` + `CursorAnchorInfo` (Android), `ImmGetCompositionStringW`/TSF (Windows), `zwp_text_input_v3` (Wayland), and **512-char sliding-window `<textarea>` + `EditContext`** (Web). | **Rejected hidden `UITextField`/`NSTextField`:** Breaks native iOS loupe/handles/Scribble alignment on custom GPU text.<br>**Rejected `EditContext`-only on Web:** Not supported on Safari/iOS WebKit or Firefox in 2026. |
| **6. Accessibility & Agent Tree** | **`accesskit` (`v0.24+`) across all 5 native platforms**, with the frame's `accesskit::TreeUpdate` + `InspectorMetadata` exposed directly as Rune's **AI Agent Inspector & Automation Tree**. | **Rejected custom per-OS accessibility bridges:** Thousands of lines of duplicated ObjC/JNI/COM glue.<br>**Rejected separate DOM/inspector tree for agents:** Diverges from real screen-reader semantics. |
| **7. Layout** | **`taffy` (`0.9.x`, incremental retained tree) + `SumTree` prefix-sum height virtualizer for lists + built-in FLIP/Spring layout-rect interpolation.** | **Rejected Flutter `BoxConstraints`-only:** Poor LLM ergonomics (`Expanded` in `ScrollView` runtime crashes) and lacks CSS Grid.<br>**Rejected `Clay` / `Morphorm`:** Lack CSS Grid and deep LLM pretraining familiarity. |

### 9.2 Recommended Rust Crate Dependencies

```toml
[dependencies]
# Text Shaping, Fonts, Bidi & Segmentation
harfrust = "0.14"
skrifa = { version = "0.42", features = ["std", "autohint_shaping"] }
read-fonts = "0.37"
fontique = "0.8"
parley = "0.12"
icu_segmenter = "2.0"
icu_properties = "2.0"

# Buffer & Virtualized List Indexing
sum_tree = { path = "crates/sum_tree" } # Forked/extracted from zed/crates/sum_tree
rope = { path = "crates/rope" }         # Forked/extracted from zed/crates/rope

# Layout
taffy = { version = "0.9", features = ["flexbox", "grid", "taffy_tree"] }
etagere = "0.2"                         # Shelf-packing allocator for glyph & emoji atlases

# Accessibility (Core + Platform Adapters)
accesskit = "0.24"
[target.'cfg(target_os = "macos")'.dependencies]
accesskit_macos = "0.24"
[target.'cfg(target_os = "ios")'.dependencies]
accesskit_ios = "0.4"
[target.'cfg(target_os = "windows")'.dependencies]
accesskit_windows = "0.31"
[target.'cfg(target_os = "linux")'.dependencies]
accesskit_unix = "0.20"
[target.'cfg(target_os = "android")'.dependencies]
accesskit_android = "0.6"
```

### 9.3 Top Risks Ranked

1. **Risk #1 (High): iOS `UITextInput` + `UITextInteraction` edge cases across iOS 17/18/19 (Scribble, Floating Trackpad Cursor, Autofill, Dictation, Hardware Keyboard).**
   - *Why:* UIKit's `UITextInteraction` makes undocumented synchronous re-entrant calls into `UITextInput` during autocorrect and trackpad gestures.
   - *Mitigation:* Port `GPUITextInputView` (`zed/crates/gpui_ios/src/ios/text_input.rs`) and its deferred `reconcileTextInput` state machine verbatim, augmented with Flutter's `beginFloatingCursorAtPoint:` (`FlutterTextInputPlugin.mm:780-845`), and test on real iOS Simulator via `fieldwork-prove`.
2. **Risk #2 (High): Cross-platform stem weight & Apple `.AppleSystemUIFont` (`SF Pro`) optical fidelity when moving from CoreText to `harfrust` + `skrifa`.**
   - *Why:* Users immediately notice if macOS/iOS text looks 5% thinner, wider, or differently tracked than native SwiftUI/AppKit.
   - *Mitigation:* Apply `SF Pro` `trak` table optical tracking in `harfrust` on Apple platforms, and support both CoreGraphics `rasterize_glyph` (with 5-level luminance dilation from `kitlangton/gpui`) on Apple and `skrifa` + stem-darkening dilation on Windows/Linux/Android/Web.
3. **Risk #3 (Medium-High): Animated text jitter and re-wrap stutters during 120 Hz spring layout transitions.**
   - *Why:* Naive layout springs re-wrap text at every intermediate width and re-snap glyphs to device pixel grids, causing visible 1px crawling and line-reflow pops.
   - *Mitigation:* Enforce two invariants in the renderer: (a) during an active transform spring, disable integer pixel-grid snapping and lock the glyph atlas lookup to unhinted linear-metric outlines at the target octave scale; (b) during animated width transitions, freeze wrap width to the target width (or use `Overflow::FadeMask`) unless explicitly opted into live reflow.
4. **Risk #4 (Medium): Virtualized list scroll-anchor jumping with dynamic streaming markdown/chat rows.**
   - *Why:* When unmeasured items above the viewport resolve to a different height than estimated, naive scroll-top offsets jump visibly.
   - *Mitigation:* Anchor the scroll state to `(anchor_item_id, pixel_offset_within_item)` in the `SumTree<ListItemSummary>` rather than a raw global `scroll_top` pixel float.
5. **Risk #5 (Low-Medium): AccessKit iOS (`accesskit_ios`) maturity for complex custom gestures and virtualized lists.**
   - *Why:* `accesskit_ios` (`v0.4.0`) is the newest AccessKit adapter and requires host cooperation for `accessibilityScroll:` and offscreen buffer nodes.
   - *Mitigation:* Keep a 1-viewport semantic overscan buffer in virtualized lists and wire `Action::ScrollIntoView` + `accessibilityPerformEscape` directly into Rune's scroll and navigation primitives.

---

## 10. Open Questions (Requiring Prototype or Benchmark)

1. **`skrifa` + CPU/GPU stem darkening vs CoreGraphics `CGContext` glyph rasterization on macOS/iOS:** Can `skrifa` (`Target::Smooth` with custom 5-level luminance dilation in our shader/outline dilator) match CoreGraphics `CTFontDrawGlyphs` + `AppleFontSmoothing` pixel-for-pixel on `SF Pro` at `11–15pt` on 2x Retina, allowing us to drop `CGContext` glyph rasterization on Apple entirely, or should we keep `CGContext` rasterization on Apple as an `AppleGlyphRasterizer` backend? *(Needs a side-by-side pixel-diff prototype at 5 luminance levels.)*
2. **Hero vector text threshold (`28pt` vs `36pt`) and GPU cost on mobile:** At what exact point size does rendering unhinted `skrifa` outlines directly through the Vello/Slug GPU path pipeline match or beat the glyph atlas in frame time and memory bandwidth on an iPhone 16 Pro at 120 Hz? *(Needs a GPU frame-time benchmark on iOS device.)*
3. **Android `InputConnection` batch edit behavior on Samsung Keyboard vs Gboard:** Samsung Keyboard issues aggressive `beginBatchEdit` / `setComposingRegion` sequences across word boundaries that differ from Gboard. *(Needs a small Android IME harness test against our `SumTree` UTF-16 offset mapping.)*

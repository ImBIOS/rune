# 01 — GPUI, Zed Text & Rendering Architecture, and Warp's UI Framework (`warpui`)

**Sources inspected directly:**
- `kitlangton/gpui` (`gpui 0.2.2` + Kit's commit `2582af8`, "Fix Metal display link fallback and rendering quality")
- `zed-industries/zed` (`5447804`, Oct 2026) — `crates/gpui`, `crates/gpui_apple`, `crates/gpui_macos`, `crates/gpui_ios`, `crates/gpui_linux`, `crates/gpui_windows`, `crates/gpui_wgpu`, `crates/gpui_web`, `crates/sum_tree`, `crates/rope`, `crates/text`, `crates/multi_buffer`, `crates/editor`
- `warpdotdev/warp` (`f8a9c71`, Oct 2026) — `crates/warpui_core` (MIT), `crates/warpui` (MIT), and ["How Warp Works"](https://www.warp.dev/blog/how-warp-works)
- a private Mac launcher app, cited as `launcher/` (`docs/motion.md`, `docs/glass.md`, `docs/perf-log.md`, `docs/raycast-research.md`, `src/native_ui.rs`)
- `scrawl` (`src/native/pill/{mod.rs,model.rs,atlas.rs,pill.metal}`, `scrawl/perf/start-latency.md`)

---

## Executive Summary

1. **GPUI has grown into a 174k-LoC multi-crate platform engine (`gpui*` across 13 crates at Zed `5447804`), up from ~97k LoC in `gpui 0.2.2`.** Between `0.2.2` and Oct 2026, Zed split GPUI into `gpui` (101,878 LoC), `gpui_apple` (5,351 LoC, shared Metal + CoreText), `gpui_macos` (9,711 LoC), `gpui_ios` (3,684 LoC), `gpui_linux` (16,934 LoC), `gpui_windows` (14,798 LoC), `gpui_wgpu` (7,969 LoC, replacing `blade-graphics` and powering Linux + Web), and `gpui_web` (7,866 LoC, WASM + WebGPU/WebGL2 + Canvas2D fallback).
2. **GPUI's renderer is a flat 7-primitive batcher (`Shadow`, `Quad`, `Underline`, `MonochromeSprite`, `SubpixelSprite`, `PolychromeSprite`, `Path`, `Surface`) sorted by a 4-ary spatial R-tree (`BoundsTree`).** It achieves ~0.2–0.8 ms GPU encode/render times for typical editor frames by packing primitives into a single 64 KB-aligned `InstanceBuffer` (`crates/gpui_apple/src/metal_renderer.rs:58-77`) and merging non-overlapping draw batches across `DrawOrder` via `BoundsTree<Bounds<ScaledPixels>>` (`crates/gpui/src/bounds_tree.rs:14-18`, `MAX_CHILDREN = 4`).
3. **GPUI's biggest rendering weaknesses are full-window redraws, lack of offscreen layer/filter compositing, and synchronous window-resize barriers.** GPUI clears and redraws the entire window drawable every dirty frame (`MTLLoadAction::Clear`, no dirty-rect scissor damage). Its `AnyView::cached` mechanism (`crates/gpui/src/view.rs:258-312`, `crates/gpui/src/window.rs:4014-4123`) only skips *CPU* layout/prepaint/paint by replaying `Scene` slices (`Scene::replay`, `crates/gpui/src/scene.rs:208-274`). Worse, `Path` batches clear and resolve a full-viewport 4x MSAA intermediate texture on *every* path batch (`crates/gpui_apple/src/metal_renderer.rs:1100-1215`), and live window resizing sets `presentsWithTransaction = true` with `command_buffer.waitUntilScheduled()` on the main thread (`crates/gpui_apple/src/metal_renderer.rs:845-853`).
4. **Two of Kit's four `kitlangton/gpui` (`2582af8`) fixes are still missing in upstream Zed (`5447804`).** Upstream Zed now includes perceptual luminance-based glyph dilation (`glyph_dilation_for_color`, `crates/gpui_apple/src/text_system.rs:265-274, 510-516`) and triangular blue-noise gradient dithering (`gradient_color`, `crates/gpui_apple/src/shaders.metal:326-331`). However, upstream Zed **still** lacks Gaussian shadow dithering in `shadow_fragment` (`crates/gpui_apple/src/shaders.metal:472-502`), still leaves `CAMetalLayer` colorspace unconfigured when `use_display_p3` is false (`crates/gpui_apple/src/metal_renderer.rs:321-334`), and still lacks the `CVDisplayLinkCreateWithCGDisplay` fallback in `crates/gpui_macos/src/display_link.rs:28-56` (causing `kCVReturnInvalidDisplay = -6661` panics/failures on headless or asleep Mac Studio displays).
5. **GPUI's 3-phase `Element` pipeline (`request_layout` → `prepaint` → `paint`) + `EntityMap` lease model is ergonomic for static/editor UIs but brittle for choreographed motion and nested state.** Entities are leased out of a `SecondaryMap<EntityId, Box<dyn Any>>` onto the stack during `update` (`crates/gpui/src/app/entity_map.rs:138-233`), causing runtime `double_lease_panic` crashes whenever an observer, event callback, or AppKit delegate re-enters an entity currently being updated.
6. **Zed's text engine (`sum_tree` → `rope` → `text` → `multi_buffer` → `editor::display_map`) is best-in-class and cleanly decoupled from GPUI.** `SumTree<T>` (`crates/sum_tree/src/sum_tree.rs:18`, `TREE_BASE = 6`, 12 items/node) + `Chunk` (`crates/rope/src/chunk.rs:9-33`, 128-byte `ArrayString` with four `u128` bitmaps for `chars`, `chars_utf16`, `newlines`, `tabs`) provides $O(\log N)$ seek between UTF-8 bytes, UTF-16 code units, and `Point(row, col)` using hardware `popcnt`/`tzcnt`, and `LineLayoutCache` (`crates/gpui/src/text_system/line_layout.rs:458-560`) adds zero-allocation `shape_line_by_hash` (`crates/gpui/src/text_system.rs:689-720`).
7. **Upstream GPUI added `spring.rs`, `hot_reload.rs` (`subsecond`), `AccessKit` (`window/a11y.rs`), `gpui_ios`, and `gpui_web`, but animation remains a closure wrapper.** Even with `SpringAnimation` (`crates/gpui/src/spring.rs:1-214`), GPUI animations work by cloning and rebuilding the wrapped element subtree inside `request_layout` on every tick (`crates/gpui/src/elements/animation.rs:208-246`), with no persistent velocity across interrupted retargets, no off-main-thread compositor, and no GPU-property-only fast path.
8. **The launcher and Scrawl both hit hard architectural walls in GPUI and replaced GPUI windows with native AppKit/Core Animation/Metal surfaces.** The launcher's GPUI morph suffered 6 structural visual defects (`launcher/docs/motion.md:20-75`), 11–18 ms cold glyph/emoji stalls (`launcher/docs/perf-log.md:72-97`), and 641 µs `SyncList` re-measurements, leading it to move its panel to a `WindowServer`-composited `CASpringAnimation` + SwiftUI panel (`launcher/src/native_ui.rs`). Scrawl replaced GPUI's `HudPillWindow` with a 440-line 3-pass native `CAMetalLayer` + `NSGlassEffectView` renderer (`scrawl/src/native/pill/mod.rs:1-31`) to get offscreen blur/glow passes, sub-millisecond cold start (`0.08 ms` hidden init vs `6.93 ms` eager), and zero idle CPU.
9. **Warp's `warpui` (`crates/warpui_core` + `crates/warpui`, MIT-licensed in `warpdotdev/warp`) is GPUI v1's direct sibling—originally co-designed with Nathan Sobo in 2021.** Warp kept the retained `Presenter` + Flutter-style `SizeConstraint` `Element` tree while adding two notable innovations GPUI lacks: thread-local automatic dependency tracking via `Tracked<T>` (`crates/warpui_core/src/core/autotracking/mod.rs:1-200`, eliminating manual `cx.notify()`) and a dual GUI + Ratatui TUI presenter (`crates/warpui_core/src/presenter/tui.rs:1-150`). However, its rendering pipeline (3 shaders, shelf-next-fit atlas without eviction, no subpixel AA, no offscreen filter passes) is less capable than GPUI's.
10. **Recommendation for Rune: Do NOT fork GPUI (or `warpui`); build a fresh, modular architecture while stealing `sum_tree`/`rope`, `BoundsTree` batching, `etagere` + `LineLayoutCache`, and `kitlangton/gpui` (`2582af8`) shader/text refinements.** Forking GPUI locks Rune into a 174k-LoC monolithic `App`/`Window` god-object where fixing borrow-reentrancy, retained compositor layers, interruptible spring retargeting, offscreen filter graphs, and damage-tracked rendering requires rewriting the core anyway.

---

## 1. GPUI Renderer (`gpui`, `gpui_apple`, `gpui_wgpu`, `gpui_windows`)

### 1.1 Scene Representation & Batching (`crates/gpui/src/scene.rs`, `bounds_tree.rs`)
Every frame, GPUI's paint phase populates a flat `Scene` struct (`crates/gpui/src/scene.rs:34-46`):

```rust
pub struct Scene {
    pub(crate) shadows: Vec<Shadow>,
    pub(crate) quads: Vec<Quad>,
    pub(crate) paths: Vec<Path<ScaledPixels>>,
    pub(crate) underlines: Vec<Underline>,
    pub(crate) monochrome_sprites: Vec<MonochromeSprite>,
    pub(crate) subpixel_sprites: Vec<SubpixelSprite>,
    pub(crate) polychrome_sprites: Vec<PolychromeSprite>,
    pub(crate) surfaces: Vec<PaintSurface>,
    layer_stack: Vec<DrawOrder>,
    bounds_tree: BoundsTree<Bounds<ScaledPixels>>,
}
```

- **Spatial `DrawOrder` assignment via `BoundsTree`:** Instead of assigning monotonically increasing z-indices (which would break batching whenever a parent draws a quad, then text, then another sibling quad), `Scene::insert_primitive` queries `bounds_tree.insert(bounds)` (`crates/gpui/src/scene.rs:171-206`). `BoundsTree` (`crates/gpui/src/bounds_tree.rs:14-18`) is a custom 4-ary R-tree (`MAX_CHILDREN: usize = 4`) where each node stores the maximum `u32` ordering among its descendants. Querying the tree finds the highest `DrawOrder` of any *spatially intersecting* primitive already in the scene and assigns `max_overlapping_order + 1`. Non-overlapping primitives of the same kind therefore receive identical `DrawOrder`s (or sort adjacent to each other).
- **Finish & `PrimitiveBatch` iterator:** At the end of `Window::draw`, `Scene::finish()` (`crates/gpui/src/scene.rs:283-292`) sorts all 7 primitive vectors by `(draw_order, id)` using `.sort_unstable()`. During `present()`, `Scene::batches()` (`crates/gpui/src/scene.rs:299-415`) runs a 7-way merge iterator that coalesces contiguous primitives of the same kind (and same `AtlasTextureId` for sprites) into a single `PrimitiveBatch` instanced draw call.
- **Coordinate spaces:** Layout runs in logical `Pixels(f32)`, scene primitives are stored in `ScaledPixels(f32)` (`Pixels * scale_factor`), and atlas/rasterization coordinates use integer `DevicePixels(i32)` (`crates/gpui/src/scene.rs:10-25`).

### 1.2 Metal Backend (`crates/gpui_apple/src/metal_renderer.rs`, `shaders.metal`)
On macOS and iOS, `MetalRenderer` (`crates/gpui_apple/src/metal_renderer.rs:226-265`) renders into a `CAMetalLayer`:
- **Single-buffer instancing:** All per-frame instance data (`Quad`, `Shadow`, `Underline`, `MonochromeSprite`, `SubpixelSprite`, `PolychromeSprite`, `PathRasterizationVertex`, `PathSprite`) is bumped into one shared `InstanceBuffer` (`crates/gpui_apple/src/metal_renderer.rs:58-77`, default capacity `2 * 1024 * 1024` bytes = 2 MB, grown dynamically) with 64 KB-aligned sub-allocations (`align = 64 * 1024`). If the buffer overflows mid-frame, `draw()` returns `false`, doubles `instance_buffer_size`, and retries the command buffer encode (`crates/gpui_apple/src/metal_renderer.rs:483-509`).
- **Triple-buffered pool:** `InstanceBufferPool` (`crates/gpui_apple/src/metal_renderer.rs:79-121`) holds `max_buffers_in_flight = 3` (`CAMetalLayer.maximumDrawableCount = 3`), returning buffers to the pool in the `MTLCommandBuffer` completion handler block (`crates/gpui_apple/src/metal_renderer.rs:835-843`).
- **8 Render Pipeline States:**
  1. `quads` (`quad_vertex` / `quad_fragment`, `crates/gpui_apple/src/shaders.metal:333-444`): SDF rounded rectangle with per-corner radii (`Corners<ScaledPixels>`), per-edge border widths (`Edges<ScaledPixels>`), dashed/solid border antialiasing, and solid/linear-gradient background fill (`BackgroundTag::Solid | LinearGradient | PatternSlash`).
  2. `shadows` (`shadow_vertex` / `shadow_fragment`, `crates/gpui_apple/src/shaders.metal:446-502`): Analytic 4-step Gaussian integral of an SDF rounded box (`erf` approximation along X, 4-sample quadrature along Y).
  3. `underlines` (`underline_vertex` / `underline_fragment`, `crates/gpui_apple/src/shaders.metal:551-621`): Straight or sine-wave (`wavy`) underlines.
  4. `monochrome_sprites` (`monochrome_sprite_vertex` / `monochrome_sprite_fragment`, `crates/gpui_apple/src/shaders.metal:623-702`): Samples `R8Unorm` glyph/SVG masks from `MetalAtlas`, supports 2x2 `TransformationMatrix` (for rotated SVGs), and outputs premultiplied `HSLA -> linear/sRGB RGBA`.
  5. `subpixel_sprites` (`subpixel_sprite_vertex` / `subpixel_sprite_fragment`, `crates/gpui_apple/src/shaders.metal:704-777`): Dual-source blending (`color(0), index(0)` and `color(0), index(1)`) with `Source1Color` blend factor for LCD subpixel text AA. (Note: disabled by default on macOS in `gpui_apple` where grayscale + luminance dilation is preferred, used on Windows/Linux).
  6. `polychrome_sprites` (`polychrome_sprite_vertex` / `polychrome_sprite_fragment`, `crates/gpui_apple/src/shaders.metal:779-858`): `BGRA8Unorm` atlas tiles (color emojis, raster images) with SDF corner clipping, grayscale filter, and opacity.
  7. `paths_rasterization` + `paths` (`path_rasterization_vertex`/`fragment` and `path_vertex`/`fragment`, `crates/gpui_apple/src/shaders.metal:504-549`): Arbitrary vector paths (`PathBuilder`, `crates/gpui/src/path_builder.rs:1-250`). Filled paths use Lyon (`lyon::tessellation::FillTessellator`) or Loop-Blinn quadratic curve UV coordinates (`st_position` in `PathVertex`); stroked paths use `lyon::tessellation::StrokeTessellator`.
  8. `surfaces` (`surface_vertex` / `surface_fragment`, `crates/gpui_apple/src/shaders.metal:860-910`): Hardware-decoded `CVPixelBuffer` (`kCVPixelFormatType_420YpCbCr8BiPlanarFullRange`) YCbCr-to-RGB video surfaces via `CVMetalTextureCache`.

### 1.3 Severe Bottlenecks in GPUI's Renderer
1. **Path rendering allocates & clears a full-viewport 4x MSAA texture per path batch:** Look at `MetalRenderer::draw_paths_to_intermediate` (`crates/gpui_apple/src/metal_renderer.rs:1100-1215`). Every time `Scene::batches()` yields a `PrimitiveBatch::Paths` (which cannot be merged across an intervening `Quad` or `MonochromeSprite` if bounding boxes overlap), GPUI ends the main `MTLRenderCommandEncoder`, starts a new render pass on a **full-window** `path_sample_count = 4` MSAA texture + resolve texture (`MTLLoadAction::Clear` across the entire viewport, not just the path's bounding box!), renders the path triangles, ends the encoder, and starts a *new* main `MTLRenderCommandEncoder` with `MTLLoadAction::Load` to blit the resolved texture via `path_sprite`. Having N interleaved path layers in a frame triggers $2N + 1$ full render-pass switches and $N$ full-window 4x MSAA clears.
2. **No offscreen layer stack, backdrop filter, or group opacity:** `window.with_element_opacity(opacity, ...)` (`crates/gpui/src/window.rs:1249`) simply multiplies `element_opacity` into each individual quad/sprite color at CPU paint time. Overlapping children within a translucent container bleed through each other (Classic alpha-compositing bug) instead of compositing as a group. There is zero support for offscreen Gaussian blur, vibrancy, or shader post-processing passes—which is why Scrawl had to bypass GPUI for its glowing/blurred HUD pill (`scrawl/src/native/pill/mod.rs:1-31`).
3. **Full-window redraw every frame & `Scene::replay` limits:** GPUI has no dirty-rect scissor/damage tracking at the GPU level; `draw()` always runs `MTLLoadAction::Clear` on the full `CAMetalDrawable`. `Entity::cached` (`crates/gpui/src/view.rs:258`) + `Scene::replay` (`crates/gpui/src/scene.rs:208-274`) copies the previous frame's `Quad`/`MonochromeSprite` structs from `rendered_frame.scene` into `next_frame.scene` and re-inserts every primitive into `BoundsTree` on the CPU. If *any* animation ticks in a window, the entire window's `Scene` is re-sorted, re-uploaded to `InstanceBuffer`, and re-rendered by the GPU.
4. **Synchronous `presentsWithTransaction` stall during resize:** During window resize (`crates/gpui_macos/src/window.rs:3320-3341`), GPUI sets `renderer.set_presents_with_transaction(true)`, which forces `command_buffer.commit(); command_buffer.wait_until_scheduled(); drawable.present();` on the main thread (`crates/gpui_apple/src/metal_renderer.rs:845-853`).

### 1.4 Cross-Platform Renderers (`gpui_wgpu`, `gpui_windows`)
- **`gpui_wgpu` (`crates/gpui_wgpu/src/wgpu_renderer.rs:1-250`):** In `gpui 0.2.2`, Linux used `blade-graphics`. By Oct 2026 (`5447804`), Zed replaced `blade-graphics` with `gpui_wgpu` (7,969 LoC), which now powers **both** `gpui_linux` (Wayland + X11 via Vulkan/GL) and `gpui_web` (WebGPU + WebGL2 fallback). It implements the exact same 7-primitive pipeline in WGSL (`crates/gpui_wgpu/src/shaders.wgsl`) including dual-source blending (`wgpu::Features::DUAL_SOURCE_BLENDING`) when supported by the adapter (`crates/gpui_wgpu/src/wgpu_renderer.rs:150-166`), falling back to single-source alpha blending on WebGL2/uncooperative drivers.
- **`gpui_windows` (`crates/gpui_windows/src/directx_renderer.rs:1-200`):** Uses raw Direct3D 11 (`ID3D11Device`, `IDXGISwapChain1`) bound to `IDCompositionDevice` / `IDCompositionVisual` (`DirectComposition`) for transparent/acrylic window support and low-latency flip-model presentation, with 4x MSAA intermediate textures for paths (`PATH_MULTISAMPLE_COUNT = 4`, line 29) and HLSL shaders compiled at build time.

### 1.5 Texture Atlas (`etagere` in `crates/gpui_apple/src/metal_atlas.rs`)
`MetalAtlas` (`crates/gpui_apple/src/metal_atlas.rs:1-250`) wraps `etagere::BucketedAtlasAllocator`:
- Separate texture lists by `AtlasTextureKind`:
  - `Monochrome` → `MTLPixelFormatR8Unorm` (glyphs, rasterized `resvg` SVGs)
  - `Subpixel` → `MTLPixelFormatBGRA8Unorm`
  - `Polychrome` → `MTLPixelFormatBGRA8Unorm` (color emojis, images)
- Starting page size: **1024×1024** for Monochrome/Subpixel, **2048×2048** for Polychrome (`crates/gpui_apple/src/metal_atlas.rs:138-145`), dynamically allocating additional `MTLTexture` pages when `etagere` fills up.
- Uploads happen synchronously on the CPU into `MTLStorageModeManaged` (Intel) or `MTLStorageModeShared` (Apple Silicon unified memory) via `replaceRegion_mipmapLevel_withBytes_bytesPerRow` (`crates/gpui_apple/src/metal_atlas.rs:265-285`).

### 1.6 Exact Diff Analysis: `kitlangton/gpui` (`2582af8`) vs Upstream Zed (`5447804`)
In `kitlangton/gpui`, commit `2582af8` ("Fix Metal display link fallback and rendering quality", 4 files changed, +127/-16 lines) fixed four concrete production bugs in `gpui 0.2.2`:

| Fix in `kitlangton/gpui` (`2582af8`) | How it works in `kitlangton/gpui` | Status in Upstream Zed (`5447804`, Oct 2026) |
|---|---|---|
| **1. `CVDisplayLink` fallback (`src/platform/mac/display_link.rs:25-60`)** | When `CVDisplayLinkCreateWithCGDisplay(display_id, ...)` fails with `kCVReturnInvalidDisplay` (`-6661`, common on headless/asleep displays or virtual displays), falls back to `CVDisplayLinkCreateWithActiveCGDisplays` and calls `CVDisplayLinkSetCurrentCGDisplay` best-effort. | **MISSING in upstream Zed.** `crates/gpui_macos/src/display_link.rs:28-56` still calls `CVDisplayLinkCreateWithCGDisplay` and immediately returns `Err` on non-zero `CVReturn`! Worse, `crates/gpui_macos/src/window.rs:839-845` skips starting the display link if `!occlusionState.contains(NSWindowOcclusionStateVisible)`, but `WindowFrameSource` still uses a 60 Hz `dispatch_source_create(DISPATCH_source_type_timer)` fallback only when occluded (`display_link.rs:240-310`). |
| **2. Explicit `sRGB` `CAMetalLayer` colorspace (`src/platform/mac/metal_renderer.rs:257-263`)** | Sets `layer.set_colorspace(CGColorSpaceCreateWithName(kCGColorSpaceSRGB))` on the `CAMetalLayer` so macOS WindowServer composites `BGRA8Unorm` in a deterministic sRGB color space instead of untagged device RGB. | **PARTIALLY MISSING in upstream Zed.** `crates/gpui_apple/src/metal_renderer.rs:321-334` only sets `kCGColorSpaceDisplayP3` when `use_display_p3()` is true (`colorspace` setting =P3) and sets `null()` when false, leaving default sRGB windows untagged! |
| **3. Gradient + Shadow dithering (`src/platform/mac/shaders.metal:326-331, 495-503`)** | Adds interleaved gradient noise `dither_noise(position)` (`fract(52.9829189 * fract(dot(position, float2(0.06711056, 0.00583715))))`) with triangular PDF `(n1 + n2 - 1.0) / 255.0` to **both** linear gradients (`gradient_color`) **and** `shadow_fragment` (scaled by `min(alpha * 2.0, 1.0)`), eliminating 8-bit Mach banding on dark glass/shadows. | **HALF-MERGED in upstream Zed.** `crates/gpui_apple/src/shaders.metal:326-331` has the exact same `gradient_color` triangular dither, **but `shadow_fragment` (`crates/gpui_apple/src/shaders.metal:472-502`) has NO dithering**, so subtle dark drop shadows still band in 8-bit `BGRA8Unorm`. |
| **4. Perceptual luminance glyph dilation (`src/platform/mac/text_system.rs:274-305, 514-525`)** | Replaces constant black-on-white CoreGraphics glyph rasterization (`set_gray_fill_color(0.0, 1.0)`) with 5 discrete dilation levels (`0..=4`) keyed in `RenderGlyphParams::dilation` based on perceptual luminance `(0.2126*R + 0.7152*G + 0.0722*B).sqrt()`, calling `cx.set_should_smooth_fonts(true)` + `cx.set_gray_fill_color(luminance, 1.0)` so light text on dark backgrounds gets native macOS stem thickening. | **MERGED in upstream Zed.** Present verbatim in `crates/gpui_apple/src/text_system.rs:265-274, 510-516` (`glyph_dilation_for_color`, `params.dilation as f64 * 0.25`). |

---

## 2. Element / View / Entity Model (`crates/gpui/src/{app.rs,view.rs,element.rs,window.rs}`)

### 2.1 Ownership & the `EntityMap` Lease Model (`crates/gpui/src/app/entity_map.rs`)
GPUI solves Rust's single-owner graph problem by making `App` the sole owner of all stateful models and views (`crates/gpui/src/_ownership_and_data_flow.rs:1-40`):
- `EntityMap` (`crates/gpui/src/app/entity_map.rs:56-60`) stores `entities: SecondaryMap<EntityId, Box<dyn Any>>` alongside `ref_counts: Arc<RwLock<EntityRefCounts>>` (a `SlotMap<EntityId, AtomicUsize>`).
- `Entity<T>` is a lightweight handle (`EntityId` + `Weak<RwLock<EntityRefCounts>>` + `PhantomData<T>`) that increments/decrements the atomic refcount on `Clone`/`Drop` and pushes zero-refcount IDs onto `dropped_entity_ids` (`crates/gpui/src/app/entity_map.rs:190-209`).
- **How `entity.update(cx, |this, cx| ...)` works:** `EntityMap::lease` (`crates/gpui/src/app/entity_map.rs:138-167, 219-227`) **physically removes** `Box<dyn Any>` from `self.entities` (`self.entities.remove(entity_id)`), moves it onto the call stack inside a `Lease<T>`, runs the closure with `(&mut T, &mut Context<T>)`, and re-inserts it in `end_lease` (`self.entities.insert(entity_id, entity)`).
- **The Re-Entrancy Footgun (`double_lease_panic`):** Because `lease` removes the `Box<dyn Any>` from `SecondaryMap`, *any* attempt to `.read(cx)` or `.update(cx)` that same `Entity<T>` while its `update` closure is still on the call stack hits `None` and panics at runtime (`crates/gpui/src/app/entity_map.rs:230-233`):
  ```rust
  panic!("cannot {operation} {entity_type} while it is already being updated")
  ```
  While `Context::notify()` and `Context::emit()` queue effects onto `App::pending_effects` (`VecDeque<Effect>`) and flush them in a loop only when the outermost `App::update` finishes, synchronous callbacks (such as window activation/appearance callbacks, focus listeners, or helper methods passed an `&mut App` that also hold an `Entity<Self>`) routinely trigger this panic. Note `crates/gpui/src/window.rs:1992-2000` where Zed itself had to spawn an async task on `foreground_executor` inside `on_appearance_changed` specifically to avoid a synchronous App borrow panic!

### 2.2 Views (`Render`), Components (`RenderOnce`), and the 3-Phase `Element` Pipeline
In GPUI (`crates/gpui/src/view.rs:197-247`, `crates/gpui/src/element.rs:1-520`):
- **Stateful Views (`impl Render for MyView`):** Hold persistent state in `Entity<MyView>`. Calling `cx.notify()` marks the `EntityId` dirty in `window.dirty_views` (`crates/gpui/src/window.rs:2265-2275`).
- **Stateless Components (`#[derive(IntoElement)] impl RenderOnce for MyButton`):** Consumed by value (`fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement`) each frame.
- **3-Phase `Element` Trait (`crates/gpui/src/element.rs:50-145`):**
  1. **`request_layout(&mut self, global_id, inspector_id, window, cx) -> (LayoutId, Self::RequestLayoutState)`:** Builds the Taffy layout tree (`TaffyLayoutEngine`, `crates/gpui/src/taffy.rs:31-107`, which wraps `taffy::TaffyTree` with `taffy.disable_rounding()` and custom pixel-snapping in `compute_layout`). Views call `Render::render` during this phase (or during `prepaint` if cached).
  2. **`prepaint(&mut self, global_id, inspector_id, bounds, request_layout, window, cx) -> Self::PrepaintState`:** Runs **after** Taffy solves `compute_layout` at the root (`crates/gpui/src/window.rs:3721-3728`). Every element now knows its resolved `Bounds<Pixels>`. Elements register `Hitbox`es (`window.insert_hitbox`), push `DispatchNode`s into `DispatchTree` for keyboard/action routing, build `AccessKit` nodes if a screen reader is active (`crates/gpui/src/element.rs:428-463`), and shape text lines.
  3. **`paint(&mut self, global_id, inspector_id, bounds, request_layout, prepaint, window, cx)`:** Runs after `self.mouse_hit_test = self.next_frame.hit_test(self.mouse_position)` (`crates/gpui/src/window.rs:3758`). Because hit-testing happens *between* `prepaint` and `paint`, elements can query `hitbox.is_hovered(window)` during `paint` and emit hover colors/cursors on the same frame without a 1-frame lag! Elements push primitives into `window.next_frame.scene` and register mouse listeners (`window.on_mouse_event`).
- **`ElementArena` (`crates/gpui/src/arena.rs`, `window.rs:395-476`):** `AnyElement` boxes are bump-allocated into a per-`App` `Arena` (cleared at the end of `window.draw(cx)` via `arena_clear_needed.clear(cx)`, `crates/gpui/src/window.rs:1910-1912`) so constructing thousands of `Div`/`Svg`/`Canvas` elements per frame does not hit the system allocator.
- **Cross-frame Element State (`GlobalElementId`):** Elements with an explicit `.id(...)` push onto `window.element_id_stack` (`ElementIdStack`, `crates/gpui/src/window.rs:1023`), forming a path-hashed `GlobalElementId` (`crates/gpui/src/element.rs:250-285`). `window.with_element_state::<S, _>(global_id, ...)` moves state from `rendered_frame.element_states` (`FxHashMap<(GlobalElementId, TypeId), ElementStateBox>`) into `next_frame.element_states` (`crates/gpui/src/window.rs:1203-1210`). Unvisited state is automatically dropped at frame end (immediate-mode GC).
- **View Caching (`Entity::cached`, `crates/gpui/src/view.rs:258, 484-643`):** Opt-in only (`entity.cached(style)`), requiring a caller-supplied definite `StyleRefinement` because GPUI skips calling `Render::render` during `request_layout`! During `prepaint_view` (`view.rs:546-561`), if `bounds`, `content_mask`, and `text_style` match the previous frame and `!window.dirty_views.contains(&entity_id)`, GPUI calls `window.reuse_prepaint` and `window.reuse_paint` (`crates/gpui/src/window.rs:4014-4123`), which slice-clones hitboxes, mouse listeners, dispatch tree nodes, line layouts, and `Scene` primitives from `rendered_frame` into `next_frame`. **Pain point:** Notice comments at `window.rs:3463-3500` and `3893-3900`—because `reuse_prepaint`/`reuse_paint` rely on fragile index ranges (`Range<PrepaintStateIndex>`, `Range<PaintIndex>`) into flat `Frame` vectors, any mutation that shifts vector indices (such as nested `deferred_draws` or `.pop()` on `input_handlers`) corrupts cached subtrees and panics!

### 2.3 Focus, Actions, and Keybinding Dispatch (`crates/gpui/src/key_dispatch.rs`)
- **`FocusHandle` & `FocusId` (`crates/gpui/src/window.rs:543-726`):** Ref-counted slotmap key owned by `App::focus_handles`. During `prepaint`, `.track_focus(&focus_handle)` attaches a `FocusId` to the current `DispatchNode` in `DispatchTree` (`crates/gpui/src/key_dispatch.rs:220-224`).
- **`DispatchTree` & `KeyContext` (`crates/gpui/src/key_dispatch.rs:71-250`):** Every frame builds a tree of `DispatchNode`s mirroring the element hierarchy. Each node can hold a `KeyContext` (e.g., `"Editor && mode == full"`), `focus_id`, `key_listeners`, and `action_listeners` (`TypeId` → closure).
- **Capture & Bubble Phases:** When a keystroke arrives, `DispatchTree::dispatch_key` computes the `focus_path` from the root `DispatchNode` down to the focused `FocusId`, matches single-key or multi-key chord sequences (`"cmd-k left"`) against the `Keymap` from deepest context to root, and dispatches the resulting `Action` (`Capture` phase top-down, then `Bubble` phase bottom-up).

---

## 3. Text & Layout System (`crates/gpui/src/text_system.rs`, `sum_tree`, `rope`, `text`, `multi_buffer`)

### 3.1 Platform Shaping & Rasterization Backends
GPUI abstracts platform text shaping behind `PlatformTextSystem` (`crates/gpui/src/platform.rs`):
1. **macOS & iOS (`AppleTextSystem`, `crates/gpui_apple/src/text_system.rs:1-749`):**
   - Uses `font-kit` for font loading/metrics and Apple **CoreText** (`CTLine::new_with_attributed_string`, `crates/gpui_apple/src/text_system.rs:540-636`) for shaping.
   - **Ligature-breaking trick across syntax runs (`crates/gpui_apple/src/text_system.rs:567-579`):** Because CoreText shapes an entire `CFAttributedString` at once and will form ligatures across adjacent syntax tokens if their attributes are identical, GPUI alternates `px(f32::from(font_size).next_up())` (1 ULP float increment!) on adjacent `FontRun`s (`break_ligature = !break_ligature`) so CoreText breaks ligatures at token boundaries (e.g. when a cursor or selection splits `!=`).
   - **Subpixel positioning & Dilation (`crates/gpui_apple/src/text_system.rs:480-537`):** Rasterizes glyphs into an 8-bit grayscale `CGContext` (or 32-bit RGBA premultiplied for color emojis, swapped to straight-alpha BGRA) with `SUBPIXEL_VARIANTS_X = 4, SUBPIXEL_VARIANTS_Y = 1` (`crates/gpui/src/text_system.rs:49-52`), shifting the glyph origin by `subpixel_variant / 4.0` device pixels and applying 5-step luminance dilation (`0.0..=1.0` gray fill color with `set_should_smooth_fonts(true)`).
2. **Linux & Web (`CosmicTextSystem`, `crates/gpui_wgpu/src/cosmic_text_system.rs:1-200`):**
   - Uses `cosmic-text` (`FontSystem`, `ShapeLine`, `ShapeBuffer` — which uses `rustybuzz` / HarfBuzz under the hood) for shaping and `swash` (`ScaleContext`) for subpixel-positioned glyph/emoji rasterization.
   - On `gpui_web` (`crates/gpui_web/src/text_system.rs:1-200`), `WebTextSystem` wraps `CosmicTextSystem` (loaded with embedded fonts) and adds a **Canvas2D fallback** (`CanvasFontFallback`) that measures and rasterizes missing CJK/emoji glyphs via the browser's native `<canvas>` 2D context using system fonts!
3. **Windows (`DirectWriteTextSystem`, `crates/gpui_windows/src/direct_write.rs:1-200`):**
   - Uses `IDWriteFactory5`, `IDWriteTextAnalyzer1`, and `IDWriteFontFallback` for shaping, ClearType subpixel rendering with gamma/contrast tables (`DirectXRenderer::get_font_info`, `directx_renderer.rs:32-37`), and a dedicated Direct3D 11 shader pass (`ShaderModule::EmojiRasterization`, `direct_write.rs:63-165`) to rasterize layered COLRv0/COLRv1/DirectWrite color glyphs into the atlas.

### 3.2 `LineLayoutCache` & Zero-Allocation Hash Lookups (`crates/gpui/src/text_system/line_layout.rs:458-560`)
- `LineLayoutCache` maintains two `FrameCache` maps (`previous_frame` and `current_frame`). Every shaped line is looked up in `current_frame`; on miss, it checks `previous_frame`, moves the `Arc<LineLayout>` into `current_frame` if found, or calls `platform_text_system.layout_line(...)` on miss. At `finish_frame()`, `previous_frame` is replaced by `current_frame`, automatically evicting any line layout that wasn't used in the frame just drawn.
- **`shape_line_by_hash` (`crates/gpui/src/text_system.rs:689-720`):** Added in recent Zed so the editor can hash rope chunks + `FontRun`s and hit `lines_by_hash` *without* allocating a contiguous `SharedString` per visible line on every frame.

### 3.3 Zed's Rope, `SumTree`, CRDT `Buffer`, `MultiBuffer`, and `DisplayMap`
Zed's text architecture is a layered tower of immutable, persistent, `Arc`-shared B+ trees (`crates/sum_tree/src/sum_tree.rs:1-200`):

```
[ DisplayMap ]      ← Highlighting + BlockMap (diagnostics/creases)
      ↑
[ WrapMap ]         ← Soft-wrapping transforms (SumTree<WrapTransform>)
      ↑
[ TabMap ]          ← Hard-tab expansion transforms
      ↑
[ FoldMap ]         ← Code folding transforms (SumTree<FoldTransform>)
      ↑
[ InlayMap ]        ← LSP inlay hints injected without mutating buffer
      ↑
[ MultiBuffer ]     ← Excerpts from 1..N buffers (SumTree<Excerpt>) — powers project search & split diffs
      ↑
[ text::Buffer ]    ← CRDT operation log, Lamport anchors, UndoMap, SumTree<Fragment>
      ↑
[ rope::Rope ]      ← SumTree<Chunk> (128-byte ArrayString + 4x u128 bitmaps)
```

1. **`SumTree<T: Item>` (`crates/sum_tree/src/sum_tree.rs:18`):** A copy-on-write B+ tree (`Arc<Node<T>>`) with `TREE_BASE = 6` (up to $2 \times 6 = 12$ child pointers or leaf items per node). Every internal node caches a `Summary` (monoid with `zero()` and `add_summary()`) for each child. A single `SumTree` can be indexed and binary-searched (`cursor.seek(&target, Bias::Left)`) along **any `Dimension<'a, S>`** derived from `S` in $O(\log_{12} N)$ time, and cloned in $O(1)$ (`Arc::clone`) for background threads!
2. **`rope::Rope` & `Chunk` (`crates/rope/src/{rope.rs,chunk.rs}`):**
   - `Rope` is simply `SumTree<Chunk>` (`crates/rope/src/rope.rs:26-28`).
   - Each leaf `Chunk` (`crates/rope/src/chunk.rs:9-33`) holds an inline `ArrayString<128, u8>` (`MAX_BASE = 128`, `MIN_BASE = 64`) plus **four `u128` bitmaps** (1 bit per byte of the 128-byte chunk!):
     - `chars: u128` — bit $i$ is 1 iff `text[i]` is a UTF-8 code point start byte.
     - `chars_utf16: u128` — bit $i$ is 1 for every UTF-8 start byte, *plus* bit $i+1$ is 1 for 4-byte UTF-8 sequences ($\ge 240$, which take 2 UTF-16 code units). Thus `(chars_utf16 & mask).count_ones()` gives the exact UTF-16 offset in 1 CPU instruction!
     - `newlines: u128` — bit $i$ is 1 iff `text[i] == b'\n'`.
     - `tabs: u128` — bit $i$ is 1 iff `text[i] == b'\t'`.
   - Each node summarizes `TextSummary { len: usize, chars: usize, len_utf16: OffsetUtf16, lines: Point, first_line_chars: u32, last_line_chars: u32, last_line_len_utf16: u32, longest_row: u32, longest_row_chars: u32 }`. Converting between byte offset, `Point(row, column)`, and LSP `PointUtf16(row, utf16_col)` descends the `SumTree` to the 128-byte `Chunk` and uses bitwise mask + `count_ones()` (`popcnt`) / `leading_zeros()` (`lzcnt`)—no linear byte scanning.
3. **`text::Buffer` & `Anchor` (`crates/text/src/{text.rs,anchor.rs}`):**
   - Implements an operation-based CRDT (`BufferSnapshot`, `crates/text/src/text.rs:113-124`) backed by `visible_text: Rope`, `deleted_text: Rope`, `fragments: SumTree<Fragment>`, and `insertions: SumTree<InsertionFragment>`.
   - `Anchor` (`Lamport` timestamp + offset within insertion fragment + `Bias`) survives arbitrary concurrent local and remote edits and resolves back to a byte/`Point` offset in $O(\log N)$ via `fragments`.
4. **`multi_buffer::MultiBuffer` (`crates/multi_buffer/src/multi_buffer.rs:74-93`):**
   - Presents either a single `Buffer` (`singleton: true`) or a `SumTree<Excerpt>` stitching slices from multiple `Buffer`s into one unified coordinate space (`MultiBufferRow`, `MultiBufferPoint`, `Anchor`). Every Zed editor view—single file, multibuffer project search results, inline git diff hunks, and split diffs—renders a `MultiBuffer`.
5. **`editor::DisplayMap` (`crates/editor/src/display_map.rs:1-145`):**
   - A 5-layer pipeline (`InlayMap` → `FoldMap` → `TabMap` → `WrapMap` → `BlockMap`) where each layer is a `SumTree<Transform>` with `TransformSummary { input: TextSummary, output: TextSummary }`. Incrementally synced via `Patch<T>` (`Edit<InputDim>` → `Edit<OutputDim>`), allowing soft-wrapping (`WrapMap` runs asynchronously on a background thread when lines exceed a threshold), code folding, inlay hints, and inline diagnostic/AI blocks without mutating the underlying `MultiBuffer`.

### 3.4 IME & Accessibility Integration
- **macOS `NSTextInputClient` (`crates/gpui_macos/src/window.rs:236-270, 3368-3587`):** Implemented via Objective-C runtime class registration on `GPUIView`, forwarding `insertText:replacementRange:`, `setMarkedText:selectedRange:replacementRange:`, `unmarkText`, `firstRectForCharacterRange:actualRange:`, and `characterIndexForPoint:` to the active element's `PlatformInputHandler` (registered during `paint` via `window.handle_input(&focus_handle, handler, cx)`).
- **iOS `UITextInput` (`crates/gpui_ios/src/ios/text_input.rs:129-200`):** Implemented via a hidden responder `GPUITextInputView` subclassing `UIView` with custom `GPUITextPosition`, `GPUITextRange`, and `GPUITextSelectionRect` classes.
- **Web `ImeMirror` (`crates/gpui_web/src/window.rs:57, 70-86`):** Synchronizes a hidden DOM `<textarea>` (`ImeMirror`) with the canvas caret.
- **Accessibility (`crates/gpui/src/window/a11y.rs`, `crates/gpui/src/_accessibility.rs`):** Recently added `AccessKit` integration (`accesskit::TreeUpdate` built lazily during `prepaint` only when `window.a11y.is_active()` is true, `crates/gpui/src/window.rs:3691-3694, 3790-3815`, using `GlobalElementId::accesskit_node_id()`, `crates/gpui/src/element.rs:253-255, 428-463`).

---

## 4. Animation, Platform Layer, Hot Reload, and Real-World Failure Modes

### 4.1 Animation Architecture (`crates/gpui/src/elements/animation.rs`, `spring.rs`)
- **In `gpui 0.2.2` (`kitlangton/gpui`):** Only time-based `Animation { duration, oneshot, easing: Rc<dyn Fn(f32) -> f32> }` (`ease_in_out`, `cubic_bezier`, `pulsating_between`, `bounce`).
- **In Zed `5447804` (`crates/gpui/src/spring.rs:1-214`):** Upstream Zed added an analytic underdamped/critically-damped/overdamped harmonic oscillator solver (`SpringConfig { stiffness, damping, mass }`, presets `SpringConfig::snappy()` = stiffness 400 / damping 30, `bouncy()` = 300 / 15, `smooth()` = 200 / 25) and `SpringAnimation<T: Interpolatable>` (`f32`, `Pixels`, `Point<Pixels>`, `Size<Pixels>`, `Hsla`).
- **Why GPUI's animation model is still fundamentally limited:**
  1. **Immediate-mode subtree cloning on every tick:** Look at `AnimationElement::request_layout` (`crates/gpui/src/elements/animation.rs:208-246`) and `SpringElement::request_layout` (`crates/gpui/src/spring.rs:410-465`). On every frame while an animation is active, GPUI clones the underlying element (`self.element.clone()`), calls the user's animator closure `animator(element, delta)`, schedules `window.request_animation_frame()`, and runs full **Taffy `request_layout` → `prepaint` → `paint` → full-window `Scene` sort → GPU clear & redraw**.
  2. **No velocity preservation on retargeting:** `SpringAnimation` (`crates/gpui/src/spring.rs:165-195`) computes position analytically from `(from, to, start: Instant)` with zero initial velocity ($\dot{x}(0) = 0$). If `to` changes mid-flight (interruptible UI interaction), resetting the spring discards current velocity $\dot{x}(t)$ instead of integrating or solving the initial-value problem with non-zero $v_0$.
  3. **Main-thread bound:** Because animations run inside `Window::draw` via `next_frame_callbacks` (`crates/gpui/src/window.rs:1878-1887`), any main-thread hitch (even 8.4 ms on a 120 Hz ProMotion display) drops frames across every animation in the window.

### 4.2 Why the Launcher and Scrawl Had to Bypass GPUI
Kit's own production apps (the launcher and `scrawl`) provide the most concrete empirical evidence of where GPUI breaks down:

#### Case Study A: The launcher (`launcher/docs/{motion.md,perf-log.md}`)
The launcher originally built its panel and compact pill morph in GPUI (`kitlangton/gpui`) and hit two walls:
1. **Six structural motion/compositing defects (`launcher/docs/motion.md:20-75`):**
   - *Two competing coordinate spaces:* Moving/resizing the `NSWindow` via AppKit (`window.resize()`) while rendering the inner pill inside GPUI's `Scene` caused 1-frame desynchronization ("two systems fighting over the same pixels").
   - *Top-left anchor jump on collapse:* Switching from the expanded 680×424 window to the compact 100×36 pill caused the pill to render at `(0,0)` (top-left of the large window) for 1–2 frames before `NSWindow::setFrame:` took effect.
   - *Hard content pop & background snap:* Because GPUI cannot cross-fade two retained view snapshots as offscreen textures with independent scale/blur transforms, the search bar and results list popped out in 1 frame (`Frame 12 → Frame 13`), and background alpha snapped (`0.95 → 0.30`).
   - *Fix in the launcher:* The launcher replaced the GPUI morph and launcher panel with native AppKit/SwiftUI (`launcher/src/native_ui.rs:1-99`) using a single `NSGlassEffectView` / `CAMetalLayer` driven by `CASpringAnimation` in `WindowServer` at a locked 120 Hz with zero per-frame Rust/CPU participation.
2. **Measured GPUI CPU frame times & cold-glyph spikes (`launcher/docs/perf-log.md:1-145`):**
   - Steady-state GPUI `Window::draw` CPU frame time (cached): **2.65 ms** before optimization, **1.46 ms** after removing per-row `SyncList` re-measurement (**641 µs/frame** wasted re-measuring row 0) and `TextInput` re-layout (**266 µs/frame**).
   - **Cold glyph/emoji rasterization spikes:** Opening the Emoji grid in GPUI caused **11.1–18.4 ms** frame spikes (2.2× the 8.33 ms 120 Hz budget) because `AppleTextSystem::rasterize_glyph` (`crates/gpui_apple/src/text_system.rs:435-537`) synchronously allocates a `CGContext`, renders the `CTFont` glyph on the CPU main thread, and uploads to `MetalAtlas` during `paint`.
   - **Memory footprint:** Even an idle GPUI window with `MetalRenderer` (3× 2 MB `InstanceBuffer`s + `MetalAtlas` pages + font caches) consumed **78.8 MB RSS** at launch (`launcher/docs/perf-log.md:135`).

#### Case Study B: Scrawl (`scrawl/src/native/pill/mod.rs`, `scrawl/perf/start-latency.md`)
Scrawl's HUD pill needs a liquid-glass capsule (`NSGlassEffectView`), an offscreen Gaussian-blurred waveform glow (`glow_a`, `glow_b` intermediate `RGBA8Unorm` textures), and 120 Hz spring animations:
- GPUI's `Scene` cannot express multi-pass offscreen blur (`Pass 1: Clear glow_a` → `Pass 2: H-blur glow_a -> glow_b` → `Pass 3: Composite V-blurred glow_b + SDF shapes + R8 atlas text onto drawable`, `scrawl/src/native/pill/mod.rs:11-23`).
- Scrawl replaced GPUI's `HudPillWindow` with a 440-line native `NSPanel` + `CAMetalLayer` + `CVDisplayLink` module (`scrawl/src/native/pill/{mod.rs,model.rs,atlas.rs,pill.metal}`).
- **Measured startup win (`scrawl/perf/start-latency.md:26-35`):** Deferring `NSPanel`/`CAMetalLayer`/pipeline creation until the pill is shown cut `HudPillHandle::spawn()` cold-start latency from **6.93 ms** to **0.08 ms** (an **86×** reduction), and the display link fully stops (`CVDisplayLinkStop`) when idle for **0% CPU/GPU usage**.

### 4.3 Mobile (`gpui_ios`), Web (`gpui_web`), Hot Reload (`subsecond`), and Compile Times
- **`gpui_ios` (`crates/gpui_ios/src/ios/{window.rs,text_input.rs}`, 3,684 LoC):** Uses `objc2-ui-kit` (`UIWindow`, `UIViewController`, `GPUIMetalView` with `CAMetalLayer`, `CADisplayLink`, `UIPanGestureRecognizer`, `UIScreenEdgePanGestureRecognizer` for back-swipe, and `GPUITextInputView` implementing `UITextInput`). Still early: `TouchEvent` in `crates/gpui/src/interactive.rs:114-117` explicitly notes *"Dispatch contract (core implementation pending)"*.
- **`gpui_web` (`crates/gpui_web/src/{window.rs,text_system.rs}`, 7,866 LoC):** Runs GPUI in WASM on a full-bleed `<canvas>` using `gpui_wgpu` (`WebGPU` or `WebGL2`), `ResizeObserver`, `requestAnimationFrame`, `ImeMirror`, and `WebTextSystem` (`CosmicTextSystem` + `<canvas>` 2D fallback for system CJK/emoji fonts).
- **Hot Reload (`crates/gpui/src/hot_reload.rs:1-82`, `crates/gpui/src/view.rs:344-352`):** Compiled when `--cfg gpui_hot_reload` is active. Integrates Dioxus's **`subsecond`** binary hot-patching crate (`dx serve --hot-patch`). Every `ViewElement` render passes through `subsecond::HotFn::current(render_view_impl::<V>).call((view, window, cx))` (`crates/gpui/src/view.rs:351`), and when `subsecond` applies a jump-table patch, `hot_reload::register` marks all views in all windows dirty and calls `window.refresh()`.
- **Compile-time & Dependency Weight:** `gpui` + platform crates total **174,157 LoC** and pull in heavy transitive dependencies (`taffy`, `lyon`, `resvg`/`usvg`, `etagere`, `cosmic-text`/`swash`/`fontdb`, `font-kit`, `accesskit`, `wgpu`/`naga`, `objc2-*`/`windows`, `blade-macros`). Clean builds of `gpui` take ~25–45s on Apple Silicon M-series Max/Ultra, and incremental links without `subsecond` or `lld`/`wild` take 2–6s.

---

## 5. Warp's UI Framework (`warpui_core` + `warpui`)

### 5.1 Shared Genealogy with GPUI & Open-Source Status
In Warp's July 2021 engineering post (["How Warp Works"](https://www.warp.dev/blog/how-warp-works)), Warp explained that they partnered with **Nathan Sobo** (before he left to co-found Zed) to build a Flutter-inspired Rust UI framework on top of Metal and `SumTree`. As of 2026, `warpdotdev/warp` (`f8a9c71`) is open-source on GitHub: the terminal application is AGPL-3.0, while **`crates/warpui_core` (91,602 LoC) and `crates/warpui` (38,862 LoC) are MIT-licensed** (`crates/warpui_core/Cargo.toml:7`, `crates/warpui/Cargo.toml:7`).

Studying `warpui_core` side-by-side with `gpui` reveals that **`warpui` is the evolutionary sibling of GPUI v1**:
- Both share `App` / `AppContext`, `EntityId`, `ViewHandle<V>` / `ModelHandle<M>`, `Action`, hierarchical `Keymap` context matching, `Scene`, and `sum_tree`.
- Where Zed rewrote GPUI v1 into GPUI v2 (replacing Flutter constraints with Taffy flexbox and replacing retained `Box<dyn Element>` trees with 3-phase arena-allocated immediate/retained elements), Warp kept and evolved the original `Presenter` architecture.

### 5.2 How `warpui` Differs from GPUI v2
1. **Retained `Box<dyn Element>` per View (`Presenter`, `crates/warpui_core/src/presenter.rs:28-37, 318-381`):**
   - `Presenter` holds `rendered_views: EntityIdMap<Box<dyn Element>>`.
   - When a view is invalidated, `Presenter::invalidate` calls `app.render_view(window_id, view_id)` **only** on the invalidated `view_id`s and replaces their `Box<dyn Element>` in `rendered_views` (`presenter.rs:318-331`). Unchanged child views are embedded via `ChildView<T>` (`presenter.rs:282-287`) and never re-run `View::render`!
   - Layout is single-pass Flutter-style box constraints (`Element::layout(&mut self, constraint: SizeConstraint, ctx: &mut LayoutContext, app: &AppContext) -> Vector2F`, `crates/warpui_core/src/elements/gui/mod.rs:109-115`), followed by `after_layout` and `paint`.
2. **Thread-Local Automatic Dependency Tracking (`Tracked<T>`, `crates/warpui_core/src/core/autotracking/mod.rs:1-200`):**
   - Unlike GPUI (where forgetting `cx.observe(&model, ...)` or `cx.notify()` is a constant source of stale-UI bugs), `warpui_core` provides `Tracked<T>`.
   - During `View::render`, `autotracking::render_view` sets a thread-local `CACHE.rendering_view = Some(View { window_id, view_id })` (`autotracking/mod.rs:174-192`).
   - Reading a `Tracked<T>` via `Deref` automatically records `(View, TrackedId)` in a bidirectional `HashMap`. Mutating a `Tracked<T>` via `DerefMut` automatically pushes all dependent `View`s into `CACHE.invalidations`, which `App` drains alongside manual `ctx.notify()` calls!
3. **3-Primitive Renderer (`Rect`, `Image`/`Icon`, `Glyph`) + `rstar::RTree` Hit Map (`crates/warpui_core/src/scene.rs:29-123`):**
   - `warpui` has **no** vector path tessellator (`lyon`) and **no** separate shadow or underline pipeline. Instead, `Rect` (`crates/warpui/src/platform/mac/rendering/metal/shaders/shaders.metal:9-43, 117-200`) evaluates drop shadow (4-step Gaussian `erf`), per-corner radius, per-edge solid/dashed border, and linear gradient inside a **single uber-fragment shader** (`rect_fragment_shader`)!
   - Icons and SVGs are rasterized once via `resvg` into `TextureCache` and drawn via `image_fragment_shader`.
   - Glyphs (`glyph_fragment_shader`) support horizontal alpha fades (`GlyphFade::Horizontal { start, end }`, `scene.rs:78-83`) so overflowing terminal/tab text fades out smoothly in the shader without extra mask layers.
   - Each `Layer` in `Scene` builds an `rstar::RTree<Rectangle>` (`scene.rs:31`) of opaque hit rects so `EventContext::is_covered(point)` and `visible_rect(origin, size)` can test z-layer occlusion during event dispatch (`presenter.rs:246-258`).
   - **Weakness:** `warpui`'s glyph atlas (`crates/warpui/src/rendering/atlas/allocator.rs:13-55`) is a naive shelf-next-fit allocator (`ATLAS_SIZE = 1024`) with **zero eviction**—when `GlyphConfig` changes it blows away all textures (`glyph_cache.rs:80-86`), otherwise it allocates new 1024×1024 `MTLTexture`s forever.
4. **Dual GUI + Ratatui TUI Runtime (`crates/warpui_core/src/presenter/tui.rs:1-150`):**
   - Enabled via `--features tui` (`ratatui = "0.30"`). Allows the exact same `App`, `Model`, `EntityId`, `Action`, and `Keymap` core to render either to a GPU window (`Presenter` → `Scene`) or to a terminal screen (`TuiPresenter` → `TuiFrame` cell buffer).
5. **Platform Split:**
   - macOS uses custom AppKit + raw Metal (`crates/warpui/src/platform/mac/rendering/metal/renderer.rs:1-200`), reporting **1.9 ms average redraw time (>144 FPS on 4K)** in Warp's telemetry.
   - Linux, Windows, and WASM use `winit` + `wgpu` + `cosmic-text`/`swash` (`crates/warpui/src/windowing/winit/` and `crates/warpui/src/rendering/wgpu/renderer.rs:1-145`).

---

## 6. What to Steal vs. What to Avoid

### Steal (With Exact File Citations)
1. **Zed's `sum_tree` + `rope` (`Chunk` with 4× `u128` bitmaps):**
   - *Source:* `zed:crates/sum_tree/src/sum_tree.rs:18-200` and `zed:crates/rope/src/{rope.rs,chunk.rs:9-102}`.
   - *Why:* Self-contained (7,765 LoC combined for `sum_tree` + `rope`), zero GPUI dependency, $O(1)$ clone (`Arc`), $O(\log_{12} N)$ multi-dimensional seek across byte/char/UTF-16/`Point(row, col)`, and hardware-accelerated `popcnt`/`tzcnt` within 128-byte leaf chunks. Use it directly or vendor it verbatim for Rune's text buffers and virtualized list height prefix sums.
2. **`BoundsTree` Spatial Z-Order Assignment for Instanced Batching:**
   - *Source:* `zed:crates/gpui/src/bounds_tree.rs:14-180` and `zed:crates/gpui/src/scene.rs:171-206`.
   - *Why:* A ~200-line 4-ary R-tree that assigns each primitive `max_overlapping_order + 1` instead of a global counter. This turns painter's-algorithm trees (e.g. 100 list rows each drawing a background quad + icon + text) into 3 instanced GPU draw calls instead of 300.
3. **Kit's GPUI fork (`kitlangton/gpui`) (`2582af8`) Rendering & DisplayLink Fixes:**
   - *Source:* `kitlangton/gpui` commit `2582af8`:
     - Triangular blue-noise dithering in **both** gradients and Gaussian drop shadows (`src/platform/mac/shaders.metal:326-331, 495-503`).
     - Perceptual luminance-driven 5-step CoreText glyph dilation (`src/platform/mac/text_system.rs:274-305, 514-525`).
     - Explicit `kCGColorSpaceSRGB` (or `kCGColorSpaceExtendedSRGB` / `DisplayP3`) tagging on `CAMetalLayer` (`src/platform/mac/metal_renderer.rs:257-263`).
     - `CVDisplayLinkCreateWithCGDisplay` → `CVDisplayLinkCreateWithActiveCGDisplays` fallback (`src/platform/mac/display_link.rs:25-60`) + complete stop when idle (`scrawl/src/native/pill/mod.rs:310-340`).
4. **Two-Generation `LineLayoutCache` + `shape_line_by_hash` + Ligature-Breaking ULP Trick:**
   - *Source:* `zed:crates/gpui/src/text_system/line_layout.rs:458-560`, `zed:crates/gpui/src/text_system.rs:689-720`, `zed:crates/gpui_apple/src/text_system.rs:567-579`.
   - *Why:* Swapping `current_frame` and `previous_frame` `FxHashMap`s gives zero-overhead frame-to-frame layout retention and automatic eviction of offscreen lines without LRU bookkeeping, while `px(font_size.next_up())` cleanly breaks CoreText ligatures across syntax/selection boundaries.
5. **Hit-Testing Between `prepaint` (Layout Resolved) and `paint`:**
   - *Source:* `zed:crates/gpui/src/window.rs:3728-3765`.
   - *Why:* Running `hit_test(mouse_position)` immediately after layout/hitbox registration—before `paint`—means hover states and cursors are always 0-frame-lag accurate even when layout shifts under the cursor.
6. **Warp's `Tracked<T>` Automatic Dependency Tracking & `GlyphFade`:**
   - *Source:* `warp:crates/warpui_core/src/core/autotracking/mod.rs:1-200` and `warp:crates/warpui_core/src/scene.rs:78-89`.
   - *Why:* Automatic read-tracking during `render` eliminates forgotten `cx.notify()` / `cx.observe()` bugs; shader-level horizontal `GlyphFade` clips overflowing text with a smooth gradient at zero extra draw-call or mask-texture cost.
7. **Dioxus `subsecond` Jump-Table Hot Patching Hook at View Boundaries:**
   - *Source:* `zed:crates/gpui/src/hot_reload.rs:1-82` and `zed:crates/gpui/src/view.rs:344-352`.
   - *Why:* Wrapping view render functions in `subsecond::HotFn::current(...)` gives sub-second Rust UI hot-reloading while preserving live `App`/entity state.

### Avoid (With Concrete Reasons)
1. **Avoid GPUI's Full-Viewport 4x MSAA Intermediate Pass per `Path` Batch:**
   - *Where:* `zed:crates/gpui_apple/src/metal_renderer.rs:1100-1215`.
   - *Why:* Clearing and resolving a full-window 4x MSAA texture on every interleaved path batch destroys tile memory bandwidth. Either render vector shapes via analytical SDFs / Vello / slug-style compute shaders, or scissor-clip the MSAA resolve to the tight union of the batch's `Bounds`.
2. **Avoid GPUI's `EntityMap` Remove-on-Lease Pattern Without Granular State Splits:**
   - *Where:* `zed:crates/gpui/src/app/entity_map.rs:138-233`.
   - *Why:* Leasing an entire view/model out of `SecondaryMap` guarantees runtime `double_lease_panic` crashes whenever nested callbacks, observers, or OS delegates fire synchronously. Fine-grained reactive signals/atoms or split state cells avoid whole-view lease panics.
3. **Avoid Animation-by-Element-Subtree-Rebuild (`AnimationElement` / `SpringElement`):**
   - *Where:* `zed:crates/gpui/src/elements/animation.rs:208-246` and `zed:crates/gpui/src/spring.rs:410-465`.
   - *Why:* Cloning the element subtree and re-running Taffy layout + full-window CPU/GPU draw on every 120 Hz tick wastes CPU and prevents compositor-level transform/opacity/filter animations. Springs must also preserve velocity $(\dot{x}_0 \ne 0)$ on retargeting.
4. **Avoid Lack of Offscreen Compositor Layers / Filter Passes:**
   - *Where:* `zed:crates/gpui/src/scene.rs:34-46`.
   - *Why:* Without offscreen layer targets (`RenderLayer` with group opacity, transform matrix, clip mask, Gaussian blur, and blend mode), you cannot build Scrawl-grade glass morphs, cross-dissolving view transitions, or glow passes without dropping down to raw `CAMetalLayer` / `CALayer`.
5. **Avoid Fragile Vector-Slice Index Caching (`reuse_prepaint` / `reuse_paint`):**
   - *Where:* `zed:crates/gpui/src/window.rs:4003-4123` and `zed:crates/gpui/src/view.rs:540-642`.
   - *Why:* Caching views by recording `(start_idx..end_idx)` slices into global per-frame vectors breaks whenever deferred draws or input handlers shift indices, and still forces the CPU to copy primitives and rebuild `BoundsTree` every frame. Retain per-view display lists (`RenderNode` / `Layer`) directly!
6. **Avoid Synchronous Main-Thread Glyph/Emoji Rasterization Without Prewarming/Async Staging:**
   - *Where:* `zed:crates/gpui_apple/src/text_system.rs:435-537` (`launcher/docs/perf-log.md:72-97`).
   - *Why:* Cold CoreText/CGContext rasterization of 50+ emojis stalls the main thread for **11–18 ms**, blowing past the 8.33 ms 120 Hz budget.

---

## 7. Recommendation for Rune

### Decisive Verdict: Build Fresh (Modular Crate Architecture) — Do NOT Fork GPUI or `warpui`

**Rune should NOT fork GPUI (neither `gpui 0.2.2` nor Zed HEAD `5447804`) and should NOT fork `warpui`.** Instead, Rune should **build a fresh, layered UI & rendering engine** while directly vendoring/adapting Zed's standalone text crates (`sum_tree`, `rope`) and porting ~1,500 lines of proven low-level algorithms (`BoundsTree`, `etagere` atlas + `LineLayoutCache`, and `kitlangton/gpui` `2582af8` shader/text refinements).

#### Why Starting Fresh Beats Forking GPUI
1. **The 4 architectural walls that forced the launcher and Scrawl off GPUI are load-bearing in GPUI's core:**
   - In GPUI, `Window` (`crates/gpui/src/window.rs`, 9,979 LoC) + `App` (`crates/gpui/src/app.rs`) + `Scene` (`crates/gpui/src/scene.rs`) are tightly coupled around a **single-pass, flat-vector, full-window-clear** frame loop.
   - Adding **retained compositor layers** (group opacity, 2D/3D layer transforms, offscreen blur/glow render passes like Scrawl's 3-pass HUD pill, and cross-fade view transitions like the launcher's morph), **damage-tracked partial redraws**, **velocity-preserving retargetable springs**, and **fine-grained reactive state that doesn't panic on re-entrant borrows** requires replacing `Scene`, `MetalRenderer::draw`, `ViewElementState`, `AnimationElement`, and `EntityMap`—i.e., rewriting the heart of GPUI while fighting 174,157 lines of Zed-specific coupling.
2. **Massive complexity and compile-time tax for features Rune doesn't want:**
   - Upstream GPUI is 174k LoC (and growing rapidly for Zed's specific IDE needs). Maintaining a fork of GPUI (as `kitlangton/gpui` already demonstrated) means either freezing on an old snapshot (`0.2.2`, losing `gpui_wgpu`, `gpui_ios`, `gpui_web`, `AccessKit`, `subsecond`) or rebasing against thousands of Zed-internal commits per month.
3. **The highest-leverage parts of Zed are already small and separable:**
   - `sum_tree` (3,327 LoC) + `rope` (4,438 LoC) have **zero** dependency on `gpui`.
   - `BoundsTree` is 240 LoC; `MetalAtlas` (`etagere`) is 405 LoC; `shaders.metal` (with `2582af8` fixes) is 910 LoC; `LineLayoutCache` is ~300 LoC. You can bring all of GPUI's best engineering into Rune in **<10,000 LoC** without inheriting the 165,000 LoC of baggage.

### Rejected Alternatives & Why
1. **Rejected Alternative 1: Forking `kitlangton/gpui` (`gpui 0.2.2` + `2582af8`)**
   - *Why rejected:* `gpui 0.2.2` is a 97k-LoC monolithic crate frozen before Zed's `gpui_apple`/`gpui_wgpu`/`gpui_ios`/`gpui_web`/`AccessKit`/`subsecond` work. It still suffers from the 11–18 ms cold emoji stalls, 78.8 MB idle RSS, `double_lease_panic` re-entrancy, lack of offscreen passes, and full-window redraws that forced the launcher and Scrawl to write native SwiftUI/Metal escape hatches.
2. **Rejected Alternative 2: Tracking Upstream Zed's `gpui` (`5447804`) as a Git Dependency or Rebased Fork**
   - *Why rejected:* Upstream `gpui` is not published as a stable independent product—its APIs churn weekly to serve Zed, it still lacks two of Kit's `2582af8` fixes (shadow dithering and `CVDisplayLink` fallback), and its flat `Scene` + `InstanceBuffer` architecture still cannot do offscreen blur/glow passes, compositor-driven morphs, or partial-damage rendering.
3. **Rejected Alternative 3: Forking Warp's `warpui_core` + `warpui` (`f8a9c71`, MIT)**
   - *Why rejected:* Although MIT-licensed and possessing nice ideas (`Tracked<T>`, 3-shader simplicity, Ratatui TUI backend), `warpui` is 130,464 LoC of 2021-era GPUI-v1 architecture with custom Flutter-style layout (no CSS Grid/Taffy), a naive shelf-next-fit glyph atlas with no eviction, no subpixel AA or luminance glyph dilation, and heavy Warp-specific telemetry/asset coupling.

---

## 8. Open Questions for Rune's Design

1. **Compositor Layer Hybrid vs. Pure GPU Scene:** Should Rune's window compositor use native `CALayer` / `CAMetalLayer` sublayers for window-level glass/morph surfaces (like the launcher and Scrawl proved on macOS via `NSGlassEffectView` + `WindowServer` `CASpringAnimation`) while running a multi-pass GPU scene graph inside each surface, or should Rune implement its own multi-pass offscreen layer compositor across all platforms?
2. **Vector Path Rendering Strategy:** Since GPUI's Lyon + 4x MSAA full-window intermediate pass is a major bottleneck and `warpui` skips arbitrary paths entirely, should Rune restrict UI primitives to analytical SDFs + `resvg` sprite caching, or adopt a tile-based GPU path rasterizer?
3. **Reactive State Model:** Should Rune adopt `warpui`-style automatic read tracking (`Tracked<T>` / fine-grained signals) so views never need manual `cx.notify()` and never hit `double_lease_panic` on re-entrant reads, or keep explicit entity handles with split read/write arenas?
4. **Text Buffer Scope:** Does Rune only need `sum_tree` + `rope` + single-buffer undo/anchors (~10k LoC), or does it also need Zed's full `multi_buffer` + 5-layer `DisplayMap` (`InlayMap` → `FoldMap` → `TabMap` → `WrapMap` → `BlockMap`) for code-editor-class views?

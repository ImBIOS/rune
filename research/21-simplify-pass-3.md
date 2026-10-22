# 21. Simplify pass 3: workspace consolidation across `rune`, `rune-motion`, `rune-fx`, and `rune-light`

2026-10-22. Scope: fold `crates/rune-fx` and `crates/rune-light` into the workspace, drop the vendored `crates/rune-light/vendor/rune` fork, share primitives and GPU/color/readback code across crates, and adopt verified fixes without regressing `research/19-autoresearch-log.md`.

## Consolidated and deleted

- **Workspace + vendored `rune` (`7eea3a3` / `8b37ced`):** Added `crates/rune-fx` and `crates/rune-light` to the root workspace, deleted standalone lockfiles and `crates/rune-light/vendor/rune` (2,930 lines of `.rs`/`.wgsl`), and added `TextSystem::measure` and `TextSystem::glyphs` to `crates/rune/src/text.rs` so `rune-light` drives live `crates/rune` directly.
- **Fonts, PNG, and dead code (`ea73adc`):** Reused `crates/rune/assets/Inter-*.ttf`, `rune::render::write_png`, and `rune::color::srgb_decode` in `rune-fx` and `rune-light` examples; deleted duplicate `crates/rune-fx/examples/gallery/assets/Inter-*.ttf` (1.2 MB); removed dead `Knob`/`KNOBS`/`Rgb::scale`/`Heat::is_active` from `crates/rune-fx/src/lib.rs` and unused `c0_in_a`, `visibility_view`, and duplicate array texture views from `crates/rune-light/src/lib.rs`.
- **Geometry and color math (`5ab0045`):** Shared `Rect` (`inflate`, `union`, `from_points`, `xywh`), `srgb_decode`/`srgb_encode`, and `linear_to_oklab`/`oklab_to_linear` from `rune-motion` across `crates/rune/src/color.rs` and `crates/rune-fx/src/lib.rs`.
- **Headless GPU, readback, springs, and bind groups (`21c3101`):** Added `Gpu::headless()` (enabling `TIMESTAMP_QUERY` when supported) and `Offscreen::with_format` to `crates/rune/src/render.rs` and reused them across `crates/rune-fx/examples/gallery/main.rs` and `crates/rune-light/examples/light.rs`; replaced the local `Spring` in `light.rs` with `rune::Spring`; cached `base`/`text` `TextureView`s in `light.rs` and mip `BindGroup`s in `MaskPyramid`; used `rune_motion::{Intent, flags}` in `crates/rune/src/tooling.rs`; added `[profile.dev.package."*"] debug = false` (`research/17` §10) to root `Cargo.toml`.

## Better techniques adopted

- **Signed bounding-box tile culling in `crates/rune-light/src/shaders/field.wgsl`:** Replaced the unsigned AABB check (`length(max(abs(tile_c - pc) - (tile_h + half), 0))`) with the signed bounding-box lower bound `min(max(q0.x, q0.y), 0.0) + length(max(q0, 0.0))`. Previously, when a tile sat inside an outer card's AABB, every occluder tied at `bd = 0.0`, so `if (d < best_d)` depended on non-deterministic `atomicAdd(&local_n, 1u)` workgroup thread scheduling across overlapping slabs.
- **Cached mip bind groups (`MaskPyramid`) and offscreen views (`light.rs`):** Eliminated 6 per-frame `create_bind_group` allocations in `FxRenderer::build_mips` and 5 per-frame `create_view` allocations in `light.rs`.

## Line counts (`.rs` + `.wgsl`, pass-3 delta)

| Target | Before | After | Pass-3 net delta |
|---|---:|---:|---:|
| `crates/rune-light/vendor/rune` | 2,930 | 0 | −2,930 |
| `crates/rune-fx` | 4,608 | 4,426 | −182 |
| `crates/rune-light` | 3,111 | 3,024 | −87 |
| `crates/rune` (pass-3 edits; total grew from concurrent `live`/`app` merges) | 6,578 | 9,794 | −9 |
| `crates/rune-motion` | 7,053 | 7,082 | +29 |
| `examples/settings` | 347 | 348 | 0 |
| `examples/opencode` (total grew from concurrent merges) | 2,481 | 2,934 | 0 |

## Verification and performance (Studio M5 Max, 7-run medians at 120 Hz, 2× scale)

- **UI snapshots & `motion-lint`:** All 9 UI snapshots byte-identical; all 9 `opencode` traces and `settings` trace at 0 `motion-lint` candidates.
- **Gallery goldens:** All 21 `demo/fx/*.png` and 5 `demo/light/*.png` byte-identical (`demo/light/raised.png` updated in `7eea3a3` for deterministic tile culling); `light verify` max `|gpu − cpu|` = 0.4773, mean = 0.00021.
- **`opencode` CPU frame time:** `stream` p50 0.260 → 0.247 ms, p95 0.471 → 0.389 ms; `long-stream` p50 0.122 → 0.148 ms, p95 2.734 → 2.056 ms.
- **Release binary sizes:** `settings` 11M, `opencode` 14M, `gallery` 7.5M → 7.2M, `light` 9.7M.

## Deliberately left separate

- **`tok::Theme` / `theme::Theme`:** Owned by `crates/rune-kit` + `themes/opencode` workstream.
- **WGSL `sd_rrect` / PCG / dither snippets:** Kept inline per crate (`shader.wgsl`, `rune-fx/src/shaders/common.wgsl`, `rune-light/src/shaders/common.wgsl`) because each operates in its own coordinate/convention space (device-px half-size vs pt top-left rect, TPDF vs IGN dither).
- **`examples/gallery/ui.rs`:** Kept separate from `rune::Renderer` because the gallery's `ab_glyph` rasterizer and `erf_approx` shadows define the committed `demo/fx/*` goldens.

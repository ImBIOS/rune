# 12. Rendering research and tricks for Rune (2D/UI GPU rendering, 2023-2026)

Scope: analytic AA and coverage, sparse strips, Slug, Rive feathering, blur and backdrop, glass lensing, SDF shapes,
GPU-driven UI, Apple TBDR, damage tracking, HDR, dithering, colour. Written 2026-10-21. Sources were fetched via search
snippets and (where quoted) real source; items marked *(from memory)* or *(estimate)* were not re-verified. Nothing was
built or measured here.

## Summary

- **Most of the PLAN §4.3 primitive list is already right.** Research mostly confirms it and sharpens details. Nothing
  found says "change the architecture".
- **Biggest new finds:** (1) `vello_cpu` is a fast, SIMD, multithreaded CPU renderer, so it can be the *reference
  renderer* tenet 7 asks for, for free. (2) Rive's *vector feathering* is an analytic Gaussian blur of arbitrary
  paths, and its "never blur above 16 px, scale up instead" rule applies to Rune's backdrop too. (3) Evaluating the
  closed-form springs *in the vertex shader* is a trick only a render-thread-owns-animation design can use.
- **Backdrop blur:** the Jimenez 13-tap down / 9-tap tent up pyramid is still the right tool (2014, still current).
  The new problem is temporal: an animated blur radius must be continuous, so blend between pyramid levels.
- **Glass in-app:** open-source "liquid glass" shaders (WebGL, Metal gist, a GPUI fork) all use the same recipe: SDF
  height field, normal, refraction offset, chroma split, Fresnel/specular, blurred copy. It is cheap *given* a
  backdrop copy. The copy is the expensive part, so scissor it to the glass bounds.
- **Slug:** patent dedicated to public domain 2026-03-17; MIT reference shaders now include dynamic dilation and drop
  the band-split and supersampling complications. Good for scale-animated text and icons.
- **Skip or defer:** GPU-driven indirect rendering (UI scenes are too small), partial present (power, not feel; Metal
  gives no buffer age), HDR UI, Apple tile shaders for blur (tiles cannot see neighbours).
- Exact-count perf gates (tenet 7) fit every technique below: all are countable (taps, passes, instances, bytes).

## Ranked ideas

### 1. `vello_cpu` as the CPU reference renderer (and `vello_gpu` as the complex-vector lane) — **adopt now / spike**

- Sources: [Stampfl thesis, ETH, Oct 2025](https://ethz.ch/content/dam/ethz/special-interest/infk/inst-pls/plf-dam/documents/StudentProjects/MasterTheses/2025-Laurenz-Thesis.pdf);
  [Vello ARCHITECTURE.md](https://cdn.jsdelivr.net/gh/linebender/vello@main/ARCHITECTURE.md);
  [Linebender Jul 2025](https://linebender.org/blog/tmil-19/), [Oct 2025](https://linebender.org/blog/tmil-22/);
  [sparse-strips v0.0.8](https://github.com/linebender/vello/releases/tag/sparse-strips-v0.0.8);
  [benchmarks](https://laurenzv.github.io/vello_chart/).
- What it is: paths flatten, bin into tiles, and become run-length "sparse strips" (coverage kept only at edges,
  solid runs stored compactly). One CPU pipeline (`vello_common`) feeds `vello_cpu` (SIMD via `fearless_simd`
  NEON/SSE4.2/WASM, optional multithreading) and `vello_gpu` (plain vertex/fragment, wgpu and WebGL2, no compute).
  Compute Vello moved to `research/`, matching PLAN. Measured (M1 Pro, Blend2D harness, preliminary): vello_cpu is
  second to Blend2D in many tests and often beats Skia and Cairo, more so on large geometry. It already has blurred
  rounded-rect paints, opacity layers, non-isolated blending, and early image filters including blur.
- Rune use: (a) the **reference path** from tenet 7 / §6b "same pixels on every platform": render the same scene
  description through vello_cpu and diff against the GPU lane (tolerance per primitive). (b) The deterministic
  headless lane for agents on machines with no GPU (CI, Linux boxes). (c) `vello_gpu` as the `SparseStrips`
  primitive for icons/illustrations. API is explicitly unstable; pin a version and wrap it.
- Cost/risk: alpha-quality, API churn, MSRV bumps. CPU AA differs slightly from GPU analytic AA, so diff tolerance
  must be calibrated per primitive, not zero. Verdict: adopt vello_cpu as oracle now; spike vello_gpu.

### 2. Evaluate analytic springs in the vertex shader — **spike** *(my idea; no source)*

- Rune already uses closed-form springs (position, velocity, target, t0, omega, zeta). Upload those few floats as
  instance data and evaluate `x(t)` in the vertex shader with a per-frame `time` uniform. While anything animates,
  the app thread does nothing, and the render thread rewrites only the uniform. Instance buffers become retained,
  rewritten only on retarget or tree change, instead of rebuilt per frame.
- Gains: zero CPU per animated node per frame (a list of 500 sliding rows costs the same as one); sample time is the
  display-link target timestamp, so late frames still land on the right pose; the GPU and CPU copies of the same
  function make a perfect differential test (render the same `t` on both).
- Caveats: hit-testing, clip tables, text positions and layout children need the CPU value too (same function, run
  lazily only when queried, e.g. on input). Float32 vs f64 drift: define springs relative to t0 so `t - t0` is small.
  Properties that feed other computations (shadow offset depends on position) need the shader to evaluate several
  springs per instance; budget a fixed number of tracks per instance (e.g. 4) in the layout of the instance struct.
- Verdict: spike, measuring encode CPU time with 1k animating nodes vs the baseline of per-frame instance rebuild.

### 3. Pyramid backdrop blur (Jimenez dual filter) with continuous radius — **adopt now**

- Sources: [Jimenez, SIGGRAPH 2014](https://www.iryoku.com/next-generation-post-processing-in-call-of-duty-advanced-warfare/);
  [three.js port with numbers](https://tobis.vision/notes/three-js-bloom-jimenez-dual-filter) (claims 9x faster than
  the stock bloom; a radius-22 Gaussian is 86 taps at one level vs 9 on the pyramid);
  [WGSL downsample with Karis average](https://docs.rs/crate/myth_render/latest/source/src/pipeline/shaders/entry/post_process/bloom/downsample.wgsl).
  Dual Kawase (ARM, SIGGRAPH 2015) is the cheaper 5-tap cousin, *(from memory)*.
- Mechanics: 13-tap downsample (weights 0.125/0.0625/0.03125, sum 1), 3x3 tent upsample added into the finer level,
  Karis luminance weighting on the first level only for stability against sparkle. Wide blur comes from depth of the
  pyramid, not kernel width.
- Rune use for the `Backdrop` primitive: (a) scissor everything to the union of glass bounds plus the blur margin;
  (b) pick pyramid depth from the requested sigma; (c) **fractional radius**: crossfade the two nearest pyramid
  levels by the fractional part so an animated radius never steps (the actual risk for Kit's motion rules: a radius
  spring must not snap between integer levels); (d) store the pyramid in rgba16f or rg11b10 to avoid banding and
  dither at the final composite (see idea 9); (e) cache the blurred pyramid when the underlying scene is unchanged
  (glass over static content costs one composite).
- Rive's rule is a useful twin: [`draw.cpp`](https://github.com/rive-app/rive-runtime/blob/f4e896a4/renderer/src/draw.cpp)
  never renders a blur wider than 16 px at the render scale; it renders smaller and scales up. The pyramid is the same
  idea.
- Cost/risk: low. Downsampled upsample can leak the glass edge into the blur; premultiply and use the glass mask at
  composite (the three.js liquid-glass repos premultiply on the first pass for this reason).
- Peak-alpha boost (Scrawl) is orthogonal and composes after the blur.

### 4. Liquid-glass-style lens as an in-app material — **spike**

- Sources (all MIT-ish, small): [liquid-glass-js `webgl.ts`](https://github.com/Amir-Abushanab/liquid-glass-js/blob/main/packages/core/src/webgl.ts)
  (SDF mask, baked displacement map RG, specular B, chroma split, adaptive specular: additive on dark, multiplicative
  on bright, plus a vibrancy pull toward mid grey for legibility);
  [liquid-glass-studio](https://github.com/iyinchao/liquid-glass-studio/) (refraction, dispersion, Fresnel,
  superellipse shapes, **smooth-union blob merging**, spring-driven shape animation, WebGL2 and WebGPU);
  [Metal gist](https://gist.github.com/destefanis/b69545e63b38eb91a13c7c5d330d1345);
  [GPUI fork PR adding BlurRect and LensRect primitives](https://github.com/df49b9cd/zed/pull/1) (Apr 2026, shows the
  same shape of primitive in GPUI).
- What Rune would do: one `Lens` material on `UberQuad`: compute the SDF, derive a height profile from the distance
  (smoothstep or erf edge falloff), normal = gradient of height, refracted sample offset = normal * strength, sample
  the backdrop at three offsets for dispersion, mix with the blurred pyramid for frost, add Fresnel and a
  specular rim. For rounded rects and superellipses the height and gradient are closed-form per pixel, so the
  baked displacement map in those repos is unnecessary and the shape can animate every frame (no map regeneration).
- The real cost is the backdrop copy (render pass break, see idea 7). Scissor-copy only the glass bounds plus
  displacement margin; for several glass shapes copy their union rect once.
- The "adaptive specular + vibrancy" trick matters for §6b "real contrast": it is a legibility control, and the
  contrast checker can verify it.
- This is the in-app answer to PLAN §4.4's limit ("Rune can't draw Liquid Glass"): the system's own shader is out of
  reach, but a plausible lens is not, and it works on web/Windows/Linux too, which native underlays never will.
- Risk: it will not match Apple's exactly; design decision for Kit. Verdict: spike with one hero element, compare
  against `NSGlassEffectView` side by side.

### 5. Slug for scale-animated text and icons — **spike** (already in the plan; new facts)

- Sources: [A Decade of Slug](https://terathon.com/blog/decade-slug.html) (patent #10,373,352 dedicated to the public
  domain 2026-03-17), [reference shaders, MIT/Apache](https://github.com/EricLengyel/Slug), the JCGT 2017 paper,
  [dilation write-up](https://terathon.com/blog/glyph-dilation.html).
- New facts: the 2026 pixel shader is "a significant upgrade"; the vertex shader has **dynamic dilation** (expand the
  bounding polygon by half a pixel in viewport space per vertex, using the MVP and viewport, so no wasted padding and
  no missing edge pixels under perspective or scale); adaptive supersampling and the band-split optimisation were
  removed (simpler). Curve and band data live in textures (fixed 4096-wide); per pixel it solves quadratic roots in
  two directions.
- Rune use: text or icons under scale springs (a title growing 0.9 to 1.0) re-rasterise glyph-atlas entries at
  every size, which pops; Slug has no atlas. Pair with a coverage-based weight boost (their `SLUG_WEIGHT` sqrt) to
  keep light-on-dark text from thinning. Keep the atlas for body text at 1x (cheaper per pixel).
- Cost/risk: fragment cost is higher than atlas sampling; quadratic outlines only (TrueType native, CFF cubics need
  conversion); no hinting. Gate with a per-scene counter of Slug pixels. Verdict: spike after the glyph atlas works.

### 6. Rive-style analytic feathering for soft shapes — **later / steal one trick now**

- Sources: [Rive blog (Apr 2025)](https://rive.app/blog/how-rive-reinvented-feathering-for-the-vectorian-era),
  [intro (Feb 2025)](https://rive.app/blog/introducing-vector-feathering),
  [renderer source: `draw.cpp`, `tessellate.glsl`](https://github.com/rive-app/rive-runtime/blob/5360c834/renderer/src/shaders/tessellate.glsl),
  [Khronos slides](https://www.khronos.org/assets/uploads/developers/presentations/Rive.pdf).
- What it does: widens the one-pixel AA ramp into a Gaussian CDF ramp (`erf`) of the chosen width, computed
  procedurally from the same coverage triangles. A 1D lookup texture holds the CDF and its inverse. At corners the
  integral is done with about four samples per fragment; in the tessellator, curvature dims the feather by
  flattening the curve's control points (`dimming = 1 - curvature/pi`). No offscreen blur pass, resolution
  independent, cheap on weak GPUs. Large radii fall back to an atlas path rendered small then scaled up.
- Rune use: free-form soft glows and inner/outer shadows of arbitrary paths (icons, blobs) without an offscreen
  pass. For rounded rects the Levien closed form (idea 8) is simpler and exact; feathering matters only for
  non-rect paths. It needs an own tessellator with signed-winding triangles (Rive's coverage scheme), which Rune does
  not have; `vello_cpu`'s blur filter or an offscreen blur is the pragmatic route until then.
- Steal now: (1) CDF 1D texture trick for any ramp (it replaces `erf` approximations in the shader); (2) "design-tool
  blur = 2 sigma wide" convention (feather radius = amount * stddevs / 2) so Figma-like numbers match.
- Verdict: later, behind Rune having its own path tessellator.

### 7. Apple TBDR discipline (framebuffer fetch, memoryless, load/store) — **later, Metal backend only**

- Sources: [WWDC20 Harness Apple GPUs](https://developer.apple.com/videos/play/wwdc2020/10602/),
  [WWDC21 image processing on Apple silicon](https://developer.apple.com/videos/play/wwdc2021/10153/),
  [Metal 2 on A11: imageblocks](https://developer.apple.com/videos/play/tech-talks/603/) and
  [tile shading](https://developer.apple.com/videos/play/tech-talks/604/),
  [Tailor your apps for TBDR](https://developer.apple.com/documentation/metal/tailor-your-apps-for-apple-gpus-and-tile-based-deferred-rendering).
- Key facts: alpha blending runs in tile memory and the target is written once per pass; programmable blending lets a
  fragment read the tile's current colour (merges multi-pass effects with no intermediate store); memoryless
  textures live only in tile memory (must be `DontCare`/`Clear` load and `DontCare` store, no sampling, no blit or
  compute); `LoadActionDontCare`/`Clear` avoids reading what is overwritten.
- Rune use: (a) **group opacity** and non-trivial blend modes (multiply, screen, plus-lighter) via framebuffer fetch
  in the one pass, avoiding offscreen layers (matches PLAN §4.2). (b) Always `DontCare` loads where the frame is
  fully redrawn. (c) MSAA is unnecessary for UI (analytic AA), so memoryless MSAA is not a lever.
- Limits: a backdrop *sample* (blur, lens) reads neighbouring pixels, which tile memory cannot provide; tile shaders
  see only their own tile. So glass still needs a pass break (store, then load) and the copy in idea 4. Raster order
  groups (also used by Rive on D3D11/Metal) serialise overlapping fragments; Rive uses them for exact path
  blending, which Rune does not need for the uber-quad lane.
- wgpu: **I could not confirm** any framebuffer-fetch or pixel-local-storage support in wgpu *(treat as unsupported)*;
  this is a concrete reason to keep the raw-Metal HAL on the plan as a later optimisation. Verdict: later, justified
  only by measured offscreen-layer cost.

### 8. Closed-form blurred rounded rects, inset shadows, and squircles — **adopt now**

- Sources: [Levien, blurred rounded rects](https://raphlinus.github.io/graphics/2020/04/21/blurred-rounded-rects.html)
  (rounded-rect distance field, effective radius `sqrt(rc^2 + 1.25 rb^2)`, 1D erf cross-section, `erf7` using a
  reciprocal sqrt); [Evan Wallace, fast rounded-rect shadows](https://madebyevan.com/shaders/fast-rounded-rectangle-shadows/)
  (product of two erf boxes; CC0 code); [Vello's implementation, #665](https://github.com/linebender/vello/pull/665)
  plus the active inset-shadow work ([#1374](https://github.com/linebender/vello/issues/1374),
  [#1718](https://github.com/linebender/vello/pull/1718)) and [#1724](https://github.com/linebender/vello/pull/1724)
  (cutting `powf` calls).
- Rune use: the `UberQuad` already plans analytic Gaussian shadows (PLAN §4.3). Take Levien's formula as the base,
  differential-test it against numerically integrated results (the CPU reference) per radius and sigma, and add
  inset shadows as `1 - blurred` per Vello's design. The same field gives soft per-side gradient edges for the "no
  hard clip edges" rule, since a blurred rounded rect is exactly a soft mask.
- **Squircles (continuous corners):** iOS-style corners are not circles; Figma's "corner smoothing" parameter is the
  common reference and a superellipse is the cheap stand-in *(from memory)*. Implicit shapes have no exact distance,
  so use `f / |grad f|` as an AA distance (valid near the edge, wrong far away; fine because only the edge band is
  shaded). Test against a high-resolution CPU rasteriser for AA ramp error.
- **Smooth union for morphing shapes:** polynomial `smin` (Quilez) over a handful of SDFs gives the metaball merge
  seen in liquid-glass-studio; lerping two SDFs is a cheap shape morph. Fix a maximum number of operands per `Blob`
  primitive (4) so the shader stays branch-free. This is how Rune can do the launcher's glass morph without SwiftUI on
  non-Apple platforms.
- Cost/risk: low; the main risk is the AA width under non-uniform scale and rotation: derive the pixel footprint
  from the Jacobian (`fwidth` or explicit inverse transform), not a scalar scale. Add a rotation/skew scene.

### 9. Dithering and perceptual colour — **adopt now**

- Sources: Jimenez interleaved gradient noise (same CoD talk as idea 3; cheap per-pixel hash, good temporal
  behaviour); Christoph Peters, free blue-noise textures *(from memory, no link fetched)*; Ottosson's Oklab and
  gamut-clipping posts *(from memory)*; PLAN §4.3 already cites Kit's GPUI fork (`kitlangton/gpui`) dithering fixes.
- Rune use: interpolate gradients in Oklab (or OKLCH for hue ramps) and blend in linear light on Display P3, as the
  plan says; then dither once at the final write, with triangular-PDF noise of about +/-1 LSB, using a tiled blue
  noise texture or IGN. One shared function for gradients, shadows, backdrop output, and video.
- Two traps worth encoding as tests: (1) **noise swim**: a screen-space-fixed pattern over scrolling content shimmers
  relative to the content; use content-local coordinates for scrolling layers (or accept it for static UI). Never
  animate the noise per frame (breaks "zero cost at rest" and snapshot determinism); the seed must be a pure function
  of pixel coordinates. (2) **Banding in blurred backdrops**: a blur of 8-bit content upsampled by a pyramid bands;
  dither after the final upsample and keep intermediates in 16-bit.
- Deterministic golden images then stay stable because the dither is deterministic and the same on CPU and GPU lane.

### 10. Damage tracking / partial redraw — **skip for now; design the hook**

- Sources: [Flutter Impeller partial repaint, issue #124526](https://github.com/flutter/flutter/issues/124526) and
  [PR #40959](https://github.com/flutter/engine/pull/40959); [Flutter damage-rect discussion](https://github.com/flutter/flutter/issues/33939)
  (Metal has no buffer-age API; track which of the 2-3 drawables you got by texture identity);
  [Slint's Metal surface keeping per-drawable ages](https://github.com/slint-ui/slint/blob/507b650e/internal/renderers/skia/metal_surface.rs);
  [EGL_KHR_partial_update](https://registry.khronos.org/EGL/extensions/KHR/EGL_KHR_partial_update.txt).
- Findings: Metal drawables are not preserved, so partial repaint means keeping your own full-frame texture and blitting
  the damaged area into the drawable (Impeller measured the blit at about 0.5 us on iPhone 13, only bypassing partial
  repaint above 70 % damage; I would treat that figure as encode time rather than GPU time) *(estimate)*; drawable
  ages can drift, so track per drawable.
- For Rune: the benefit is battery and thermals, not feel; frames that animate usually damage most of the window or a
  big glass region anyway, and at rest Rune submits nothing. Verdict: skip. Keep a per-frame damage rect in the scene
  (cheap: union of changed instance bounds) so a counter shows how much a frame *could* skip; revisit if Scrawl or the launcher
  idle-animation power shows up in profiling.

### 11. GPU-driven UI (indirect draws, bindless, compute culling) — **skip**

- A UI frame is one pass with one or a few instanced draws and a few thousand instances (PLAN §4.3). CPU encode cost is
  already tiny; indirect command buffers or compute culling pay when there are tens of thousands of distinct draws
  *(general knowledge)*. The pieces worth taking are the cheap ones: bindless-style texture arrays or one atlas to avoid
  rebinding, and instance buffers written once per change (idea 2). Compute culling is useful only for huge scrolling
  lists, where CPU range queries over a sorted arena are simpler and exactly testable.

### 12. HDR / EDR UI — **later**

- *(From memory, not re-fetched.)* On Apple, `CAMetalLayer.wantsExtendedDynamicRangeContent` with an
  `rgba16Float` extended-linear Display P3 layer gives headroom above 1.0, and the headroom is queried per screen
  and changes with brightness. For Rune it means HDR video/photos and specular highlights could exceed SDF white.
  The cost is double the bandwidth for the frame. Keep the pipeline format-generic and linear (already planned) so EDR is
  a switch, not a rewrite; keep chrome in SDR. Verdict: later.

## Steal list

- `vello_cpu` as reference and headless renderer; Vello's blurred-rounded-rect code as a CPU oracle for the analytic
  shadow.
- Levien `erf7` + effective-radius formula; Evan Wallace's product-of-erf boxes (CC0).
- Jimenez 13-tap/tent pyramid with Karis first level; fractional-level crossfade (new for Rune).
- Rive: 16 px blur cap then upscale; CDF lookup texture; "2 sigma = design blur amount" convention.
- Liquid-glass recipe: SDF height, normal-based refraction, per-channel dispersion, adaptive specular, vibrancy toward
  mid grey, polynomial smooth union for merging shapes.
- Slug dynamic dilation and the MIT reference shaders; sqrt coverage for optical weight.
- Slint/Impeller per-drawable age tracking if partial repaint is ever added.
- TBDR load/store discipline and framebuffer fetch group opacity (Metal backend).

## Avoid list

- Pre-baked displacement maps for glass: closed-form SDF is exact and animatable.
- Gaussian convolution with large kernels or a separable pass at full resolution for backdrop.
- MSAA for UI primitives (analytic AA is cheaper and deterministic).
- Per-frame animated dither noise (breaks determinism and zero cost at rest).
- Compute-shader Vello as a dependency (moved to `research/`; needs compute, memory-hungry).
- Indirect/bindless engines for a UI-sized draw count.
- Assuming wgpu exposes framebuffer fetch.
- Treating Apple tile shaders as a blur tool (tiles cannot read neighbours).

## Recommendation for Rune

1. Adopt now: closed-form blurred rounded rect and inset shadow (idea 8), pyramid backdrop with fractional radius
   (3), Oklab + deterministic dither (9), `vello_cpu` as the reference renderer (1).
2. Spike next, each with an exact counter and a differential test: vertex-shader springs (2), glass lens (4), Slug for
   animated text (5), `vello_gpu` lane (1).
3. Later: feathering (needs a tessellator), framebuffer-fetch group opacity (needs raw Metal), HDR.
4. Skip: GPU-driven draws, partial present, MSAA.

Rejected alternatives: compute Vello (compatibility, memory); SDF/MSDF text (corner rounding, atlas pops, Slug
supersedes it now the patent is gone); full-resolution separable Gaussian (cost, banding); wrapping SwiftUI/CA for
all glass (does not reach web, Windows, Linux).

## Open questions

- How does fractional pyramid crossfade look at the transition between a thin and a thick blur while the glass also
  moves? Needs a scene in the deterministic harness plus the motion linter.
- Does the vertex-shader spring path survive retarget bursts (a drag) without per-frame instance rewrites? Measure
  encode time at 1k animating nodes.
- Can wgpu on Metal skip the drawable's initial clear and use an `IOSurface`-backed texture presented via CALayer
  contents (retained, damage-friendly)? Unverified.
- Apple's real glass is likely a different material model (not a plain lens); is "close enough" acceptable to Kit
  for non-Apple platforms, with native underlays kept on Apple?
- Slug quadratic-only limit: do Rune's fonts (SF, CFF fonts) need an offline cubic-to-quadratic step, and what is the
  fragment cost at 120 Hz on iPhone-class GPUs?
- Dither noise swim on scrolling content: local-coordinate dither vs screen-space; needs a visual test.

# 23. Text raster quality and motion

S5 from PLAN.md. Headless bitmaps only; no window or display capture. Results
below are filled from outputs, not from the hypothesis.

## Oracle and metrics

`crates/rune/examples/text_quality.rs` renders Inter Regular and system SF at 11, 12, 13, 15
and 24 pt, 1×/2×, #181818 on #eeeeee and its inverse. Five strings per size
cover prose, UI labels, digits, ambiguous letters, ligatures and a stem probe.
It exports the exact Parley glyph IDs, quarter-pixel x positions and snapped
baselines. `scripts/coretext-quality.swift` draws those same glyphs with the
same font bytes into an sRGB CoreGraphics bitmap, with grayscale antialiasing,
font smoothing and fractional positioning enabled. This isolates **raster and
blend quality**. It does not claim parity with CoreText's shaping, optical-size
selection, SF tracking or line breaking. SF is supplied from the local system
font, never redistributed.

Measured on an Apple M5 Max, macOS 27.0.1 (26A434). Font SHA-256:

- Inter: `40d692fce188e4471e2b3cba937be967878f631ad3ebbbdcd587687c7ebe0c82`.
- `/System/Library/Fonts/SFNS.ttf`: `44890958f3e0b997028121ed7c7a789f3f5cce227c7a93ce8f86ec0db26da571`.

SF's registered family is `System Font`. The CoreText oracle explicitly sets
`opsz` to point size × scale, matching Parley's current variation selection.
Without this, the 2× reference uses a different SF outline. This calibration
covers regular Latin UI text, not CJK, emoji, bold/italic, arbitrary backgrounds,
or native SF tracking. The CoreText bitmap is a reference, not an AppKit capture.

`scripts/text-quality.py` reports Gaussian SSIM (sigma 1.5, population covariance,
data range 1) on each row's ink-bounded horizontal crop. Each row gets equal
weight; full-page blank backgrounds do not inflate the score. It also reports
signed and absolute relative ink-weight error, normalized ink MAE, and per-glyph
advance-cell coverage/weight. Overlapping side bearings belong to the adjacent
cell; these are attributed cells, not independently rasterized isolated glyphs.
No image alignment search, rescaling or blur is used to improve scores.

The phase sweep moves `lHEximno` through two device pixels, 64 timestamps per
pixel. Each frame is a separate 40 px strip rendered through the real GPU path.
Inverse-sRGB decoding recovers linear coverage; coefficient of variation,
peak-to-peak range and adjacent-frame RMS quantify brightness shimmer. This
coverage metric is separate from perceived brightness after nonlinear blending.

## Baseline policy and constraints

- `text.rs` already uses four x phases and whole-pixel baselines, including
  negative coordinates and carry into the next pixel. Hinting is on at 1× and
  off at 2×. Atlas is **2048² R8** (the older README said 1024²).
- `shader.wgsl` uses exact texel loads for ordinary text. Bilinear filtering is
  reserved for scaled flight snapshots. Changing filtering for ordinary text
  would add blur to already phase-rasterized glyphs.
- The baseline `runtime.rs::glyph_gamma` uses `0.78 + 0.22 * sRGB(luminance)`.
- `rune-motion/src/layer.rs::settle_after` already computes a conservative
  analytic settle time; the store lands at `(target, 0)` and removes the track.
  Do not replace this with frame-by-frame epsilon tests or a second text spring.
- Preserve the shaped-layout cache, retained Taffy cache, virtual rows,
  reusable cluster scratch and zero-allocation presentation gate.

## Sources inspected

1. [Wang et al., Image quality assessment: From error visibility to structural
   similarity, IEEE TIP 2004](https://ece.uwaterloo.ca/~z70wang/research/ssim/).
   SSIM is a spatial image metric, not proof of native subjective equivalence.
2. [Skia, The Raster Tragedy](https://skia.org/docs/dev/design/raster_tragedy/).
   Distinguishes linear blending, the guessed-background gamma hack, optical
   sizing and contrast enhancement. Full-pixel grayscale gamma adjustment is
   not the same as LCD subpixel correction. CoreText's optical emboldening can
   depend on nominal size, unlike hinting at final pixel size.
3. Kit's Apache-2.0 GPUI fork (`kitlangton/gpui`), commit
   `2e52b98886063ddfa828312c83415a14208dfe87`,
   `src/platform/mac/text_system.rs:194–201, 444–458`. Five luminance buckets
   (0, .25, .5, .75, 1) select CoreText smoothing through the **fill color of an
   alpha-only CGContext**. This is not a five-radius morphological max filter;
   blindly copying bucket numbers into swash dilation would not reproduce it.
4. [Research 15 §7](15-research-motion-perception.md): pixel-y, quarter-phase x,
   analytic settling, shared container tracks, a phase-sweep brightness oracle.
5. [WebRender macOS rasterizer](https://searchfox.org/mozilla-central/source/gfx/wr/wr_glyph_rasterizer/src/platform/macos/font.rs)
   probes actual OS smoothing behavior, distinguishes dilation from LCD AA,
   quantizes color for its smoothing masks and preblends grayscale. Its native
   CoreGraphics masks cannot be copied into a deterministic swash path. Historical
   [issue 1767](https://github.com/servo/webrender/issues/1767) documents separate
   light/dark weightings, not a universal gamma constant.

## Experiments

| Experiment | Decision | Evidence |
|---|---|---|
| Independent swash oracle, default hinting, zero emboldening | Control | All four Inter GPU images and phase metrics equal production baseline. |
| No hinting at 1× | Keep with calibrated weight | Inter 1× dark SSIM .971615 → .981808; light .958593 → .964075. Alone it makes light-mode weight slightly worse. |
| 3×3 max-filter coverage dilation plus gamma sweep | Reject | A diagnostic approximation, not CoreText's outline smoothing. Broadens outside pixels and needs extra work; centered outline emboldening wins. |
| Centered swash outline emboldening, .1 device px | Partial | Improves 2× weight but overweights 1× light-on-dark with the old exponent. |
| Centered emboldening, .1 point (.1/.2 device px), scale-aware exponent | Keep | Weight error improves in all eight font/polarity/density groups; SSIM improves in seven. SF dark 1× is the exception below. |
| Fixed exponent `.375 + .875 * sRGB(luminance)` | Reject | Good static Inter fit, but SF dark 2× brightness variation regresses. The final density-aware expression improves all eight brightness probes. |
| Five luminance-keyed atlas copies | Reject for this pass | kitlangton/gpui's buckets invoke CoreText, not five interchangeable swash radii. A single shape plus continuous blend correction avoids up to 5× cache cardinality and bucket jumps during theme animation. |
| Bilinear filtering of ordinary glyphs | Reject | Quads already land on device pixels with rasterized x phases. Resampling them again softens text; flight filtering remains separate. |

Calibration used the first two strings at each size; the remaining three were
held out. With .1/.2 device-pixel centered emboldening, the best gamma-only
training fits were .45/.45 for dark-on-light and 1.15/1.20 for light-on-dark.
The retained candidate uses one continuous expression,
`0.35 + (1.15 + 0.2 * clamp(scale - 1, 0, 1)) * sRGB(foreground_luminance)`.
This balances static weight with phase-dependent brightness, rather than
minimizing the training error alone. Outline expansion is recentered in both axes:
swash's `embolden(d)` adds
`d` translation as well as expansion. The cache key includes `d`: 12 pt at 2×
and 24 pt at 1× have the same pixel size but different nominal-size smoothing.
An early build emitted empty quads for spaces because emboldening gives empty
outlines nonzero bounds. Rejecting all-zero masks restores the original instance
count; the phase/cache test includes whitespace to prevent recurrence.

The offline `coverage-*.png` diagnostic overwrites overlapping glyph masks; it
is only a parameter-search approximation. All static results below come from
actual GPU source-over output, not that approximation.

### GPU results, Inter

All SSIM values are row-crop means. Weight is mean absolute row ink-weight error.

| Mode | SSIM before → after | Weight error before → after |
|---|---:|---:|
| Dark, 1× | .971615 → .980707 | 7.46% → 6.46% |
| Dark, 2× | .982463 → .985549 | 11.93% → 6.32% |
| Light, 1× | .958593 → .982740 | 30.14% → 4.04% |
| Light, 2× | .968310 → .990795 | 22.84% → 3.46% |

### GPU results, SF

| Mode | SSIM before → after | Weight error before → after |
|---|---:|---:|
| Dark, 1× | .949609 → .948209 | 8.10% → 6.20% |
| Dark, 2× | .977342 → .979416 | 12.42% → 7.13% |
| Light, 1× | .923434 → .950077 | 30.69% → 2.68% |
| Light, 2× | .963039 → .986747 | 23.87% → 2.80% |

The SF dark 1× SSIM loss is .001400 (held-out strings: .950056 → .949516),
despite better average weight. This is a deliberate tradeoff for better light
text weight and lower phase brightness variation, not universal CoreText parity.
The other seven held-out groups improve. Per-font/stage aggregates are in
`demo/text/metrics.json`. `scripts/text-quality.py` regenerates full row and
attributed-glyph results, including 11/12/13/15/24 pt separately, in the ignored
run directory. The public report deliberately omits that large diagnostic data.
Group means hide size-specific losses: dark 1× SF at 24 pt loses .017929 SSIM,
and dark 1× Inter at 13 pt loses .010482. The retained policy favors the combined
weight/motion result; it is not a per-size static optimum.

### Motion

The production quarter-phase/pixel-baseline policy and conservative analytic
settle were already implemented. Keep them rather than introducing a second
text spring or switching hinting during motion. Two focused tests exercise
negative origins, phase carry, four cached phases per glyph, different nominal
sizes at equal pixel size, exact analytic landing, 120 identical resting samples
and no atlas writes after phase warmup. The warmed streaming allocation gate
still passes (zero allocations).

| Probe | Before | After |
|---|---:|---:|
| Raw coverage CV, 1× | .03688% | .02164% |
| Raw coverage CV, 2× | .01896% | .01165% |
| Inter display ink CV, light 1× | 1.54294% | .99005% |
| Inter display ink CV, dark 1× | .71390% | .20733% |
| Inter display ink CV, light 2× | .43394% | .27093% |
| Inter display ink CV, dark 2× | .46632% | .14171% |
| SF display ink CV, light 1× | 2.15455% | 1.65172% |
| SF display ink CV, dark 1× | .50179% | .23311% |
| SF display ink CV, light 2× | 1.20865% | .75191% |
| SF display ink CV, dark 2× | .22162% | .13237% |

Display ink is the polarity-normalized sRGB pixel difference from the background,
computed with the shader's exponent and linear source-over equation from the GPU
coverage sweep. It is not a psychophysical flicker threshold. Residual variation
is nonzero; the result supports **less shimmer**, not an unqualified “no shimmer”.
SF raw-coverage CV improves too, but its 1× peak-to-peak coverage rises from
.08582% to .10970%, and adjacent-frame coverage RMS from .01386% to .01444%.
Thus not every raw metric improves. This is a 13 pt horizontal stem probe at
virtual timestamps, not physical display cadence or a whole-font motion audit.

## Performance

Headless release on the same M5 Max, 120 Hz virtual time, one warmup and seven measured runs
per workload. Each pair alternates the saved original binary and the final one;
pair order reverses on successive runs. Synthetic fixtures only.
CPU includes update/build/encode/submit, excludes readback and is not GPU latency.
Both binaries end with **1,126 instances**. The atlas remains 2048² R8, four x
phases, no color-keyed copies, and warmed presentation still allocates nothing.

| Workload | CPU p50 before → after | CPU p95 before → after | First CPU frame before → after |
|---|---:|---:|---:|
| Stream | .313 → .295 ms | .665 → .604 ms | 4.98 → 5.39 ms |
| 2,000-message stream | .327 → .311 ms | 3.891 → 3.695 ms | 9.70 → 10.40 ms |

The machine was contended. p95 MADs were .171/.115 ms for stream and .203/.241 ms
for long-stream; first-frame MADs .55/.88 and .15/1.05 ms. Do not claim a speedup.
No warmed-frame regression is evident; a small first-raster cost remains possible.
Earlier unpaired baseline/final medians (.259/.334 ms stream p50) drifted with
load, so the paired comparison is authoritative. All paired runs and MADs are
retained in `demo/text/performance.json`. Research/19's structural caches,
virtualization and cluster scratch were not changed.

Saved executable SHA-256:
before `c2bb31da0a331b0fc68e9edbd228ac7df1e42ff3058851d32655d49f4f0cdb44`,
after `e4c7d51a1ef2b8b05fdc6ec1d9e66944d7cc39c5fc5ce1a312a28c5f86d8b368`.

## Images and goldens

`demo/text/{inter,sf}-{light,dark}-{1,2}x.png` shows before / after / CoreText,
one representative line at each of the five sizes. `*-diff-*.png` shows absolute
pixel error amplified 4×. `*-phases-*.png` shows nine raw-coverage positions.
`ui-crops.png` contains actual settings/OpenCode/kit before/after crops.

Inspected light/dark crops: dark-on-light stems gain the missing weight, rounded
edges remain antialiased, and 2× light-on-dark edges lose some bloom. Advance
positions and baselines stay fixed. SF 1× still has visible raster differences
from CoreText. The kit's Linux Chromium reference is a different oracle; its
scores need not improve when moving toward macOS weight.

`scripts/text-goldens.py` refreshes seven settings/OpenCode stills, two kit index
images and twelve kit side-by-sides. It keeps the committed web columns and
recomputes Rune plus the diff, without another browser capture. Updated scores
live in `demo/text/kit-metrics.json`.
The streaming still is standardized to `--at 4`; its former capture time was
not recorded, so its change includes that state adjustment. Historical motion
strips/videos retain their recorded states and are not current text goldens.

## Reproduction

```sh
mkdir -p .lab-output/text/target/perf
cargo test -p rune quality_tests
cargo test -p rune --test alloc
cargo build --profile perf -p settings -p kit-gallery   # (the OpenCode fixture example has since moved out of this repository)
cargo build --release -p rune --example text_quality
swiftc -O scripts/coretext-quality.swift -o .lab-output/text/coretext-quality
cp target/perf/{opencode,settings,kit-gallery} .lab-output/text/target/perf/
cp target/release/examples/text_quality .lab-output/text/

# Headless bitmaps on the measurement host. Repeat for SFNS.ttf / "System Font".
FONT=crates/rune/assets/Inter-Regular.ttf
.lab-output/text/text_quality perf/autoresearch/text/before/inter "$FONT" Inter auto 0 legacy
.lab-output/text/text_quality perf/autoresearch/text/after/inter "$FONT" Inter
.lab-output/text/coretext-quality perf/autoresearch/text/before/inter
.lab-output/text/coretext-quality perf/autoresearch/text/after/inter
uv run --with numpy --with pillow --with scikit-image scripts/text-quality.py perf/autoresearch/text/before/inter
uv run --with numpy --with pillow --with scikit-image scripts/text-quality.py perf/autoresearch/text/after/inter
# After both fonts:
uv run --with numpy --with pillow scripts/text-report.py perf/autoresearch/text demo/text
mkdir -p target/perf target/release
cp .lab-output/text/target/perf/{opencode,settings,kit-gallery} target/perf/
cp .lab-output/text/target/perf/{opencode,settings,kit-gallery} target/release/   # for text-goldens.py
python3 scripts/bench-ui.py --only stream,long-stream --out perf/autoresearch/text/perf-after
python3 scripts/text-bench.py --before .lab-output/text-before/target/perf/opencode \
  --after target/perf/opencode --out perf/autoresearch/text/perf-paired-new
uv run --with numpy --with pillow scripts/text-goldens.py
```

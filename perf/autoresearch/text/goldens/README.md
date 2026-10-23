# Symbol fallback goldens

2026-10-22, headless on the Mac mini through HiveMind, CAS sync disabled and source SHA-256 checked.

- `scripts/text-goldens.py` regenerated all named kit, OpenCode and settings stills. The tracked `demo/kit`, `demo/opencode` and `demo/settings-*.png` files are byte-identical: their displayed shortcuts already resolve in the fonts used by those scenes. `symbol-diffs.json` records the comparisons (zero changed pixels).
- `scripts/snap-ui.py` regenerated the example scenes under `examples/`.
- Added full keybind and keybind-tooltip light/dark snapshots.
- `plex-symbols/{before,after}.png` isolate IBM Plex Mono with its new fallback chain disabled/enabled, using Rune's headless glyph-atlas coverage at 2×. Fixed character cells isolate the affected glyphs. `diff-crop.png` stacks before, after and amplified difference. Latin and the four arrows are identical; only the previously missing keyboard symbols change.

The two targeted tests passed: Plex symbol resolution and palette secondary-text contrast on white, near-black and mid-grey backgrounds (both foreground polarities for mid-grey). The three release examples built. Mini emitted an existing rust-objcopy/libLLVM stripping warning; executables and snapshots completed normally.

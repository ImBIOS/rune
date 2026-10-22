# Text calibration

Each font/mode/density sheet reads **before → after → CoreText**. Rows are
11, 12, 13, 15 and 24 pt. The `diff` sheets show absolute error ×4; `phases`
sheets show nine horizontal positions from the raw-coverage sweep.

- [Method, tradeoffs and reproduction](../../research/23-text-quality.md)
- [Inter, light 1×](inter-light-1x.png)
- [SF, light 2×](sf-light-2x.png)
- [Settings, OpenCode and kit crops](ui-crops.png)
- `metrics.json`: per-font, per-stage raster and motion aggregates. Regenerate
  per-row and glyph-cell measurements with `scripts/text-quality.py`.
- `performance.json`: isolated paired timings and later integration checks,
  including the saved integration executable hashes.
- `kit-metrics.json`: refreshed samples against the existing Linux web references.

These isolate rasterization and blending at matched glyph positions. They do not
measure native shaping or display presentation. SF dark 1× has a small SSIM
regression; residual motion brightness variation remains nonzero.

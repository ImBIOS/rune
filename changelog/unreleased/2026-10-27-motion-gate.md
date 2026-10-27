- **`rune::motion_gate` lets an app gate its own motion in `cargo test`.** `Run` records the
  app's real `Runtime` frame by frame on the virtual clock, `check` lists what a person would see
  go wrong (pops, jumps, lurches, overshoot, flicker, text swaps, motion that never settles or
  keeps waking) against the `RULES` thresholds, and `gate` runs the app's cells in parallel and
  fails on findings its allowlist doesn't explain. `scripts/motion-audit/gate` runs it with a
  cell filter, `--dump` and `--sheet` evidence; `scripts/motion-audit/motion-trace.py` reads an
  app's own frame-cost trace from a real run.

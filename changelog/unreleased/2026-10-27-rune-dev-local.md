- **`scripts/rune-dev --local` hot-reloads on one machine.** The compiler, the edit guard and the
  app run here with no build Mac, and an edit that needs a restart rebuilds and relaunches the
  app. `RUNE_DEV_WRAP` runs it under a command prefix (default: plain `cargo`).
  `scripts/rune-hot --manifest … --patch … [--port …]` is the same with an app script's flags.

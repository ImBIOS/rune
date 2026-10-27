- **Breaking: the browser dev panel is gone.** The `rune/dev` feature (and `rune-motion/dev`),
  `App::dev_panel`, the `rune::dev` module and the `tune!` macro were removed, along with
  `Runtime::motion_blur_enabled` and `examples/dev-panel`. Replace each `tune!("key", default, ..)`
  with its default as a plain constant (hot reload picks up edits), drop `dev = ["rune/dev"]`
  from the app's features, and audit scenes, pages and overlays with `App::playground`.
  `rune::scene_clock()` works without any feature.

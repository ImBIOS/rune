- **New crate `rune-knobs`: live tuning values and the ⌘⇧K panel.** A `Knob` is a named
  slider or choice with a range and a default, grouped by the keyed element it tunes. The panel
  comes as a side panel that pushes the page over (`side`), a floating panel (`layer`), or rows
  for an app's own layout (`inline`, `actions`), such as `App::playground`'s side column.
  Without the `panel` feature, knobs read their defaults and the panel isn't built.
  `examples/knobs` shows it.

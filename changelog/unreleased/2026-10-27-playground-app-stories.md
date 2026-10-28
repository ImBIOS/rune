- **The playground opens on a story from app code.** `Tx::open_playground_at(story, variant)`
  takes a path, a name or a slug, through the same lookup as `--playground`/`--variant` and
  `Runtime::open_playground`; from `App::on_open_url` a link opens straight onto the story at
  launch. A bad name at runtime is logged and opens the list. `rune::playground::slug(path)`
  makes a story's stable link slug (`"Timeline/Send"` is `"timeline-send"`), and every lookup
  takes one. The `playground` module is public for it.
- **A story's side controls can take keys.** `Story::side_keys(view, key)` and
  `Playground::side_keys(scene, view, key)` are `side` for controls that take keys: → or Tab
  puts the keyboard on them, the view gets `active` to mark what the keys move, and the hook
  gets each key first. Esc and Tab leave; ← no longer does there. `rune_knobs::side_key(groups,
  lens)` is the hook for `rune_knobs::inline`: ↑↓ pick a knob, ←→ step it.
- **Playground stories take closures.** `Story::new`, `from_real`, `overlay`, `poll`, `trigger`,
  `variant` and `side`, `Play::after`, and `Playground::page`, `trigger`, `variant`, `side` and
  `side_real` take any `Fn` (or `FnOnce` for a step) where they took a `fn`, so a story table
  can build its stories in a loop. Code passing fns compiles unchanged. `Story::trigger_tx` and
  `Playground::trigger_tx` take a step that needs only the `Tx`.
- **Folded playground groups stay folded after it closes**, for as long as the app runs, where
  they used to unfold on close. `Playground::folds(load, save)` keeps them from one run to the
  next: `load` runs once as the playground first opens, and `save` gets the folded groups' names
  on each change.

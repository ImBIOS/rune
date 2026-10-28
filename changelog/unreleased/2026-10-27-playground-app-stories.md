- **The playground takes an app's story table.**
  - `Tx::open_playground_at(story, variant)` opens it on a story from app code, by path, name or
    slug, through the same lookup as `--playground`/`--variant` and `Runtime::open_playground`.
    From `App::on_open_url` a link opens straight onto the story at launch. A bad name at runtime
    is logged and opens the list. `rune::playground::slug(path)` makes a story's stable link
    slug (`"Timeline/Send"` is `"timeline-send"`), and every lookup takes one. The `playground`
    module is public for it.
  - `Story::side_keys(view, key)` and `Playground::side_keys(scene, view, key)` are `side` for
    controls that take keys: → or Tab puts the keyboard on them, the view gets `active` to mark
    what the keys move, and the hook gets each key first. Esc and Tab leave; ← no longer does
    there. `rune_knobs::side_key(groups, lens)` is the hook for `rune_knobs::inline`: ↑↓ pick a
    knob, ←→ step it.
  - `Story::new`, `from_real`, `overlay`, `poll`, `trigger`, `variant` and `side`,
    `Play::after`, and `Playground::page`, `trigger`, `variant`, `side` and `side_real` take any
    `Fn` (`FnOnce` for a step) where they took a `fn`, so a story table can build its stories in
    a loop, each capturing its row. Code passing fns compiles unchanged. `Story::trigger_tx` and
    `Playground::trigger_tx` take a step that needs only the `Tx`.
  - Folded groups stay folded after the playground closes, for as long as the app runs, where
    they used to unfold. `Playground::folds(load, save)` keeps them from one run to the next.

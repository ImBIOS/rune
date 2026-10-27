- **`App::playground`: a storybook built into the window.** ⌘⇧D (Ctrl+Shift+D off macOS, or
  `Playground::shortcut`, `None` to turn it off) shows the app's scenes plus its own
  `Playground::page`s and `Story`s, grouped by the part of their names before a `/`. One shows at
  a time on a plain stage, live and interactive, with a column beside it for the story's `side`
  view and its single-letter triggers. R makes the story's state again and runs its `'r'`
  trigger; `Play::after(secs, step)` queues timed follow-ups on the logical clock, so a replay
  runs the same every time. ↑↓/J K move between stories, ⌘↑ ⌘↓ go to the ends, → or Tab move
  into the side column, Esc leaves. An element focused in the stage keeps its keys until Esc.
  Triggers and side views name their scene as `--playground` does, by path or just name. Root
  `on_paste`, `on_drop_files` and `on_window_focus` still reach the app above the bar, and the
  stage's root while it's open. Scenes get a fresh state, stories their own; the real
  state keeps polling and is untouched. It's on in debug builds (`.enabled`, `RUNE_PLAYGROUND=0`);
  its bar takes 26 pt off the bottom of the app's content, and `.bar(false)` leaves the app's
  layout as it was. Snapshots, `--scene` and goldens never include it unless `--playground
  [story]` is passed. `WindowSpec::playground` gives each window its own; hot reload drops it.
  `examples/playground` shows each kind.

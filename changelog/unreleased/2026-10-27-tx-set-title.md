- **`Tx::set_title(title)` sets the window's title** from a handler or `update`, without
  asking for a frame. The window opens with `AppSpec`'s title and, closed and opened again,
  with the last one set. Headless `--tree` output gains a `"title"` field.

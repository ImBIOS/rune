- **The playground opens on a story from app code.** `Tx::open_playground_at(story, variant)`
  takes a path, a name or a slug, through the same lookup as `--playground`/`--variant` and
  `Runtime::open_playground`; from `App::on_open_url` a link opens straight onto the story at
  launch. A bad name at runtime is logged and opens the list. `rune::playground::slug(path)`
  makes a story's stable link slug (`"Timeline/Send"` is `"timeline-send"`), and every lookup
  takes one. The `playground` module is public for it.

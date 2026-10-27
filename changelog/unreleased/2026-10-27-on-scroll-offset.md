- **`El::on_scroll_offset` follows a scroll viewport exactly.** On a scroll viewport it is
  called with a `ScrollPos` (the offset and how far it can scroll) once at first, then in
  every frame scrolling moves it (the wheel, a glide, a reveal) or its content resizes, after
  that frame's scroll step and before it draws. A
  `Live` set from it shows in that same frame, so a header that shrinks or fades as a page
  scrolls rides the scroll 1:1, where `on_drawn_layout` hears a frame late and not at all
  while a glide runs. `Scene::present` presents values set after `Scene::frame` without
  stepping time. `El::on_layout`, `on_screen_layout` and `on_drawn_layout` moved, unchanged,
  to `layout_watch.rs`.

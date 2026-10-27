- **`Cx::layout_with` and `Cx::layout_size` read what a view needs from an element's rect,
  re-running it only when that changes.** `layout_with(key, f)` returns `f` of the rect
  `Cx::layout` would, and re-runs the view only when `f` of the new rect differs from `f` of
  the old one: `layout_with("feed", |r| r[1] < 0.0)` re-runs a header on the frames the feed
  scrolls past the top, not on every scrolled frame. `layout_size` is `layout_with` the
  `[w, h]`, so a view sizing something from an element inside a scrolling viewport no longer
  re-commits on every scroll frame. Both settle on the first layout as `Cx::layout` does, so
  the first frame has the right value. `memo::layout_with` and `memo::layout_size` do the same
  for components without a `Cx`, and memos that read them are reused while the value holds.
  `Cx::layout` keeps its half-point threshold, but movement below it now adds up until it
  counts instead of being lost frame by frame, and a memo holding a `Cx::layout` read is
  reused through moves below it. Layout reads and `on_layout` report an element's width and
  height as layout sized it, so moving an element no longer changes its size by a rounding
  error.

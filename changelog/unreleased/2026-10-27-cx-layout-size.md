- **`Cx::layout_size` reads an element's size without following its position.** It returns
  `[w, h]` of what `Cx::layout` would, and re-runs the view only when the width or height
  changes, so a view that sizes something from an element inside a scrolling viewport no longer
  re-commits on every scroll frame. First-layout settling works as it does for `Cx::layout`, so
  the first frame has the right size. `memo::layout_size` is the same for components without a
  `Cx`, and memos that read a size are reused while the element only moves. `Cx::layout` is
  unchanged.

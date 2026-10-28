- **A motion watcher can read every frame cheaply.** `Runtime::motion_elements(&mut out)` fills a
  reused list with what a frame-by-frame watcher needs of each presented element (id, parent,
  drawn and visible rects, opacity, scale, loop phase, a hash of its words and `MotionElement`
  flags) with no strings copied, and `Runtime::element_path(id)` gives its path and key to cache.
  `elements()` is unchanged.

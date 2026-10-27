- **`El::backdrop_blur(sigma)`: frosted glass over the window's own content.**
  `el().backdrop_blur(8.0).fill(tint)` blurs what is already drawn behind the element, inside
  its rounded or continuous corners and its clip, and draws the fill over it as a tint, with the
  border and children on top. It works through plain `rune::run`, follows scrolling, springs and
  opacity, and renders the same headless. The renderer splits the pass at the element, blurs a
  copy with a half-resolution pyramid cut to the element plus three sigma, and composites it
  through the corner mask. A frame blurs at most `MAX_BACKDROP_BLURS` (4) elements; later ones,
  and any drawn into a target without `COPY_SRC` or a caller's own pass (`Renderer::draw_range`),
  draw only their fill. Frames without one are unchanged. The web backend now asks the canvas
  for `COPY_SRC`. `examples/frosted-bar` shows a list scrolling under a frosted bottom bar.

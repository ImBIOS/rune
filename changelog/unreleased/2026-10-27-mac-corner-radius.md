- **`MacWindowOptions::corner_radius(r)` rounds transparent, borderless macOS windows and
  panels.** The content view's layer clips everything with a continuous curve
  (`.corner_curve_circular()` for a circular arc), so the root view no longer needs to clip
  itself to the radius; a border inside the edge is still the app's. Opaque and bordered
  windows ignore it. Transparent windows with a system shadow now call `invalidateShadow`
  once the first frame after each show, resize, scale change or launcher prewarm reaches the
  screen, so the shadow traces the drawn shape instead of an empty or older frame.

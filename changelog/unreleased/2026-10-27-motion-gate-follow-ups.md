- **Adding or dropping `El::group_opacity` fades, and a shrunk-end reader sticks in the tail.**
  An element that takes on a group-opacity rest level, or drops one (a parked queue row's
  actions shown at rest), now fades from or to full on its motion instead of cutting. A reader
  a collapse glided to the new end sticks to it once the rows are within half a point, so rows
  arriving in the collapse's long spring tail grow in under a pinned end instead of mounting
  below the viewport, and `on_far_from_bottom` measures a shrink glide from the end it heads
  for rather than reporting far for a moment.

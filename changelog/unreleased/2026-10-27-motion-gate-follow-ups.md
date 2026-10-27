- **Adding or dropping `El::group_opacity` fades, and a shrunk-end reader sticks in the tail.**
  An element that takes on a group-opacity rest level, or drops one (a parked queue row's
  actions shown at rest), now fades from or to full on its motion instead of cutting, and a
  dropped one leaves nothing behind once it lands; an instant opacity motion drops it at once.
  A reader a collapse glided to the new end sticks to it once the rows are within half a point,
  without the rows' last fraction lifting it, so rows arriving in the collapse's long spring
  tail grow in under a pinned end instead of mounting below the viewport. `on_far_from_bottom`
  measures from the end collapsing rows head for, so a glide landing early or rows regrowing
  mid-glide don't report far for a moment.

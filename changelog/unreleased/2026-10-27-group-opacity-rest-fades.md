- **A new `El::group_opacity` rest level fades instead of cutting.** Changing an element's rest
  level (a row's actions shown or hidden at rest) used to multiply the new level in on the next
  frame, so they snapped 0↔1. The rest now moves on the element's `opacity_motion`, or the
  commit's spring without one; an instant commit, Reduce Motion and a new mount land it in the
  frame.

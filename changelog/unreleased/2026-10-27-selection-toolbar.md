- **A toolbar can float over a text selection.** `Cx::selection_rects()` returns the selection's
  line boxes in window points (empty with nothing selected) and re-runs the view when one moves
  more than half a point, as `Cx::layout` does; a `memo` that reads it is rebuilt when they move.
  `TextSelection::backward()` says the selection was made dragging up or left, so a toolbar can
  open at the pointer's end. `SelectionChange::dragging()` says a press is still making the
  selection, and **`El::on_selection_change` handlers the selection is inside now hear one more
  call when that press releases**, with the same selection and `dragging()` false, so a handler that treated every
  call as a change sees one extra call per drag. `El::keeps_text_selection()` makes a press on an
  element (a Quote button) leave the selection and the keyboard focus as they are, so its
  handler can read the selection, and a drag that starts on it doesn't start a new one.

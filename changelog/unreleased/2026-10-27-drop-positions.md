- **File drops say where they land: `Tx::drop_point()` and `El::on_drop_move`.** In
  `on_drop_files`, `on_drop_rejected` and `on_drop_hover(true)`, `tx.drop_point()` gives the
  drag's point on the element (its own points, following scroll and transforms, and the
  window's), so an editor can insert a dropped file where it landed. `.on_drop_move(|s, p, tx|
  ..)` hears the point when the target is reached and as the drag moves over it, at most once a
  frame, for a drop caret. Hosts already passed every position (AppKit's at every step, the
  sampled pointer elsewhere); `Runtime::drag_files_to` now also queues a move, and asks for a
  frame, when the drag moves over a target with `on_drop_move`. Nothing changes for targets
  without one.

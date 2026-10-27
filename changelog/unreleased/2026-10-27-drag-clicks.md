- **`Drag::clicks` counts a run of presses for `on_drag`.** 1, then 2, 3 and on for presses on
  the element each within the double-click interval and 4 pt of the one before (AppKit's
  `clickCount`), the same on every phase, so an app-drawn editor can select by words or lines
  and extend by them as a double- or triple-click drag moves. A press that drags past 4 pt
  ends the run. `on_double_press` is unchanged. Code that builds a `Drag` itself (Rune's own
  only; its private field keeps apps from it) sets `clicks`.

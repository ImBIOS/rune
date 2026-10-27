- **A glide the app asked for holds a still pointer's hover until it stops.** During
  `Tx::scroll_to_bottom` or `Tx::scroll_to`, each row passing under a pointer that hadn't moved
  took the hover for a frame, so hover-revealed controls flashed down the list. As on macOS,
  hover now waits for the pointer to move or the glide to end, then lands on the row under it.
  A wheel or trackpad scroll still hovers each row it brings under the pointer, and ends the
  hold. Presses still let go during the glide, and a row that leaves the tree under the held
  pointer still hears `on_hover_change(false)`.

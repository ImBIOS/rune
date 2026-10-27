- **Content shrinking under a reader who scrolled up glides to its new end.** When a growth
  stack's rows left or shrank until it ended above the reader's offset, the viewport sat still,
  then rode the presented end down at the collapse's own speed (thousands of points a second
  from a standstill in one frame). It now glides to the layout's new end from rest on
  `motion::GLIDE`, as `Tx::scroll_to` does, never past the presented end, and sticks to the end
  once the rows get there. The glide follows a further removal to the newer end, ends if rows
  come back past the offset, and leaves the viewport to a wheel, a fling or a kept row until
  the rows settle. Settled, instant and reduced-motion changes still land at once. Any glide
  whose end the content shrinks past now lands there instead of overshooting and springing back.

- **A resting window's timer runs `poll` alone and draws only when it changed something.**
  A due `wake_after` on a shown window with nothing moving used to build, lay out, draw and
  present a frame every time, even when `poll` returned false. Now `poll` runs first, as it
  does for a hidden window, and the next timer is armed; the view, layout and a frame follow
  only when the poll returns true, asks for a scroll, a window frame or the next frame
  (`wake_after(ZERO)`), or leaves a request the window must draw for, and that frame doesn't
  poll again. An app polling a few times a second while idle builds no frames for those
  polls. Motion, scrolls, queued input, hover and tooltip delays, loops, live values,
  `SurfaceDraw` content and polls that step every frame take their frames as before.

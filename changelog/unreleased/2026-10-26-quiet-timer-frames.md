- **A resting window presents nothing for a timer frame that draws what's on screen.** When
  only a due timer (`wake_after`, `poll`) wakes an idle window, the frame is built before a
  drawable is taken, and when its instances, clear colour, surface size, format, scale and
  blending match the last frame presented, with no glyph or image upload, `SurfaceDraw` or
  system material in it, the window acquires and presents nothing. Cursor, IME, secure input,
  help tags and the clipboard still follow the frame. A `poll` that returns true on every tick
  with nothing new on screen no longer costs a GPU frame each time. Input, resizes, scale and
  occlusion changes, animation, traces and compositors draw as before.

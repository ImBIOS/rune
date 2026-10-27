- **`Cursor::Image` shows an app's own cursor.** `Cursor::Image(CursorImage::png(bytes, hot))`
  takes a PNG drawn at 2x (a 64×64 px PNG is a 32×32 pt cursor) and a hot spot in points,
  clamped into the image. The OS draws it, so a pencil over a canvas keeps up with the pointer
  and reaches the window's edge. It comes and goes like the system cursors under hover, press
  capture, overlays and focus changes. On macOS it's an `NSCursor` sized in points, with the
  window's cursor rects off while it shows; elsewhere it's a winit custom cursor made at the
  window's scale, set before the loop sleeps the first time each image is used. Each image is
  decoded once and its OS cursor cached (32 images, 8 MiB). A PNG that doesn't decode, or is
  over 256 px a side, shows the arrow and is logged once. `Cursor` is still `Copy` and `Eq`;
  two images are equal when they have the same bytes and hot spot, which for the same bytes in
  memory is an address check. New example: `examples/sketch`.

- **Images: EXIF orientation, and a decoder panic fails only its image.** `ImageCache` turns a
  JPEG upright by its EXIF orientation (a phone photo stored sideways used to draw sideways,
  with its width and height swapped). A panic inside a codec on a malformed file used to end
  the cache's worker, leaving that image pending forever and failing every later one with
  "image worker stopped"; it now fails that image with "image decoder panicked: …" and the
  worker goes on.

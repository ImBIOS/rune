- **`Player::preload` decodes a clip's first frame before anything draws it.** A clip's poster
  was only requested the first frame it drew, so a page opening on a clip showed an empty box
  for a few frames while a worker opened it. An app that knows which page comes next (Seer
  warms the item Got it lands on) can now call `preload()` on its clips; the first frame that
  draws one shows its picture. It does nothing for audio, once a poster or player has started,
  or headless, where frames decode as they draw.

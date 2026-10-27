- **A window an `AppHandle::update` opens opens in the same turn.** The window host took the
  other requests (open, show, focus, place, quit) before running the queued updates, so one an
  update made waited for the next wake; a windowless app opening its window from an update
  dropped the GPU it had prewarmed at launch first. Updates queued before launch now also run
  before the window's first frame.

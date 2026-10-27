- **`OverlayHandle::set_exclude_from_capture` changes a live overlay's capture exclusion.**
  `OverlayOptions::exclude_from_capture` was fixed when the native window was made, so an app
  that lets recorders see its overlays on demand (a demo mode) had to recreate them. The new
  call flips macOS `sharingType` between none and read-only, and the Windows display affinity,
  on the existing window, shown or hidden; `capture_exclusion` reports the new state.

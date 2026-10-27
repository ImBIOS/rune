- **`Tx::input_time()` says when a handler's input arrived.** Key, IME, pointer, wheel and
  trackpad gesture handlers get `Some(instant)`: on macOS the timestamp of the `NSEvent`
  AppKit dispatched, converted to an `Instant`, elsewhere the moment the window took the event
  from winit. Input that piles up between frames keeps each event's own time. Timers, polls,
  updates, layout callbacks and input fed to a `Runtime` directly see `None`. Nothing else
  changes; handlers that don't ask pay nothing but a stamp on each queued action.

- **`macos_drop` tests skip their pasteboard round trips in a session with no pasteboard.**
  A headless or SSH-only Mac (no window server) has no pasteboard server, and the six tests that
  write a private pasteboard panicked there. They now probe once, print a one-line skip note and
  pass; the pure parts (hover stand-ins, promised types, file URLs, numbered names, URL names,
  operations, step coalescing) run everywhere. No change to apps.

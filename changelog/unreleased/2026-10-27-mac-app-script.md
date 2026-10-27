- **`scripts/mac-app` installs and relaunches a Mac app in place.** It copies a built binary into
  `~/Applications/NAME.app` with an icon, an optional URL scheme for `App::on_open_url` and an
  optional microphone usage string, then starts the new copy before the old one quits, so with
  `rune_mac::remember_frame` the window doesn't blink. `--wait-idle` holds an unattended swap
  while the app is in use and relaunches behind the app in front; `rune_mac::busy` holds it while
  the app is busy. `MAC_APP_SIGN` picks a signing identity (ad hoc by default). See "Relaunching
  a Mac app" in `crates/rune/README.md`.

- **Apps receive links opened with them.** `App::on_open_url(|s, url, tx| ..)` (and
  `WindowSpec::on_open_url`) runs on macOS for each `myapp://...` link the system opens with the
  app, including the one that launched it: Rune installs a get-URL Apple event handler before the
  event loop starts (winit 0.30 doesn't forward these), queues links until the app has started
  (its first window open), then runs them in order as `AppHandle::update`s, waking an idle loop.
  Only apps with the hook install the handler. The app's bundle declares the scheme
  (`CFBundleURLTypes`; see the README) and must validate the string. Other platforms never call
  it.

- **Apps receive links opened with them.** `App::on_open_url(|s, url, tx| ..)` (and
  `WindowSpec::on_open_url`) runs on macOS for each `myapp://...` link the system opens with the
  app, including the one that launched it, which runs before the window's first frame: the event
  loop installs a get-URL Apple event handler before it starts (winit 0.30 doesn't forward these)
  and keeps links until a window whose app has the hook starts, then runs them in order as
  `AppHandle::update`s, waking an idle loop. Without a hook the handler goes once launch is over,
  and an app's own handler is left alone. The app's bundle declares the scheme
  (`CFBundleURLTypes`; see the README) and must validate the string. Other platforms never call
  it.

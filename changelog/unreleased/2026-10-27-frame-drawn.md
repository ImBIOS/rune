- **Apps can watch their own frames.** `App::on_frame_drawn(|rt| ..)` runs after each drawn frame
  of a desktop window with the runtime as that frame presented it (`rt.elements()`, `rt.state`),
  and `rune::last_input()` says when the thread last took an input event from the platform, so an
  app can log a motion glitch it sees in real use. Drawing doesn't change.

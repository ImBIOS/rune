- **Tests can drive the playground.** `App::headless_runtime(size, env)` builds a windowless
  runtime with `App::playground` attached. On it, `Runtime::open_playground(story, variant)`
  opens a story as `--playground NAME --variant V` does, `Runtime::playground_trigger(key)` runs
  a trigger as its key does, and `Play::after` steps run as `Runtime::frame` moves the logical
  clock. `Runtime::playground_state::<P>()` reads the stage's state and
  `Runtime::playground_element(key)` finds an element on the stage; `rt.state` stays the real
  state. `Play::clear()` drops the stage's queued steps, a loop's included, so a trigger can
  take over from a running one.

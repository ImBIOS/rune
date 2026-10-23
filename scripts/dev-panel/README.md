# Rune dev panel

Build `rune-dev-panel` with `--features dev` on a build worker, then run the
binary on the app host. It announces `http://127.0.0.1:7863/`: open that, the
app serves the knobs page and its WebSocket on the same port.
`--windowless` keeps a live runtime for protocol work without opening a window.

The page is knobs' own `mountPage`, pinned to
`@kitlangton/knobs@0.4.0-next.20`. Every Copy action (header, group, row) emits
`rune::tune!` lines through knobs' `formatRust`; playback and scene controls
are left out. Rune contains no panel implementation.

The page lives here as source (`index.ts`). Apps embed a minified bundle,
`crates/rune/src/dev/panel.js`; regenerate it after changing the page or the
knobs pin. `cargo test -p rune --features dev --lib dev` fails while it is stale.

```sh
(cd scripts/dev-panel && bun install)
bun scripts/dev-panel/build.ts           # rewrite the embedded bundle
bun scripts/dev-panel/build.ts --check   # or only verify it
bun scripts/dev-panel/serve.ts           # page development: rebuilt per start
```

`serve.ts` serves an unminified development build on `http://127.0.0.1:7862`
and proxies the app's socket; `PORT` changes its port and `RUNE_DEV_WS` the
socket. For a tailnet HTTPS preview, Tailscale Serve can point straight at the
app's port. Inspect Tailscale's existing Serve mappings first.

The same socket accepts JSON `set`, `setBatch`, `reset`, `action`, `get`, and
`tree` requests. Optional `id` is echoed in `ack`, `tree`, and `error`. An ack
means the command reached a committed frame. A transition ack marks the settled
source frame; its action runs on the following frame. `state` messages include
the current frame counter, descriptors, complete values, and watch readouts.

```json
{"type":"action","key":"rune.transition.Join","id":1}
{"type":"set","key":"rooms.join.spring","value":{"duration":0.5,"bounce":0},"id":2}
{"type":"set","key":"rune.paused","value":true,"id":3}
{"type":"action","key":"rune.step","id":4}
{"type":"tree","id":5}
```

App integration and clock semantics: [DESIGN.md](../../DESIGN.md).

The existing `lab` example also accepts `--features dev`: its Motion scene's
rune-fx shutter pass obeys the panel's Motion blur toggle. Compositors opt in
with `Config { motion_blur: true, ..Default::default() }` and read
`Runtime::motion_blur_enabled()` when constructing their effect pass.

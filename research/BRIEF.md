# Rune research brief (shared by every research agent)

Rune is Kit Langton's planned GPU-powered UI framework and design language: "a better GPUI" for fast,
multiplatform apps. Kit wants to own the full stack (renderer, text, layout, state, animation, platform
shells, tooling), built on the best technology that exists today rather than wrapping someone else's
framework. Today is 2026-10-21; prefer current sources (2025-2026) and say when something is older.

## What matters, in priority order
1. **Runtime performance and feel.** 120 Hz ProMotion on Mac and iPhone, no hitches, low input-to-photon
   latency. The feel benchmark is SwiftUI/Core Animation (render server, interruptible springs, gesture
   velocity handoff). Kit is a motion designer: springs specified by duration + bounce (bounce 0 default),
   interruptible retargeting that keeps position and velocity, layout animation, nothing pops or jumps,
   no hard clipped edges (soft gradient masks), identity-preserving enter/exit.
2. **Hot reload.** Edit to pixels in under ~300 ms with app state preserved, on Mac AND on a connected iPhone.
3. **Agent-written and agent-verified.** Code will mostly be written by coding agents. Optimise for agents
   building and verifying UI: headless deterministic rendering, screenshot tests, frame traces, a queryable
   inspector (tree, layout, timings), fast compiles, small explicit APIs, great error messages.
   Code need not be beautiful; it must be efficient and good.
4. **Reach.** macOS and iOS first (plausibly), then web (WebGPU), Windows, Linux, Android.

Possible first consumers: Kit's apps Flit (Rust launcher; started on GPUI, then moved the launcher panel to
SwiftUI and its 120 Hz glass morph to Core Animation), Scrawl (Rust + GPUI + a native Metal/CA pill),
Hex (dictation app), OpenCode Desktop, and Anomaly's products generally ("build infinite software").

## Rules for research agents
- Write your notes to the exact file path you are given under `~/code/open-source/rune/research/`.
  Markdown. Cite sources inline as links (docs, source files with paths/line numbers, talks, blog posts,
  issues). Prefer reading real source code over marketing pages. Put measured numbers where you can,
  and mark estimates as estimates.
- Structure: summary (5-10 bullets) → findings per item → "steal" / "avoid" lists → recommendation for
  Rune with rejected alternatives and why → open questions.
- To read source, shallow-clone into `/private/var/folders/4t/_jnrck017r5f_jttbc19b7s40000gn/T/opencode/maukea-src/<name>`
  (`git clone --depth 1`), reuse if another agent already cloned it.
- Never run `cargo` builds/tests directly on this machine (Studio). Builds go through HiveMind:
  `hivemind run --kind cargo -- <cmd>` (or `ssh pro` for long-running interactive spikes). Never open GUI
  windows, record, or benchmark on Studio's real screen; never play audio. Never start an OpenCode server.
- Do not edit anything outside `~/code/open-source/rune/research/` (and your scratch clone dir).
- Do not commit; the coordinator commits.
- Final reply to the coordinator: under 300 words, the key conclusions and your recommendation.

# Changelog

Rune has no releases yet; consumers pin `dev` revs. This lists changes that can move an app's output
without a code change (goldens, layout, input), newest first, with the rev that introduced each.

## dev

- **`memo(id, version, || el)` reuses a subtree whose inputs haven't changed.** The closure
  runs again only when `version` or the window's dark mode, scale or reduced-motion preference
  changes; otherwise the view gets the subtree the last build returned, with the same node ids,
  hover, focus and springs. An offscreen `virtualize` row that is memoized costs a lookup, so a
  long transcript whose counter ticks every frame no longer rebuilds every message.
  `Cx::layout` and `Cx::caret` read inside the closure are tracked: a reuse still re-runs the
  view when they move, and builds again when one returns something else. `TextEdit` now
  implements `Hash` over what a field draws (text, selection, composition, focus), so a
  memoized field passes the edit itself as its version. Hot reload and dev tunables drop the
  cache. Virtualized lists also find their mounted rows by binary search now, rather than
  walking every row each commit.
- **Markdown keeps every document on screen parsed.** `Markdown` and `markdown::parse_cached`
  kept the last 32 sources, so a transcript showing more than 32 messages parsed every one of
  them on every commit. The cache (and the code-colour and table-width caches beside it) now
  keeps whatever the last two commits drew, and evicts the least recently drawn of the rest
  past a memory budget. A streaming message reparses only from its last top-level block, and
  only its newest string stays cached.
  New `rune::commit_generation()` counts commits on the thread, for caches like these.
- **`El::truncate_middle()` and `El::truncate_head()`; `Type::caption()`.** Middle truncation
  keeps a single line's start and end with "…" between, as `NSLineBreakByTruncatingMiddle`
  does for paths; head truncation keeps the end. `.truncate()` now cuts between graphemes
  instead of chars, so a combining accent or emoji sequence is never split (text without them
  draws as before), and a truncated selectable text copies its whole string even across the
  ellipsis. A link or span the cut hides is dropped rather than moved onto "…" (which takes
  the style of the text it replaces, as AppKit draws it). `Type::caption()` is `small` on a 16-point line (11 / 16 in the built-in themes),
  for secondary text under a control; it's a method, so app themes that build `Type` as a
  literal get it without a change.
- **`Switch` draws as macOS 27's `NSSwitch`: a capsule track and a pill knob.** The compact
  (default) switch was a 24 × 16 track with a rounded-square 12-point knob; it is now an
  `NSSwitch` mini, 36 × 16 with a 21 × 14 knob inset 1 point, so it is 12 points wider.
  `Switch::control_size(ControlSize)` picks the other measured sizes: Small 44 × 20 (knob
  26 × 16), Regular 54 × 24 (32 × 20), Large 64 × 28 (38 × 24) and ExtraLarge 80 × 36 (48 × 30).
  `State::Hover` and `State::Pressed` stretch the knob towards the middle instead of rounding
  its corners. The standard appearance keeps its 28 × 16 size with a capsule track and round
  thumb.
- **Breaking (visual): a `Button` with no size set is 24 points tall, as a regular macOS
  push button, not 28.** New `ControlSize { Mini, Small, Regular, Large, ExtraLarge }` (AppKit's
  names, `#[non_exhaustive]`) with `Button::control_size` and `IconButton::control_size` sizes
  controls as macOS 27 measures them: push buttons 16, 20, 24, 28 and 36 points tall with 8, 10,
  12, 14 and 18 points of padding a side, Mini in 9-point type and Small in `ty.small_medium`;
  icon buttons are squares of the same heights. `ButtonSize` and `.size()` are deprecated and
  keep their old metrics, so existing calls draw as before: for a `Button`, `Small` (24) is
  `Regular`, `Normal` (28) is `Large` and `Large` (32) sits between `Large` and `ExtraLarge`;
  for an `IconButton` the names match (20, 24 and 28 are `Small`, `Regular` and `Large`). Only a
  `Button` that never called `.size()` moves: set `.control_size(ControlSize::Large)` to keep
  28. Toast actions now use `Regular` (same height, 12-point padding and the `md` radius).
- **`--keys` caps title-case key names.** A key name without a glyph drew upper case
  ("CAPSLOCK", or "SPACE" from a script's text key under a modifier). It now draws title case
  ("Capslock", "Space"), as the in-app keycaps do; single letters stay capitals and `F12` is
  unchanged.
- **Hiding a window or overlay is a focus loss for its keys.** `on_physical_input` handlers
  used to hear `PhysicalInput::FocusLost` only when the platform reported a blur, which a hide
  on Wayland, with `release_gpu`, or of an overlay never did, and never in `--desktop` or
  headless scripts. They now hear it on every hide and every `Focus(false)`, once until the
  window has focus again, and a hidden window or overlay runs it without waiting to show.
- **Modifier keys on their own reach `on_key`, when an app asks.** `App::modifier_keys()` (also
  `WindowSpec::modifier_keys()` and `ConfiguredOverlay::modifier_keys()`) sends Shift, Control,
  Option/Alt, Command/Super and Fn going down and up as `Key::Modifier { modifier, pressed }`,
  with the side in `Modifier` (`ShiftLeft`, `AltRight`, …, `Modifier::side()`). `mods` hold
  the state after the change. A focused `on_physical_input` handler, such as a shortcut
  recorder, hears the key first, and one it takes is never sent. A blur or hide releases every
  key held. Nothing changes for apps that don't opt in, but `Key` has a new variant, so an
  exhaustive `match` on `Key` with no `_` arm stops compiling: add `Key::Modifier { .. } => …`
  or a `_` arm.
- **`App::try_run` returns startup errors.** `App::try_run`, `try_run_windows_with` and
  `try_run_windows_with_args` return `Err(String)` when the event loop can't start (no Wayland
  or X11 display, as over ssh or in CI) or fails, where `run` panics. On a Mac no one is logged
  in to, an app used to start AppKit and wait forever without a window; it now fails at once
  with "no window server session", and `run` panics with that message. `run` and its siblings
  otherwise behave as before.
- **`Pixels::set` wakes the windows showing it.** A `Pixels` is now a live source like `Live`:
  `set` (from any thread) wakes exactly the windows and overlays whose scene shows it with
  `El::pixels`, and no others. Sets between frames coalesce into one wake and one upload; a
  hidden or covered window sleeps and draws the latest image when it shows again; an element
  keeps waking while its exit (or its parent's) draws it, and stops once it's removed or its
  window closes. `LiveText` on an exiting element now keeps updating the same way, where it
  used to freeze as the exit began. Apps that woke a window after `set` themselves can drop
  that. Finished `ImageCache` decodes also wake the windows showing them.
- **`El::on_screen_layout` follows a window's own moves in the same frame.** An animated
  resize or centre, `place()`, a position change and a resize landing used to report the new
  layout against the previous origin, then correct it a frame later when the move arrived.
  The window now reads its origin as soon as it sets its frame.
- **Prewarmed overlays report where they are.** A prewarmed overlay's `El::on_screen_layout`
  used to report rects from (0, 0) when it warmed, and its warm frame drew at the scale winit
  gave the window rather than the panel's display. A rect that moved while it was hidden
  also wasn't delivered when it was shown. It now warms at its placed origin and display
  scale, and the reveal delivers rects that moved while hidden.
- **A fading-out overlay clicks through on macOS.** `OverlayHandle::fade_out(d)` stopped Rune's
  input at once, but a panel that wasn't prewarmed still took clicks and scrolls for the whole
  fade, so they never reached the window below. It now ignores the mouse from the start of the
  fade.
- **`wake_after(Duration::ZERO)` never spins the loop.** A wake due sooner than a frame asks
  for the next frame. A window or overlay a display link paces (macOS) draws it on the next
  tick: windows now keep their link running for it too, as overlays do. Hidden, covered, faded
  out or unpaced (no display link, or not macOS), it waits a frame interval (60 Hz) from the
  last poll. It used to wake the loop at once, so a poll that kept asking ran as fast as the
  loop could turn while covered, after a fade-out or without a display link.
- **Overlays on macOS draw at the scale of the display they're placed on.** An overlay used to
  take its scale from winit's window when shown, which for a hidden panel needn't match the
  display it's about to appear on, so a panel placed on a display of another scale could draw
  its first frames at the wrong scale. It now reads the panel's `backingScaleFactor` once it's
  on screen, and before that the scale of the display holding most of its frame. `OverlayPlacement::Rect` on
  `DisplayTarget::Primary` documents that it's a global rect that may sit on any display.
- **`El::on_screen_layout`** reports an element's rect in global screen points (from the
  primary display's top-left, y down), the space of an overlay's `OverlayPlacement::Rect` on
  `DisplayTarget::Primary`, so a window can open an overlay exactly over one of its elements.
  It runs when the element lays out somewhere new and when its window moves. Headless runs
  report against the script's virtual desktop: a window opens centred on the primary display,
  or at its `position`, and `place()` moves it.
- **Overlays draw through a fade-out.** `OverlayHandle::fade_out(d)` stops input at once but
  keeps drawing until the fade ends (it used to stop drawing at once), so an exit transition
  started in the same turn plays out as the window fades, and the panel isn't treated as
  occluded while its alpha falls. Showing again mid-fade carries that motion on instead of
  settling it. A shown overlay whose poll calls `tx.wake_after(Duration::ZERO)` now keeps its
  display link running and polls every frame, instead of stopping after each frame and
  restarting on a timer, so a poll stepping its own animation needs no looping element.
- **`El::drag_threshold(points)`** sets how far the pointer travels before that element's
  `drag_out` starts, in place of `DRAG_THRESHOLD` (4 pt, 8 on Linux), which stays the default.
- **Hidden overlays follow theme changes.** A hidden or prewarmed overlay switching between
  light and dark used to keep the old theme until it was shown and the theme changed again; it
  now takes the change while hidden and redraws in it before it's revealed.
- **Overlay window levels and fades on macOS.** `ConfiguredOverlay::macos_level(level)` and
  `OverlayHandle::set_macos_level(level)` set the AppKit window level (e.g. 3 for
  `.floating`), changeable while shown. `OverlayHandle::fade_in(d)` and `fade_out(d)` fade
  the window's alpha with AppKit's ease; elsewhere they show and hide at once. Overlay panels
  now go exactly where they're placed at any level, including over the menu bar, where AppKit
  used to push windows below it at levels under the menu bar's. Overlays read the system
  light or dark theme when their window is created rather than on the first theme change.
- **`Shadow::core_animation(x, y, radius, color)`** builds the shadow a CALayer draws for a
  `shadowRadius`: Core Animation's blur has a standard deviation of about the radius, so
  `blur` is twice it.
- **Continuous corners.** `El::continuous_corners()` rounds an element's fill, border, image,
  outline, `clip_rounded` and system material with Apple's continuous curve (CALayer
  `.continuous`, SwiftUI `.continuous`) instead of circular arcs; shadows stay circular.
  `LayerSpec::clip_radius` takes a negative radius for the same shape. Adds the public field
  `Visual::continuous_corners`, so a `Visual { .. }` literal without `..Default::default()`
  stops building. `system_material(SystemMaterial::Glass)` turns the flag on, since
  `NSGlassEffectView` draws no other curve, so the element's fill, clip and shadow hole match
  the glass; `Popover` follows the flag.
- **System backdrop materials.** `El::system_material(SystemMaterial::Glass | Popover)` backs
  an element with Liquid Glass (`NSGlassEffectView`, macOS 26+; the popover material before)
  or the popover material, as a native view under a transparent window's Metal layer that
  moves in the same Core Animation transaction as each frame. Opaque windows, other platforms
  and headless renders draw the fill or a neutral tint, so goldens don't change. An outer
  shadow on a material element no longer paints under its interior when the native view is
  on. Each native view stays with its element while it's mounted; an element dimmed to zero
  takes none. Once the last material element unmounts, the window stops presenting inside
  transactions. `rune::{SystemMaterial, MAX_NATIVE_MATERIALS}` are new root exports.
  `RUNE_SYSTEM_MATERIAL=legacy|unsynced|off` selects other paths. New example:
  `examples/glass-card`. See the crate README's "System materials".
- **Occluded overlays and windows idle instead of spinning; their timers still fire.** A shown
  overlay or window whose displays sleep or that is covered no longer turns the event loop
  (about a full core before) when its drawable is occluded or times out. It runs updates, `poll`
  and due `wake_after` timers without drawing, so a timer can still hide it; re-checks the surface
  after 250 ms, backing off to 2 s; and redraws once when uncovered. A covered overlay commits
  nothing until then, so motion started while it was covered begins on that frame. A
  `wake_after` from `AppHandle::update` or `OverlayHandle::update` now arms while the window or
  overlay is hidden, covered or minimised, rather than on its next drawn frame. Without a display
  link (made while the displays slept) macOS motion is paced by a ~60 Hz timer rather than a busy
  loop, and an overlay's 120 Hz pointer poll for `OverlayInput::Regions` rests while it is occluded.
- **Help tooltips.** `El::help("…")` shows a tooltip after the system hover delay, as SwiftUI's
  `.help`. A press, scroll, key or the pointer leaving hides it until the pointer comes back. It
  takes no hover or clicks. macOS windows use AppKit's own tooltip, registered per element on
  device pixels and replaced only when it moves, and left out behind a modal or under a popover;
  overlays and other platforms, and headless, have Rune draw one (0.5 s, the double-click time
  on Windows; `Runtime::help_delay`). Headless
  `--snap` shows the drawn tooltip, and `--tree` gains a top-level `tooltip` plus `help` on each
  element. `ElementInfo` gains `help` and `selected`. Elements without `help` are unchanged.
- **Selectable text.** `El::selectable(true)` lets people select and copy read-only text, as
  SwiftUI's `.textSelection(.enabled)`: drag, double-click a word, triple-click a paragraph,
  shift-click to extend, Cmd/Ctrl+C, Cmd/Ctrl+A, and a Copy context menu on macOS. After a
  click on the text, plain Cmd/Ctrl+A selects all of it and plain Cmd/Ctrl+C copies a non-empty
  selection, before the view's key handlers; with Shift, Option or Control added, or nothing
  selected, they fall through as before. Inside a row that handles input the row keeps its
  clicks, hover, press paint and context menu; a drag that selects text doesn't press, and in a
  draggable row dragging drags the row. A view that changes, removes or unselects the text drops
  its selection. A link inside selectable text still opens on click. Scripts gain a
  `triple-click` event. Text without `selectable` is unchanged.
- **`El::selection_color`** sets the selection highlight of a field or selectable text; both
  draw it the same way, and fields draw exactly as before. Kit's `k.selection()` is the colour
  its fields use (unchanged), so selectable text can match them.
- **Radial and angled linear gradients, opt-in sRGB blending, and shadow blur docs.**
  `El::radial_gradient(center, radius, stops)` draws circular (`f32`) or elliptical (`[rx, ry]`)
  radial gradients with up to 8 `(f32, Color)` stops in `0.0..=1.0` in the instanced uber-quad
  pass (`KIND_RADIAL`), with `center` in normalized element coordinates (`[0.5, 0.5]` = centre)
  and `radius` in points; `El::linear_gradient(angle, stops)` draws linear gradients at `angle`
  radians (CSS convention) with up to 8 stops. Both compose with `.fill`, `.border`, `.radius`,
  `.opacity` and `.blur`, and spring stop colours in Oklab. `App::srgb_blend(true)`,
  `WindowSpec::srgb_blend(true)` and `ConfiguredOverlay::srgb_blend(true)` (plus
  `Renderer::set_srgb_blend`, `SurfaceRenderer::set_srgb_blend`, `Runtime::srgb_blend` and
  `Color::premul_srgb`) opt into gamma-space (sRGB) compositing for translucent fills, gradients,
  shadows, blurs, shapes, text, premultiplied and straight-alpha images, native/video surfaces,
  headless `--keys` overlays and `--snap-on-patch` so faint glows over dark backgrounds match CSS
  and Core Animation (`CALayer.shadowRadius ≈ σ`, CSS `blur = 2σ`); `srgb_blend` is ignored with a
  warning when a `Compositor` is set, and default linear-light rendering is unchanged.
  `rune::RadialRadius` is a new root export.
- **macOS drag-out matches an AppKit drag source more closely.** New `El::on_drag_start(|s, tx|)`
  runs once the system takes the drag, before its `on_drag_end`, so the app can style the source
  while it's dragged (the example dims its tile to 0.28). Every start now gets exactly one end, in
  order: a drag-out no system took (headless, or a cancelled press) ends `Cancelled` by the next
  frame. New `DragOperation` (`Copy`, `Move`, `Link`) with `DragItem::operation(op)` to offer just
  that, `allow(op)` to add one and `offers(op)`; `allow_move()` is `allow(DragOperation::Move)`,
  and a drag still offers only Copy by default. Modifier keys pick among the offered operations
  only when there are several. `DragItem::opaque(color)` composites the drag image over `color`,
  and `DragImage::over_element()` starts a supplied image over the element (at its `size`, else
  the element's full unclipped size) instead of centred under the pointer. Behaviour changes: the
  default snapshot is placed at its pixel-aligned crop, so it is no longer resampled to the
  element's fractional rect, and a drag let go over the app's own windows ends `Cancelled` and
  slides back (winit's file-drop handler used to accept it as a Copy and queue a `DroppedFile` for
  the app's own file).
- **Apps can resize and centre their window.** `Tx::resize_window(size)` animates the
  window's content size on the change's spring with the top edge fixed and the window kept on
  its screen; `Tx::resize_window_instant` jumps, and `Tx::center_window` centres as AppKit's
  `center` does. `WindowHandle::{resize, resize_instant, center}` do the same from outside a
  handler. On macOS the content follows the frame every display frame; Windows and Linux
  resize at once; Wayland doesn't keep the window on screen. Headless runs move the viewport
  on the virtual clock. Requested sizes stay inside the window's minimum and maximum.
  `App::fit_content_height()` opts a window into following its content's natural height, with
  no default minimum height. See the crate README's "Resizing and centring a window".
- **Headless recordings of the whole desktop.** `--desktop BACKDROP.png` renders every window a
  multi-window app makes, each at its placed frame with rounded corners and a soft shadow (larger
  for the key window), over the backdrop scaled to fill the output. Showing, hiding, placing and
  focusing follow the script and the app over time, and script pointer coordinates are output
  points. It works with `--video`, `--out`, `--snap`, `--strip`, `--keys` and `--pointer`;
  `--tree` describes the `--window` window and lists every window's frame. `headless_run()` adds
  `desktop()`, `window_frames()` and `windows_at(t)` (`rune::tooling::WindowFrame`). Runs without
  `--desktop` are unchanged. `examples/launcher` records a notes app and its ⌘Space panel.
- **A scroll cancels a held press.** Scrolling a list by wheel or fling while a press inside it
  is held now cancels the press, as a `UIScrollView` cancels its content's touches: the element
  lets go and the release doesn't press. A drag keeps its capture, and programmatic scrolls
  (a chat pinned to its bottom) don't cancel.
- **Nested scales compose.** A scaled element inside a scaled element used to scale about its
  own unscaled centre, dropping its parent's anchor, so it jumped when its own scale (a press, a
  live scale) turned on, and a product of 1 ignored the parent's offset. It now scales about its
  centre where its parent draws it, and hit testing follows. This moves the output of apps
  that nest scales; a single scale draws exactly as before.
- **Text scales with its box, and `El::scale`.** Text inside a scaled element used to keep its
  full size while the box shrank around it. Under `live_scale`, `press_scale`, scale transitions
  and scale beats in a sequence loop, glyphs, span backgrounds, selection and caret now scale
  with the box. Text under a still scale rasterizes at the scaled size; during a spring it
  samples its rest-size glyphs. This moves the output of any app that scales text. Text that
  isn't under a scale is unchanged. `El::scale(k)` is a static scale on the same mechanism:
  about the centre, no layout change, with hit testing on the drawn rect. A `scale` an app
  changes (a zoom slider) samples rest-size glyphs until it holds still for 0.1 s, then
  rasterizes at the scale rounded to 1/32, so the glyph atlas stays bounded.
- **Pictures size from their image.** An element with `pixels`/`pixels_fit` that sets only
  its width or only its height takes the other side from the image's aspect ratio (it used to
  take its content's size, usually 0). Pictures with both sides set, or neither, are
  unchanged. `El::aspect_ratio(r)` sets a ratio on any element. A picture whose image lands
  or changes shape takes its new size at once, even on a frame that re-runs the view.
- **System monospaced font.** `Family::SystemMono` shapes with SF Mono on macOS, Cascadia
  Mono or Consolas on Windows, and fontconfig's `monospace` on Linux, loaded from the system
  on first use and falling back to the mono slot. `Family` gains a variant, so an app's
  exhaustive `match` on it needs a new arm.
- **Double press.** `El::on_double_press` runs on the second of two presses inside the system
  double-click interval (now read on Windows too) and 4 pt of each other; `on_press` still runs
  for both. Only clicks pair: a press that travels more than 4 pt, or is held past the interval,
  is neither half of one. Scripts gain a `double-click` event. Elements without it are unchanged.
- **Press scale.** `El::press_scale(0.97)` shrinks an element about its centre while a press
  on it is held and springs it back on release. Layout and hit testing keep the resting size,
  and a held press keeps its hover. Reduce Motion turns it off. Nothing changes for elements
  without it; rune-kit buttons don't opt in.
- **An explicit `optical_size(o)` now also picks the `trak` tracking:** text is tracked as Core
  Text tracks `o`-point text scaled to the set size, so
  `TextStyle::new(size * k, w).optical_size(size)` matches a SwiftUI view at `size` with
  `scaleEffect(k)`. This is what Core Text does with its optical size attribute
  (`kCTFontOpticalSizeAttribute`): a 28 pt font at optical size 14 lays out exactly as 14 pt
  text scaled 2x. Setting only the `opsz` axis (`variations("'opsz' 14")`, or
  `kCTFontVariationAttribute`) still tracks at the point size. Auto optical size is unchanged.
- **rune-kit `Markdown` colours fenced code** in ts, tsx, js, json, rust, swift, css, html,
  sh/bash, sql and yaml; on by default, so code blocks in those languages render with coloured
  tokens (goldens with such blocks move). Unknown languages and indented blocks stay plain, and
  layout is unchanged (colour-only spans). New: `rune_kit::markdown::Syntax`,
  `MarkdownStyle::syntax(kind, color)` and `syntax_highlight(bool)`. `Block` is unchanged.
- **`MarkdownStyle::diff_text(added, removed)` and `diff_text_tint(amount)`** colour the text
  of a diff's added and removed lines (`diff_text_tint` blends the text colour toward the
  theme's green and red). Off by default: line text keeps the body colour.
- **rune-kit `Markdown` table cells use tabular figures** (`'tnum' 1`), so numbers line up down
  a column; only digits change width. New `MarkdownStyle` builders: `table_outline(Option<Color>)`
  (`None` draws no outline and adds a rule under the last row), `table_rules`,
  `table_header_text` and `table_tabular_figures(bool)`. `table(border, header_fill)` still sets
  the outline and the rules. The default look is otherwise unchanged.
- **rune-kit `Markdown` tables no longer wrap a cell when the table has room.** Columns started
  at zero width and grew by their content width, but each column's padding was reserved
  first, so wide columns came up short and their longest cell wrapped. Columns now start at
  their content's width and shrink in proportion to their text when the table doesn't fit.
  Most tables' column widths move by a few points.
- **Video and audio playback.** `Player` (alias `Video`) plays a file or URL:
  `el().video(&clip, SurfaceFit::Fit)` draws a video (muted by default), and `Player::audio`
  plays audio with no element (unmuted by default). macOS uses AVFoundation; headless runs
  are deterministic and silent; other platforms report `Failed`. Adds the
  `objc2-av-foundation`, `objc2-core-media` and `objc2-core-graphics` macOS dependencies.
  `rune::{Player, Video, MediaSource, PlayerState}` are new root exports, so a glob import
  of `rune::*` can clash with an app type of the same name. See the crate README's "Playing
  video and audio".
- **Headless recordings honour single-window transparency and physical size**, including
  alpha in PNG and ProRes output. Default window pixels are unchanged. `--duration auto --out`
  selects headless PNG recording; `--out` alone uses automatic duration unless `--native` is set.
  Exact mark names such as `step-1` take precedence over time offsets, malformed offsets report
  the available marks, and video filenames starting with `-` work.
  `headless_run().events()` includes pop-up menu picks and dismissals.
- **Global script shortcuts only run their registered handler**, even when a window is key;
  they never reach `on_key` or physical-input handlers. Unregistered chords fail the script.
- **Recording keycaps follow light and dark backgrounds.** `--keys-theme auto|light|dark`
  selects the colours of their fixed screencast style; `auto` follows the app's root fill.
  Overlays blend correctly over transparent frames, and a failed video encoder stops the run
  and reports its exit status.
- **Headless frames can show the script's keys.** `--keys [CORNER]` draws screencast keycaps
  (typing grouped into bursts, ⌃⌥⇧⌘ chords with repeat counts, glyphs for named keys) over
  `--video`, `--out`, `--snap` and strip frames, and `--pointer` a ripple at each press. A
  separate runtime draws them after the app, so `--tree` and `--trace` are unchanged.
- **Scripts can press global chords.** `{"type": "shortcut", "chord": "cmd+space"}`
  runs the app's registered shortcut handler. `{"shortcut": …}` is unchanged.
- **Scripts can mark times.** `{"t": 3.2, "type": "mark", "name": "end"}` has no input effect;
  apps read marks through `headless_run().marks()` and `mark(name)`. `--at` and `--start`
  take `mark:NAME` with an optional offset (`mark:end+0.5`), and `--duration auto` (the
  default for `--video`) runs to the `end` mark, or one second after the last event.
- **Apps can read the headless run.** `rune::tooling::headless_run()` returns the run's script
  path, its expanded events with their times, `--scale`, `--fps`, the viewport, `--start`, the
  frames it writes and its last frame's time, from `init` on. It is None in a window.
- **Headless runs can write a finished video.** `--video out.mp4` (`.mov` for ProRes 4444,
  `.webm` for VP9) streams raw frames into an `ffmpeg` child at `--fps` (default 60), so no
  frames touch disk; ffmpeg must be on `PATH`. Its length is `--frames N`, `--duration S`, or
  through one second after the script's last event. Readback now keeps three frames in flight
  and PNGs (`--out`, `--snap`) encode on worker threads.
- **rune-kit feature `mermaid` draws ```` ```mermaid ```` flowcharts and sequence diagrams
  natively** inside `Markdown` once the fence closes, and through `rune_kit::mermaid::render`.
  Without the feature, or for unsupported diagrams, the fence stays a code block (with a
  "Diagram not supported" hint when the feature is on). New `MarkdownStyle` builders:
  `diagram`, `diagram_surface`, `diagram_note`, `diagram_font`, `diagram_max_width`.
- **rune-kit `Markdown` renders GFM tables and tints ```` ```diff ```` fences.** Pipe tables
  were flattened into run-on text before; they are now tables with a header row, alignment
  and columns sized from their content (narrow tables wrap, then clip). `diff` and `patch`
  fences tint `+`/`-` lines and dim `@@` headers instead of drawing a plain code block.
  Documents with neither render exactly as before. New `MarkdownStyle` builders: `table`,
  `table_min_column`, `diff`, `diff_hunk`.
- **rune-kit `Markdown` works without a `Kit`:** `Markdown::render(&MarkdownStyle)` with
  `MarkdownStyle::neutral(dark)` or `MarkdownStyle::from_seed(seed)`. `build(&kit)` is unchanged.
- **rune-kit `Tooltip` works without a `Kit`:** `Tooltip::label(..).render(&TooltipStyle,
  &mut layer)` with `TooltipStyle::{neutral, from_kit}` and its builders. `build(&kit)` is
  unchanged.
- **`rune::text::measure(text, spans, style, max_width)`** returns a text's laid-out size outside
  a frame, with the fonts the app loaded; `Some(0.0)` gives its min-content width.
- **Text in fonts with a `trak` table (the macOS system font) now has the same width at every
  display scale, matching AppKit.** The font's size-specific tracking used to be looked up at
  the size in device pixels, so at 2x 14 pt text was tracked as 28 pt text. It is now applied
  at the point size, as Core Text does, within 0.003 pt per glyph of Core Text at 8–34 pt. At
  2x, system-font text gets narrower per glyph by 0.18 pt at 13 pt, 0.34 pt at 14 pt, 0.52 pt
  at 16 pt and 0.63 pt at 17–20 pt (a 34-character 14 pt label is 11.6 pt narrower), and wider
  by 0.19 pt at 11 pt and 0.36 pt at 8 pt; 12 pt and 24 pt barely move. Text at 1x moves by
  at most 1/2048 em per glyph. Wrapped text can break at different words. Fonts without
  `trak`, like the bundled Inter and IBM Plex Mono, are unchanged. `TextStyle::tracking` adds
  to the font's tracking; `TextStyle::features("'trak' 0")` turns the font's tracking off.
- **Shortcut labels follow layouts that change under ⌘ (macOS).** `Shortcut::keycaps` (the
  recorder, its error text and native menu key equivalents) now reads the key's character
  under the chord's ⌘ and ⌃, so on "Dvorak – QWERTY ⌘" ⌘ plus the US-K key shows ⌘K, not ⌘T,
  and menu items fire on the key they show. ⇧ and ⌥ still show the base key (⌥⌘K, not ⌥⌘˚),
  as AppKit does. The key keycap is now a capital ("K", was "k"), F-keys read "F5", and on
  macOS named keys use menu glyphs: ↩ ⇥ ⎋ ⌫ ⌦ ↖ ↘ ⇞ ⇟ (Space stays "Space"). `Keybind`
  upper-cases only single characters, so "Space" no longer draws "SPACE". The layout lookup
  also declares `UCKeyTranslate`'s string lengths at their real 8-byte width; it wrote past a
  4-byte stack slot on every call.
- **Native pop-up and context menus.** `tx.popup_menu(PopupMenu, anchor, on_pick)` opens an
  `NSMenu` over an element key (a picker's selected item sits over the element, as SwiftUI's
  menu-style `Picker` does) or at a `PointerPress`. `on_pick` gets the picked id after the menu
  closes, or `None` on dismissal or when its window hides, closes or quits. Frames and updates
  keep running while it's open. Other
  platforms answer `None` on the next frame (`popup_menu_supported()` is false). New:
  `PopupMenu`, `PopupItem`, `PopupAnchor`, `PopupMenuError`. Headless trees gain a `popupMenu`
  object while one is open, and scripts gain `menu-pick` and `menu-cancel` events. Nothing
  changes for apps that don't call it.
- **`El::matched(id)` adds shared-element motion** (SwiftUI `matchedGeometryEffect`, Motion
  `layoutId`). When an element with an id leaves and another with that id mounts elsewhere in
  the same commit, the new one springs from the old one's presented rect, radius, fill and
  opacity to its own layout (`REFLOW` by default), painting above the window with its
  children scaled to cover the moving frame. `rune_motion::Scene::match_geometry_offset`
  starts a match from a rect offset outside the scene (a scroll). Apps that don't call
  `matched` draw the same pixels. In `rune_motion::Scene`, screen travel
  (`LayoutAnim::screen()`) now lands only once its ancestors' presence, scale, opacity and
  blur settle too, so it can't jump onto a parent mid-transition.
  `matched` draw the same pixels.
- **Elements can be dragged out to other apps (macOS).** `El::drag_out(|s| Some(DragItem))`
  starts a system drag once the pointer passes `DRAG_THRESHOLD`, carrying file URLs, PNG data
  or text to Finder, System Settings and other apps; `El::on_drag_end` reports a `DragOutcome`.
  A drag-out element never runs `on_drag`, and once the system drag starts its press is
  released without firing `on_press`. Elements without `drag_out` are unchanged, and other
  platforms ignore it. Headless traces log `drag-out <path>` and `drag-end <outcome> <path>`,
  and `--tree` marks the node `dragOut`. `ElementInfo` has a new pub `drag_out` field, so code
  that builds it as a literal or destructures it without `..` needs updating.
  `rune::{DragItem, DragImage, DragOutcome}` are new root exports, so a glob import of
  `rune::*` can clash with an app type of the same name.
- **On macOS, the first `open()` of a window that doesn't exist yet now activates the app**,
  like every later `open()`. Before, the new window became key while another app stayed
  active, so typing and ⌘W went to that app. The window opened at launch and nonactivating
  panels are unchanged.
- **`App::window` windows still open when the main window closes now run their `on_close`,
  and `WindowSpec::from_app` starts from the app's `window_options`** (before, it used the
  defaults, so headless runs dropped `transparent` and `physical_size`).
- **One app can host windows with different state types.** `App::window(WindowSpec<T>)` adds
  an ordinary window with its own state and returns its `AppHandle<T>` and `WindowHandle`.
  It honours `visible: false`, closes by hiding (its `on_close` runs and reopening keeps the
  state), and never keeps the app running alone. `run_windows*` factories may return
  `AnyWindowSpec`s (`WindowSpec::boxed()` or `.into()`) to mix state types. Factories
  returning `Vec<WindowSpec<S>>` still compile, but a turbofish like `run_windows::<S>(..)`
  no longer does: drop it. Headless `--window KEY` and `--list-windows` include `App::window`
  windows and render them with their own state.
- **Hit testing leaves out an element's right and bottom edges everywhere.** Press, hover,
  secondary press, links, wheel targets and hover and press highlights now use the same
  half-open test as pointer-move, `on_hover_change` and overlays (like `NSPointInRect` and
  `PtInRect`). A pointer exactly on a shared edge belongs to the right or lower sibling
  for every handler. A pointer exactly on a lone element's right or bottom edge no longer
  hovers or presses it.
- **rune-kit `List` rows with an action are keyboard-reachable.** When any row has
  `on_select`, the rows sit in a `listbox` focus group: Tab enters the list once (on the
  selected row, if any), arrows move between rows, and Enter selects. Those rows show
  their highlight on keyboard or pointer focus (`focus_highlight`) instead of on hover. Lists
  without actions are unchanged.
- **A disabled, invalid rune-kit `TextInput` no longer has a red caret.** The caret now follows
  the text colour, as `TextArea`'s does. Only that one state changes.
- **rune-light's `Scene` and `Gi` have an `emission` field.** `Scene::emission` (an optional
  output-resolution texture) lets effect layers light the UI through the GI, scaled by
  `Gi::emission`. Struct literals need `emission: None` and `..Default::default()`. GI media now use
  sub-texel coverage, so a moving emitter no longer pops. New: `LightRenderer::inspect`,
  per-cascade `Timings` and `GlassRenderer` (thick refracting glass).
- **rune-fx effects look different, and some structs gained fields.** Fire rises and narrows with
  a white-yellow core and smoke, electricity strikes and re-strikes as fractal lightning, glows fall
  off within about 2.5 radii, sparkles are a few crisp four-point stars, and one-shots (ripples,
  pings, zaps, bursts) overlap instead of restarting. `FxLayer::opacity`, `ElectricParams::zap`,
  `SparkleParams::burst` and `FireParams::smoke` are new pub fields, so struct literals without
  `..Default::default()` need them. New: `OneShots`, `FxRenderer::blur_backdrop` (frosted glass)
  and `FxRenderer::set_fire_sim`/`simulate` (an opt-in GPU fluid for fire and smoke).
- **Rotating icons turn their artwork, not just their outline.** An icon with
  `.looping(LoopSpec::rotation(..))`, such as rune-kit's `Loader` arc, turned its quad but kept
  sampling the atlas unturned, so the arc stayed put while the rotating square cropped it. The
  artwork now turns with the quad and is bilinear-filtered while turned. Icons at rest
  (angle 0) draw exactly as before.
- **Rune builds for Intel Macs.** `x86_64-apple-darwin` stopped compiling where Rune read
  the Objective-C runtime's `BOOL`, which is `i8` there and `bool` on Apple silicon.
- **Overlay and launcher panels keep their view when something restyles winit's hidden
  window.** On macOS an overlay or nonactivating launcher panel shows winit's view while
  winit's own window stays hidden. That hidden window answered `contentView` with the moved
  view, so AppKit calls on it that rebuild the frame (`setStyleMask:`, `setContentView:`) pulled
  the view out of the panel, which then dropped every click. Starting a ScreenCaptureKit stream
  in the same app does this: ReplayKit restyles the first window in `NSApp.windows`. Now only
  winit gets the moved view; AppKit and other frameworks see the hidden window's real, empty
  content. Likewise only winit's frame, level and mouse-ignoring calls on a launcher's hidden
  window move its panel, so restyling that window no longer resizes the panel, and a title set
  there also stays on the hidden window for `Window::title()`. If the view is taken anyway,
  the panel puts it back on the next event-loop turn or event and releases an overlay press
  it was holding, and an overlay press whose release never reached winit stops holding the
  pointer after 2 s. The hidden window also keeps winit's class: Rune adds its forwards to winit's
  window and view classes instead of subclassing each window, which under KVO had made
  `-[window class]` return nil.
- **Borders can be dashed.** `El::border_dash(dash, gap)` dashes an element's `border` or
  `border_gradient`, in points, with butt ends:
  `el().radius(13.0).border(1.0, amber).border_dash(4.0, 3.0)`. Dashes follow the ring's
  centre line round the corners. The pattern starts with a dash where the top edge leaves the
  top-left corner and runs clockwise, and its period stretches or shrinks a little so a whole
  number of dashes meets itself at that seam. `El::border_dash_phase(points)` shifts it, like
  SwiftUI's `dashPhase`; a phase of `dash + gap` is one period. Border colours spring as before;
  the phase is applied as given. The fill shows through the gaps, so a dashed element draws its
  fill and its ring as two instances, the ring as the new `render::KIND_DASHED` (10), which
  `rune-ref` renders too. Elements without dashes paint the same instances as before.
  `Visual` has new `border_dash` and `border_dash_phase` fields.
- **Text can draw in capitals.** `El::text_case(TextCase::Uppercase)` (or `Lowercase`) draws
  a text element's letters cased without changing its string, like SwiftUI's `textCase`:
  `text("Text channels").text_case(TextCase::Uppercase)` draws "TEXT CHANNELS". Casing is
  `str::to_uppercase`'s, so "ß" draws "SS". The dev tree, `ElementInfo::text`, link labels and
  live text keep the authored string, and `Span` and `Link` ranges stay byte ranges of it even
  when casing changes the length. Text fields ignore it and draw what is typed, so their caret
  and selection are unchanged. `TextStyle` is unchanged.
- **Exiting elements are marked, and gone from lookups.** `ElementInfo` has a new `exiting`
  field, true for an element playing its exit transition and for everything inside it. `--tree`
  (and the hot host's tree) still lists them, now with `"exiting": true`; elements at rest add no
  key, so existing tree goldens don't change. The dev panel's `tree` reply leaves them out, so a
  check that something left passes as soon as it starts leaving; send
  `{"type":"tree","exiting":true}` to keep them, marked. `Runtime::find`, which `--script` key
  positions and the hot host's `tap` use, already skipped an exiting element and now also skips
  its children; a script that tapped a child of a leaving element by key now stops with "no
  element with key". `Runtime::presented` still reads exiting elements.
- **Dev-panel transitions can capture data.** `dev::Config::transition` now takes any
  `impl Fn(&mut S, &mut Tx) + 'static`, so a transition can close over a scene's fixture:
  `.transition("Open", "list", move |s, tx| s.open(&room, tx))`. `dev::Transition::with` builds
  one for `Config::transitions`. `Transition::new` still takes a plain `fn`, so
  `let t = Transition::<Ui>::new;` lists keep building, but it is no longer `const`, `Transition`
  is `Clone` rather than `Copy`, and its `action` field is private.
- **Held keys activate once, as in AppKit.** Auto-repeats no longer press a focused button,
  menu item, row or link (Return, Space), open a popup button (arrows), dismiss another modal
  (Escape), blur a field (Escape) or submit it (Return). Rune consumes those repeats. After a held
  Return, Escape or Space moves focus, its later repeats reach nothing: not the app's `on_key`,
  the control that took focus or its physical input handler. Other held keys keep repeating. A
  rune-kit `Combobox` chooses once on a held Return and closes once on a held Escape. On X11, a
  held global shortcut fires once, as on macOS and Windows. Typing, deletion, caret movement,
  arrows in lists and menus, and Tab still repeat. To act on every repeat, handle the key in `on_key` and
  check `tx.key_repeat()`.
- **`--script` events at t = 0 see the first layout.** A script whose first events are at
  t = 0 (or omit `t`) now lays out a first frame, with the initial state and queued updates,
  before running them. `{"t": 0, "type": "click", "key": "..."}` used to fail with "no element
  with key", and t = 0 clicks or moves at x/y hit nothing. Frame 0 then draws the events'
  effects, so changes they start animate from the mounted state rather than mounting settled.
  Scripts without t = 0 events are unchanged.
- **`ConfiguredOverlay::setup(fn(&mut T, Waker))`**, like `AppSpec::setup`, so an overlay can
  build its own `ImageCache` or channel instead of borrowing the main window's waker. It runs
  once on the event thread when the loop starts, before the overlay's first frame and before
  updates queued on its handle. A wake re-runs the overlay's view: a shown overlay draws a
  frame, which polls, and a hidden one runs a frame without drawing. Headless runs don't
  create overlays, so they don't call it.
- **`Cursor::Crosshair` and `Cursor::Hidden`.** A crosshair for picking a point or dragging out
  a rect, and no OS cursor for views that draw their own. Windows set them, and overlays now
  set an element's `Cursor` too (before, they left the OS cursor alone) while the pointer is
  over a hit area of a shown overlay that takes input, including a non-key macOS overlay.
  `Hidden` is a transparent cursor image, so it is per-window like any shape and never confines
  the pointer. It shows the arrow again when hover leaves the element, the pointer leaves the
  window (including a drag released outside it), the element is removed, the window loses focus
  or hides, or the overlay hides. The cursor now comes from what's under the pointer in the
  current frame, so an element playing its exit transition has no cursor and a sibling that
  replaces it under a still pointer sets its own. On macOS a non-key overlay sets its cursor
  only while its app is frontmost; the opt-in `macos-background-cursor` feature lifts that with
  a private WindowServer property (an App Store review risk), and without it none of that code
  is in the binary. `Cursor` is now `#[non_exhaustive]`: an app that matches it needs a
  wildcard arm.
- **Key handlers can tell a held key's repeats from its first press.** `tx.key_repeat()` is true
  in the window's `on_key`, an overlay's `.on_key`, `on_input_key` and `on_physical_input` while
  the OS auto-repeats a held key, and false for the first press. `KeyEvent` is unchanged, so
  existing literals keep building. Headless drivers queue a repeat with `Runtime::repeat_key`,
  and `--script` keys and `physical` chords take `"repeat": true`. Built-in behaviour is
  unchanged: held keys still type, move the caret, Tab through focus and press a focused button.
- **Unselected segments hover.** A `SegmentedControl` with `on_pick` now lights an unselected
  segment under the pointer: the `hover` overlay fills it and its label turns `text_base`, on the
  first frame in, fading out over the runtime's default 0.1 s. The selected segment and controls
  without `on_pick` are unchanged. A forced `State::Hover` on an unselected segment now also
  fills it, so snapshots match.
- **Native traces record when frames reach the screen.** On macOS, `--native --trace` frames
  now add `target_ms` (the display link's target) and `present_ms` (Metal's `presentedTime`
  for the frame's drawable), plus `present_dt_ms`, the gap from the previous presented frame
  when the two were drawn back to back. A skipped drawable writes `present_ms: null`. Each line
  is written once its present time arrives, so frames land in the file two or three frames
  late, still in order. When the trace ends it prints a summary to stderr: present-interval
  p50/p95/max, intervals under 2 ms (bunched) and over 1.5 refresh periods (missed), and the
  target-to-present lag. Every other field is unchanged, and readers that ignore unknown keys
  read the new traces as before. Overlays, other platforms and headless traces add no present
  fields, and nothing changes without `--trace`.
- **A late frame no longer slows motion down.** Windows, overlays and the hot and dev hosts now
  share one frame-step rule (`frame_clock`). Motion already running advances by the real time
  since the last frame, up to 250 ms, instead of 1/30 s at most. Before, a host marked idle
  mid-motion stepped only 1/120 s, so after a hitch springs fell behind and finished late. Motion
  a frame starts or retargets starts at that frame's time and steps at most 1/120 s, so it no
  longer jumps by a late frame's gap, and a retargeted spring turns from where it is at that
  time. Both happen in one runtime pass, so poll, the motion trace and the settle flag see one
  frame. While other motion runs, a 60 Hz frame now starts new motion with a 1/120 s step rather
  than 1/60 s. The hot host's first frame after idle no longer steps new motion by the idle gap.
  Scroll flings now integrate their decay exactly, so a fling travels about 1% less in total and
  a long frame carries the same distance as the short frames it stands for.
- **Focus follows an app selection.** In a `focus_follows` group, when the app selects a
  different item (a ⌘-digit shortcut, say) while focus is in the group, focus moves to the
  `focus_selected` item and keeps its keyboard ring, instead of staying on the old row.
- **List arrows stop at the ends.** Up on the first row and Down on the last row of a
  `list`, `tree` or `grid` focus group now stay put, like NSTableView, instead of wrapping.
  Menus, select popups (`listbox`) and segmented or radio groups still wrap.
- **ShortcutRecorder can retry an unchanged chord.** `ShortcutRecorderState::mark_unregistered()`
  says the shown shortcut isn't registered (its launch registration failed, say). Recording
  the same chord then goes through `on_change` again instead of being a no-op, until a change
  succeeds. Without the call, behavior is unchanged.
- **Global shortcuts need a modifier.** `App::global_shortcut`, `register_shortcut` and
  ShortcutRecorder refuse a chord without ⌘, ⌃ or ⌥ (Ctrl, Alt or Super elsewhere), such as
  Space, Enter or ⇧A, which would take the key from typing in every app. F1–F24 still work
  alone. Registration returns `InvalidAccelerator("space needs ⌘, ⌃ or ⌥")`, and the recorder
  shows the refused chord with "Add ⌘, ⌃ or ⌥." without calling `on_change`. Opt out with
  `App::allow_unmodified_shortcuts()` and `ShortcutRecorder::allow_unmodified()`.
  `Shortcut::require_modifier()` checks a chord. Apps that bound a bare key now get an error.
- **A window's first updates can register on a later window.** `run_windows` now builds every
  window's host before any of them opens and drains its queued updates, so an update queued on
  the first window that calls `register_shortcut` or `set_menu` on the second gets `Ok` instead
  of `NotRunning`. Each window's state initializer and startup shortcuts now run before the
  first window opens; windows still open in order.
- **Incremental release builds; benchmarks move to `--profile perf`**. This repo's
  `[profile.release]` is now incremental, so a one-line edit in `rune` rebuilds an example in
  about 1.5 s instead of about 23 s. The new `perf` profile keeps the old release codegen
  (opt-level 3, 16 codegen units, no LTO, not incremental), and `cargo bench` stays
  non-incremental. The runtime benches (`bench-ui`, `bench-startup`, `bench-live`,
  `bench-gpu-springs`, `text-bench`, `native-pacing`) time only `target/perf/` binaries and exit
  with an error on anything else. Tests in the docs now run in debug. Apps that depend on Rune
  use their own profiles and are unaffected.
- **`El::border_gradient(width, top, bottom)`**. A border whose colour runs from `top` at the
  element's top edge to `bottom` at its bottom edge, such as a top-lit hairline rim on a capsule.
  It has `border`'s corners, coverage and antialiasing, and like `border` it doesn't affect layout.
  Whichever of the two is called last wins, and both stops spring like `border`'s colour. Kind 4
  instances (`KIND_GRADIENT`) with a nonzero `params.x` now draw a filled rect with a gradient
  ring (`border` at the top, `uv` at the bottom) and honour `params.z` softness. Rune only emits
  them for this border, but hand-built kind 4 instances that left a border width in `params.x`
  now draw a ring. `rune-ref` matches and its canonical hashes change.
- **macOS Quit runs `on_close`.** The app menu's Quit, Cmd-Q, the Dock's Quit and logout now
  run `on_close` once, and flush clipboard clears, before the process ends, as the close button
  does. A window already hidden under `keep_running` has closed, so its hook doesn't run again.
  `AppHandle::quit` and `Tx::quit` still skip it.
- **Headless `{"physical": "alt+super+space"}` releases its modifiers.** After the key's down
  and up, the script now sends the modifiers coming up, as a keyboard does, so a recorder no
  longer shows ⌥⌘ held in later stills. Scripts whose stills showed held modifiers change.
- **ShortcutRecorder shows the refused chord and lets the app word the error.** After
  `on_change` refuses a chord, the keycaps keep showing it (not just the held modifiers) until
  another modifier starts the next chord, which also clears the error. The message under it
  defaults to sentence case naming the chord ("⌃J is already in use.") instead of the
  `ShortcutError` Display text; `.error_message(|error, chord| ...)` replaces it, with
  `rune_kit::shortcut_error_message` as the fallback, and `ShortcutRecorderState::refused()`
  reads the chord. Recorder text changes; no goldens show it.
- **`AppHandle::shortcut_owner(&Shortcut)`** names who holds a chord after a
  `ShortcutError::Conflict`, when Rune or the desktop knows: this App or another Rune App in
  the process (its title), or on macOS an enabled system shortcut ("Spotlight", "Mission
  Control", or "macOS" for unnamed ones), named at the user's remapped chord. None otherwise,
  and headless runs never ask the desktop. `Conflict` is still a unit variant. Registering a
  chord another Rune App in the process holds now returns `Conflict` on every backend, the
  portal included. No output change.
- **Keyboard focus scrolls into view.** Tab, Shift-Tab, arrows, Home, End and type-select
  reveal the control they reach inside its scroll view, as `Tx::scroll_into_view` does, so
  focus never moves off-screen.
- **Overlay drags report screen travel.** When an overlay moves its own window (`place`,
  `set_placement`, `set_align`) during a drag, `Drag::dx`/`dy` keep measuring pointer travel
  instead of travel relative to the moving window, so a dragged capsule follows the pointer
  without feeding back. New `El::on_hover_change` reports entering and leaving an element's
  rect, children included, without taking their presses or hover; a held press stays inside.
- **Mac keyboard navigation for menus, selects and lists**. Groups with role `menu`, `listbox`,
  `list` or `tree` move with Up and Down only (Left and Right no longer move them), and those
  plus `tablist` jump to the item whose label you type. `El::focus_highlight(color)` gives a
  row one highlight for pointer and keys, as in `NSMenu`: hovering focuses the row (never
  taking focus from a text field) and the first arrow reveals the current or checked item.
  `El::focus_follows()` makes a group press each item it moves to (a source list). A
  non-input `combobox` opens on Up and Down. rune-kit `Menu` and `Select` items use the shared
  highlight, and with `on_close` an open menu or select traps focus, closes on Escape and
  returns focus to its trigger. Text fields take macOS's Control bindings (Ctrl-A, E, B, F, P,
  N, D, H, K). On macOS, with Keyboard navigation off in System Settings, Tab visits only text
  fields and lists, as in AppKit; `Runtime::set_full_keyboard_access` overrides it.
- **Hover lands at once.** A `.hover(color)` overlay (and a hover group's `group_opacity`) used
  to spring in, so a highlight trailed the pointer. Without `.hover_motion`, hover now appears on
  the first frame under the pointer and fades out over 0.1 s, like a native sidebar or menu.
  `.hover_motion(enter, exit)` still sets both curves.
- **App icons on Windows and Linux; aspect-fit images**. `ImageCache::app_icon` now rasterizes
  an executable's or `.ico`'s icon on Windows and a `.desktop` entry's hicolor or `pixmaps`
  icon (PNG or SVG) on Linux, where it used to resolve to an error, so apps that fell back on
  error now draw the icon. `El::pixels_fit(&pixels, SurfaceFit::Fit | Fill)` keeps an image's
  aspect ratio; `El::pixels` still stretches, unchanged.
- **Indeterminate `Progress` animates.** `Progress::new(None)` used to draw an empty track; it
  now sweeps a quarter-width segment across it every 1.4 s, centred and still under Reduce
  Motion. The sweep is `LoopSpec::slide(period, from, to)`, a new looping offset in fractions of
  the node's own size that descendants and hit tests follow. Like other loops it draws no frames
  while hidden, scrolled away or reduced, including when the segment is outside its clip.
- **`--script` taps links by id**. A script event's `"key"` (and
  `Runtime::find`) falls back to the link whose id matches when no element has that key, so
  `{"type":"tap","key":"https://x.dev"}` presses an `El::link` or Markdown link, aiming at its
  first line box. Before, the run stopped with "no element with key".
- **Late children of virtual rows play their entrance**. A child added
  to a row of a `.virtualize(..)` stack after the row first appeared (a "Copied" chip, say) now
  fades in with its transition, as it would in a plain stack, instead of popping in. Rows
  scrolled back into view still mount settled with their whole subtree.
- **Timers drive motion in windowless dev runs**. With the dev panel
  attached and no visible window, a timer that starts a transition now animates it in real time
  on hidden ticks, as a shown window would. Before, the timer's frame left the motion frozen
  until the next input or panel command.
- **Stuck scrolls hold their bottom across fitting and overflowing**.
  When a stick-to-bottom scroll's content goes between overflowing its viewport and fitting it
  while the viewport resizes, the rows no longer jump by the size change and slide back.
  Bottom-aligned rows (`justify_end` in a filling stack) stay on the bottom edge every frame;
  top-aligned rows glide between the bottom and the top. A row added to a bottom-aligned stack
  that fits now pushes the rows above it up as it grows, where they used to jump up first.
  Settled frames and `--snap` goldens are unchanged.
- **Linux portal shortcuts use zbus 6.** zbus 5's zvariant, zbus_names and zvariant_derive are
  flagged unmaintained (RUSTSEC-2026-0370, -0371, -0372) and no longer appear in Rune's default
  dependency graph. Linux builds need Rust 1.89 or newer. The opt-in `native-file-dialog` feature
  still reaches zbus 5 through `rfd`'s `ashpd`, which has no zbus 6 release yet.
- **Steady 60 Hz in Low Power Mode on ProMotion Macs.** Low Power Mode caps the built-in panel
  at 60 Hz while macOS still reports 120 Hz, so Rune drew a few fast frames after each change
  before settling. Windows and overlays on the built-in display now pace at 60 Hz in Low Power
  Mode, with no fast start and about half the input-to-screen latency. `--trace` headers report
  60 Hz there.
- **`LayerSpec` literals need filter fields**. The public `blur` and
  `clip_radius` fields break external `LayerSpec { opacity, blend }` literals. Add
  `..Default::default()` for their previous unfiltered, rectangular behavior.
- **Rounded subtree clips and dormant dissolves**. `clip_rounded(radius)`
  clips ordinary paint directly and isolated output after filtering, preserving blur from sources
  beyond the clip. `dissolve()` skips isolation at opacity endpoints, where overlapping opaque
  children now use source-over; it activates additive isolation during fades, including sequences.
- **Blur filters the whole subtree**. `blur`, `live_blur`, `blur_motion`
  and presence blur now soften text, images and video together. `Transition::blur_replace()` peaks
  at Gaussian sigma 1.5 pt instead of the old shape-only 4 pt coverage radius; its scale and timing
  stay the same. The packed instance remains 128 bytes, with clip radius in attribute 7 `fade[3]`.
- **Opt-in guarded Rust view editing**. `scripts/rune-dev` patches
  Settings and fixture-only OpenCode views while retaining state, scroll and springs.
  Declaration, signature and other build-input changes require a restart. `rune/hot`
  is off by default; measured edit-to-pixels latency remains above the one-second target.
- **`SecretCopy::pending()`** makes a copy that is never written and stays `Pending`, so a
  scene can show a copy in flight without building a `Tx`. No output change.
- **`NativeSurface::set_mirrored(bool)`** flips video horizontally at draw time, for
  front-camera previews. It is off by default, so existing output doesn't change.
- **Exiting password fields drop their secret.** When a secure text input starts its exit
  animation, its copy of the text is wiped and replaced by its mask, so it paints the same dots
  (no placeholder, no caret) through the exit without holding the password.
- **`Tx::copy_secret` returns a `SecretCopy` handle.** `state()` / `holds()` tell whether the
  copy is still the last thing Rune wrote and the clipboard hasn't changed (the worker watches
  the change count while a secret is held), whether a write failed, and whether it was cleared or
  replaced; `clear_now()` clears it early. `Tx::clear_secret_clipboard()` clears the latest
  secret from any window. Tests can share `rune::clipboard::memory()` between runtimes with
  `Runtime::use_clipboard`. The worker now keeps a keyed fingerprint of the secret, not its text.
- **Command chords don't type, and single-line fields stay on one line.** Text typed with Cmd
  (macOS) or Ctrl held, such as Shift-Cmd-Space, no longer inserts into any field; it goes on to
  the app's `on_key` instead. Ctrl+Alt (AltGr) still types on Windows and Linux, and Option still
  types on macOS. Secure and `single_line()` fields drop control characters from typing, paste and
  IME commits: pasted line breaks and tabs are stripped, like `<input type=text>`, and
  Shift/Option-Enter submits instead of inserting a newline. Multi-line fields are unchanged.
- **RollingText sets like one text.** Each rolling glyph sits at its advance in the whole label
  shaped once, with kerning and contextual forms, where each used to round up to a device pixel
  on its own. Rolled labels get narrower and tighter (IBM Plex Mono 11 pt "Copy password": 91 →
  86 pt) and at rest draw the same glyphs at the same positions as the plain string. New
  `El::advance_in(run, at)` measures and draws a text as its grapheme in a longer run.
- **Stuck scrolls hold their bottom while they resize**. A stick-to-bottom
  scroll whose own height animates (a banner opening above it, a footer below) now keeps its last
  row on its bottom edge every frame. Before, the rows jumped by the size change and slid back.
  Settled frames and `--snap` goldens are unchanged.
- **Editing scrolls the caret into view**. Typing, pasting, IME composition
  or caret keys in a field inside a scroll viewport now scroll its nearest viewport just far enough
  to show the caret line, as a browser textarea does. Before, the caret could run off the bottom.
- **Growth rows play an explicit `.transition`**. Rows inserted into a
  growth stack (and elements inserted inside them) used to ignore `.transition(...)` and only grow
  open. One that sets a transition now also plays its entrance; rows without one are unchanged,
  and rows a virtual list remounts on scroll still appear settled.
- **Absolute children size against their parent**. `absolute`/`pin` children
  now resolve `w_full`, `w_frac`, `h_full` and right/bottom pins against their parent's padding
  box, as documented, instead of the window (Taffy hands them to the nearest positioned ancestor,
  and elements were static). Left/top offsets were already parent-relative. Root overlays (scrims,
  dialogs, toasts, anchored popovers) are unchanged; nested ones move, e.g. rune-kit `List::divider`
  now spans its row.
- **Accessory apps get an app menu when showing their Dock icon.** On macOS,
  `set_dock_icon(true)` installs About, Services, Hide and Quit when no main menu exists,
  preserving app-supplied menus and composing with Rune's custom menu items.
- **Headless runs resolve `place()`.** A `--script` object can declare `"displays"` and a
  `"pointer"`; `AppHandle::place` resolves against them (one 1920x1080-pixel display by
  default), prints a `rune-placed` line and adds `"placed"` to `--tree`. A script object
  without `"events"` now means no events instead of an error.
- **Launcher panels never reveal stale pixels.** A launcher shown in the same turn as an
  `update()` renders one fenced hidden frame before its reveal, where it used to show the
  cached pre-update frame for a frame. The update's motion is not settled; it runs on after
  the reveal. Reveals with unchanged state keep the cached fast path.
- **Launcher panels run `poll` on a plain wake.** `AppHandle::wake()` and the setup `Waker`
  used to skip a launcher's `poll` unless an update came with them. A shown launcher now
  redraws; a hidden one polls and renders its cached frame at alpha zero when `poll` reports
  a change, with no frame loop.
- **Linux builds link `libxkbcommon`** (`a7f0f56`). Display-targeted overlays added the `xkbcommon`
  crate, so CI and build hosts without `libxkbcommon-dev` (Fedora: `libxkbcommon-devel`) fail at
  link time. See "Linux system packages" in `crates/rune/README.md`.
- **`.truncate_fade()` fades 1.5 em instead of 1 em**.
  Overflowing labels fade over a wider edge; labels that fit don't change. New
  `.truncate_fade_width(em)` sets another width.
- **Emoji with `U+FE0F` draw in colour.** `❤️`, `☀️`, `✔️` and other characters a text font also
  has use the colour emoji font when the emoji selector follows them; without it they keep their
  text form, as before.
- **The caret and backspace step over whole graphemes.** Arrows, Shift-arrows and Backspace treat
  a ZWJ family, a flag or a skin-toned hand as one character instead of splitting its code points.
- **Inline code chips draw as one rect.** Touching span backgrounds on a line merge even when
  their runs have different fonts or sizes, at the height of the widest run. Markdown's padding
  spaces no longer draw lighter slivers at each end of a code chip.
- **Italics.** `TextStyle::italic()` and `Span::italic` pick the family's italic face (SF's on
  macOS) or lean the upright face 12° when it has none (bundled Inter). rune-kit `Markdown` draws
  `*emphasis*` in italic, so its emphasis runs change pixels; advances don't move.
- **Math alphanumerics and non-Latin scripts render instead of tofu.** Math alphanumerics and
  symbol blocks, plus Hebrew, Arabic, Devanagari, Bengali and Thai, load system fonts the first
  time a text uses them. Text that used these characters changes pixels and advances; Latin-only
  text and the existing goldens don't move. New `El::justify()` spreads wrapped lines to the
  width, from the right in right-to-left text; `Align` behaves as before.
- **Colour emoji** (#7). Emoji fall back to the system's colour emoji font (Apple Color Emoji,
  Segoe UI Emoji, Noto Color Emoji) and draw as colour images from the image atlas, where they
  used to draw as blank gaps. Text without emoji renders as before.

- **Prewarmed overlay reveals preserve elapsed motion.** A hidden commit keeps its animation
  timing when `show()` arrives in the same poll or a later turn: a recent transition reveals
  in flight, an old one at rest. Content, geometry and live values refresh before reveal;
  hidden motion does not drive a frame loop. Initial preparation and ordinary overlay shows
  still settle their first frame.
- **rune-kit menus open instantly**. `Menu`,
  `context_menu`, submenus and `Select`'s list now appear fully on the next frame instead of fading
  and scaling in from 0.96, and close with a plain `.25 / 0` fade (no scale). Menu item highlights
  land the frame the pointer arrives and leave over 100 ms ease-out. No `--snap` goldens move:
  they capture frame 0, where overlays already mount without an entrance.
- **Wrapped centred and end-aligned text aligns each line** (#6). A wrapping `Align::Center`/`End`
  text now centres (or right-aligns) every line inside its wrap width, instead of moving the whole
  block as one left-aligned paragraph. `--snap` goldens with multi-line centred text move.
- **Single-line text fields** (#6). `El::single_line()` keeps a text input on one line: it scrolls
  sideways to keep the caret visible, clips to the field, and rests at the start when unfocused.
  rune-kit `TextInput` (except `multiline`) and `List` search opt in; core `text_input` is unchanged.
- **Scroll views clip their content** (`3333f37`). `El::scroll` now clips its children to its box with a
  hard edge, so overflowing content no longer draws, or takes presses, outside it. An explicit
  `clip_soft`/`clip_fade` still sets the edge. No `--snap` goldens move.
- **Optical sizing on by default** (`2eae09d`, from `edc9892`). Text with an `opsz` axis now uses each
  run's point size, as AppKit does: system SF at 13 pt renders as SF Text, so labels are slightly
  wider. Layout is otherwise unchanged, but `--snap` goldens with SF text move (Scrawl: all 7, PAE
  ~0.85). Opt out per style with `.optical_sizing(OpticalSize::FontDefault)`. Fonts without `opsz`
  are unaffected.
- **Presses inside scrolled views land** (`b3c7bdb`, #5). A captured press is released against
  where the element was drawn (after scrolling), not its unscrolled layout rect, so `on_press` now
  fires for rows scrolled into view.

# Changelog

Rune has no releases yet; consumers pin `dev` revs. This lists changes that can move an app's output
without a code change (goldens, layout, input), newest first, with the rev that introduced each.

## dev

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
- **Stuck scrolls hold their bottom while they resize** (`rooms-gaps` branch). A stick-to-bottom
  scroll whose own height animates (a banner opening above it, a footer below) now keeps its last
  row on its bottom edge every frame. Before, the rows jumped by the size change and slid back.
  Settled frames and `--snap` goldens are unchanged.
- **Editing scrolls the caret into view** (`rooms-gaps` branch). Typing, pasting, IME composition
  or caret keys in a field inside a scroll viewport now scroll its nearest viewport just far enough
  to show the caret line, as a browser textarea does. Before, the caret could run off the bottom.
- **Growth rows play an explicit `.transition`** (`rooms-gaps` branch). Rows inserted into a
  growth stack (and elements inserted inside them) used to ignore `.transition(...)` and only grow
  open. One that sets a transition now also plays its entrance; rows without one are unchanged,
  and rows a virtual list remounts on scroll still appear settled.
- **Absolute children size against their parent** (`rooms-gaps` branch). `absolute`/`pin` children
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
- **`.truncate_fade()` fades 1.5 em instead of 1 em** (Abattoir: 1.5 em on overflowing text).
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
- **rune-kit menus open instantly** (Abattoir "Open menus instantly, fade only the close"). `Menu`,
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

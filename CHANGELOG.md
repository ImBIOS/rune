# Changelog

Rune has no releases yet; consumers pin `dev` revs. This lists changes that can move an app's output
without a code change (goldens, layout, input), newest first, with the rev that introduced each.

## dev

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

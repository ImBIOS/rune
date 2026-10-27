- **`Tx::scroll_key_to` and `El::sticky_top_showing` land a new page right on its first frame.**
  `scroll_key_to(key, at)` scrolls instantly so element `key`'s top sits `at` points below the
  viewport's top, computed from the layout of the commit the handler causes, so it applies
  before that layout draws (no marker, no correction a frame later). `sticky_top_showing(show)`
  is `sticky_top` held with only the element's bottom `show` points in view, measured from its
  height as laid out in the frame that draws it, so a header whose height changes (or a new
  page's taller one) sticks in the right place at once instead of a frame late.

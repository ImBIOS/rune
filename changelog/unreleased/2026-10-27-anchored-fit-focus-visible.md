- **Dropdowns fit the window, and a click shows no focus ring.** `El::anchored_fit` places an
  overlay like `anchored` and, when it fits on neither side of its anchor, puts it on the side
  with more room and caps its height (or width) to that room, so a scroll viewport inside
  (`.shrink().min_h(0.0)`) scrolls instead of running off the window. rune-kit's `Popover`,
  `Menu`, `Select` and `PopupList` use it above or below their trigger, and their rows scroll
  with faded edges; a `Popover` body now shrinks. Focus shows only after keyboard navigation,
  as CSS `:focus-visible`: Tab, the arrows, Home, End, Page Up and Page Down (and type-select
  that moves focus) show it, while Space, Escape or typing after a click no longer do.

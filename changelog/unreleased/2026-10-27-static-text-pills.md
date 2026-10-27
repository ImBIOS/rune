- **Pills in static text.** `El::pill(range, PillStyle)` and `El::pills(..)` draw byte ranges of a
  `text` or `rich` element as the rounded chips a field draws for `Mark::Pill`: chip colour,
  label colour and icon from the `PillStyle`, a label that never splits across lines, and chip
  padding that takes room in the line. Selection and copy treat the label as text, and `El::spans`
  called afterwards keeps the label colours. On a field it does nothing (use `El::pill_style`).

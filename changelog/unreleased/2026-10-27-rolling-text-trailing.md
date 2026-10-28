- **A right-aligned `RollingText` rolls new digits in their columns.** `RollingText::trailing()`
  says the label widens to the left, so a glyph that rolls in starts where it will rest instead of
  riding in from the right with the label's slide ("9s" → "10s" no longer rolls "10" over the
  "s"). Glyphs now also slide on the label's width-leading X spring, so the ones that stay hold
  still. Labels without `trailing` roll as before.

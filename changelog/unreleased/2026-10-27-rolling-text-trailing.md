- **Every `RollingText`'s glyphs now slide on the row's width-leading X spring**, not the plain
  label spring, so glyphs that stay hold still against the row's edge while the width changes. This
  moves the glyphs of every rolling label. **A right-aligned `RollingText` rolls new digits in their
  columns:** `RollingText::trailing()` says the label widens to the left, so a glyph that rolls in
  starts where it will rest instead of riding in from the right with the label's slide ("9s" →
  "10s" no longer rolls "10" over the "s"). A trailing label's row and glyphs slide on the plain
  label spring (no width lead), so a container that right-aligns it should slide on that spring
  too.

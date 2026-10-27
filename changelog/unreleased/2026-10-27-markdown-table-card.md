- **Markdown tables on a card.** `MarkdownStyle::table_body(fill, radius, shadows)` puts the
  body rows on a card of their own, rounded and raised, with rules between body rows only and
  none under the header, which sits on the frame. `table_frame(fill, ring, pad)` draws a
  filled, rounded frame with an inset ring in place of the outline, `table_header_pad(x, y)`
  pads header cells apart from body cells, `table_full_width(true)` stretches a table to the
  text's width (columns widen in proportion to their content), and `table_margin(top, bottom)`
  sets the space around a table. `table_card(&kit)` sets all of these from the kit's tokens:
  a raised `bg_base` card 2 points inside a `layer_01` frame, a faint medium header and 13-point
  cells. Defaults are unchanged. `MarkdownStyle` now lives in `markdown/style.rs`; its path is
  the same.

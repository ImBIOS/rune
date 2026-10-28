- **Inline code chips get real padding and a gap from the text around them.** Markdown pads
  the chip `MarkdownStyle::inline_code_pad` points around the code (3.5 by default) instead of
  painting one narrow space each side, and sets two unpainted narrow spaces each side, so what's
  left is a small margin. Code in a link is a chip in the link's colour without an underline.
  Span backgrounds snap to whole device pixels, sit balanced on the cap height rather than a
  mono face's tall line box, and merge across a link's underline instead of leaving a seam at
  the chip's edge. `MarkdownStyle::list(indent, gap)` sets the marker column and the gap between
  items (32 and 4, as before), and `code_block_padding(x, y)` the room inside code blocks and
  diffs (12 by 12).

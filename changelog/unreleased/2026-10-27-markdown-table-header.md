- **Markdown tables: the header's rule, fill and weight are set on their own.**
  `MarkdownStyle::table_header_rule(c)` draws the hairline under the header in its own colour
  (a web `th` with a stronger `border-bottom` than the `td`s), `table_header_fill(c)` sets the
  header fill without the border, and `table_header_weight(w)` gives the header a medium weight
  beside bold `strong` text. Defaults are unchanged.

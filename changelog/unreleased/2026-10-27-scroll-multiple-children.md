- **Scroll viewports measure all direct children.** `El::scroll`, `El::scroll_x` and
  `El::scroll_xy` now measure their content extent across every present direct child instead of
  only the first, so a scroll container holding several rows or tiles directly scrolls to its
  end, follows `stick_bottom`, and reveals later children with `Tx::scroll_into_view` without
  needing a wrapper column or row. Growth-stack tracking (`El::growth`) finds the growth-stack
  child among direct children and warns once in debug builds if `.growth(..)` is placed on the
  scroll viewport itself or followed by trailing sibling elements.

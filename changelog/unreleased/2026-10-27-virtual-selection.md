- **Text selection across virtual rows.** `El::selectable` and `El::selection_group` now keep
  working across unbuilt and scrolled-out rows of an `El::virtual_rows` or `El::virtualize` list:
  the virtual list pins the rows holding the selection's anchor and focus (storing endpoints by
  row identity, relative path and byte offset so they survive rows mounting and unmounting around
  them), Cmd+A and `Tx::select_all_text(key)` select from the first row to the last row of the
  virtual list, `Tx::select_text(key, range)` pins and builds an unbuilt target row without
  scrolling, and Cmd+C / `Runtime::selected_text()` / `Tx::text_selection()` build any unmounted
  rows between the selection's ends on demand (view only, capped at 10,000 unbuilt rows and 4 MiB
  of UTF-8 text) using the same `\t` and `\n` separators as mounted rows.

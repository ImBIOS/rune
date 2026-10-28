- **Children can leave the way the commit that removes them says.** `El::child_exit(t)` gives
  children removed from an element in a commit the transition `t` in place of their last render's.
  `RollingText` uses it so old digits leave the way new ones enter, including on a first count
  down or a value that turns back mid-roll. A trailing `RollingText` now holds leaving digits in
  their columns too, matches from the right like an odometer ("19s" → "100s" keeps only the
  "s"), and keeps its staggered roll spring when its width changes. `rune::text::advance` gives a
  line's unrounded advance.

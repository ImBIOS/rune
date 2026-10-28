- **`Combobox` reads like a command menu.** Its search field sits borderless under a search
  icon, over a divider, with no focus outline (the caret shows focus), and each option shows its
  `detail` in muted text after the label. The result list fits the window like the other
  dropdowns, never shorter than one row, and scrolls with faded edges. New builders:
  `trigger(El)` replaces the default field trigger, `placement`, and `max_height` (192 by
  default). `Popover::padding` sets the body's inset (12 by default; 0 for edge-to-edge bodies).

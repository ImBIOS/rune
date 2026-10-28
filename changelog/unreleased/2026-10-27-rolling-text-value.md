- **A `RollingText` can roll the way its number moves.** `RollingText::value(v)` is SwiftUI's
  `.numericText(value:)`: while the value rises, new glyphs enter from `from` (digits move up, the
  default); when it falls, they enter from the opposite edge. Text that changes while the value
  holds, or with a value that isn't finite, rolls from `from`. Labels without a value are unchanged.

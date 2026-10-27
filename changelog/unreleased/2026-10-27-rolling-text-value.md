- **A `RollingText` can roll the way its number moves.** `RollingText::value(v)` is SwiftUI's
  `.numericText(value:)`: while the value rises, new glyphs enter from `from` (digits move up, the
  default); when it falls, they enter from the opposite edge. Labels without a value are unchanged.

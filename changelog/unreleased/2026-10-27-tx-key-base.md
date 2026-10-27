- **`Tx::key_base()`: the character a key types with no modifiers.** In a key handler it gives
  the key's character under the current keyboard layout with no modifiers held, from winit's
  `key_without_modifiers` on macOS, Windows and Linux: `Some('a')` for ⌥A, which arrives as
  `Key::Text("å")`, and for AZERTY's A key, where `Tx::physical_key` says `KeyQ`. Bindings by
  letter that follow the layout (as `DefaultKeyBinding.dict` binds ⌥-letters) match on it. A
  script key with a `"code"` takes a `"base"`, else its one-character name pressed without
  Shift or Option.

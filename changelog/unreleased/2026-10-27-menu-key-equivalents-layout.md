- **Menus show the accelerator's own key on every keyboard layout.** A `MenuItem` or
  `PopupItem` key equivalent is now the character its accelerator names: `cmd+b` is ⌘B on
  Dvorak too, and fires from the key that types "b", where it used to show and take ⌘X (the
  key in QWERTY's B position) away from Cut. `cmd+shift+=` is "=" with ⇧⌘; arrows, Return,
  Delete, Escape, Tab, Space and F-keys use AppKit's characters. The app, named, Dock and
  context menus all follow it, and a custom item with a standard item's key (⌘X) now takes it
  on any layout as it does on QWERTY. New `Shortcut::menu_keycaps()` gives the same label for
  menus and hints; the playground bar and rune-kit's button shortcut tooltips use it.
  `Shortcut::keycaps()` still names the key at the shortcut's position in the current layout,
  for global shortcuts and the shortcut recorder.

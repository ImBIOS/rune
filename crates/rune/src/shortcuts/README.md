# Global shortcuts

```rust,ignore
let app = App::new(spec)
    .global_shortcut("cmd+shift+space", |state, tx| {
        state.quick_panel_open = true;
        if let Some(token) = tx.activation_token() {
            state.app_handle.activate(token);
        } else {
            state.app_handle.open();
        }
    })
    .windowless();
app.run();
```

The handler receives the same state as its `App`/`WindowSpec`, on the app thread.
It runs through `Runtime::update`, including animation and shell requests. Native
activation wakes the loop even while no window is visible.

Static builder failures print once and can be drained with
`handle.take_shortcut_errors()`. The builder defers registration until the app's
host exists, so `run_windows_with` factories can construct apps normally.

Runtime registration returns the actual result:

```rust,ignore
let id = handle.register_shortcut("ctrl+alt+k".parse()?, |state, tx| {
    state.open_quick_panel(tx.activation_token());
})?;
handle.unregister_shortcut(id)?;
```

Call runtime registration after the host starts (an `on_window` or queued
`handle.update` callback works). Calls before startup or after shutdown return
`NotRunning`. On macOS/Windows other threads may call it, but wait for the app
thread, so the app thread must not join those callers. Linux registration can
run off-thread. Dropping the host unregisters everything,
even if other threads retain `AppHandle`s.

## Desktop behavior

- macOS: Carbon `RegisterEventHotKey`, no Accessibility permission. Command is
  the Super modifier. Hardware key positions follow the US layout; F21–F24 have
  no Carbon key code and return `InvalidAccelerator`.
- Windows: `RegisterHotKey` on a message-only window owned by the event thread.
  Repeat is disabled. Windows-reserved combinations may fail registration.
- X11: checked passive grabs on every root, including CapsLock/NumLock variants.
  A failed variant rolls back the registration. Keys use the current keymap's
  base keysyms; restart/re-register after changing the keymap.
- Wayland: xdg-desktop-portal GlobalShortcuts. No portal returns `Unsupported`.
  The portal may ask for permission or let the user choose another trigger;
  `preferred_trigger` is a preference, not an exclusive-grab guarantee. Cancellation
  is `PermissionDenied`, not `Conflict`. Each shortcut owns a portal session so
  unregister also works with version 1. The activation token is available only
  during its handler, through `Tx::activation_token()`.

Registration is synchronous, including portal consent, with a two-minute bound.
Register startup shortcuts before showing windows; use a worker thread for
runtime Wayland binding so a consent dialog does not block the event thread.

Only an actual registration creates native resources: macOS/Windows need no
listener thread; X11/Wayland each create a listener that is joined on teardown.
No-shortcut apps never open a shortcut display or portal connection.

## Deterministic testing

Headless scripts register virtually and invoke the same handlers:

```json
[{"t": 0, "shortcut": "cmd+shift+space"}]
```

Unknown/unregistered shortcuts fail the script. Parsing is case-insensitive;
Display normalizes modifiers to `ctrl+alt+shift+super`, then the lowercase key.
`cmd`, `command`, `meta`, and `logo` alias `super`; `option` aliases `alt`.

`scripts/check-global-shortcut-macos.py <shortcut_probe>` checks two-process
conflicts and real CuaDriver input on Pro under an exclusive `/tmp/pro-gui.lock`.
The probe stays windowless and verifies handler thread identity.

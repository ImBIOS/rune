# Overlay windows for Rooms

`run_windows` invokes a display-aware factory once and creates one runtime per
`WindowSpec`. Live handles and application services can be shared across those
states. Wakes carry the native window ID; hiding, occlusion and closing gate the
corresponding runtime's producer notifications.

The same lifetime host backs `run`, `App::run` and `run_windows`.
`WindowSpec::from_app` retains per-window AppHandle activation, windowless start,
keep-running, close callbacks and optional GPU release. Default close removes one
host; global quit terminates the loop. A retained host keeps its state on reopen.
All active hosts share one GPU instance/device through a weak pool; each retains
its own renderer and atlas. Releasing the last GPU-owning host releases the device.

`WindowOptions::overlay(display)` covers a monitor in physical desktop pixels,
including negative origins. It requests transparency, no decorations, topmost
placement and click-through. `WindowHandle` weakly references the native window;
it supports visibility, close, hit-testing, window level, a scoped raw handle and
the cached macOS CGWindowID used by ScreenCaptureKit exclusions.

The renderer's instance layout and primary shader remain unchanged. Transparent
windows clear to transparent black. Metal advertises PostMultiplied, but wgpu-hal
only marks CAMetalLayer non-opaque: Core Animation still expects premultiplied
pixels. Metal therefore draws directly without conversion. Other straight-alpha
surfaces and transparent headless PNGs use a separate linear-space conversion
pass. A GPU differential test checks transparent, translucent and opaque pixels
against a CPU linear-space reference with a one-byte tolerance; it passed on
Metal through HiveMind.
Unsupported transparency falls back to opaque presentation with a diagnostic;
unsupported click-through reports the platform error instead of aborting startup.

`--list-windows` and `--window KEY` use one virtual 1920×1080 display. Headless
selection uses the supplied per-window state and never calls native creation
callbacks. The overlay example pairs a control window with a cursor and SVG
stroke on each monitor; production stops after four seconds.

## Boundaries

The native shell has been compiled, not exercised against a desktop. Placement,
click-through toggling, actual OS wake counts and capture exclusion still need
Rooms integration testing. Monitor hot-plug reconciliation, non-activating panels,
all-Spaces/fullscreen-space behavior and status items are not implemented.

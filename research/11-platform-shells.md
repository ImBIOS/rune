# 11 — Platform Shells & Platform Order

Research for **Rune** (working title), an experimental Rust UI & animation framework for macOS, iOS, web, Windows, Linux, and Android.
Checked October 2026 against local checkouts of `winit` (`0.31.0-beta.3`), Zed's `gpui` (`gpui_macos`, `gpui_ios`, `gpui_web`, `gpui_windows`, `gpui_linux`), `makepad`, `flutter/engine`, and Kit's production apps (`scrawl`, `hex`, a private Mac launcher and a private iOS client).

---

## Summary

- **Write thin per-platform shells (`rune-macos`, `rune-ios`, `rune-web`, etc.) behind a narrow `Platform` / `PlatformWindow` trait; do not build on `winit`.** In `winit` `0.31.0-beta.3`, macOS live resize does not synchronize `CAMetalLayer` presentation with AppKit's `CATransaction`, macOS `NSTextInputClient` cannot read document context (`selectedRange` returns `NSNotFound` and `attributedSubstringForProposedRange` returns `None`), and iOS (`winit-uikit`) only implements `UIKeyInput` instead of `UITextInput`—breaking autocorrect, Japanese/Chinese IME, Dictation, Scribble, and selection handles.
- **Per-platform shells are smaller than `winit` while doing far more.** Zed's `gpui_macos` (9,711 LOC), `gpui_ios` (3,561 LOC), and `gpui_web` (6,994 LOC) are comparable in size to `winit-appkit` (8,667 LOC), `winit-uikit` (3,471 LOC), and `winit-web` (7,794 LOC), yet include full `NSTextInputClient` / `UITextInput` / hidden-`<textarea>` IME bridges, native menus, pasteboard, drag-and-drop, and `presentsWithTransaction` resize synchronization.
- **macOS 26 / iOS 26 Liquid Glass requires native backdrop views (`NSGlassEffectView` / `UIVisualEffectView` / `GlassEffectContainer`) behind or around a transparent `CAMetalLayer`.** Apple's private `CASDFElementLayer` + `CABackdropLayer` pipeline samples live desktop/wallpaper buffers behind the window that a client Metal shader cannot read. Scrawl's pill (`scrawl/src/native/pill/mod.rs:447-453, 1111-1147`) proves the exact pattern: an `NSGlassEffectView` under a transparent `CAMetalLayer` (`presentsWithTransaction = true`), updated together inside `CATransaction::begin() ... commit()`.
- **120 Hz ProMotion requires explicit frame-rate pinning on both macOS and iOS.** On macOS 14+, `NSView.displayLinkWithTarget:selector:` returns a `CADisplayLink` that adaptively throttles to 30 Hz (33.9 ms) unless `setPreferredFrameRateRange:` is pinned to `CAFrameRateRange { minimum: 120.0, maximum: 120.0, preferred: 120.0 }` (`makepad/.../macos_app.rs:1458-1482`). If using `CVDisplayLink`, keep instances immortal per `CGDirectDisplayID` (`gpui_macos/src/display_link.rs:1-86`) to avoid CoreVideo teardown segfaults. On iOS, pair `CADisplayLink.preferredFrameRateRange = CAFrameRateRangeMake(80, 120, 120)` with `CADisableMinimumFrameDurationOnPhone = YES` in `Info.plist`.
- **Incremental adoption in existing SwiftUI/UIKit apps (the iOS client, `hex`) is a first-class requirement.** Rune must work in two modes on Apple platforms: **Full-Window Shell** (Rune owns `NSApplication`/`UIApplication`) and **Embedded View** (`RuneMetalView: UIView` / `NSView` wrapped in `UIViewRepresentable` / `NSViewRepresentable`, sharing a single process-wide `MTLDevice` and `MTLCommandQueue`).
- **Native view embedding inside a Rune window should use Z-ordered underlay/overlay slots (`NSGlassEffectView` / `WKWebView` / `AVPlayerLayer` / `MKMapView`), not general-purpose interleaved slicing.** Flutter's `FlutterPlatformViewsController.mm` requires `EmbedderViewSlice` picture-recording, `OverlayLayerPool` allocations, and `FlutterTouchInterceptingView` gesture delay buffers for every interleaved platform view. Restricting embedded native views to background underlays (glass materials, video, maps) or top overlays (sheets, popovers, inspectors) eliminates intermediate GPU render targets.
- **Web in 2026 has universal WebGPU (including Safari 26 on macOS/iOS), but `EditContext` is still Chromium-only.** For web text input, steal `gpui_web`'s sliding-window hidden `<textarea>` (`gpui_web/src/ime_mirror.rs:1-198`, 512 UTF-16 code units each side of the selection, coalesced once per frame) plus an offscreen DOM accessibility mirror (`accesskit_web` pattern).
- **Ship in strict platform order: (1) macOS → (2) iOS + SwiftUI/UIKit host view → (3) Web (WASM + WebGPU) → (4) Windows → (5) Linux (Wayland-first) → (6) Android.** macOS and iOS share ~40% of their platform code (`gpui_apple`: Metal, CoreText, CoreAnimation, `dispatch2`, `objc2`) and serve all of Kit's immediate apps (`scrawl`, `hex`, the launcher and the iOS client), while Web is mandatory for interactive docs, playgrounds, and shareable demos.

---

## 1. `winit` vs Own Per-Platform Shells vs SDL3

### Codebase Size Reality Check (Measured October 2026)

| Codebase | macOS | iOS | Shared Apple | Web | Windows | Linux | Android | Total |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| **`winit` `0.31.0-beta.3`** (`winit-core`: 7,799) | 8,667 | 3,471 | — | 7,794 | 12,864 | 21,669 (WL 9,897 + X11 11,772) | 1,885 | **64,149 LOC** |
| **Zed `gpui`** (`platform.rs`: 1,731) | 9,711 | 3,561 | 3,865 | 6,994 | 13,187 | 16,422 (WL + X11 + Portals) | — | **55,471 LOC** |

`winit` is not smaller than a purpose-built GUI shell—it is larger, because it carries generality for game engines, legacy OS versions, and raw window handle permutations while omitting the exact primitives a UI toolkit needs (menus, clipboard, drag-and-drop, system dialogs, native backdrop views, and document-aware IME).

### Where `winit` (`0.31.0-beta.3`) Fails a High-Craft UI Framework

1. **macOS IME (`NSTextInputClient`) is stateless and broken for context-aware input:**
   In `winit/winit-appkit/src/view.rs:344-414`:
   - `selectedRange` (`lines 384-387`) unconditionally returns `NSRange { location: NSNotFound, length: 0 }` with a `// TODO` comment.
   - `attributedSubstringForProposedRange` (`lines 344-351`) unconditionally returns `None` (`// TODO`).
   - `insertText:replacementRange:` (`lines 285-310`) ignores `_replacement_range` (`// TODO: Implement replacement_range`).
   - Without `selectedRange` and `attributedSubstringForProposedRange`, macOS inline predictive text, press-and-hold accents, Japanese/Chinese reconversion, and Emoji replacement (`Ctrl+Cmd+Space` replacing a word) fail or insert at the wrong offset.
   - Compare `gpui_macos/src/window.rs:2125-2258`, which routes `selectedRange`, `markedRange`, `attributedSubstringForProposedRange`, `insertText:replacementRange:`, `setMarkedText:selectedRange:replacementRange:`, and `firstRectForCharacterRange:` directly to the focused widget's `PlatformInputHandler`.

2. **iOS (`winit-uikit`) has no `UITextInput` implementation at all:**
   In `winit/winit-uikit/src/view.rs:94-156`, `WinitView` only implements the minimal `UIKeyInput` protocol (`hasText`, `insertText:`, `deleteBackward`), and `insertText:` (`lines 102-135`) loops `.chars()` to synthesize fake `WindowEvent::KeyboardInput` key-down/key-up events.
   - Because it does not implement `UITextInput` (`textInRange:`, `replaceRange:withText:`, `selectedTextRange`, `markedTextRange`, `setMarkedText:selectedRange:`, `tokenizer`), iOS disables multistage CJK IME composition, autocorrect replacement spans, inline predictive text, Dictation, Scribble (Apple Pencil), and native Loupe/selection drag handles.
   - Compare `gpui_ios/src/ios/text_input.rs:1-1180`, which implements all 30+ `UITextInput` selectors in Rust on `GPUITextInputView` with generation-stamped `GPUITextPosition` / `GPUITextRange` objects so UIKit's async text-system queries never apply stale ranges after app state changes.

3. **macOS Live Resize Jank and `RunLoop` Re-entrancy:**
   During interactive window edge dragging on macOS, AppKit spins a modal nested run loop inside `-[NSWindow _resizeWithEvent:]` in `NSEventTrackingRunLoopMode`.
   - `winit-appkit` (`observer.rs`) uses `CFRunLoop` before-waiting/after-waiting observers to synthesize `RedrawRequested`, which fights AppKit's synchronized `CATransaction` resize pass unless the view implements `displayLayer:` with `CAMetalLayer.presentsWithTransaction = true`.
   - In `gpui_macos/src/window.rs:2260-2285`, the content view implements `wantsUpdateLayer -> YES` and `displayLayer:`. During `displayLayer:`, GPUI stops its display link, flips `metal_layer.set_presents_with_transaction(true)` (`gpui_macos/src/metal_renderer.rs:297-300`), renders synchronously at the new `bounds` and `backingScaleFactor`, calls `command_buffer.waitUntilScheduled()`, calls `drawable.present()`, and restores `presentsWithTransaction = false` when the display link resumes (`window.rs:2058-2075`). The window frame and Metal content resize in the exact same WindowServer commit with zero rubber-banding or black gutter.

4. **Embedded View Mode (`UIViewRepresentable` / `NSViewRepresentable`):**
   `winit` assumes ownership of `UIApplicationMain` (`winit-uikit/src/event_loop.rs`) or `NSApplication` activation policy and delegate (`winit-appkit`). Embedding a single Rune canvas inside an existing SwiftUI screen in the iOS client or `hex` is awkward and unsupported in `winit`, whereas a thin `RuneHostView` (`UIView` / `NSView`) shares 90% of `rune-ios` / `rune-macos`'s windowing/input/rendering code without owning `UIApplication`.

5. **SDL3 (`sdl3-sys`):**
   SDL3 improved GPU and high-DPI APIs over SDL2 and added `SDL_AddEventWatch` for live resize callbacks, but it remains a C game runtime: it lacks native macOS `NSGlassEffectView` / `NSPanel` vibrancy integration, full document-context `UITextInput` on iOS, native macOS `NSMenu` responder-chain validation, and web accessibility trees, while adding a C build dependency across every target.

---

## 2. macOS Deep Dive (`rune-macos`)

### Windowing (`NSWindow`, `NSPanel`, Traffic Lights, Fullscreen, Key Status)
- **Standard vs Floating Utility Windows:**
  Support two window classes in `rune-macos` (`gpui_macos/src/window.rs:197-205`):
  - `GPUIWindow: NSWindow` for standard document/app windows.
  - `GPUIPanel: NSPanel` for floating overlays, HUDs, pills, and launchers (`NSWindowStyleMaskNonactivatingPanel`, `collectionBehavior = CanJoinAllSpaces | FullScreenAuxiliary`, `level = NSFloatingWindowLevel` or `NSPopUpMenuWindowLevel` (`101`)).
- **Non-Activating Key Input & Cold-Start Warm-Up:**
  - In both Scrawl (`scrawl/src/native/pill/mod.rs:342-358`) and the launcher (`launcher/native/Launcher.swift:26-44`), subclass `NSPanel` to override `canBecomeKeyWindow -> YES` and `canBecomeMainWindow -> NO`. This allows typing and Escape/shortcut handling in a floating launcher or recording pill without stealing active-app status from the user's frontmost app.
  - **Cold-start warm-up:** On first launch, `NSPanel` + Metal/SwiftUI shader compilation can cause a 1-frame flash. The launcher (`launcher/native/Launcher.swift:566-578`) solves this by setting `panel.alphaValue = 0`, calling `orderFrontRegardless()` + `layoutIfNeeded()`, then `orderOut(nil)` and restoring `alphaValue = 1` at startup.
- **Traffic Lights & Titlebar Integration:**
  - Use `NSWindowStyleMaskTitled | NSWindowStyleMaskFullSizeContentView` with `titlebarAppearsTransparent = YES` and `titleVisibility = NSWindowTitleHidden`.
  - Override `layout` in the `NSWindow`/`NSPanel` subclass or inspect `standardWindowButton:` (`NSWindowCloseButton`, `NSWindowMiniaturizeButton`, `NSWindowZoomButton`) to position the traffic lights at a custom inset (`gpui_macos/src/window.rs:780-865`) and report `WindowControls` geometry to Rune's layout engine so app toolbars automatically pad around traffic lights (and shrink that padding to `0` when entering native fullscreen via `windowWillEnterFullScreen:` / `windowDidExitFullScreen:`).
  - Support native double-click-on-titlebar (`AppleMiniaturizeOnDoubleClick` / `performMiniaturize:` / `zoom:`) and `window.performWindowDragWithEvent:` when dragging an empty toolbar region.

### Liquid Glass (`NSGlassEffectView` on macOS 26) + `CAMetalLayer` Composition
 Apple's macOS 26 (`Tahoe`) introduced `NSGlassEffectView` (and `NSGlassEffectContainerView` for metaball-style merging of adjacent glass elements).
- **Why a pure Metal shader cannot replicate native Liquid Glass:**
  Inside WindowServer, `NSGlassEffectView` builds a layer tree containing `SDFLayer` + `CABackdropLayer` + `CASDFElementLayer`. WindowServer composites the live desktop wallpaper and windows *behind* your `NSWindow` into that `CABackdropLayer` with chromatic refraction and specular rim lighting before compositing your window's layers on top. A client process's `CAMetalLayer` has no read access to behind-window pixels without ScreenCaptureKit permissions and 1-frame latency.
- **How Scrawl composites `NSGlassEffectView` + `CAMetalLayer` in lockstep (`scrawl/src/native/pill/mod.rs`):**
  1. **View Hierarchy (`lines 422-455, 644-675`):**
     `panel.contentView` (`FlippedRootView`, `wantsLayer = YES`) contains:
     - **Underlay:** `NSGlassEffectView` (`setStyle: 0` for Regular or `1` for Clear, `setCornerRadius: h * 0.5`, `setTintColor: NSColor(srgb: 0.04, 0.04, 0.06, 0.25)`).
     - **Overlay:** `PillInteractiveView` (`wantsLayer = YES`), hosting a `CAMetalLayer` (`opaque = NO`, `pixelFormat = MTLPixelFormatBGRA8Unorm`, `framebufferOnly = YES`, `presentsWithTransaction = YES`).
  2. **Critical AppKit Initialization Quirk (`lines 447-453`):**
     `window.displayIfNeeded()` **must** be called once on the main thread while `glass_view` is non-hidden (`alphaValue = 0.0`) so AppKit executes its first `updateLayer` pass and constructs the internal `SDFLayer` + `CABackdropLayer` + `CASDFElementLayer` tree. If `glass_view` starts with `isHidden = YES`, AppKit skips building the backdrop layers until the first visible frame, causing a un-refracted pop.
  3. **Zero-Lag Synchronized Morphing (`lines 1111-1147`):**
     When the pill morphs size, position, or corner radius at 120 Hz, updating `NSGlassEffectView` outside the Metal frame causes a 1-frame tear between the glass capsule and the GPU-rendered waveform/controls inside it. Scrawl eliminates this by wrapping both in a single explicit `CATransaction`:
     ```rust
     CATransaction::begin();
     CATransaction::setDisableActions(true);
     // 1. Update NSGlassEffectView frame, cornerRadius, alphaValue + layoutSubtreeIfNeeded()
     self.sync_glass_view(glass_rect, corner_radius, glass_alpha, ...);
     // 2. Encode Metal render pass into CAMetalDrawable (with clearColor = 0,0,0,0)
     // ...
     cmd.commit();
     cmd.wait_until_scheduled();
     drawable.present(); // Because presentsWithTransaction = true, waits for CATransaction::commit()
     CATransaction::commit();
     ```
  4. **Fallback on macOS 14–15:** Fall back cleanly to `NSVisualEffectView` (`material = HUDWindow` or `UnderWindowBackground`, `blendingMode = BehindWindow`, `state = Active`) with a `CAShapeLayer` mask.

### Frame Pacing & ProMotion (120 Hz) on macOS
There are two display-link APIs on macOS, each with a sharp edge discovered in our reference codebases:
1. **`NSView.displayLinkWithTarget:selector:` (`CADisplayLink`, macOS 14+):**
   Used by Makepad (`makepad/platform/src/os/apple/macos/macos_app.rs:1458-1482`).
   - **Gotcha:** By default on ProMotion MacBook Pros, macOS's `CADisplayLink` uses adaptive frame pacing and drops to **30 Hz (33.9 ms)** when it does not detect native CoreAnimation layer animations!
   - **Fix (`macos_app.rs:1464-1474`):** Query `window.screen.maximumFramesPerSecond` (120 on ProMotion) and immediately call:
     ```rust
     let rate = max_fps as f32;
     let range = CAFrameRateRange { minimum: rate, maximum: rate, preferred: rate };
     let _: () = msg_send![display_link, setPreferredFrameRateRange: range];
     ```
2. **`CVDisplayLink` (CoreVideo):**
   Used by GPUI (`gpui_macos/src/display_link.rs:1-86`).
   - **Gotcha:** Stopping and deallocating a `CVDisplayLink` when a window closes or switches monitors races with CoreVideo's high-priority background thread and causes intermittent segfaults inside `CVDisplayLink::runIOThread` (`ZED-7XR`, Zed PR `#32116`).
   - **Fix (`display_link.rs:16-75`):** Maintain a process-global `Mutex<BTreeMap<CGDirectDisplayID, ImmortalDisplayLink>>` that creates at most one `CVDisplayLink` per display ID, never drops it, and uses `CVDisplayLinkStart` / `CVDisplayLinkStop` with reference counting across active windows on that display.
- **Recommendation for Rune:** Require macOS 14+ and use `NSWindow`/`NSView.displayLinkWithTarget:selector:` (`CADisplayLink`), which runs directly on the main run loop without cross-thread dispatch latency, and pin `preferredFrameRateRange` to `(80.0, max_fps, max_fps)` while animations or gestures are active.

### Pointer, Trackpad Momentum, Gesture Phases & Force Touch
- **`NSTrackingArea`:** Install an `NSTrackingArea` with `NSTrackingMouseMoved | NSTrackingActiveAlways | NSTrackingInVisibleRect | NSTrackingMouseEnteredAndExited` (`gpui_macos/src/window.rs:640-655`) so cursor changes and hover states work even when a floating panel or unfocused window is hovered.
- **Scroll Phases (`phase` vs `momentumPhase`):**
  macOS `NSEventTypeScrollWheel` carries two distinct `NSEventPhase` bitmasks:
  - `event.phase`: physical finger contact on the trackpad (`Began`, `Changed`, `Stationary`, `Ended`, `Cancelled`, or `MayBegin` when two fingers rest on the trackpad before moving).
  - `event.momentumPhase`: synthetic inertial scroll events generated by macOS after fingers lift (`Began`, `Changed`, `Ended`).
  - **Crucial detail:** When a user flicks a scroll view on a Mac trackpad and then puts two fingers back down on the trackpad, AppKit sends `event.phase == NSEventPhaseMayBegin` (and an `Ended` on the previous `momentumPhase`). Rune must expose both `phase` and `momentum_phase` plus `hasPreciseScrollingDeltas` (`scrollingDeltaX`/`Y` in points vs line ticks for notched mouse wheels) so custom spring/scroll physics can immediately interrupt momentum on finger touch-down.
- **Magnify, Rotate, SmartMagnify & Force Touch:**
  - Forward `magnifyWithEvent:` (`event.magnification`, `event.phase`), `rotateWithEvent:` (`event.rotation`), `smartMagnifyWithEvent:` (two-finger double-tap to zoom), and `pressureChangeWithEvent:` (`event.pressure` in `0.0..=1.0`, `event.stage` `1` = click, `2` = deep press / Force Touch) plus `NSHapticFeedbackManager::defaultPerformer().performFeedbackPattern:performanceTime:` for tactile detents when scrubbing sliders or snapping alignment guides.

### Menus, Menu-Bar Extras, Clipboard, Drag-and-Drop & Services
- **Menus & Responder Chain:** Build real `NSMenu` / `NSMenuItem` trees (`gpui_macos/src/menu.rs`) wired to an action selector that queries `validateMenuItem:` against Rune's focused responder chain. This gives macOS native Help-menu search, native keyboard shortcut learning, and native context menus (`NSMenu::popUpContextMenu:withEvent:forView:`) for free.
- **Menu-Bar Extras (`NSStatusItem`):** Provide a first-class `StatusItem` API on `rune-macos` (`NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength)`) that can either show a native `NSMenu` or anchor a borderless `NSPanel` popover below the status bar button's `window.frame`.
- **Clipboard (`NSPasteboard`) & Drag-and-Drop (`NSDraggingDestination` / `NSDraggingSource`):**
  Register `NSPasteboardTypeString`, `NSPasteboardTypeFileURL`, `NSPasteboardTypePNG`, `NSPasteboardTypeTIFF`, and custom UTI types (`window.registerForDraggedTypes:`). Implement `draggingEntered:`, `draggingUpdated:`, `draggingExited:`, and `performDragOperation:` (`gpui_macos/src/window.rs:2338-2435`) to convert external file/image/text drops into `PlatformInput::FileDrop` events with cursor coordinates.
- **Services & Quick Look:** Forward `validRequestorForSendType:returnType:` for macOS Services and `acceptsPreviewPanelControl:` / `beginPreviewPanelControl:` (`QLPreviewPanel`, as in `launcher/native/Launcher.swift:46-75`) so pressing Space on a file item opens native Quick Look.

---

## 3. iOS Deep Dive (`rune-ios`)

### Lifecycle (`UIApplication`, `UIScene`), `CAMetalLayer` & 120 Hz ProMotion
- **`UIScene` Lifecycle:** Modern iOS (and iOS 26+) requires scene-based lifecycle (`UIWindowSceneDelegate`) rather than legacy `UIApplicationDelegate.window` (`gpui_ios/src/ios/application.rs:145-315`).
- **`CAMetalLayer` Backing:** Subclass `UIView` (`RuneMetalView`) and override `+ (Class)layerClass` to return `[CAMetalLayer class]` (`gpui_ios/src/ios/window.rs:84-98`). Set `view.contentScaleFactor = UIScreen.mainScreen.nativeScale` (or `windowScene.screen.nativeScale`) and update `metalLayer.drawableSize` inside `layoutSubviews` so rotation and iPad Stage Manager / Split View resizes are synchronous.
- **120 Hz ProMotion on iPhone & iPad:**
  Two pieces are required—missing either locks the app to 60 Hz:
  1. `Info.plist` **must** include `<key>CADisableMinimumFrameDurationOnPhone</key><true/>`. Without this key, iPhone Pro displays ignore `CADisplayLink` requests above 60 Hz for third-party Metal views.
  2. On the `CADisplayLink` instance, call `setPreferredFrameRateRange:` with `CAFrameRateRangeMake(80.0, 120.0, 120.0)` during active animations/scrolls, and pause (`setPaused:YES`) when the scene is idle (`gpui_ios/src/ios/application.rs:331-340` currently omits `setPreferredFrameRateRange:`, leaving a ProMotion gap we should fix in Rune).
- **Concentric Corner Radius (iOS 26):** Use `UICornerConfiguration::containerConcentricRadius()` (`gpui_ios/src/ios/window.rs:391-425`) to read the hardware screen bezel corner radius so edge-aligned sheets and cards match the physical display curvature.

### Safe Areas, Keyboard Avoidance & Interactive Keyboard Dismissal
- **Safe Areas:** Observe `safeAreaInsetsDidChange` on the root `UIView` and feed `WindowInsets { top, left, bottom, right }` in logical points to Rune's layout root (`gpui_ios/src/ios/window.rs:342-389`).
- **Keyboard Avoidance & Interactive Frame Tracking:**
  Subscribe to `UIKeyboardWillChangeFrameNotification` / `UIKeyboardDidChangeFrameNotification` (`gpui_ios/src/ios/window.rs:285-338`), converting `UIKeyboardFrameEndUserInfoKey` into the view's coordinate space (`convertRect:fromCoordinateSpace:`) and reading `UIKeyboardAnimationDurationUserInfoKey` and `UIKeyboardAnimationCurveUserInfoKey` (curve `7` is Apple's private CASpringAnimation curve).
  - **Better on iOS 16+ / iOS 26:** Attach a constraint or observation to `view.keyboardLayoutGuide.followsUndockedKeyboard = YES` (`UIKeyboardLayoutGuide` / `UITrackingLayoutGuide`), which tracks the keyboard frame *frame-by-frame* during interactive scroll-to-dismiss gestures (`UIScrollViewKeyboardDismissModeInteractive`).

### Text Input (`UITextInput`), Selection Handles, Dictation & Scribble
- Do **not** use `UIKeyInput`. Use `gpui_ios/src/ios/text_input.rs` as the blueprint:
  - Implement `UITextInput` + `UITextInputTraits` directly on the Metal host view (or a dedicated responder subview).
  - Stamp custom `UITextPosition` and `UITextRange` Objective-C subclasses with a monotonically increasing `session_token: u64` (`text_input.rs:42-198`) that increments whenever the underlying Rust text buffer mutates outside UIKit's knowledge. Any stale range UIKit queries returns `nil` instead of panicking on out-of-bounds UTF-16/UTF-8 slicing.
  - Before changing selection or text from Rust, bracket the mutation with `[inputDelegate selectionWillChange:self]` / `[inputDelegate selectionDidChange:self]` or `textWillChange:` / `textDidChange:`.
  - Implement `firstRectForRange:` and `selectionRectsForRange:` returning `UITextSelectionRect` objects so iOS draws native floating autocorrect bubbles, the magnifying Loupe, Dictation glow, and Apple Pencil Scribble previews at the exact glyph bounds rendered by Rune's GPU text engine.

### Scroll Physics & Gesture Recognizers: Native vs Custom
- **Scroll Physics (`UIScrollView` vs Custom):**
  iOS users immediately notice non-native scroll deceleration or rubber-banding. Rather than embedding hidden `UIScrollView` instances for every scrollable list, replicate Apple's exact analytical scroll equations in Rust:
  - **Deceleration:** Exponential decay with `UIScrollViewDecelerationRateNormal = 0.998` per millisecond (`v(t) = v_0 * 0.998^t`, stopping threshold `|v| < 0.05 pt/ms`).
  - **Overscroll Rubber-Banding:** `offset = (1.0 - (1.0 / ((distance * 0.55 / dimension) + 1.0))) * dimension` (constant `c = 0.55`), transitioning into a critically damped spring (`ζ = 1.0`, response `~0.45s`) on release or boundary impact.
- **Gesture Disambiguation (`UIGestureRecognizer` Interop):**
  - Install a `UIScreenEdgePanGestureRecognizer` (`edges = UIRectEdgeLeft`) on the root view (`gpui_ios/src/ios/window.rs:248-275`) so system/navigation back-swipe gestures work naturally.
  - When Rune is hosted inside a UIKit/SwiftUI `NavigationStack` or `UIScrollView`, implement `UIGestureRecognizerDelegate` (`gestureRecognizerShouldBegin:` and `gestureRecognizer:shouldRecognizeSimultaneouslyWithGestureRecognizer:`) on a touch-forwarding recognizer on `RuneMetalView` so Rune can claim horizontal drags (e.g., scrubbing a timeline or carousel) while letting vertical drags or left-edge back-swipes pass through to the parent SwiftUI/UIKit container.

### Haptics & Dynamic Type
- **Haptics:** Wrap `UIImpactFeedbackGenerator` (`Light`, `Medium`, `Heavy`, `Soft`, `Rigid`), `UISelectionFeedbackGenerator`, and `UINotificationFeedbackGenerator` behind a zero-allocation `PlatformWindow::trigger_haptic(HapticPattern)` method, plus `CHHapticEngine` (CoreHaptics) for continuous texture/transient curves synced to spring velocity.
- **Dynamic Type:** Read `UITraitCollection.currentTraitCollection.preferredContentSizeCategory` and observe `UIContentSizeCategoryDidChangeNotification` (`registerForTraitChanges:@[UITraitPreferredContentSizeCategory.self]`). Expose the user's Dynamic Type scale factor (`UIFontMetrics.defaultMetrics.scaledValueForValue:`) to Rune's style system.

### Platform View Embedding & Bidirectional Hosting (Incremental Adoption in the iOS Client / Hex)

#### How Existing Frameworks Embed Native Views (and the Cost)
- **Flutter (`FlutterPlatformViewsController.mm:46-210`, `FlutterPlatformViews_Internal.h:23-160`):**
  Uses Hybrid Composition. When a Dart widget tree places a native `UIView` (`WKWebView`, `MKMapView`) between two Flutter widgets:
  1. The raster thread slices the display list around the platform view (`EmbedderViewSlice`), rendering background widgets to the base `CAMetalLayer` and foreground widgets into a pooled `FlutterOverlayView` (`OverlayLayerPool`) allocated as an extra full-size `CAMetalLayer` above the `UIView`.
  2. Every clip path on the platform view requires a `FlutterClippingMaskView` (`UIView.maskView`), which forces an offscreen CoreAnimation alpha mask pass.
  3. Every touch goes through `FlutterTouchInterceptingView`, which buffers `UITouch` sequences on the main thread until the Dart UI thread replies over an async hop whether its gesture arena won or lost.
- **Compose Multiplatform (`UIKitView` / `UIKitViewController`):**
  Places native `UIView`s in an overlay/underlay `UIView` container synchronized in `layoutSubviews`, punching a transparent hole (`BlendMode.Clear`) into the Skia/Metal canvas when using an underlay.
- **React Native (Fabric):**
  Every component *is* a `UIView`, so embedding is free, at the cost of no unified custom GPU canvas and main-thread `UIView` layout overhead.

#### Rune's Bidirectional Strategy
1. **Hosting Rune inside SwiftUI / UIKit (`RuneHostView` — Priority #1 for the iOS client and `hex`):**
   - Provide a C-ABI / Swift package wrapper (`RuneKit`) exposing `RuneUIView: UIView` and `RuneView: UIViewRepresentable` (and `RuneNSView` on macOS).
   - **Shared GPU Context:** Multiple `RuneUIView` instances in an existing SwiftUI app (e.g., an animated voice orb in `hex`, a streaming code/diff canvas or canvas diagram in the iOS client) share a single singleton `WgpuContext` / `MTLDevice` / `MTLCommandQueue` and glyph atlas in Rust, while each `RuneUIView` owns a lightweight `CAMetalLayer` surface and participates in a single shared `CADisplayLink`.
   - **Intrinsic Size & Transparent Background:** `RuneUIView` sets `isOpaque = NO`, `layer.opaque = NO`, and supports `sizeThatFits:` / `intrinsicContentSize` driven by Rune's layout pass so SwiftUI's `ScrollView` or `VStack` can size a Rune component naturally.
2. **Embedding Native Views inside Rune (Underlay / Overlay Slots):**
   - Instead of Flutter's arbitrary N-layer `EmbedderViewSlice` interleaving, support two explicit native-view slots per Rune window:
     - **Underlay slot** (behind the transparent `CAMetalLayer`, with Rune punching an alpha-0 cutout rect + corner radius in its Metal pass): used for `NSGlassEffectView` / `UIVisualEffectView` (Liquid Glass), `AVPlayerLayer` (zero-copy hardware video), and `MKMapView`.
     - **Overlay slot** (child `UIView`/`NSView` positioned above the `CAMetalLayer` synced inside `CATransaction`): used for `WKWebView`, `PHPickerViewController`, `PKCanvasView`, or native sheets/popovers.
   - Because Rust runs on the main UI thread (unlike Flutter's separate Dart UI / Raster threads), touch routing and `CATransaction` frame syncing happen **synchronously on the same thread** with zero `FlutterTouchInterceptingView` buffering latency.

### Liquid Glass on iOS 26
As documented in `ios-client/OpenCode/Glass.swift:1-59` and `ios-client/docs/native-ui-guidance.md:11-34`:
- iOS 26 introduces `GlassEffectContainer`, `.glassEffect(glass.interactive(), in: shape)` in SwiftUI, and `UIGlassEffect` / `UIGlassContainerEffect` in `UIVisualEffectView` for UIKit.
- When Rune runs in **Embedded View** mode inside the iOS client, SwiftUI toolbars and floating composer pills use native `OpenCodeGlassContainer` / `.openCodeGlass(in:)` while Rune renders content surfaces or interactive visualizations.
- When Rune owns the full window on iOS 26, `rune-ios` can instantiate `UIVisualEffectView(effect: UIGlassEffect.regular)` in the **Underlay slot** behind the transparent `CAMetalLayer` using the exact same `CATransaction`-synced architecture as Scrawl's macOS pill.

### Rust-to-iOS Build & Deploy Ergonomics
Inspected across `hex/ios/project.yml` and `ios-client/project.yml`:
- **Avoid `cargo-mobile2` and `dx bundle` for serious iOS work:** They generate brittle template Xcode projects that break as soon as an app adds Share Extensions (`OpenCodeShare`), Custom Keyboard Extensions (`HEXKeyboard`), App Groups, or Xcode Cloud (`ci_scripts/ci_post_clone.sh`).
- **Recommended Pipeline (`XcodeGen` + `cargo` staticlib/XCFramework + `xcrun devicectl`):**
  1. Define the Xcode project declaratively in `project.yml` (`XcodeGen`, used in both `hex` and the iOS client).
  2. Compile Rust as a `staticlib` (`librune_app.a`) for `aarch64-apple-ios` (device) and `aarch64-apple-ios-sim` (simulator) with a header/Swift bridge (`objc2` when Rust owns `main`, or `uniffi` / `@_cdecl` C-ABI when Swift owns `@main`).
  3. For simulator iteration: `cargo build --target aarch64-apple-ios-sim` → `xcodebuild` → `xcrun simctl install booted ... && xcrun simctl launch booted ...`.
  4. For physical iPhone ProMotion testing: `xcrun devicectl device install app --device <id> <App.app>` + `xcrun devicectl device process launch --device <id> --console <bundle-id>` gives sub-5-second incremental deploy and live stdout/stderr streaming over USB-C or Wi-Fi.

---

## 4. Web Deep Dive (`rune-web`)

### WebGPU in 2026
- **Browser Support:** As of 2026 (including **Safari 26** on macOS 26 and iOS 26, Chrome 113+, Edge 113+, and Firefox 141+), WebGPU (`navigator.gpu`) is enabled by default across all major desktop and mobile browsers.
- **Fallback:** Keep `wgpu`'s WebGL2 backend as a compile-time/runtime fallback (`Backends::BROWSER_WEBGPU | Backends::GL`) only for legacy corporate/Linux browsers or headless test environments without WebGPU adapters.
- **High-DPI Canvas Sizing (`ResizeObserver` `devicePixelContentBoxSize`):**
  As implemented in `gpui_web/src/window.rs:161-163, 455-510`, attach a `ResizeObserver` with `{ box: "device-pixel-content-box" }` (falling back to `contentRect * window.devicePixelRatio` on browsers lacking `devicePixelContentBoxSize`). This prevents 1-pixel rounding blur at fractional browser zoom levels (e.g., 110%, 125%).

### Canvas vs DOM Hybrid & Native Scroll Feel
- Render UI into a full-viewport `<canvas tabindex="-1" style="touch-action: none; user-select: none;">` (`gpui_web/src/window.rs:114-135`).
- **Trackpad vs Mouse Wheel on Web:** Browser `WheelEvent`s do not expose macOS's `phase` / `momentumPhase` directly. Distinguish trackpads from discrete mouse wheels using `event.deltaMode == WheelEvent.DOM_DELTA_PIXEL` + fractional/high-frequency deltas vs `DOM_DELTA_LINE`, and detect pinch-to-zoom via `wheel` events with `event.ctrlKey == true` (the standard convention browsers use for trackpad two-finger pinch).

### Text Input on Web: Hidden `<textarea>` Sliding Mirror vs `EditContext`
- **`EditContext` status in 2026:** The W3C `EditContext` API is shipped in Chromium (Chrome/Edge 121+), where it cleanly decouples IME composition from the DOM. However, **Safari 26 (WebKit) and Firefox still do not support `EditContext`**, meaning any production web shell must maintain a hidden DOM input element.
- **The Right Architecture — Steal `gpui_web/src/ime_mirror.rs` (`lines 1-198`):**
  - Naively using an empty hidden `<input>` breaks mobile soft keyboards (iOS Safari, Android Gboard) because backspace, swipe-typing, and autocorrect inspect the text surrounding the caret before emitting edits.
  - `gpui_web`'s `ImeMirror` creates a fixed `1px × 1px`, `opacity: 0`, `font-size: 16px` `<textarea>` (using `16px` prevents mobile Safari/Chrome from auto-zooming the viewport on focus; using `<textarea>` instead of `<input>` prevents the browser from silently stripping `\n`).
  - On touch devices (`matchMedia("(pointer: coarse)")`), it sets `inputmode="none"` until the user completes a tap (not a scroll pan!) on an editable widget (`window.rs:81-86`, `ime_mirror.rs:119-123`).
  - It mirrors a sliding window of `CONTEXT_CHARS = 512` UTF-16 code units before and after the current selection (`ime_mirror.rs:40-48`), and coalesces writes to the `<textarea>` at most once per event-loop turn (`schedule_sync`) because every write to `textarea.value` restarts the browser's IME connection.

### Accessibility on Web (DOM Mirror / AccessKit)
- A raw `<canvas>` is invisible to VoiceOver, NVDA, and browser Find-in-Page.
- Synchronize Rune's `accesskit::TreeUpdate` into a visually hidden (`opacity: 0; pointer-events: none; position: absolute`) ARIA DOM subtree positioned over the canvas (using `accesskit_web` or a lightweight semantic DOM projector) so screen readers, Tab navigation, and mobile accessibility gestures highlight exact widget bounding boxes.

### WASM Binary Size, Startup & Threading (`SharedArrayBuffer` / COOP-COEP)
- **Single-Threaded Default on Web:** Requiring `SharedArrayBuffer` (for WASM pthreads / `rayon`) forces every host page to serve `Cross-Origin-Opener-Policy: same-origin` and `Cross-Origin-Embedder-Policy: require-corp` headers. Those headers break embedding Rune demos inside documentation sites, GitHub Pages, iframes, or third-party blogs.
  - **Rule:** Rune's core layout, animation, and rendering pipeline on `wasm32-unknown-unknown` **must run cleanly on the main browser thread without `SharedArrayBuffer`** (`ForegroundExecutor` + `BackgroundExecutor` backed by browser microtasks / `postMessage` / optional Web Workers via message passing, as in `gpui_web/src/dispatcher.rs`).
- **Binary Size Budget:** Strip debug symbols, compile with `opt-level = "z"` or `"s"` + `lto = "fat"` + `wasm-opt -Oz`, and dynamically stream-load fonts rather than embedding multi-megabyte `.ttf` files in `.wasm`. Target **< 1.5 MB Brotli-compressed `.wasm`** for initial load.

---

## 5. Windows, Linux & Android

### Windows (`rune-windows`)
Grounded in `gpui_windows` (13,187 LOC):
- **Windowing & Composition (`DirectComposition`):**
  Create a Win32 `HWND` (`WS_EX_NOREDIRECTIONBITMAP`) paired with `IDCompositionDevice` / `IDCompositionTarget` / `IDCompositionVisual` (`gpui_windows/src/directx_renderer.rs:118-135`). Using DirectComposition with a flip-model swap chain (`DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL`, `DXGI_ALPHA_MODE_PREMULTIPLIED`) enables per-pixel transparency, Windows 11 Mica/Acrylic backdrops (`DwmSetWindowAttribute` with `DWMWA_SYSTEMBACKDROP_TYPE`), and smooth resize.
- **VSync & Frame Pacing (`vsync.rs:1-85`):**
  Query the monitor refresh period via `DwmGetCompositionTimingInfo` (`qpcRefreshPeriod`) and wait on `DwmFlush()` on a dedicated VSync thread. As `gpui_windows/src/vsync.rs:45-58` notes, when a monitor sleeps or is occluded, `DwmFlush()` returns immediately (< 1 ms); detect `elapsed < 1ms` and fall back to `sleep(interval)` so the app never busy-loops at 10,000 FPS.
- **Precision Touchpad Gestures (`DirectManipulation`):**
  Do **not** rely solely on legacy `WM_MOUSEWHEEL` / `WM_MOUSEHWHEEL` on Windows—they lack gesture phase (`Began`/`Ended`) and trackpad pinch zoom. Steal `gpui_windows/src/direct_manipulation.rs:1-160`: create an `IDirectManipulationManager` + `IDirectManipulationViewport` on the `HWND` and call `viewport.SetContact(pointer_id)` on `WM_POINTERHITTEST` when `pointer_type == PT_TOUCHPAD`. This delivers smooth pixel-accurate translation, inertia, and pinch-scale callbacks with explicit gesture phases.
- **Text & IME:** Use DirectWrite (`IDWriteFactory5`) for system font fallback and ClearType/grayscale contrast parameters (`directx_renderer.rs:32-37`), and `ImmAssociateContextEx` / `ImmSetCandidateWindow` (`IMM32` + TSF compatibility) for candidate window positioning.
- **GPU Backend:** Use `wgpu` on DX12 (or a direct DX11/DX12 backend behind Rune's renderer trait), ensuring `DXGI_ERROR_DEVICE_REMOVED` / `DEVICE_RESET` triggers an atlas wipe and forced full redraw (`gpui_windows/src/window.rs:68-75`).

### Linux (`rune-linux`)
Grounded in `gpui_linux` (16,422 LOC):
- **Wayland-First, X11 Fallback:**
  Build the event loop on `calloop` (`gpui_linux/src/linux/wayland/window.rs:10`). Target Wayland natively via `wayland-client` + `wayland-protocols`:
  - `xdg_wm_base` / `xdg_toplevel` / `xdg_popup` for windows and popups.
  - `wp_fractional_scale_v1` + `wp_viewporter` (`window.rs:21, 28`) for crisp native fractional scaling (125%, 150%, 175%) without XWayland blur.
  - `wl_surface::frame` callbacks (`pending_frame_callback`, `window.rs:136`) for compositor-driven VSync per monitor.
  - `zwp_text_input_v3` for IME composition and candidate rectangle placement (`set_cursor_rectangle`).
  - `zxdg_toplevel_decoration_v1` to negotiate server-side vs client-side window decorations (`CSD`/`SSD`), plus `zwlr_layer_surface_v1` (`wlr-layer-shell`) so Rune can build Linux status bars, launchers, and notifications.
- **XDG Desktop Portals (`ashpd`):**
  Steal `gpui_linux/src/linux/xdg_desktop_portal.rs:1-150`: connect an `ashpd` D-Bus listener into `calloop` to reactively track dark/light `ColorScheme`, cursor theme/size, titlebar button layout, and native sandboxed file pickers (`org.freedesktop.portal.FileChooser`) and screen sharing.

### Android (`rune-android`)
- **`GameActivity` / Custom `Activity` + `SurfaceView` vs `TextureView`:**
  - Full-screen Rune apps should use a lightweight Kotlin `Activity` hosting a `SurfaceView` (which gets its own hardware compositor layer and avoids extra GPU texture copies) synced to `Choreographer.getInstance().postFrameCallback` for 120 Hz VSync.
  - When Rune is embedded inside a scrolling Jetpack Compose / Android `View` hierarchy, use `TextureView` (or `SurfaceView` with `setZOrderMediaOverlay`) so it participates in view clipping and alpha transitions.
- **IME (`InputConnection`) & Accessibility (`AccessibilityNodeProvider`):**
  - Android software keyboards (Gboard, Samsung Keyboard) communicate via `View.onCreateInputConnection(EditorInfo)` returning a stateful `BaseInputConnection` (`setComposingText`, `commitText`, `deleteSurroundingText`, `getExtractedText`). Pure native `NativeActivity` cannot implement `InputConnection` without JNI calls to a Java/Kotlin `View`.
  - Pair a ~300-line Kotlin host view (`RuneSurfaceView.kt`) that implements `onCreateInputConnection` and `AccessibilityNodeProvider` (bridged to `accesskit`) with JNI calls into `rune-android`.

---

## 6. Platform-Specific Feel: Native vs Rune's Own

| Concern | Use OS Native | Build in Rune (Platform-Tuned) | Why |
|---|---|---|---|
| **Scroll physics** | — | **Rune's own**, parameterized by platform (`MacTrackpad`, `iOSMomentum`, `DesktopWheel`) | Gives deterministic 120 Hz spring interruption, programmatic scroll-to-element, and cross-view coordinated animations while matching iOS `0.998` deceleration + `0.55` rubber-band math and macOS `NSEventPhase` momentum handoff. |
| **Text selection UX** | **OS IME & Mobile Callout Bar** (`UITextInput` / `UIMenuController` / `UIEditMenuInteraction`) | **Rune renders selection highlights, carets & desktop handles** | On iOS, returning `selectionRectsForRange:` from `UITextInput` lets UIKit render the native Loupe and Copy/Paste/Translate pill while Rune renders the glyphs and selection quads at 120 Hz. |
| **Context menus** | **OS Native on macOS** (`NSMenu`) **& iOS** (`UIEditMenuInteraction` / `UIContextMenuInteraction`); **Rune custom** on Web/Win/Linux (or when custom rich previews are needed) | Configurable per menu | macOS users expect native `NSMenu` translucency, screen-edge flipping, and Services integration; rich app-specific command palettes use Rune's own overlay layer. |
| **Focus rings & Keyboard nav** | Respect OS settings (`NSApplication.fullKeyboardAccessEnabled`, `UIAccessibilityIsReduceMotionEnabled`, Windows high-contrast) | **Rune's own focus graph & animated focus ring** | Only Rune knows widget geometry and spring transitions; focus ring thickness, color (`accentColor`), and Tab traversal rules adapt to the active platform's conventions. |
| **File dialogs, Share sheets, Permissions** | **Always OS Native** (`NSOpenPanel`/`NSSavePanel`, `UIActivityViewController`/`PHPickerViewController`, XDG FileChooser Portal, Web `<input type="file">`) | Never custom | Security sandboxes (TCC, App Sandbox, Wayland Portals, browser security) require native OS pickers. |

---

## 7. Steal / Avoid Lists

### Steal
1. **Scrawl's `CATransaction`-synchronized `NSGlassEffectView` + `CAMetalLayer` pipeline** (`scrawl/src/native/pill/mod.rs:447-453, 644-675, 1111-1147`): `presentsWithTransaction = true`, initial `window.displayIfNeeded()` while `alphaValue = 0.0` so AppKit materializes `SDFLayer` + `CABackdropLayer`, and `CATransaction::begin() ... cmd.wait_until_scheduled(); drawable.present(); CATransaction::commit()` for zero-lag glass morphing.
2. **The launcher's non-activating `NSPanel` + cold-start warm-up + Quick Look responder chain** (`launcher/native/Launcher.swift:26-75, 566-578`): `canBecomeKey = true`, `canBecomeMain = false`, invisible warm-up pass on startup, and `QLPreviewPanel` control forwarding.
3. **Makepad's `CADisplayLink` 120 Hz ProMotion pinning on macOS 14+** (`makepad/platform/src/os/apple/macos/macos_app.rs:1458-1482`): pinning `setPreferredFrameRateRange:` to `window.screen.maximumFramesPerSecond` so macOS never throttles custom GPU views to 30 Hz.
4. **GPUI's `displayLayer:` synchronous live-resize dance on macOS** (`gpui_macos/src/window.rs:2058-2075, 2260-2285`): pausing the display link during `displayLayer:`, enabling `presentsWithTransaction = true`, rendering at the new size, and committing inside AppKit's resize transaction.
5. **GPUI's `GPUITextInputView` on iOS** (`gpui_ios/src/ios/text_input.rs:42-350`): full `UITextInput` implementation in Rust with `session_token`-stamped `UITextPosition`/`UITextRange` and deferred `reconcileTextInput`.
6. **GPUI Web's `ImeMirror` sliding-window hidden `<textarea>`** (`gpui_web/src/ime_mirror.rs:1-198`): 512-UTF-16-unit context window around the selection, `font-size: 16px`, `inputmode="none"` until tap completion on touch screens, and single-write-per-frame coalescing.
7. **GPUI Windows' `DirectManipulationHandler` & VSync sleep fallback** (`gpui_windows/src/direct_manipulation.rs:1-160`, `vsync.rs:41-60`): native precision-touchpad scroll/pinch phases on Windows and occlusion-safe `DwmFlush()` frame pacing.
8. **GPUI Linux's `calloop` + Wayland fractional scale + `ashpd` portal stream** (`gpui_linux/src/linux/wayland/window.rs`, `xdg_desktop_portal.rs`): clean event-loop integration of Wayland protocols and D-Bus desktop settings.

### Avoid
1. **Avoid `winit` as a dependency.** Working around its missing `UITextInput` on iOS, incomplete `NSTextInputClient` on macOS, lack of `presentsWithTransaction` resize sync, and inability to mount as a lightweight `UIViewRepresentable` costs more effort than writing thin shells directly with `objc2` / `web-sys` / `windows` / `wayland-client`.
2. **Avoid tearing down `CVDisplayLink` on window close** (`gpui_macos/src/display_link.rs:16-30`); prefer macOS 14+ `NSView.displayLinkWithTarget:selector:` (`CADisplayLink`) or immortal per-display `CVDisplayLink`s.
3. **Avoid Flutter-style general N-layer `EmbedderViewSlice` platform view slicing.** Allocating intermediate full-screen `CAMetalLayer`s and buffering touch events across threads (`FlutterPlatformViewsController.mm`) adds memory, GPU bandwidth, and input lag. Use explicit Underlay and Overlay view slots on the main thread instead.
4. **Avoid requiring `SharedArrayBuffer` (COOP/COEP headers) on Web.** It makes embedding Rune canvases in docs, blogs, and standard web pages impossible without custom server headers.
5. **Avoid `cargo-mobile2` or `dx bundle` for iOS project management.** Use `XcodeGen` (`project.yml`) + a `cargo` staticlib/XCFramework script so app extensions (`OpenCodeShare`, `HEXKeyboard`), entitlements, and Xcode Cloud continue to work like normal Xcode projects.

---

## 8. Recommendations for Rune

### 8.1 Own Thin Shells vs `winit`
**Decision: Write Rune's own thin per-platform shells (`rune-platform-*`).**
- **Rejected alternative (`winit` `0.31`):** Rejected because text input (`UITextInput` / `NSTextInputClient`), live-resize transaction sync (`presentsWithTransaction`), native materials (`NSGlassEffectView` / `UIGlassEffect`), and `UIViewRepresentable` / `NSViewRepresentable` embedding are architectural load-bearing walls, not bolt-on plugins.
- **Rejected alternative (`SDL3`):** Rejected because it adds a large C dependency without solving native Apple/Web text input, accessibility, or SwiftUI/UIKit view hosting.

### 8.2 The Shell API Surface (The Rust Traits)
Keep the boundary between `rune-core` and platform shells narrow—roughly 5 traits in `rune_core::platform` (modeled on a cleaner, slimmer version of `gpui/src/platform.rs`):

```rust
pub trait Platform: 'static {
    fn run(&self, on_Ready: Box<dyn FnOnce(&mut AppContext)>);
    fn dispatcher(&self) -> Arc<dyn PlatformDispatcher>;
    fn open_window(&self, options: WindowOptions) -> anyhow::Result<Box<dyn PlatformWindow>>;
    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>>;
    fn clipboard(&self) -> &dyn PlatformClipboard;
    fn set_menus(&self, menus: Vec<Menu>, keymap: &Keymap);
}

pub trait PlatformWindow: 'static {
    fn bounds(&self) -> Bounds<LogicalPixels>;
    fn scale_factor(&self) -> f32;
    fn safe_area_insets(&self) -> Edges<LogicalPixels>;
    fn appearance(&self) -> WindowAppearance;
    fn request_frame(&self, FrameRequest); // ActiveAnimation (120Hz) vs SingleFrame
    fn draw(&mut self, scene: &Scene, glass_underlays: &[GlassRegion], native_overlays: &[NativeOverlay]);
    fn set_input_handler(&mut self, handler: Option<PlatformInputHandler>);
    fn set_cursor(&mut self, cursor: CursorStyle);
    fn trigger_haptic(&self, pattern: HapticPattern);
    fn update_accessibility_tree(&mut self, update: accesskit::TreeUpdate);
    // Callbacks registered by rune-core:
    fn on_frame(&mut self, cb: Box<dyn FnMut(FrameInfo)>);
    fn on_input(&mut self, cb: Box<dyn FnMut(PlatformInput) -> EventResult>);
    fn on_resize(&mut self, cb: Box<dyn FnMut(Size<LogicalPixels>, f32, Edges<LogicalPixels>)>);
}

pub trait PlatformTextInput {
    fn text_for_range(&mut self, range_utf16: Range<usize>, adjusted: &mut Option<Range<usize>>) -> Option<String>;
    fn selected_range(&mut self, ignore_marked: bool) -> Option<SelectionUtf16>;
    fn marked_range(&self) -> Option<Range<usize>>;
    fn replace_text_in_range(&mut self, range_utf16: Option<Range<usize>>, text: &str);
    fn replace_and_mark_text_in_range(&mut self, range_utf16: Option<Range<usize>>, new_text: &str, selected_utf16: Option<Range<usize>>);
    fn unmark_text(&mut self);
    fn bounds_for_range(&mut self, range_utf16: Range<usize>) -> Option<Bounds<LogicalPixels>>;
    fn character_index_for_point(&mut self, point: Point<LogicalPixels>) -> Option<usize>;
}
```
Notice `draw(&mut self, scene: &Scene, glass_underlays: &[GlassRegion], native_overlays: &[NativeOverlay])`: passing native glass/underlay regions alongside the GPU `Scene` lets `rune-macos` and `rune-ios` synchronize `NSGlassEffectView` / `UIVisualEffectView` frames and corner radii inside the **same `CATransaction`** as the Metal drawable presentation (`scrawl/src/native/pill/mod.rs:1111-1147`).

### 8.3 Native View Embedding Strategy
Support two complementary mechanisms:
1. **Rune-in-Native (`RuneHostView`):** Every platform shell exposes a constructor that wraps an existing native view (`UIView`, `NSView`, `<canvas>`, `HWND`, `SurfaceView`) instead of creating a top-level OS window. On iOS and macOS, ship a thin Swift wrapper (`RuneView: UIViewRepresentable` / `NSViewRepresentable`) so the iOS client, `hex`, `scrawl`, and the launcher can drop a 120 Hz Rune view into any SwiftUI screen today, sharing one process-wide `MTLDevice` and `CADisplayLink`.
2. **Native-in-Rune (Underlay & Overlay Slots):** Within a Rune window, support declarative `NativeUnderlay` (for `NSGlassEffectView` / `UIGlassEffect`, `AVPlayerLayer`, `MKMapView` behind a transparent hole in the Metal pass) and `NativeOverlay` (for `WKWebView`, system pickers, and popovers above the Metal layer), synced synchronously on the main thread inside `CATransaction`.

### 8.4 Platform Order & Effort Estimates

| Priority | Platform Shell | Est. Size | Est. Effort | Why This Order |
|---:|---|---:|---:|---|
| **1** | **macOS (`rune-macos` + `rune-apple`)** | ~4,500 LOC (`macos`) + ~2,000 LOC (`apple` shared) | **2–3 weeks** | Fastest local compile/debug cycle; directly powers `scrawl` and the launcher; establishes the `objc2` + `CAMetalLayer` + `CATransaction` + `NSGlassEffectView` + CoreText foundation shared with iOS. |
| **2** | **iOS (`rune-ios` + `RuneKit` Swift host)** | ~3,000 LOC Rust + ~400 LOC Swift | **2 weeks** | Reuses `rune-apple` (CoreText, Metal/wgpu, `dispatch2`, `objc2`); unlocks 120 Hz ProMotion touch/spring validation on iPhone and immediate incremental adoption inside the iOS client and `hex` via `RuneView: UIViewRepresentable`. |
| **3** | **Web (`rune-web`, WASM + WebGPU)** | ~3,500 LOC | **2 weeks** | Safari 26 + Chrome/Firefox/Edge now all support WebGPU; essential for interactive documentation, motion playgrounds, and zero-install demos shared over Tailscale or the web. |
| **4** | **Windows (`rune-windows`)** | ~6,500 LOC | **3–4 weeks** | Largest desktop user base outside macOS; `gpui_windows` provides a complete blueprint for DirectComposition, `DwmFlush` VSync, and `DirectManipulation` trackpad gestures. |
| **5** | **Linux (`rune-linux`, Wayland-first)** | ~6,000 LOC (Wayland + `ashpd` portals) | **3 weeks** | Focus on Wayland (`xdg-shell`, `wp-fractional-scale-v1`, `text-input-v3`, `wlr-layer-shell`, `ashpd`); defer native X11 unless XWayland proves insufficient for specific users. |
| **6** | **Android (`rune-android`)** | ~2,500 LOC Rust + ~400 LOC Kotlin | **2–3 weeks** | Lowest immediate priority for Kit's apps; architectural requirements (JNI `InputConnection` + `Choreographer` + `SurfaceView`/`TextureView`) are already accommodated by the `PlatformWindow` and `PlatformTextInput` traits designed in steps 1–3. |

---

## 9. Open Questions

1. **Multiple `NSGlassEffectView` instances vs `NSGlassEffectContainerView` on macOS 26 / iOS 26:** When a Rune window contains multiple floating glass pills that morph and merge (metaball effect), should `GlassRegion` group them under a single `NSGlassEffectContainerView` / `UIGlassContainerEffect` underlay, and how many concurrent glass regions can WindowServer composite at 120 Hz before backdrop bandwidth drops frames?
2. **Swift 6.2 `@MainActor` interop with `RuneKit`:** In the iOS client (`SWIFT_DEFAULT_ACTOR_ISOLATION: MainActor`), what is the cleanest zero-boilerplate Swift macro or `@MainActor` wrapper so TCA reducers/views can drive `RuneView` state props without copying large buffers across FFI?
3. **Web `EditContext` progressive enhancement:** Should `rune-web` use Chromium's `EditContext` API when `window.EditContext !== undefined` and fall back to `ImeMirror` (`<textarea>`) on Safari/Firefox, or use `ImeMirror` everywhere on Web to avoid maintaining two browser IME codepaths?
4. **Linux X11 support:** Is Wayland + XWayland sufficient for Rune's Linux target in 2026–2027, saving ~6,000 LOC of legacy X11/XCB/XIM code (`gpui_linux/src/linux/x11`), or do target Linux users still require a native X11 backend?

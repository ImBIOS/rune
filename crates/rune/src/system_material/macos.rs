//! Native material views behind a window's Metal layer, moved in the drawable's transaction.
//!
//! winit's content view is layer-backed and wgpu draws into a CAMetalLayer that is a sublayer
//! of the view's layer. Material views live in a flipped host view added under that layer,
//! which never takes clicks, so winit's view still receives every event. With
//! `presentsWithTransaction` on the Metal layer, wgpu presents the drawable into the current
//! Core Animation transaction; the shell opens one around the present and sets every view's
//! frame, corner radius and alpha inside it, so the WindowServer composites the material, the
//! shadow around it and the content on top from the same commit.

use super::{MaterialDraw, Mode, SystemMaterial, MAX_NATIVE_MATERIALS};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_video::rc::Retained;
use objc2_video::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
use objc2_video::{msg_send, sel};
use std::ptr::NonNull;

/// The host view: flipped, so frames come straight from Rune's top-left points, and never hit,
/// so clicks fall through to winit's view behind it in the hit-test order.
fn host_class() -> Option<&'static AnyClass> {
    const NAME: &std::ffi::CStr = c"RuneMaterialHost";
    if let Some(class) = AnyClass::get(NAME) { return Some(class); }
    let mut builder = ClassBuilder::new(NAME, AnyClass::get(c"NSView")?)?;
    unsafe extern "C-unwind" fn flipped(_: &AnyObject, _: Sel) -> Bool { Bool::YES }
    unsafe extern "C-unwind" fn hit_test(_: &AnyObject, _: Sel, _: CGPoint) -> *mut AnyObject { std::ptr::null_mut() }
    // SAFETY: the signatures match -[NSView isFlipped] and -[NSView hitTest:].
    unsafe {
        builder.add_method(sel!(isFlipped), flipped as unsafe extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(hitTest:), hit_test as unsafe extern "C-unwind" fn(_, _, _) -> _);
    }
    Some(builder.register())
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect { origin: CGPoint { x, y }, size: CGSize { width: w, height: h } }
}

struct Slot {
    view: Retained<AnyObject>,
    material: SystemMaterial,
    /// `NSGlassEffectView`, which rounds itself; the effect views round their layer.
    glass: bool,
    shown: Option<([f64; 4], f64, f64, bool)>,
}

#[link(name = "QuartzCore", kind = "framework")]
unsafe extern "C" {
    static kCACornerCurveContinuous: &'static AnyObject;
    static kCACornerCurveCircular: &'static AnyObject;
}

/// One window's material views.
pub(crate) struct Materials {
    mode: Mode,
    /// Native views are on: elements skip their fill, and in synced modes the Metal layer
    /// presents with transactions.
    active: bool,
    view: Option<Retained<AnyObject>>,
    host: Option<Retained<AnyObject>>,
    metal: Option<Retained<AnyObject>>,
    slots: Vec<Slot>,
}

impl Materials {
    pub(crate) fn new() -> Self {
        Materials { mode: Mode::from_env(), active: false, view: None, host: None, metal: None, slots: Vec::new() }
    }

    /// Whether this frame's elements should leave their interior to native views.
    pub(crate) fn native(&self) -> bool { self.active }

    /// After a build that found material elements: switches native views on for a transparent
    /// window. True when it did, so the caller drops this frame (built with fallback fills)
    /// and draws the next one natively, because wgpu reads `presentsWithTransaction` when it
    /// acquires a drawable and this frame's drawable is already out.
    pub(crate) fn activate(&mut self, transparent: bool, found: bool, window: &winit::window::Window, surface: &wgpu::Surface<'_>) -> bool {
        if self.active || !found || !transparent || self.mode == Mode::Off { return false; }
        let Some(view) = crate::macos::content_view(window) else { return false };
        // SAFETY: winit's content view outlives this retain, which only adds a reference.
        let Some(view) = (unsafe { Retained::retain(objc2::rc::Retained::as_ptr(&view).cast::<AnyObject>().cast_mut()) }) else { return false };
        let Some(metal) = metal_layer(surface) else { return false };
        if self.mode != Mode::Unsynced {
            // SAFETY: a CAMetalLayer property, set on the main thread before the next acquire.
            unsafe { let _: () = msg_send![&*metal, setPresentsWithTransaction: true]; }
        }
        self.view = Some(view);
        self.metal = Some(metal);
        self.active = true;
        true
    }

    /// Presents the frame with the material views matching `draws`: in one transaction with
    /// the drawable when synced, or just before it otherwise.
    pub(crate) fn present(&mut self, draws: &[MaterialDraw], scale: f32, present: impl FnOnce()) {
        if !self.active { return present(); }
        let transaction = AnyClass::get(c"CATransaction");
        let synced = self.mode != Mode::Unsynced && transaction.is_some();
        if let Some(t) = transaction {
            // SAFETY: CATransaction class methods on the main thread, balanced below.
            unsafe { let _: () = msg_send![t, begin]; let _: () = msg_send![t, setDisableActions: true]; }
        }
        self.sync(draws, scale);
        if !synced && let Some(t) = transaction {
            // SAFETY: closes the transaction opened above.
            unsafe { let _: () = msg_send![t, commit]; }
        }
        present();
        if synced && let Some(t) = transaction {
            // SAFETY: closes the transaction opened above, with the drawable inside it.
            unsafe { let _: () = msg_send![t, commit]; }
        }
    }

    fn sync(&mut self, draws: &[MaterialDraw], scale: f32) {
        let used = draws.len().min(MAX_NATIVE_MATERIALS);
        if used == 0 && self.slots.iter().all(|s| s.shown.is_none()) { return; }
        let Some(host) = self.host() else { return };
        for (k, draw) in draws.iter().take(used).enumerate() {
            let material = self.mode.resolve(draw.material);
            if self.slots.get(k).is_some_and(|s| s.material != material) {
                // SAFETY: removes a view this struct added to the host.
                unsafe { let _: () = msg_send![&*self.slots.remove(k).view, removeFromSuperview]; }
            }
            if self.slots.len() <= k {
                let Some(slot) = make_view(material, &host) else { return };
                self.slots.insert(k, slot);
            }
            let slot = &mut self.slots[k];
            let (frame, radius) = draw.points(scale);
            let state = (frame, radius, f64::from(draw.opacity.clamp(0.0, 1.0)), draw.continuous);
            if slot.shown == Some(state) { continue; }
            let view = &*slot.view;
            // SAFETY: NSView and NSGlassEffectView setters and CALayer properties on the main thread.
            unsafe {
                let _: () = msg_send![view, setFrame: rect(frame[0], frame[1], frame[2], frame[3])];
                if slot.glass {
                    let _: () = msg_send![view, setCornerRadius: radius];
                } else {
                    let layer: *mut AnyObject = msg_send![view, layer];
                    if let Some(layer) = layer.as_ref() {
                        let _: () = msg_send![layer, setCornerRadius: radius];
                        let curve: &AnyObject = if draw.continuous { kCACornerCurveContinuous } else { kCACornerCurveCircular };
                        let _: () = msg_send![layer, setCornerCurve: curve];
                    }
                }
                let _: () = msg_send![view, setAlphaValue: state.2];
                let _: () = msg_send![view, layoutSubtreeIfNeeded];
                let _: () = msg_send![view, displayIfNeeded];
            }
            slot.shown = Some(state);
        }
        for slot in self.slots.iter_mut().skip(used).filter(|s| s.shown.is_some()) {
            // SAFETY: an NSView setter on the main thread. Zero alpha rather than hidden keeps
            // the view's backdrop layers, as at creation.
            unsafe { let _: () = msg_send![&*slot.view, setAlphaValue: 0.0f64]; }
            slot.shown = None;
        }
    }

    /// The host view, added under the Metal layer on first use and kept there.
    fn host(&mut self) -> Option<Retained<AnyObject>> {
        let view = self.view.clone()?;
        if self.host.is_none() {
            // SAFETY: AppKit calls on the main thread; `host` is retained here and added to
            // winit's content view below every other subview.
            unsafe {
                let bounds: CGRect = msg_send![&*view, bounds];
                let alloc: *mut AnyObject = msg_send![host_class()?, alloc];
                let host: *mut AnyObject = msg_send![alloc, initWithFrame: bounds];
                let host = Retained::from_raw(host)?;
                // Width and height sizable.
                let _: () = msg_send![&*host, setAutoresizingMask: 18usize];
                let _: () = msg_send![&*host, setWantsLayer: true];
                let _: () = msg_send![&*view, addSubview: &*host, positioned: -1isize, relativeTo: std::ptr::null::<AnyObject>()];
                self.host = Some(host);
            }
        }
        let host = self.host.clone()?;
        // SAFETY: CALayer reads and an insert within the content view's own layer.
        unsafe {
            let root: *mut AnyObject = msg_send![&*view, layer];
            let host_layer: *mut AnyObject = msg_send![&*host, layer];
            if let (Some(root), Some(host_layer), Some(metal)) = (root.as_ref(), host_layer.as_ref(), self.metal.as_deref()) {
                let layers: *mut AnyObject = msg_send![root, sublayers];
                if let Some(layers) = layers.as_ref() {
                    let at = |layer: &AnyObject| -> usize { msg_send![layers, indexOfObjectIdenticalTo: layer] };
                    if at(metal) == usize::MAX {
                        let _: () = msg_send![root, insertSublayer: metal, above: host_layer];
                    }
                    // AppKit re-sorts the sublayers to its subviews' order whenever it updates
                    // the layer tree, such as when a hidden window is shown, which would leave
                    // the drawn frame under the material until the next frame. Siblings
                    // composite by zPosition first.
                    let _: () = msg_send![metal, setZPosition: 1.0f64];
                }
            }
        }
        Some(host)
    }
}

impl Drop for Materials {
    fn drop(&mut self) {
        if let Some(host) = &self.host {
            // SAFETY: removes the host this struct added; drop runs on the main thread with the window.
            unsafe { let _: () = msg_send![&**host, removeFromSuperview]; }
        }
    }
}

impl Mode {
    /// The material actually shown: `Glass` falls back to the popover effect view before
    /// macOS 26 and in legacy mode.
    fn resolve(self, material: SystemMaterial) -> SystemMaterial {
        match material {
            SystemMaterial::Glass if self == Mode::Legacy || AnyClass::get(c"NSGlassEffectView").is_none() => SystemMaterial::Popover,
            other => other,
        }
    }
}

/// A transparent material view in `host`.
fn make_view(material: SystemMaterial, host: &AnyObject) -> Option<Slot> {
    let glass = material == SystemMaterial::Glass;
    let class = AnyClass::get(if glass { c"NSGlassEffectView" } else { c"NSVisualEffectView" })?;
    // SAFETY: AppKit initializers and setters on the main thread; the view is retained by its slot.
    unsafe {
        let alloc: *mut AnyObject = msg_send![class, alloc];
        let view: *mut AnyObject = msg_send![alloc, initWithFrame: rect(0.0, 0.0, 0.0, 0.0)];
        let view = Retained::from_raw(view)?;
        let _: () = msg_send![&*view, setWantsLayer: true];
        if glass {
            // NSGlassEffectViewStyleRegular.
            let _: () = msg_send![&*view, setStyle: 0isize];
        } else {
            // NSVisualEffectMaterialPopover, behind window, always active.
            let _: () = msg_send![&*view, setMaterial: 6isize];
            let _: () = msg_send![&*view, setBlendingMode: 0isize];
            let _: () = msg_send![&*view, setState: 1isize];
            let layer: *mut AnyObject = msg_send![&*view, layer];
            if let Some(layer) = layer.as_ref() { let _: () = msg_send![layer, setMasksToBounds: true]; }
        }
        // Built visible at zero alpha: AppKit skips a hidden glass view's backdrop layers until
        // its first visible frame, which then shows unrefracted.
        let _: () = msg_send![&*view, setAlphaValue: 0.0f64];
        let _: () = msg_send![host, addSubview: &*view];
        let _: () = msg_send![&*view, displayIfNeeded];
        Some(Slot { view, material, glass, shown: None })
    }
}

/// The surface's CAMetalLayer, when it is one on Metal.
fn metal_layer(surface: &wgpu::Surface<'_>) -> Option<Retained<AnyObject>> {
    // SAFETY: only retains the layer wgpu presents into.
    let hal = unsafe { surface.as_hal::<wgpu::hal::api::Metal>() }?;
    let guard = hal.render_layer().lock();
    let ptr = NonNull::new(Retained::as_ptr(&*guard).cast::<AnyObject>().cast_mut())?;
    // SAFETY: a valid, live CAMetalLayer pointer.
    unsafe { Retained::retain(ptr.as_ptr()) }
}

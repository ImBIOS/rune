//! `NSDraggingSource` for [`super::begin`]. The session starts inside winit's `mouseDragged:`
//! (which dispatches `CursorMoved` synchronously), so `NSApp.currentEvent` is the drag event.
//! Pasteboard types follow what Finder and System Settings' privacy lists accept for a file.
use super::{DragImage, DragOutStart, DragOutcome, path_text};
use std::cell::{Cell, RefCell};
use objc2::{ClassType, DeclaredClass, declare_class, msg_send_id, mutability, rc::Retained,
    runtime::{AnyObject, NSObjectProtocol, ProtocolObject}};
use objc2_app_kit::{NSApplication, NSDragOperation, NSDraggingContext, NSDraggingFormation, NSDraggingItem,
    NSDraggingSession, NSDraggingSource, NSEvent, NSEventModifierFlags, NSEventType, NSImage,
    NSPasteboardItem, NSPasteboardTypeFileURL, NSPasteboardTypePNG, NSPasteboardTypeString, NSPasteboardTypeURL, NSView};
use objc2_foundation::{MainThreadMarker, NSArray, NSData, NSObject, NSPoint, NSRect, NSSize, NSString, NSURL};

type Done = Box<dyn FnOnce(DragOutcome)>;

struct Ivars {
    done: RefCell<Option<Done>>,
    mask: Cell<NSDragOperation>,
}

declare_class!(
    struct Source;
    unsafe impl ClassType for Source {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "RuneDragOutSource";
    }
    impl DeclaredClass for Source { type Ivars = Ivars; }
    unsafe impl NSObjectProtocol for Source {}
    unsafe impl NSDraggingSource for Source {
        #[method(draggingSession:sourceOperationMaskForDraggingContext:)]
        fn mask(&self, _session: &NSDraggingSession, _context: NSDraggingContext) -> NSDragOperation {
            self.ivars().mask.get()
        }
        #[method(draggingSession:endedAtPoint:operation:)]
        fn ended(&self, _session: &NSDraggingSession, _point: NSPoint, operation: NSDragOperation) {
            let done = self.ivars().done.borrow_mut().take();
            if let Some(done) = done {
                let outcome = outcome(operation);
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| done(outcome))).is_err() {
                    eprintln!("rune: drag end callback panicked");
                }
            }
        }
        #[method(ignoreModifierKeysForDraggingSession:)]
        fn ignore_modifiers(&self, _session: &NSDraggingSession) -> bool { true }
    }
);

thread_local! {
    // AppKit's session doesn't document retaining its source; one drag runs at a time, so the
    // newest source is kept until the next drag replaces it.
    static LIVE: RefCell<Option<Retained<Source>>> = const { RefCell::new(None) };
}

fn outcome(operation: NSDragOperation) -> DragOutcome {
    if operation.contains(NSDragOperation::Copy) { DragOutcome::Copied }
    else if operation.intersects(NSDragOperation::Move | NSDragOperation::Delete | NSDragOperation::Generic) { DragOutcome::Moved }
    else if operation.contains(NSDragOperation::Link) { DragOutcome::Linked }
    else { DragOutcome::Cancelled }
}

pub(super) fn begin(window: &winit::window::Window, start: DragOutStart, snapshot: Option<DragImage>, done: Done) {
    let Some(mtm) = MainThreadMarker::new() else { return done(DragOutcome::Cancelled) };
    let Some(view) = crate::macos::content_view(window) else { return done(DragOutcome::Cancelled) };
    let Some(event) = drag_event(mtm, &view) else { return done(DragOutcome::Cancelled) };
    let custom = start.item.drag_image().is_some();
    let picture = start.item.drag_image().cloned().or(snapshot);
    // SAFETY: AppKit calls on the main thread with live objects; the source outlives the
    // session through LIVE.
    unsafe {
        let (image, frame) = image_and_frame(&start, picture.as_ref(), custom);
        let items = pasteboard_items(&start);
        if items.is_empty() { return done(DragOutcome::Cancelled); }
        let dragging: Vec<Retained<NSDraggingItem>> = items.iter().enumerate().map(|(i, item)| {
            let d = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), ProtocolObject::from_ref(&**item));
            // Extra files trail a little behind the first, as Finder stacks them.
            let offset = (i.min(4) as f64) * 3.0;
            let f = NSRect::new(NSPoint::new(frame.origin.x + offset, frame.origin.y + offset), frame.size);
            d.setDraggingFrame_contents(f, image.as_deref().map(|i| &**i as &AnyObject));
            d
        }).collect();
        let mut mask = NSDragOperation::Copy;
        if start.item.allows_move() { mask |= NSDragOperation::Move; }
        let source = mtm.alloc::<Source>().set_ivars(Ivars { done: RefCell::new(Some(done)), mask: Cell::new(mask) });
        let source: Retained<Source> = msg_send_id![super(source), init];
        LIVE.with(|live| *live.borrow_mut() = Some(source.clone()));
        let session = view.beginDraggingSessionWithItems_event_source(&NSArray::from_vec(dragging), &event, ProtocolObject::from_ref(&*source));
        session.setAnimatesToStartingPositionsOnCancelOrFail(true);
        session.setDraggingFormation(NSDraggingFormation::None);
    }
}

/// The mouse event the session starts from: the event being dispatched when it's a left-button
/// press or drag, else one synthesised at the pointer (an overlay's polled pointer runs outside
/// event dispatch).
fn drag_event(mtm: MainThreadMarker, view: &NSView) -> Option<Retained<NSEvent>> {
    let current = NSApplication::sharedApplication(mtm).currentEvent();
    // SAFETY: reading event and window properties on the main thread.
    unsafe {
        if let Some(event) = &current {
            if matches!(event.r#type(), NSEventType::LeftMouseDragged | NSEventType::LeftMouseDown) { return current; }
        }
        let window = view.window()?;
        let at = window.mouseLocationOutsideOfEventStream();
        let time = current.as_ref().map_or(0.0, |e| e.timestamp());
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            NSEventType::LeftMouseDragged, at, NSEventModifierFlags::empty(), time, window.windowNumber(), None, 0, 1, 1.0)
    }
}

/// The drag image and where it starts, in the view's (flipped, top-left) points.
fn image_and_frame(start: &DragOutStart, picture: Option<&DragImage>, custom: bool) -> (Option<Retained<NSImage>>, NSRect) {
    // SAFETY: NSImage and NSWorkspace calls on the main thread (`begin`'s only caller context).
    unsafe {
        let [x, y, w, h] = start.rect;
        let image = picture.and_then(|p| {
            let image = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(p.png_data()))?;
            let size = match p.points() {
                Some((w, h)) => NSSize::new(w as f64, h as f64),
                None => image.size(),
            };
            image.setSize(size);
            Some(image)
        });
        if let Some(image) = image {
            let size = image.size();
            let origin = if custom {
                // A supplied picture is centred under the pointer.
                NSPoint::new(start.press.0 as f64 - size.width / 2.0, start.press.1 as f64 - size.height / 2.0)
            } else {
                // The snapshot sits exactly over the element (cropped to the window's top-left).
                NSPoint::new(x.max(0.0) as f64, y.max(0.0) as f64)
            };
            return (Some(image), NSRect::new(origin, size));
        }
        // No picture: the first file's Finder icon, else the element's outline only.
        let icon = start.item.paths().first().map(|path| {
            let workspace: Retained<AnyObject> = msg_send_id![objc2::class!(NSWorkspace), sharedWorkspace];
            let icon: Retained<NSImage> = msg_send_id![&workspace, iconForFile: &*NSString::from_str(&path_text(path))];
            icon.setSize(NSSize::new(64.0, 64.0));
            icon
        });
        match icon {
            Some(icon) => (Some(icon), NSRect::new(NSPoint::new(start.press.0 as f64 - 32.0, start.press.1 as f64 - 32.0), NSSize::new(64.0, 64.0))),
            None => (None, NSRect::new(NSPoint::new(x as f64, y as f64), NSSize::new(w.max(1.0) as f64, h.max(1.0) as f64))),
        }
    }
}

/// One pasteboard item per file, the PNG and text riding on the first (or alone without files).
fn pasteboard_items(start: &DragOutStart) -> Vec<Retained<NSPasteboardItem>> {
    // SAFETY: Foundation and AppKit pasteboard calls with live objects; AppKit's type
    // constants are immutable statics.
    unsafe {
        let item = &start.item;
        let mut out = Vec::new();
        for path in item.paths() {
            let text = path_text(path);
            let ns_path = NSString::from_str(&text);
            let url = NSURL::fileURLWithPath(&ns_path);
            let Some(url) = url.absoluteString() else { continue };
            let p = NSPasteboardItem::new();
            p.setString_forType(&url, NSPasteboardTypeFileURL);
            p.setString_forType(&url, NSPasteboardTypeURL);
            if item.plain_text().is_none() { p.setString_forType(&ns_path, NSPasteboardTypeString); }
            out.push(p);
        }
        if item.png_data().is_some() || item.plain_text().is_some() {
            let p = match out.first() { Some(p) => p.clone(), None => { out.push(NSPasteboardItem::new()); out[0].clone() } };
            if let Some(png) = item.png_data() { p.setData_forType(&NSData::with_bytes(png), NSPasteboardTypePNG); }
            if let Some(text) = item.plain_text() { p.setString_forType(&NSString::from_str(text), NSPasteboardTypeString); }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations_map_to_outcomes() {
        assert_eq!(outcome(NSDragOperation::Copy), DragOutcome::Copied);
        assert_eq!(outcome(NSDragOperation::Move), DragOutcome::Moved);
        assert_eq!(outcome(NSDragOperation::Delete), DragOutcome::Moved);
        assert_eq!(outcome(NSDragOperation::Generic), DragOutcome::Moved);
        assert_eq!(outcome(NSDragOperation::Link), DragOutcome::Linked);
        assert_eq!(outcome(NSDragOperation::None), DragOutcome::Cancelled);
    }
}

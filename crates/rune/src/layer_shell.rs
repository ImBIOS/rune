//! A Wayland layer-shell surface for overlays: `zwlr_layer_shell_v1` on winit's own display
//! connection, in a private event queue. The surface sits on the overlay layer, anchored to the
//! top or bottom edge, takes no keyboard focus, reserves no space, and has an empty input region
//! so every click falls through. wgpu draws into it through the raw `wl_surface`.

use std::ptr::NonNull;

use winit::raw_window_handle::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};
use wayland_backend::client::Backend;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_surface::{self, WlSurface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1};

use crate::el::Align;
use crate::overlay::{Placement, ScreenEdge};

#[derive(Default)]
struct State {
    /// The size the compositor asked for in its latest configure (0 means ours).
    configured: Option<(u32, u32)>,
    scale: Option<i32>,
    closed: bool,
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<WlCompositor, ()> for State {
    fn event(_: &mut Self, _: &WlCompositor, _: <WlCompositor as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<WlRegion, ()> for State {
    fn event(_: &mut Self, _: &WlRegion, _: <WlRegion as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<ZwlrLayerShellV1, ()> for State {
    fn event(_: &mut Self, _: &ZwlrLayerShellV1, _: <ZwlrLayerShellV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<WlSurface, ()> for State {
    fn event(state: &mut Self, _: &WlSurface, event: wl_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_surface::Event::PreferredBufferScale { factor } = event {
            state.scale = Some(factor.max(1));
        }
    }
}
impl Dispatch<ZwlrLayerSurfaceV1, ()> for State {
    fn event(state: &mut Self, layer: &ZwlrLayerSurfaceV1, event: zwlr_layer_surface_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer.ack_configure(serial);
                state.configured = Some((width, height));
            }
            zwlr_layer_surface_v1::Event::Closed => state.closed = true,
            _ => {}
        }
    }
}

/// What the overlay needs to know after the compositor talked to the surface.
pub(crate) struct Changes {
    pub scale: Option<i32>,
    pub closed: bool,
}

pub(crate) struct LayerSurface {
    queue: EventQueue<State>,
    state: State,
    layer: ZwlrLayerSurfaceV1,
    surface: WlSurface,
    display: NonNull<std::ffi::c_void>,
}

impl LayerSurface {
    /// None when the display isn't Wayland or the compositor lacks layer-shell.
    pub fn new(display: RawDisplayHandle, namespace: &str, at: Placement) -> Option<LayerSurface> {
        let RawDisplayHandle::Wayland(display) = display else { return None };
        // SAFETY: winit owns this `wl_display` for the life of the event loop, which outlives
        // every overlay; a foreign backend never closes it.
        let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
        let conn = Connection::from_backend(backend);
        let (globals, queue) = registry_queue_init::<State>(&conn).ok()?;
        let qh = queue.handle();
        let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).ok()?;
        let shell: ZwlrLayerShellV1 = globals.bind(&qh, 1..=4, ()).ok()?;
        let surface = compositor.create_surface(&qh, ());
        let region = compositor.create_region(&qh, ());
        surface.set_input_region(Some(&region));
        region.destroy();
        let layer = shell.get_layer_surface(&surface, None, Layer::Overlay, namespace.to_owned(), &qh, ());
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_exclusive_zone(0);
        let mut this = LayerSurface { queue, state: State::default(), layer, surface, display: display.display };
        this.place(at);
        this.surface.commit();
        while this.state.configured.is_none() && !this.state.closed {
            this.queue.blocking_dispatch(&mut this.state).ok()?;
        }
        (!this.state.closed).then_some(this)
    }

    pub fn place(&mut self, at: Placement) {
        let (margin, inset) = (at.margin.round() as i32, at.inset.round() as i32);
        let (vertical, top, bottom) = match at.edge {
            ScreenEdge::Top => (Anchor::Top, margin, 0),
            ScreenEdge::Bottom => (Anchor::Bottom, 0, margin),
        };
        let (horizontal, left, right) = match at.align {
            Align::Start => (Anchor::Left, inset, 0),
            Align::Center => (Anchor::empty(), 0, 0),
            Align::End => (Anchor::Right, 0, inset),
        };
        self.layer.set_size(at.size.0.ceil() as u32, at.size.1.ceil() as u32);
        self.layer.set_anchor(vertical | horizontal);
        self.layer.set_margin(top, right, bottom, left);
    }

    /// Commit role state (placement) without a new buffer; wgpu's present commits buffers.
    pub fn commit(&mut self) {
        self.surface.commit();
        let _ = self.queue.flush();
    }

    pub fn set_buffer_scale(&mut self, scale: i32) {
        if self.surface.version() >= 3 {
            self.surface.set_buffer_scale(scale);
        }
    }

    /// Handles what the compositor sent since the last call (winit reads the socket).
    pub fn dispatch(&mut self) -> Changes {
        let _ = self.queue.dispatch_pending(&mut self.state);
        let _ = self.queue.flush();
        Changes { scale: self.state.scale.take(), closed: self.state.closed }
    }

    pub fn raw_handles(&self) -> (RawDisplayHandle, RawWindowHandle) {
        let surface = NonNull::new(self.surface.id().as_ptr().cast()).expect("live wl_surface");
        (
            RawDisplayHandle::Wayland(WaylandDisplayHandle::new(self.display)),
            RawWindowHandle::Wayland(WaylandWindowHandle::new(surface)),
        )
    }
}

impl Drop for LayerSurface {
    fn drop(&mut self) {
        self.layer.destroy();
        self.surface.destroy();
        let _ = self.queue.flush();
    }
}

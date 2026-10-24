//! Dragging data out of a window into other apps: an OS drag source.
//!
//! An element marked [`crate::El::drag_out`] starts a system drag session once the pointer
//! travels [`DRAG_THRESHOLD`] points from the press. The session carries a [`DragItem`] (file
//! URLs, PNG data, plain text) and shows a [`DragImage`], by default a snapshot of the element
//! as the window last drew it. [`crate::El::on_drag_end`] reports how the drop ended.
//!
//! macOS runs the session with `NSDraggingSource`. Other platforms don't start one yet: there
//! `drag_out` is ignored and the element keeps its ordinary press and `on_drag` behaviour.
//! Windows (`DoDragDrop`), Wayland and X11 backends slot in behind the same types.
//!
//! Windows start the session from the mouse-dragged event AppKit is delivering. Overlay panels
//! track the pointer by polling, so there may be no such event; the overlay then builds a
//! dragged event at the current mouse location. The window path is tested on a real desktop;
//! the overlay path compiles and runs the same code but has no GUI test yet.

use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
mod macos;

/// Whether this platform's windows start OS drag sessions.
pub(crate) const SUPPORTED: bool = cfg!(target_os = "macos");

/// Pointer travel (points, straight-line) from the press before a drag session starts.
/// AppKit views commonly use 4; GTK's default is 8; Windows' `SM_CXDRAG` is 4 pixels.
pub const DRAG_THRESHOLD: f32 = if cfg!(all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))) { 8.0 } else { 4.0 };

/// Whether a pointer that went down at `from` and is now at `to` has started a drag.
pub(crate) fn past_threshold(from: (f32, f32), to: (f32, f32)) -> bool {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    dx * dx + dy * dy > DRAG_THRESHOLD * DRAG_THRESHOLD
}

/// What a drag carries to another app: any of file URLs, PNG data and plain text, plus the
/// picture shown under the pointer.
///
/// ```
/// # use rune::drag_out::DragItem;
/// let item = DragItem::new().file("/Applications/Example.app");
/// assert_eq!(item.paths().len(), 1);
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DragItem {
    files: Vec<PathBuf>,
    png: Option<Vec<u8>>,
    text: Option<String>,
    image: Option<DragImage>,
    allow_move: bool,
}

impl DragItem {
    pub fn new() -> Self { Self::default() }

    /// Add a file (or folder or app bundle) by path; repeat for several. Finder copies it,
    /// and System Settings' privacy lists accept an app bundle.
    pub fn file(mut self, path: impl Into<PathBuf>) -> Self {
        self.files.push(path.into());
        self
    }

    /// PNG-encoded image data, for targets that take an image rather than a file.
    pub fn png(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.png = Some(bytes.into());
        self
    }

    /// Straight-alpha sRGB RGBA8 pixels, encoded as PNG data. Ignored if the sizes disagree.
    pub fn rgba(self, width: u32, height: u32, pixels: &[u8]) -> Self {
        match encode_png(width, height, pixels) {
            Some(png) => self.png(png),
            None => self,
        }
    }

    /// Plain text. Without it, a file's path is offered as text.
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// The picture under the pointer. Defaults to a snapshot of the element.
    pub fn image(mut self, image: DragImage) -> Self {
        self.image = Some(image);
        self
    }

    /// Let the target move files instead of copying them (Finder moves within one volume).
    /// By default a drag only offers a copy, whatever modifier keys are held.
    pub fn allow_move(mut self) -> Self {
        self.allow_move = true;
        self
    }

    pub fn paths(&self) -> &[PathBuf] { &self.files }
    pub fn png_data(&self) -> Option<&[u8]> { self.png.as_deref() }
    pub fn plain_text(&self) -> Option<&str> { self.text.as_deref() }
    pub fn drag_image(&self) -> Option<&DragImage> { self.image.as_ref() }
    pub fn allows_move(&self) -> bool { self.allow_move }
    /// Carries nothing a target could receive.
    pub fn is_empty(&self) -> bool { self.files.is_empty() && self.png.is_none() && self.text.is_none() }
}

/// The picture a drag shows under the pointer, as PNG data.
#[derive(Clone, Debug, PartialEq)]
pub struct DragImage {
    png: Vec<u8>,
    /// Display size in points; `None` draws one pixel per point at the window's scale.
    size: Option<(f32, f32)>,
}

impl DragImage {
    pub fn png(bytes: impl Into<Vec<u8>>) -> Self { Self { png: bytes.into(), size: None } }

    /// Straight-alpha sRGB RGBA8 pixels. `None` if `pixels` isn't `width * height * 4` bytes.
    pub fn rgba(width: u32, height: u32, pixels: &[u8]) -> Option<Self> {
        encode_png(width, height, pixels).map(Self::png)
    }

    /// Draw it this many points wide and high.
    pub fn size(mut self, width: f32, height: f32) -> Self {
        self.size = Some((width, height));
        self
    }

    pub fn png_data(&self) -> &[u8] { &self.png }
    pub fn points(&self) -> Option<(f32, f32)> { self.size }
}

/// How a drag out of the window ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DragOutcome {
    /// The target copied the data.
    Copied,
    /// The target moved the files (only offered after [`DragItem::allow_move`]), or the drop
    /// asked the source to delete them (the Trash).
    Moved,
    /// The target made a link or alias to the files.
    Linked,
    /// Dropped nowhere that accepted it, or cancelled with Escape.
    Cancelled,
}

/// A drag the runtime asked the host to start, inside the pointer event that crossed the
/// threshold.
pub(crate) struct DragOutStart {
    pub path: String,
    pub item: DragItem,
    /// The element's visible drawn rect in window points: x, y, w, h.
    pub rect: [f32; 4],
    /// Where the pointer went down, in window points.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub press: (f32, f32),
}

pub(crate) type DragEnds = std::rc::Rc<std::cell::RefCell<Vec<(String, DragOutcome)>>>;

pub(crate) fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || rgba.len() != (width as usize) * (height as usize) * 4 { return None; }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut w = enc.write_header().ok()?;
    w.write_image_data(rgba).ok()?;
    w.finish().ok()?;
    Some(out)
}

/// Draws the last frame's instances again offscreen and reads back `rect` (window points), so
/// the drag shows what the user grabbed. `None` for formats it can't read back.
pub(crate) fn snapshot(gpu: &crate::render::Gpu, renderer: &crate::render::Renderer, size: (u32, u32), scale: f32, clear: [f32; 4], rect: [f32; 4]) -> Option<DragImage> {
    let format = renderer.format();
    let bgra = match format {
        wgpu::TextureFormat::Bgra8UnormSrgb | wgpu::TextureFormat::Bgra8Unorm => true,
        wgpu::TextureFormat::Rgba8UnormSrgb | wgpu::TextureFormat::Rgba8Unorm => false,
        _ => return None,
    };
    let px = crop(rect, scale, size)?;
    let [x, y, w, h] = px;
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("drag image"),
        size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    let padded_row = (w * 4).div_ceil(256) * 256;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("drag image readback"),
        size: u64::from(padded_row * h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    renderer.draw(&mut enc, &view, clear);
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d { x, y, z: 0 }, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded_row), rows_per_image: Some(h) } },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    gpu.queue.submit([enc.finish()]);
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    {
        let data = slice.get_mapped_range().ok()?;
        for row in 0..h {
            let start = (row * padded_row) as usize;
            rgba.extend_from_slice(&data[start..start + (w * 4) as usize]);
        }
    }
    buffer.unmap();
    to_straight_rgba(&mut rgba, bgra);
    DragImage::rgba(w, h, &rgba).map(|i| i.size(w as f32 / scale, h as f32 / scale))
}

/// `rect` (points) in device pixels, clamped to the frame: x, y, w, h. `None` when empty.
fn crop(rect: [f32; 4], scale: f32, size: (u32, u32)) -> Option<[u32; 4]> {
    let x0 = (rect[0] * scale).floor().clamp(0.0, size.0 as f32) as u32;
    let y0 = (rect[1] * scale).floor().clamp(0.0, size.1 as f32) as u32;
    let x1 = ((rect[0] + rect[2]) * scale).ceil().clamp(0.0, size.0 as f32) as u32;
    let y1 = ((rect[1] + rect[3]) * scale).ceil().clamp(0.0, size.1 as f32) as u32;
    (x1 > x0 && y1 > y0).then_some([x0, y0, x1 - x0, y1 - y0])
}

/// Frame pixels are premultiplied, in the surface's channel order.
fn to_straight_rgba(pixels: &mut [u8], bgra: bool) {
    for p in pixels.chunks_exact_mut(4) {
        if bgra { p.swap(0, 2); }
        let a = p[3];
        if a != 0 && a != 255 {
            for c in &mut p[..3] { *c = ((*c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8; }
        }
    }
}

/// Starts the OS drag session for `start` from inside the pointer event that crossed the
/// threshold. `done` runs once with how it ended (also when it couldn't start: `Cancelled`).
#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
pub(crate) fn begin(window: &winit::window::Window, start: DragOutStart, snapshot: Option<DragImage>, done: Box<dyn FnOnce(DragOutcome)>) {
    #[cfg(target_os = "macos")]
    { macos::begin(window, start, snapshot, done); }
    #[cfg(not(target_os = "macos"))]
    done(DragOutcome::Cancelled);
}

/// The path offered as text when a file drag has no text of its own.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn path_text(path: &Path) -> String { path.to_string_lossy().into_owned() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_is_straight_line_travel_beyond_the_platform_distance() {
        assert!(!past_threshold((10.0, 10.0), (10.0, 10.0)));
        assert!(!past_threshold((10.0, 10.0), (10.0 + DRAG_THRESHOLD, 10.0)), "exactly the threshold doesn't start");
        assert!(past_threshold((10.0, 10.0), (10.0 + DRAG_THRESHOLD + 0.1, 10.0)));
        let d = DRAG_THRESHOLD * 0.75;
        assert!(past_threshold((0.0, 0.0), (d, d)), "diagonal travel counts its length");
    }

    #[test]
    fn a_drag_item_collects_files_image_and_text() {
        let item = DragItem::new().file("/a.png").file("/b.txt").text("hello").png(vec![1, 2, 3]);
        assert_eq!(item.paths(), [PathBuf::from("/a.png"), PathBuf::from("/b.txt")]);
        assert_eq!(item.plain_text(), Some("hello"));
        assert_eq!(item.png_data(), Some(&[1u8, 2, 3][..]));
        assert!(!item.allows_move() && item.drag_image().is_none() && !item.is_empty());
        assert!(DragItem::new().is_empty());
        assert!(DragItem::new().allow_move().allows_move());
    }

    #[test]
    fn rgba_encodes_png_and_rejects_a_wrong_length() {
        let item = DragItem::new().rgba(1, 1, &[255, 0, 0, 255]);
        let png = item.png_data().unwrap();
        assert_eq!(&png[1..4], b"PNG");
        assert!(DragItem::new().rgba(2, 2, &[0; 4]).png_data().is_none());
        assert!(DragImage::rgba(0, 0, &[]).is_none());
        assert_eq!(DragImage::rgba(1, 1, &[0; 4]).unwrap().size(8.0, 8.0).points(), Some((8.0, 8.0)));
    }

    #[test]
    fn snapshot_crop_clamps_to_the_frame_in_device_pixels() {
        assert_eq!(crop([10.0, 5.0, 20.0, 10.0], 2.0, (400, 300)), Some([20, 10, 40, 20]));
        assert_eq!(crop([-5.0, -5.0, 10.0, 10.0], 2.0, (400, 300)), Some([0, 0, 10, 10]));
        assert_eq!(crop([190.0, 0.0, 50.0, 10.0], 2.0, (400, 300)), Some([380, 0, 20, 20]));
        assert_eq!(crop([500.0, 0.0, 10.0, 10.0], 2.0, (400, 300)), None);
    }

    #[test]
    fn readback_unpremultiplies_and_swaps_bgra() {
        let mut p = [0, 0, 128, 128, 10, 20, 30, 255];
        to_straight_rgba(&mut p, true);
        assert_eq!(p, [255, 0, 0, 128, 30, 20, 10, 255]);
    }
}

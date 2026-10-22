//! Pixel buffers an app replaces at will (a CPU-shaded HUD frame, a video frame), drawn by
//! `El::pixels`. Each runtime keeps a CPU-side RGBA atlas the renderer uploads by dirty rows,
//! like the glyph atlas, allocated only once an element shows pixels.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub const IMAGE_ATLAS_SIZE: u32 = 1024;

struct Frame {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    generation: u64,
}

/// A replaceable RGBA image, shared between the producer and the view. Clone it freely.
#[derive(Clone)]
pub struct Pixels {
    id: u64,
    frame: Arc<Mutex<Frame>>,
}

impl std::fmt::Debug for Pixels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let frame = self.frame.lock().unwrap();
        write!(f, "Pixels({}, {}x{}, gen {})", self.id, frame.width, frame.height, frame.generation)
    }
}

impl Default for Pixels {
    fn default() -> Self {
        Self::new()
    }
}

impl Pixels {
    pub fn new() -> Pixels {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Pixels {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            frame: Arc::new(Mutex::new(Frame { width: 0, height: 0, rgba: Vec::new(), generation: 0 })),
        }
    }

    /// Replaces the image: `rgba` is `width * height` straight-alpha sRGB pixels, rows top
    /// first. The next frame of any window showing it uploads the change; wake that window
    /// (an `update`, a `Live` value) when nothing else will. Larger than the atlas: ignored.
    pub fn set(&self, width: u32, height: u32, rgba: Vec<u8>) {
        assert_eq!(rgba.len(), (width * height * 4) as usize, "rgba must hold width * height pixels");
        let mut frame = self.frame.lock().unwrap();
        frame.width = width;
        frame.height = height;
        frame.rgba = rgba;
        frame.generation += 1;
    }

    pub fn size(&self) -> (u32, u32) {
        let frame = self.frame.lock().unwrap();
        (frame.width, frame.height)
    }
}

struct Slot {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    generation: u64,
    used: bool,
}

/// Where each shown `Pixels` lives in the RGBA atlas.
#[derive(Default)]
pub struct ImageAtlas {
    /// Empty until the first image; then `IMAGE_ATLAS_SIZE²` RGBA bytes.
    pub data: Vec<u8>,
    /// Rows `[y0, y1)` changed since the last upload.
    pub dirty: Option<(u32, u32)>,
    slots: HashMap<u64, Slot>,
    shelf_x: u32,
    shelf_y: u32,
    shelf_h: u32,
}

impl ImageAtlas {
    /// The atlas origin and size of `pixels`, copying a new frame in when it changed. None while
    /// it's empty or doesn't fit.
    pub fn place(&mut self, pixels: &Pixels) -> Option<[u32; 4]> {
        let frame = pixels.frame.lock().unwrap();
        let (w, h) = (frame.width, frame.height);
        if w == 0 || h == 0 || w > IMAGE_ATLAS_SIZE || h > IMAGE_ATLAS_SIZE {
            return None;
        }
        if self.data.is_empty() {
            self.data = vec![0; (IMAGE_ATLAS_SIZE * IMAGE_ATLAS_SIZE * 4) as usize];
        }
        if self.slots.get(&pixels.id).is_some_and(|s| s.w != w || s.h != h) {
            self.slots.remove(&pixels.id);
        }
        if !self.slots.contains_key(&pixels.id) {
            let (x, y) = self.allocate(w, h)?;
            self.slots.insert(pixels.id, Slot { x, y, w, h, generation: 0, used: true });
        }
        let slot = self.slots.get_mut(&pixels.id).unwrap();
        slot.used = true;
        if slot.generation != frame.generation {
            slot.generation = frame.generation;
            let row = (IMAGE_ATLAS_SIZE * 4) as usize;
            for line in 0..h as usize {
                let dst = (slot.y as usize + line) * row + slot.x as usize * 4;
                let src = line * w as usize * 4;
                self.data[dst..dst + w as usize * 4].copy_from_slice(&frame.rgba[src..src + w as usize * 4]);
            }
            let (y0, y1) = (slot.y, slot.y + h);
            self.dirty = Some(self.dirty.map_or((y0, y1), |(a, b)| (a.min(y0), b.max(y1))));
        }
        Some([slot.x, slot.y, w, h])
    }

    /// Forgets images no element showed since the last call; when nothing is left, the shelves
    /// start over so replaced sizes don't fill the atlas.
    pub fn end_frame(&mut self) {
        self.slots.retain(|_, s| std::mem::take(&mut s.used));
        if self.slots.is_empty() {
            (self.shelf_x, self.shelf_y, self.shelf_h) = (0, 0, 0);
        }
    }

    /// Marks every row for upload (a new texture).
    pub fn invalidate(&mut self) {
        if !self.data.is_empty() {
            self.dirty = Some((0, IMAGE_ATLAS_SIZE));
        }
    }

    fn allocate(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if self.shelf_x + w > IMAGE_ATLAS_SIZE {
            self.shelf_y += self.shelf_h;
            self.shelf_x = 0;
            self.shelf_h = 0;
        }
        if self.shelf_y + h > IMAGE_ATLAS_SIZE {
            return None;
        }
        let at = (self.shelf_x, self.shelf_y);
        self.shelf_x += w;
        self.shelf_h = self.shelf_h.max(h);
        Some(at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_draws_pixels_texel_for_texel_and_follows_new_frames() {
        use crate::el::{El, el};
        use crate::render::{Gpu, OFFSCREEN_FORMAT, Offscreen, Renderer};
        use crate::runtime::{Cx, Env, Runtime};
        fn view(px: &Pixels, _: &mut Cx<Pixels>) -> El {
            el().size(4.0, 4.0).pixels(px)
        }
        let px = Pixels::new();
        let mut rgba = Vec::new();
        for i in 0..16u8 {
            rgba.extend_from_slice(&[i * 16, 255 - i * 16, 40, 255]);
        }
        px.set(4, 4, rgba.clone());
        let mut rt = Runtime::new(px.clone(), view, (4.0, 4.0), Env { system_dark: true, scale: 1.0 });
        let gpu = Gpu::new(Gpu::instance(), None);
        let mut renderer = Renderer::new(&gpu.device, OFFSCREEN_FORMAT);
        let target = Offscreen::new(&gpu.device, 4, 4);
        let mut draw = |rt: &mut Runtime<Pixels>| {
            rt.frame(1.0 / 60.0);
            renderer.prepare(&gpu.device, &gpu.queue, &mut rt.text.atlas, &rt.instances, (4, 4));
            renderer.upload_images(&gpu.device, &gpu.queue, &mut rt.images);
            let mut enc = gpu.device.create_command_encoder(&Default::default());
            renderer.draw(&mut enc, &target.view, [0.0, 0.0, 0.0, 1.0]);
            target.copy(&mut enc);
            gpu.queue.submit([enc.finish()]);
            target.read(&gpu.device)
        };
        let out = draw(&mut rt);
        for (got, want) in out.chunks_exact(4).zip(rgba.chunks_exact(4)) {
            assert!(got.iter().zip(want).all(|(a, b)| a.abs_diff(*b) <= 1), "drew {got:?}, image {want:?}");
        }
        px.set(4, 4, [10, 20, 30, 255].repeat(16));
        let out = draw(&mut rt);
        assert!(out.chunks_exact(4).all(|p| p.iter().zip([10u8, 20, 30, 255]).all(|(a, b)| a.abs_diff(b) <= 1)), "a new frame shows without re-running the view: {:?}", &out[..4]);
    }

    #[test]
    fn frames_copy_in_once_per_generation_and_resize_moves_the_slot() {
        let mut atlas = ImageAtlas::default();
        let px = Pixels::new();
        assert_eq!(atlas.place(&px), None, "empty images draw nothing");
        px.set(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 128]);
        assert_eq!(atlas.place(&px), Some([0, 0, 2, 1]));
        assert_eq!(atlas.dirty.take(), Some((0, 1)));
        assert_eq!(&atlas.data[..8], &[255, 0, 0, 255, 0, 255, 0, 128]);
        atlas.place(&px);
        assert_eq!(atlas.dirty, None, "an unchanged frame uploads nothing");
        px.set(3, 2, vec![7; 24]);
        let [x, y, w, h] = atlas.place(&px).unwrap();
        assert_eq!((w, h), (3, 2));
        assert_eq!(atlas.data[((y * IMAGE_ATLAS_SIZE + x) * 4) as usize], 7);
        let other = Pixels::new();
        other.set(1, 1, vec![1; 4]);
        atlas.place(&other);
        atlas.end_frame();
        atlas.place(&px);
        atlas.end_frame();
        assert!(atlas.slots.contains_key(&px.id) && !atlas.slots.contains_key(&other.id), "unshown images are forgotten");
    }
}

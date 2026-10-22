//! Content-addressed raster images. Decode and linear-light mip generation run on one
//! sleeping worker per cache, never in a view or a render frame.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};

pub type ImageId = [u8; 32];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageError(pub String);

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}
impl std::error::Error for ImageError {}

/// Pixels are sRGB-encoded *linear-premultiplied* RGB plus linear alpha. Sampling with
/// Rgba8UnormSrgb yields linear-premultiplied RGBA, including between mip levels.
pub(crate) struct Mip {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}
pub(crate) struct Decoded {
    pub mips: Vec<Mip>,
}
struct State {
    id: ImageId,
    pixels: crate::Pixels,
    decoded: OnceLock<Result<Arc<Decoded>, ImageError>>,
}

/// A cheap shared handle. Pending images paint nothing until their worker wakes the app.
#[derive(Clone)]
pub struct Image(Arc<State>);
impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Image").field("id", &self.id()).field("size", &self.size()).finish()
    }
}
impl Image {
    /// The shared static pixel handle, ready to pass to `El::pixels` after decoding.
    pub fn pixels(&self) -> &crate::Pixels { &self.0.pixels }
    pub fn id(&self) -> ImageId { self.0.id }
    pub fn size(&self) -> Option<[u32; 2]> {
        let mip = self.decoded()?.ok()?.mips.first()?;
        Some([mip.width, mip.height])
    }
    pub fn is_ready(&self) -> bool { self.0.decoded.get().is_some() }
    pub fn error(&self) -> Option<&ImageError> { self.0.decoded.get()?.as_ref().err() }
    /// Wait for fixture assets before advancing a deterministic headless scene. Interactive
    /// apps use the cache's waker instead; decode still happens on the cache worker.
    pub fn wait(&self) -> Result<[u32; 2], &ImageError> {
        let mip = &self.0.decoded.wait().as_ref()?.mips[0];
        Ok([mip.width, mip.height])
    }
    pub(crate) fn decoded(&self) -> Option<Result<&Decoded, &ImageError>> {
        self.0.decoded.get().map(|r| r.as_ref().map(Arc::as_ref))
    }
}

enum Input { Encoded(Arc<[u8]>), Rgba(u32, u32, Arc<[u8]>) }
struct Job { image: Image, input: Input }

/// Construct once during app setup. The cache deduplicates by content, not a path or pointer;
/// dropping all handles releases decoded pixels. Dropping the cache stops its worker after
/// queued requests complete. The waker should be the one supplied by `AppSpec::setup`.
pub struct ImageCache {
    entries: Mutex<HashMap<ImageId, Weak<State>>>,
    jobs: mpsc::Sender<Job>,
}
impl ImageCache {
    pub fn new(waker: crate::Waker) -> Self {
        let (jobs, receive) = mpsc::channel::<Job>();
        std::thread::Builder::new().name("rune-images".into()).spawn(move || {
            for job in receive {
                let decoded = match job.input {
                    Input::Encoded(bytes) => decode(&bytes),
                    Input::Rgba(w, h, bytes) => mip_chain(w, h, &bytes),
                };
                let decoded = decoded.map(Arc::new);
                if let Ok(decoded) = &decoded { job.image.0.pixels.set_mips(decoded.clone()); }
                let _ = job.image.0.decoded.set(decoded);
                waker();
            }
        }).expect("image worker");
        Self { entries: Mutex::new(HashMap::new()), jobs }
    }

    /// PNG, JPEG, or modern ICNS containing a PNG representation. Legacy ICNS codecs
    /// return an explicit error; callers can provide platform-decoded RGBA instead.
    pub fn load(&self, bytes: impl Into<Arc<[u8]>>) -> Image {
        let bytes = bytes.into();
        let mut hash = blake3::Hasher::new();
        hash.update(b"rune-encoded\0");
        hash.update(&bytes);
        let id = *hash.finalize().as_bytes();
        self.request(id, Input::Encoded(bytes))
    }

    /// Straight-alpha sRGB RGBA8. Validation, premultiplication and mips happen off-thread.
    pub fn rgba(&self, width: u32, height: u32, bytes: impl Into<Arc<[u8]>>) -> Image {
        let bytes = bytes.into();
        let mut hash = blake3::Hasher::new();
        hash.update(b"rune-rgba8\0");
        hash.update(&width.to_le_bytes());
        hash.update(&height.to_le_bytes());
        hash.update(&bytes);
        self.request(*hash.finalize().as_bytes(), Input::Rgba(width, height, bytes))
    }

    fn request(&self, id: ImageId, input: Input) -> Image {
        let mut entries = self.entries.lock().unwrap();
        if let Some(state) = entries.get(&id).and_then(Weak::upgrade) { return Image(state); }
        entries.retain(|_, value| value.strong_count() > 0);
        let image = Image(Arc::new(State { id, pixels: crate::Pixels::new(), decoded: OnceLock::new() }));
        entries.insert(id, Arc::downgrade(&image.0));
        if self.jobs.send(Job { image: image.clone(), input }).is_err() {
            let _ = image.0.decoded.set(Err(ImageError("image worker stopped".into())));
        }
        image
    }
}

fn decode(bytes: &[u8]) -> Result<Decoded, ImageError> {
    let bytes = if bytes.starts_with(b"icns") { icns_png(bytes)? } else { bytes };
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format().map_err(|e| ImageError(e.to_string()))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|e| ImageError(e.to_string()))?.into_rgba8();
    mip_chain(image.width(), image.height(), image.as_raw())
}

fn icns_png(bytes: &[u8]) -> Result<&[u8], ImageError> {
    let invalid = || ImageError("invalid ICNS container or no supported PNG representation".into());
    let len = u32::from_be_bytes(bytes.get(4..8).ok_or_else(invalid)?.try_into().unwrap()) as usize;
    if len != bytes.len() { return Err(invalid()); }
    let mut pos = 8;
    let mut best = None;
    let mut area = 0u64;
    while pos < len {
        let header = bytes.get(pos..pos + 8).ok_or_else(invalid)?;
        let size = u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize;
        if size < 8 { return Err(invalid()); }
        let end = pos.checked_add(size).filter(|end| *end <= len).ok_or_else(invalid)?;
        let chunk = &bytes[pos + 8..end];
        if chunk.starts_with(b"\x89PNG\r\n\x1a\n") && chunk.len() >= 24 {
            let w = u32::from_be_bytes(chunk[16..20].try_into().unwrap());
            let h = u32::from_be_bytes(chunk[20..24].try_into().unwrap());
            let candidate = u64::from(w) * u64::from(h);
            if w <= 8192 && h <= 8192 && candidate > area { best = Some(chunk); area = candidate; }
        }
        pos = end;
    }
    best.ok_or_else(invalid)
}

fn mip_chain(width: u32, height: u32, bytes: &[u8]) -> Result<Decoded, ImageError> {
    if width == 0 || height == 0 || width > 8192 || height > 8192
        || bytes.len() != width as usize * height as usize * 4
    { return Err(ImageError("RGBA image dimensions or byte length are invalid".into())); }
    let mut linear: Vec<[f32; 4]> = bytes.as_chunks::<4>().0.iter().map(|p| {
        let a = p[3] as f32 / 255.0;
        [crate::color::srgb_decode(p[0] as f32 / 255.0) * a,
         crate::color::srgb_decode(p[1] as f32 / 255.0) * a,
         crate::color::srgb_decode(p[2] as f32 / 255.0) * a, a]
    }).collect();
    let (mut w, mut h) = (width, height);
    let mut mips = Vec::new();
    loop {
        let pixels = linear.iter().flat_map(|p| {
            let a = byte(p[3]);
            // Quantized alpha bounds premultiplied RGB, including fully transparent tails.
            let rgb = [p[0], p[1], p[2]].map(|v| byte(crate::color::srgb_encode(v.min(a as f32 / 255.0))));
            [rgb[0], rgb[1], rgb[2], a]
        }).collect();
        mips.push(Mip { width: w, height: h, pixels });
        if w == 1 && h == 1 { break; }
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        // Area resampling includes the last row/column of odd-sized sources. Work in
        // linear premultiplied floats throughout; quantization happens only on upload data.
        let mut next = Vec::with_capacity((nw * nh) as usize);
        for y in 0..nh { for x in 0..nw {
            let (x0, x1) = (x as f32 * w as f32 / nw as f32, (x + 1) as f32 * w as f32 / nw as f32);
            let (y0, y1) = (y as f32 * h as f32 / nh as f32, (y + 1) as f32 * h as f32 / nh as f32);
            let mut sum = [0.0; 4];
            for sy in y0.floor() as u32..(y1.ceil() as u32).min(h) {
                for sx in x0.floor() as u32..(x1.ceil() as u32).min(w) {
                    let weight = ((sx + 1) as f32).min(x1) - (sx as f32).max(x0);
                    let weight = weight * (((sy + 1) as f32).min(y1) - (sy as f32).max(y0));
                    for c in 0..4 { sum[c] += linear[(sy * w + sx) as usize][c] * weight; }
                }
            }
            next.push(sum.map(|v| v / ((x1 - x0) * (y1 - y0))));
        }}
        linear = next;
        (w, h) = (nw, nh);
    }
    Ok(Decoded { mips })
}
fn byte(v: f32) -> u8 { (v.clamp(0.0, 1.0) * 255.0).round() as u8 }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mips_filter_linear_premultiplied_pixels_including_odd_edges() {
        let d = mip_chain(3, 1, &[255, 0, 0, 255, 0, 255, 0, 0, 255, 0, 0, 255]).unwrap();
        assert_eq!(d.mips.len(), 2);
        assert_eq!(&d.mips[0].pixels[4..8], &[0, 0, 0, 0]);
        assert_eq!(d.mips[1].pixels[3], 170);
        assert_eq!(d.mips[1].pixels[1], 0); // no transparent-green fringe
        assert!((d.mips[1].pixels[0] as i32 - 213).abs() <= 1);
        let gray = mip_chain(2, 1, &[0, 0, 0, 255, 255, 255, 255, 255]).unwrap();
        assert_eq!(gray.mips[1].pixels, [188, 188, 188, 255]);
        let tail = mip_chain(3, 1, &[255, 255, 255, 1, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(tail.mips[1].pixels, [0, 0, 0, 0], "alpha rounded to zero cannot emit light");
    }

    #[test]
    fn cache_deduplicates_and_wakes_after_async_completion() {
        let (tx, rx) = mpsc::channel();
        let cache = ImageCache::new(Arc::new(move || { tx.send(()).unwrap(); }));
        let first = cache.rgba(1, 1, vec![20, 40, 60, 255]);
        let second = cache.rgba(1, 1, vec![20, 40, 60, 255]);
        assert!(Arc::ptr_eq(&first.0, &second.0));
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(first.size(), Some([1, 1]));
        assert_eq!(first.decoded().unwrap().unwrap().mips[0].pixels, [20, 40, 60, 255]);
        let invalid = cache.load(vec![0, 1, 2]);
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(invalid.error().is_some());
    }

    #[test]
    fn png_jpeg_and_icns_decode_and_malformed_containers_fail() {
        let source = image::RgbImage::from_pixel(8, 4, image::Rgb([20, 40, 60]));
        for format in [image::ImageFormat::Png, image::ImageFormat::Jpeg] {
            let mut encoded = std::io::Cursor::new(Vec::new());
            source.write_to(&mut encoded, format).unwrap();
            let bytes = encoded.into_inner();
            let d = decode(&bytes).unwrap();
            assert_eq!((d.mips[0].width, d.mips[0].height), (8, 4));
            assert_eq!((d.mips.last().unwrap().width, d.mips.last().unwrap().height), (1, 1));
            if format == image::ImageFormat::Png {
                let mut icns = b"icns".to_vec();
                icns.extend_from_slice(&((bytes.len() + 16) as u32).to_be_bytes());
                icns.extend_from_slice(b"icp4");
                icns.extend_from_slice(&((bytes.len() + 8) as u32).to_be_bytes());
                icns.extend_from_slice(&bytes);
                assert_eq!(decode(&icns).unwrap().mips[0].pixels, d.mips[0].pixels);
                icns[12..16].copy_from_slice(&0u32.to_be_bytes());
                assert!(decode(&icns).is_err());
            }
        }
        assert!(mip_chain(0, 1, &[]).is_err());
        assert!(mip_chain(1, 1, &[0]).is_err());
    }
}

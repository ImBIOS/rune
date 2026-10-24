//! Headless frame output: a ring of staging buffers so the GPU renders the next frame while the
//! last one is read back, and writer threads that stream frames to ffmpeg or PNG files.

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use crate::render::Offscreen;

/// Readbacks in flight: rendering frame n + 1 and n + 2 overlaps mapping frame n.
pub(crate) const DEPTH: usize = 3;

struct Slot {
    buffer: wgpu::Buffer,
    ready: Arc<AtomicBool>,
}

/// Staging buffers for an offscreen target, used in order. `T` tags each frame.
pub(crate) struct Readback<T> {
    slots: Vec<Slot>,
    next: usize,
    /// Copied frames, oldest first: slot, tag, and whether `map_async` was called.
    pending: VecDeque<(usize, T, bool)>,
    width: u32,
    height: u32,
    padded_row: u32,
}

impl<T> Readback<T> {
    pub(crate) fn new(device: &wgpu::Device, target: &Offscreen, depth: usize) -> Self {
        let padded_row = target.padded_row();
        let slots = (0..depth.max(1)).map(|_| Slot {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("readback ring"),
                size: u64::from(padded_row) * u64::from(target.height),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
            ready: Arc::new(AtomicBool::new(false)),
        }).collect();
        Readback { slots, next: 0, pending: VecDeque::new(), width: target.width, height: target.height, padded_row }
    }

    /// Frames that must finish before the next `copy` has a free buffer (waits for the oldest
    /// when every buffer is in flight).
    pub(crate) fn make_room(&mut self, device: &wgpu::Device) -> Vec<(T, Vec<u8>)> {
        let mut done = Vec::new();
        if self.pending.len() >= self.slots.len() {
            done.extend(self.pop(device, true));
        }
        done
    }

    /// Records a copy of `target` into the next free buffer. Call `submitted` after the encoder
    /// is submitted.
    pub(crate) fn copy(&mut self, encoder: &mut wgpu::CommandEncoder, target: &Offscreen, tag: T) {
        assert!(self.pending.len() < self.slots.len(), "Readback::make_room first");
        let slot = self.next;
        self.next = (self.next + 1) % self.slots.len();
        target.copy_to(encoder, &self.slots[slot].buffer);
        self.pending.push_back((slot, tag, false));
    }

    /// Starts mapping every copied buffer whose commands were submitted.
    pub(crate) fn submitted(&mut self) {
        for (slot, _, mapping) in self.pending.iter_mut().filter(|p| !p.2) {
            let ready = self.slots[*slot].ready.clone();
            ready.store(false, Ordering::Release);
            self.slots[*slot].buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                r.expect("map readback");
                ready.store(true, Ordering::Release);
            });
            *mapping = true;
        }
    }

    /// Finished frames in submission order, without waiting.
    pub(crate) fn ready(&mut self, device: &wgpu::Device) -> Vec<(T, Vec<u8>)> {
        let _ = device.poll(wgpu::PollType::Poll);
        let mut done = Vec::new();
        while let Some(frame) = self.pop(device, false) { done.push(frame); }
        done
    }

    /// Waits for every frame in flight.
    pub(crate) fn finish(&mut self, device: &wgpu::Device) -> Vec<(T, Vec<u8>)> {
        let mut done = Vec::new();
        while let Some(frame) = self.pop(device, true) { done.push(frame); }
        done
    }

    fn pop(&mut self, device: &wgpu::Device, wait: bool) -> Option<(T, Vec<u8>)> {
        let &(slot, _, mapping) = self.pending.front()?;
        assert!(mapping, "Readback::submitted after submitting the copy");
        let ready = &self.slots[slot].ready;
        if !ready.load(Ordering::Acquire) {
            if !wait { return None; }
            while !ready.load(Ordering::Acquire) {
                device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
            }
        }
        let (slot, tag, _) = self.pending.pop_front().unwrap();
        let buffer = &self.slots[slot].buffer;
        let pixels = {
            let data = buffer.slice(..).get_mapped_range().expect("mapped range");
            unpad(&data, self.width, self.height, self.padded_row)
        };
        buffer.unmap();
        Some((tag, pixels))
    }
}

/// Rows of `width` RGBA pixels from a buffer whose rows are `padded_row` bytes apart.
pub(crate) fn unpad(data: &[u8], width: u32, height: u32, padded_row: u32) -> Vec<u8> {
    let row = (width * 4) as usize;
    let mut out = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        let start = y * padded_row as usize;
        out.extend_from_slice(&data[start..start + row]);
    }
    out
}

// -------------------------------------------------------------------------------------------
// Video

/// The container and codec `--video` picks from the file extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Container {
    /// H.264, yuv420p, `+faststart`: plays everywhere.
    Mp4,
    /// ProRes 4444 with alpha, for editing.
    Mov,
    /// VP9, yuv420p.
    Webm,
}

impl Container {
    pub(crate) fn of(path: &Path) -> Result<Container, String> {
        match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
            Some("mp4") => Ok(Container::Mp4),
            Some("mov") => Ok(Container::Mov),
            Some("webm") => Ok(Container::Webm),
            _ => Err(format!("--video {}: use a .mp4, .mov or .webm file", path.display())),
        }
    }

    fn codec(self) -> &'static [&'static str] {
        // yuv420p needs even sizes; pad odd ones by a pixel. Rune renders sRGB; tag it BT.709
        // like screen recorders do, and convert with the matching matrix.
        const EVEN: &str = "pad=ceil(iw/2)*2:ceil(ih/2)*2,scale=out_color_matrix=bt709:out_range=tv,format=yuv420p";
        const TAGS: [&str; 6] = ["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"];
        match self {
            Container::Mp4 => &["-c:v", "libx264", "-preset", "medium", "-crf", "18", "-vf", EVEN,
                TAGS[0], TAGS[1], TAGS[2], TAGS[3], TAGS[4], TAGS[5], "-movflags", "+faststart"],
            Container::Mov => &["-c:v", "prores_ks", "-profile:v", "4444", "-pix_fmt", "yuva444p10le",
                "-vendor", "apl0", TAGS[0], TAGS[1], TAGS[2], TAGS[3], TAGS[4], TAGS[5]],
            Container::Webm => &["-c:v", "libvpx-vp9", "-crf", "30", "-b:v", "0", "-row-mt", "1", "-deadline", "good",
                "-cpu-used", "4", "-vf", EVEN, TAGS[0], TAGS[1], TAGS[2], TAGS[3], TAGS[4], TAGS[5]],
        }
    }
}

/// The ffmpeg arguments for raw RGBA frames on stdin.
pub(crate) fn ffmpeg_args(path: &Path, size: (u32, u32), fps: f64) -> Result<Vec<String>, String> {
    let container = Container::of(path)?;
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-f", "rawvideo", "-pix_fmt", "rgba",
        "-s", &format!("{}x{}", size.0, size.1), "-r", &fps.to_string(), "-i", "-"].map(String::from).into();
    args.extend(container.codec().iter().map(|s| (*s).to_owned()));
    args.push(path.display().to_string());
    Ok(args)
}

/// Where finished frames go, off the render thread: ffmpeg's stdin, PNG files, or both.
pub(crate) struct Writer {
    video: Option<(mpsc::SyncSender<Vec<u8>>, std::thread::JoinHandle<Result<(), String>>, PathBuf)>,
    pngs: Option<(mpsc::SyncSender<(PathBuf, Vec<u8>)>, Vec<std::thread::JoinHandle<Result<(), String>>>)>,
    size: (u32, u32),
}

impl Writer {
    pub(crate) fn new(size: (u32, u32)) -> Writer {
        Writer { video: None, pngs: None, size }
    }

    /// Starts ffmpeg; fails when it isn't installed or the extension is unknown.
    pub(crate) fn video(&mut self, path: &Path, fps: f64) -> Result<(), String> {
        let args = ffmpeg_args(path, self.size, fps)?;
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let mut child = std::process::Command::new("ffmpeg").args(&args)
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null())
            .spawn().map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => "--video needs ffmpeg on PATH (macOS: brew install ffmpeg; Debian/Ubuntu: apt install ffmpeg)".to_owned(),
                _ => format!("ffmpeg: {e}"),
            })?;
        let mut stdin = child.stdin.take().unwrap();
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(DEPTH);
        let out = path.to_owned();
        let thread = std::thread::Builder::new().name("rune-video".into()).spawn(move || {
            let mut failed = None;
            for frame in rx {
                if failed.is_none() && let Err(e) = stdin.write_all(&frame) { failed = Some(e); }
            }
            drop(stdin);
            let status = child.wait().map_err(|e| format!("ffmpeg: {e}"))?;
            match (status.success(), failed) {
                (true, None) => Ok(()),
                (_, Some(e)) => Err(format!("ffmpeg stopped reading frames ({e}); see its message above")),
                (false, None) => Err(format!("ffmpeg failed writing {} ({status})", out.display())),
            }
        }).map_err(|e| e.to_string())?;
        self.video = Some((tx, thread, path.to_owned()));
        Ok(())
    }

    pub(crate) fn has_video(&self) -> bool { self.video.is_some() }

    pub(crate) fn frame(&mut self, pixels: Vec<u8>) {
        if let Some((tx, ..)) = &self.video { let _ = tx.send(pixels); }
    }

    /// Encodes `pixels` as a PNG at `path` on a worker thread.
    pub(crate) fn png(&mut self, path: PathBuf, pixels: Vec<u8>) {
        let size = self.size;
        let (tx, _) = self.pngs.get_or_insert_with(|| {
            let (tx, rx) = mpsc::sync_channel::<(PathBuf, Vec<u8>)>(DEPTH * 2);
            let rx = Arc::new(Mutex::new(rx));
            let workers = std::thread::available_parallelism().map_or(2, |n| n.get().clamp(1, 4));
            let threads = (0..workers).map(|_| {
                let rx = rx.clone();
                std::thread::spawn(move || {
                    loop {
                        let job = rx.lock().unwrap().recv();
                        let Ok((path, px)) = job else { return Ok(()) };
                        crate::render::write_png(&path, size.0, size.1, &px).map_err(|e| format!("{}: {e}", path.display()))?;
                    }
                })
            }).collect();
            (tx, threads)
        });
        let _ = tx.send((path, pixels));
    }

    /// Waits for every frame to be written; the video path when one was made.
    pub(crate) fn finish(self) -> Result<Option<PathBuf>, String> {
        let mut video = None;
        if let Some((tx, threads)) = self.pngs {
            drop(tx);
            for t in threads { t.join().map_err(|_| "PNG writer panicked".to_owned())??; }
        }
        if let Some((tx, thread, path)) = self.video {
            drop(tx);
            thread.join().map_err(|_| "video writer panicked".to_owned())??;
            video = Some(path);
        }
        Ok(video)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_containers_follow_the_extension() {
        assert_eq!(Container::of(Path::new("a/b.MP4")), Ok(Container::Mp4));
        assert_eq!(Container::of(Path::new("b.mov")), Ok(Container::Mov));
        assert_eq!(Container::of(Path::new("b.webm")), Ok(Container::Webm));
        assert!(Container::of(Path::new("b.gif")).is_err());
        let args = ffmpeg_args(Path::new("out.mp4"), (1920, 1200), 60.0).unwrap();
        let joined = args.join(" ");
        assert!(joined.contains("-f rawvideo -pix_fmt rgba -s 1920x1200 -r 60 -i -"), "{joined}");
        assert!(joined.contains("libx264") && joined.contains("+faststart") && joined.ends_with("out.mp4"), "{joined}");
    }

    #[test]
    fn unpad_drops_row_padding() {
        let data = [1, 2, 3, 4, 9, 9, 9, 9, 5, 6, 7, 8, 9, 9, 9, 9];
        assert_eq!(unpad(&data, 1, 2, 8), [1, 2, 3, 4, 5, 6, 7, 8]);
    }
}

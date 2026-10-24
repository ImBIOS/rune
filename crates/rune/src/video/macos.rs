//! AVFoundation: AVPlayer + AVPlayerItemVideoOutput for windows, AVAssetImageGenerator for
//! posters and headless frames, and a bare AVPlayer for audio.
use super::*;
use crate::native_surface::macos::PixelBuffer;
use objc2_av_foundation::{
    AVAssetImageGenerator, AVMediaTypeAudio, AVMediaTypeVideo, AVPlayer, AVPlayerItem, AVPlayerItemStatus, AVPlayerItemVideoOutput,
    AVURLAsset,
};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo, kCGColorSpaceSRGB};
use objc2_core_media::{CMTime, CMTimeFlags};
use objc2_core_video::{
    CVGetCurrentHostTime, CVGetHostClockFrequency, kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
};
use objc2_foundation_video::{NSDictionary, NSString, NSURL};
use objc2_video::rc::Retained;
use objc2_video::runtime::AnyObject;
use objc2_video::{AnyThread, MainThreadMarker};
use std::time::Duration;

/// Paused players that keep a native player (and its decoder) for a quick resume. Older paused
/// players are released; a video's last frame stays on screen and resuming recreates it.
const PARKED_PLAYERS: usize = 4;
/// Posters never need more than this many pixels per side; headless frames keep full size.
const POSTER_MAX: f64 = 1920.0;
/// A seek that never lands stops requesting redraws (or ticks) after this long.
const SEEK_PATIENCE: Duration = Duration::from_secs(2);
/// A player item that never becomes ready fails after this long.
const LOAD_PATIENCE: Duration = Duration::from_secs(30);
/// How often playing audio checks for its end, loops and loading.
const AUDIO_TICK: Duration = Duration::from_millis(50);
/// An audio seek has landed once the player reports a time this close to the target.
const AUDIO_SEEK_SLOP: f64 = 0.25;

#[derive(Default)]
pub(crate) struct Native {
    /// Headless decodes reuse one generator.
    generator: Option<Retained<AVAssetImageGenerator>>,
    player: Option<Player>,
    posters: Arc<Mutex<Posters>>,
    poster_started: bool,
}
// The generator is only used under the Shared mutex. Player objects are created, used and
// released on the main thread: pulls and audio ticks run there and `Player`'s drop hops to it.
unsafe impl Send for Native {}

#[derive(Default)]
struct Posters {
    want: Option<f64>,
    busy: bool,
    /// A video poster and its clip's facts, or (no frame) an audio clip's facts.
    done: Option<Result<(Option<ByteFrame>, Meta), String>>,
}

struct Player {
    // Some until dropped; `Drop` moves the objects to the main thread.
    objects: Option<Objects>,
    /// Rate last applied to AVPlayer (0 = paused).
    rate: f32,
    muted: bool,
    ready: bool,
    created: Instant,
    /// When the last seek started, and where to.
    seeking: Option<(Instant, f64)>,
}
struct Objects {
    player: Retained<AVPlayer>,
    item: Retained<AVPlayerItem>,
    /// None for audio.
    output: Option<Retained<AVPlayerItemVideoOutput>>,
}

impl Player {
    fn new(objects: Objects) -> Self {
        Self { objects: Some(objects), rate: 0.0, muted: false, ready: false, created: Instant::now(), seeking: None }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let Some(objects) = self.objects.take() else { return };
        if MainThreadMarker::new().is_some() {
            unsafe { objects.player.pause() };
            return;
        }
        // The last handle went away on another thread: release on the main queue.
        struct Release(Objects);
        unsafe impl Send for Release {}
        let release = Release(objects);
        on_main(move || {
            let release = release; // Whole, so the closure is Send.
            unsafe { release.0.player.pause() };
        });
    }
}

#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    static _dispatch_main_q: u8;
    fn dispatch_async_f(queue: *const u8, context: *mut std::ffi::c_void, work: extern "C" fn(*mut std::ffi::c_void));
}

/// Run `f` on the main queue, which the window event loop drains.
fn on_main(f: impl FnOnce() + Send + 'static) {
    type Job = Box<dyn FnOnce() + Send>;
    extern "C" fn run(context: *mut std::ffi::c_void) {
        let job = unsafe { Box::from_raw(context.cast::<Job>()) };
        job();
    }
    let job: Job = Box::new(f);
    let context = Box::into_raw(Box::new(job)).cast();
    unsafe { dispatch_async_f(&raw const _dispatch_main_q, context, run) };
}

/// Paused players holding a native player, oldest first.
static PARKED: Mutex<Vec<Weak<Shared>>> = Mutex::new(Vec::new());
/// Every live audio player, for the tick.
static AUDIO: Mutex<Vec<Weak<Shared>>> = Mutex::new(Vec::new());
/// Some audio clip wants ticking. Written on the main thread.
static AUDIO_BUSY: AtomicBool = AtomicBool::new(false);
/// The tick thread is running.
static TICKING: AtomicBool = AtomicBool::new(false);

/// AVFoundation has no Ogg, WebM or Matroska demuxer; fail at once rather than wait on a load
/// that can only fail.
fn unsupported_format(source: &MediaSource) -> Option<String> {
    let path = match source {
        MediaSource::File(path) => path.to_string_lossy().into_owned(),
        MediaSource::Url(url) => url.split(['?', '#']).next().unwrap_or_default().to_string(),
    };
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    matches!(ext.as_str(), "ogg" | "oga" | "ogv" | "opus" | "spx" | "webm" | "mkv" | "mka")
        .then(|| format!(".{ext} isn't supported on macOS (AVFoundation can't read it); use mp3, m4a, wav, mp4 or mov"))
}

pub(super) fn created(shared: &Arc<Shared>) {
    if let Some(e) = unsupported_format(&shared.source) {
        shared.ctl.lock().unwrap().fail(e);
        return;
    }
    if shared.kind == Kind::Audio {
        AUDIO.lock().unwrap().push(Arc::downgrade(shared));
        if playback_enabled() {
            load_audio(shared);
        }
    }
}

/// The window loop is starting: load audio clips made before it.
pub(super) fn enable() {
    let clips: Vec<Arc<Shared>> = AUDIO.lock().unwrap().iter().filter_map(Weak::upgrade).collect();
    for clip in clips {
        load_audio(&clip);
        on_main(move || drive(&clip));
    }
}

/// Read an audio clip's duration on a worker, so tiles know it before they play.
fn load_audio(shared: &Arc<Shared>) {
    let mut native = shared.native.lock().unwrap();
    if !native.poster_started && !shared.ctl.lock().unwrap().opened() {
        request_poster(shared, &mut native, 0.0);
    }
}

fn url(source: &MediaSource) -> Result<Retained<NSURL>, String> {
    match source {
        MediaSource::File(path) => {
            if !path.exists() {
                return Err(format!("no such file: {}", path.display()));
            }
            let path = std::path::absolute(path).unwrap_or_else(|_| path.clone());
            Ok(NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy())))
        }
        MediaSource::Url(url) => NSURL::URLWithString(&NSString::from_str(url)).ok_or_else(|| format!("invalid URL: {url}")),
    }
}

fn asset(source: &MediaSource) -> Result<Retained<AVURLAsset>, String> {
    Ok(unsafe { AVURLAsset::URLAssetWithURL_options(&*url(source)?, None) })
}

fn seconds(t: CMTime) -> f64 {
    let s = unsafe { t.seconds() };
    if s.is_finite() { s } else { 0.0 }
}
fn cm(seconds: f64) -> CMTime {
    unsafe { CMTime::with_seconds(seconds, 600_000) }
}
fn zero() -> CMTime {
    CMTime { value: 0, timescale: 1, flags: CMTimeFlags::Valid, epoch: 0 }
}

/// Blocks until the asset's tracks load (a network fetch for URLs).
#[allow(deprecated)] // The synchronous loaders are what a worker or headless run wants.
fn meta(asset: &AVURLAsset, kind: Kind) -> Result<Meta, String> {
    let duration = || seconds(unsafe { asset.duration() });
    if kind == Kind::Audio {
        let audio = unsafe { AVMediaTypeAudio }.ok_or("AVMediaTypeAudio unavailable")?;
        if unsafe { asset.tracksWithMediaType(audio) }.firstObject().is_none() {
            return Err("no playable audio track (unreachable, or a format AVFoundation can't read)".into());
        }
        return Ok(Meta { duration: duration(), size: None, frame: 1.0 / 30.0 });
    }
    let video = unsafe { AVMediaTypeVideo }.ok_or("AVMediaTypeVideo unavailable")?;
    let tracks = unsafe { asset.tracksWithMediaType(video) };
    let Some(track) = tracks.firstObject() else {
        return Err("no playable video track".into());
    };
    let size = unsafe { track.naturalSize() };
    let t = unsafe { track.preferredTransform() };
    let w = (size.width * t.a + size.height * t.c).abs().round();
    let h = (size.width * t.b + size.height * t.d).abs().round();
    let min = seconds(unsafe { track.minFrameDuration() });
    let nominal = unsafe { track.nominalFrameRate() } as f64;
    let frame = if min > 0.0 { min } else if nominal > 0.0 { 1.0 / nominal } else { 1.0 / 30.0 };
    if w < 1.0 || h < 1.0 {
        return Err("video track has no size".into());
    }
    Ok(Meta { duration: duration(), size: Some((w as u32, h as u32)), frame })
}

fn generator(asset: &AVURLAsset, max: Option<f64>) -> Retained<AVAssetImageGenerator> {
    let g = unsafe { AVAssetImageGenerator::assetImageGeneratorWithAsset(asset) };
    unsafe {
        g.setAppliesPreferredTrackTransform(true);
        g.setRequestedTimeToleranceBefore(zero());
        g.setRequestedTimeToleranceAfter(zero());
        if let Some(max) = max {
            g.setMaximumSize(CGSize::new(max, max));
        }
    }
    g
}

#[allow(deprecated)] // copyCGImageAtTime is the synchronous form.
fn image(generator: &AVAssetImageGenerator, at: f64) -> Result<ByteFrame, String> {
    let image = unsafe { generator.copyCGImageAtTime_actualTime_error(cm(at), std::ptr::null_mut()) }
        .map_err(|e| e.localizedDescription().to_string())?;
    bgra(&image)
}

/// Draws a decoded image into sRGB BGRA bytes (opaque, so premultiplied equals straight).
fn bgra(image: &CGImage) -> Result<ByteFrame, String> {
    let (w, h) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    if w == 0 || h == 0 {
        return Err("decoded an empty frame".into());
    }
    let mut bytes = vec![0u8; w * h * 4];
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }));
    let info = CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
    let context = unsafe { CGBitmapContextCreate(bytes.as_mut_ptr().cast(), w, h, 8, w * 4, space.as_deref(), info) }
        .ok_or("CGBitmapContextCreate failed")?;
    CGContext::draw_image(Some(&context), CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, h as f64)), Some(image));
    drop(context);
    Ok(ByteFrame {
        width: w as u32,
        height: h as u32,
        format: Format::Bgra,
        color: ColorInfo { matrix: crate::native_surface::Matrix::Bt709, transfer: crate::native_surface::Transfer::Srgb },
        planes: [bytes, Vec::new()],
    })
}

pub(super) fn probe(native: &mut Native, source: &MediaSource, kind: Kind) -> Result<Meta, String> {
    let asset = asset(source)?;
    let meta = meta(&asset, kind)?;
    if kind == Kind::Video {
        native.generator = Some(generator(&asset, None));
    }
    Ok(meta)
}

pub(super) fn decode(native: &mut Native, source: &MediaSource, at: f64) -> Result<ByteFrame, String> {
    if native.generator.is_none() {
        let asset = asset(source)?;
        native.generator = Some(generator(&asset, None));
    }
    image(native.generator.as_ref().unwrap(), at)
}

/// One worker per clip opens it and decodes the newest requested poster (none for audio),
/// then wakes its windows, or for audio drives it on the main queue.
fn request_poster(shared: &Arc<Shared>, native: &mut Native, at: f64) {
    native.poster_started = true;
    let mut slot = native.posters.lock().unwrap();
    slot.want = Some(at);
    if slot.busy {
        return;
    }
    slot.busy = true;
    let posters = native.posters.clone();
    let clip = Arc::downgrade(shared);
    let (source, kind) = (shared.source.clone(), shared.kind);
    std::thread::Builder::new()
        .name("rune-media-poster".into())
        .spawn(move || {
            let mut opened: Option<(Option<Retained<AVAssetImageGenerator>>, Meta)> = None;
            loop {
                let Some(at) = ({
                    let mut slot = posters.lock().unwrap();
                    let want = slot.want.take();
                    if want.is_none() {
                        slot.busy = false;
                    }
                    want
                }) else {
                    return;
                };
                if clip.strong_count() == 0 {
                    return;
                }
                let result = (|| {
                    if opened.is_none() {
                        let asset = asset(&source)?;
                        let meta = meta(&asset, kind)?;
                        opened = Some(((kind == Kind::Video).then(|| generator(&asset, Some(POSTER_MAX))), meta));
                    }
                    let (generator, meta) = opened.as_ref().unwrap();
                    let frame = match generator {
                        Some(g) => Some(image(g, meta.frame_time(meta.frame_index(at)))?),
                        None => None,
                    };
                    Ok((frame, *meta))
                })();
                posters.lock().unwrap().done = Some(result);
                let Some(clip) = clip.upgrade() else { return };
                match kind {
                    Kind::Video => clip.wake_windows(),
                    Kind::Audio => on_main(move || drive(&clip)),
                }
            }
        })
        .expect("spawn media poster worker");
}

fn create_player(source: &MediaSource, kind: Kind, mtm: MainThreadMarker) -> Result<Objects, String> {
    let asset = asset(source)?;
    let item = unsafe { AVPlayerItem::playerItemWithAsset(&asset, mtm) };
    let output = (kind == Kind::Video).then(|| {
        let empty = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
        let format = CFNumber::new_i32(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange as i32);
        let attributes = unsafe {
            CFDictionary::<CFString, CFType>::from_slices(
                &[kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey],
                &[format.as_ref(), AsRef::<CFType>::as_ref(&*empty), CFBoolean::new(true).as_ref()],
            )
        };
        // CFDictionary and NSDictionary are toll-free bridged.
        let attributes = unsafe { &*(CFRetained::as_ptr(&attributes).as_ptr() as *const NSDictionary<NSString, AnyObject>) };
        unsafe {
            let output = AVPlayerItemVideoOutput::initWithPixelBufferAttributes(AVPlayerItemVideoOutput::alloc(), Some(attributes));
            output.setSuppressesPlayerRendering(true);
            item.addOutput(&output);
            output
        }
    });
    let player = unsafe { AVPlayer::playerWithPlayerItem(Some(&item), mtm) };
    Ok(Objects { player, item, output })
}

/// Seconds on the host clock (`CACurrentMediaTime`'s) for an `Instant`.
fn host_seconds(at: Instant) -> f64 {
    let now = Instant::now();
    let host = CVGetCurrentHostTime() as f64 / CVGetHostClockFrequency();
    if at >= now { host + (at - now).as_secs_f64() } else { host - (now - at).as_secs_f64() }
}

/// Bring the player in line with the handle: mute, seek, rate.
fn reconcile(p: &mut Player, c: &mut Control, now: Instant) {
    let Some(o) = &p.objects else { return };
    unsafe {
        if p.muted != c.muted {
            o.player.setMuted(c.muted);
            p.muted = c.muted;
        }
        if let Some(t) = c.seek.take() {
            o.player.seekToTime_toleranceBefore_toleranceAfter(cm(t), zero(), zero());
            p.seeking = Some((now, t));
        }
        if p.ready {
            let rate = if c.playing { c.rate } else { 0.0 };
            if p.rate != rate {
                if rate > 0.0 { o.player.setRate(rate) } else { o.player.pause() }
                p.rate = rate;
            }
        }
    }
}

/// Applies a handle edit at once on the main thread, so a paused, undrawn video stops (and goes
/// quiet) without waiting for a frame that draws it. Audio edits hop to the main thread.
pub(super) fn apply(shared: &Arc<Shared>) {
    if shared.kind == Kind::Audio {
        if !playback_enabled() || shared.ctl.lock().unwrap().failed.is_some() {
            return;
        }
        if MainThreadMarker::new().is_some() {
            drive(shared);
        } else {
            let clip = Arc::downgrade(shared);
            on_main(move || {
                if let Some(clip) = clip.upgrade() {
                    drive(&clip);
                }
            });
        }
        return;
    }
    if MainThreadMarker::new().is_none() {
        return;
    }
    let Ok(mut native) = shared.native.try_lock() else { return };
    if let Some(p) = native.player.as_mut() {
        reconcile(p, &mut shared.ctl.lock().unwrap(), Instant::now());
    }
}

/// Audio, on the main thread: what the player reports now, while it plays.
pub(super) fn live_time(shared: &Shared) -> Option<f64> {
    if shared.kind != Kind::Audio || MainThreadMarker::new().is_none() {
        return None;
    }
    let native = shared.native.try_lock().ok()?;
    let p = native.player.as_ref().filter(|p| p.ready && p.rate > 0.0 && p.seeking.is_none())?;
    let time = seconds(unsafe { p.objects.as_ref()?.player.currentTime() });
    let c = shared.ctl.lock().unwrap();
    (c.playing && !c.ended).then(|| c.duration.map_or(time, |d| time.clamp(0.0, d)))
}

/// Takes a finished poster load: the clip's facts and, for video, its poster frame.
fn take_poster(native: &mut Native, c: &mut Control, surface: Option<&NativeSurface>) {
    let Some(done) = native.posters.lock().unwrap().done.take() else { return };
    match done {
        Ok((bytes, meta)) => {
            if !c.opened() {
                c.open(Ok(meta));
            }
            if let (Some(bytes), Some(surface), None) = (bytes, surface, &native.player) {
                let id = c.next_frame_id();
                if let Err(e) = surface.push_bytes(bytes, id) {
                    c.fail(e);
                }
            }
        }
        Err(e) => c.fail(e),
    }
}

/// Fails the clip if its item failed or never loaded; marks it ready once it can play.
/// Returns false when the player is gone.
fn check_item(native: &mut Native, c: &mut Control, now: Instant) -> bool {
    let p = native.player.as_mut().unwrap();
    let o = p.objects.as_ref().unwrap();
    let status = unsafe { o.item.status() };
    let failure = if status == AVPlayerItemStatus::Failed {
        Some(unsafe { o.item.error() }.map_or_else(|| "playback failed".into(), |e| e.localizedDescription().to_string()))
    } else if status != AVPlayerItemStatus::ReadyToPlay && now - p.created > LOAD_PATIENCE {
        Some(format!("timed out loading after {} s", LOAD_PATIENCE.as_secs()))
    } else {
        None
    };
    if let Some(e) = failure {
        c.fail(e);
        native.player = None;
        return false;
    }
    if status == AVPlayerItemStatus::ReadyToPlay && !p.ready {
        p.ready = true;
        if !c.opened() {
            let size = unsafe { o.item.presentationSize() };
            let duration = seconds(unsafe { o.item.duration() });
            let size = (size.width >= 1.0 && size.height >= 1.0).then(|| (size.width.round() as u32, size.height.round() as u32));
            c.open(Ok(Meta { duration, size, frame: 1.0 / 30.0 }));
        }
    }
    true
}

/// Makes a player for a clip that should play; returns false when there is none to drive.
fn ensure_player(shared: &Arc<Shared>, native: &mut Native, c: &mut Control, mtm: MainThreadMarker) -> bool {
    if native.player.is_some() {
        return true;
    }
    match create_player(&shared.source, shared.kind, mtm) {
        Ok(objects) => {
            // A fresh player starts at the handle's position.
            if c.time > 0.0 && c.seek.is_none() {
                c.seek = Some(c.time);
            }
            native.player = Some(Player::new(objects));
            true
        }
        Err(e) => {
            c.fail(e);
            false
        }
    }
}

/// Folds the player's position into the state: seeks landing, the end, loops.
fn follow(p: &mut Player, c: &mut Control, now: Instant, frame_time: Option<f64>) {
    let o = p.objects.as_ref().unwrap();
    let current = seconds(unsafe { o.player.currentTime() });
    if frame_time.is_none() && p.seeking.is_some_and(|(_, to)| (current - to).abs() < AUDIO_SEEK_SLOP) {
        p.seeking = None;
    }
    // Seeks land asynchronously: until then the player still reports the old position, which
    // may be the end.
    let seeking = p.seeking.is_some();
    let stopped = p.rate > 0.0 && unsafe { o.player.rate() } == 0.0;
    let at_end = !seeking && (c.duration.is_some_and(|d| current >= d - 1e-3) || stopped);
    let time = if seeking {
        c.time
    } else if c.playing && !at_end {
        frame_time.unwrap_or(current).max(0.0)
    } else {
        current
    };
    if c.observe(time, at_end) {
        unsafe {
            o.player.seekToTime_toleranceBefore_toleranceAfter(zero(), zero(), zero());
            o.player.setRate(c.rate);
        }
        p.rate = c.rate;
        p.seeking = Some((now, 0.0));
        // A loop is news for anything showing play counts or progress from zero.
        c.generation += 1;
    } else if !c.playing {
        p.rate = 0.0;
    }
}

pub(super) fn pull(shared: &Arc<Shared>, surface: &NativeSurface, target: Instant) -> bool {
    let now = Instant::now();
    let mut native = shared.native.lock().unwrap();
    let mut c = shared.ctl.lock().unwrap();

    take_poster(&mut native, &mut c, Some(surface));
    if c.failed.is_some() {
        native.player = None;
        return false;
    }

    let Some(mtm) = MainThreadMarker::new() else { return false };
    if native.player.is_none() && !c.playing {
        // Poster mode: no player, no decoder session.
        let want = c.seek.take().or_else(|| (!native.poster_started).then_some(c.time));
        if let Some(at) = want {
            drop(c);
            request_poster(shared, &mut native, at);
        }
        return false;
    }
    let fresh = native.player.is_none();
    if !ensure_player(shared, &mut native, &mut c, mtm) {
        return false;
    }
    if fresh {
        unpark(shared);
    }
    if !check_item(&mut native, &mut c, now) {
        return false;
    }

    let p = native.player.as_mut().unwrap();
    reconcile(p, &mut c, now);
    if p.ready {
        let o = p.objects.as_ref().unwrap();
        let output = o.output.as_ref().unwrap();
        let item_time = unsafe { output.itemTimeForHostTime(host_seconds(target)) };
        if unsafe { output.hasNewPixelBufferForItemTime(item_time) }
            && let Some(buffer) = unsafe { output.copyPixelBufferForItemTime_itemTimeForDisplay(item_time, std::ptr::null_mut()) }
        {
            let id = c.next_frame_id();
            // SAFETY: AVPlayerItemVideoOutput hands out finished buffers it no longer writes.
            let pushed = unsafe { PixelBuffer::from_raw(Retained::as_ptr(&buffer) as *mut _) }.and_then(|b| surface.push(b, id));
            if let Err(e) = pushed {
                c.fail(e);
                native.player = None;
                return false;
            }
            p.seeking = None;
        }
        follow(p, &mut c, now, Some(seconds(item_time)));
    }
    finish(shared, native, c, now)
}

/// Seek patience and parking, shared by video pulls and audio drives. Returns whether the
/// clip is playing or seeking.
fn finish(shared: &Arc<Shared>, mut native: MutexGuard<'_, Native>, c: MutexGuard<'_, Control>, now: Instant) -> bool {
    let p = native.player.as_mut().unwrap();
    if p.seeking.is_some_and(|(since, _)| now - since > SEEK_PATIENCE) {
        p.seeking = None;
    }
    let active = c.playing || p.seeking.is_some();
    let park = p.ready && !active;
    drop(c);
    drop(native);
    if park { self::park(shared) } else { unpark(shared) }
    active
}

/// Audio, on the main thread: take a finished load, make or update the player, follow it to
/// the end, tell the app about changes, and keep the tick going while it plays or seeks.
pub(super) fn drive(shared: &Arc<Shared>) {
    if step_audio(shared) {
        AUDIO_BUSY.store(true, Ordering::Release);
        start_ticking();
    }
    shared.settle(true);
}

/// Returns whether the clip needs ticking.
fn step_audio(shared: &Arc<Shared>) -> bool {
    let Some(mtm) = MainThreadMarker::new() else { return false };
    let now = Instant::now();
    // Already being driven further up this stack (a waker that edits the clip).
    let Ok(mut native) = shared.native.try_lock() else { return true };
    let mut c = shared.ctl.lock().unwrap();
    take_poster(&mut native, &mut c, None);
    if c.failed.is_some() {
        native.player = None;
        return false;
    }
    if native.player.is_none() && !c.playing {
        return false;
    }
    let fresh = native.player.is_none();
    if !ensure_player(shared, &mut native, &mut c, mtm) {
        return false;
    }
    if fresh {
        unpark(shared);
    }
    if !check_item(&mut native, &mut c, now) {
        return false;
    }
    let p = native.player.as_mut().unwrap();
    reconcile(p, &mut c, now);
    if !p.ready {
        return c.playing;
    }
    follow(p, &mut c, now, None);
    finish(shared, native, c, now)
}

/// Main thread: drive every audio clip that might be playing.
fn tick() {
    let clips: Vec<Arc<Shared>> = {
        let mut audio = AUDIO.lock().unwrap();
        audio.retain(|w| w.strong_count() > 0);
        audio.iter().filter_map(Weak::upgrade).collect()
    };
    let mut busy = false;
    for clip in &clips {
        busy |= step_audio(clip);
        clip.settle(true);
    }
    AUDIO_BUSY.store(busy, Ordering::Release);
}

/// A thread that posts `tick` to the main queue every `AUDIO_TICK` while audio is busy.
fn start_ticking() {
    if TICKING.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::Builder::new()
        .name("rune-audio-tick".into())
        .spawn(|| {
            loop {
                std::thread::sleep(AUDIO_TICK);
                if !AUDIO_BUSY.load(Ordering::Acquire) {
                    TICKING.store(false, Ordering::Release);
                    // Something started between the check and the store: keep going if no
                    // other ticker took over.
                    if !AUDIO_BUSY.load(Ordering::Acquire) || TICKING.swap(true, Ordering::AcqRel) {
                        return;
                    }
                }
                on_main(tick);
            }
        })
        .expect("spawn audio tick");
}

fn unpark(shared: &Arc<Shared>) {
    PARKED.lock().unwrap().retain(|w| w.strong_count() > 0 && !std::ptr::eq(w.as_ptr(), Arc::as_ptr(shared)));
}

/// Keep at most `PARKED_PLAYERS` paused players; release the oldest. Main thread only.
fn park(shared: &Arc<Shared>) {
    let evicted: Vec<Arc<Shared>> = {
        let mut parked = PARKED.lock().unwrap();
        parked.retain(|w| w.strong_count() > 0);
        if !parked.iter().any(|w| std::ptr::eq(w.as_ptr(), Arc::as_ptr(shared))) {
            parked.push(Arc::downgrade(shared));
        }
        let excess = parked.len().saturating_sub(PARKED_PLAYERS);
        parked.drain(..excess).filter_map(|w| w.upgrade()).collect()
    };
    for clip in evicted {
        // A video's last frame stays on its surface; resuming recreates the player.
        let player = clip.native.lock().unwrap().player.take();
        drop(player);
    }
}

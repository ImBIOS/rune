//! One drawable submission per display tick. Metal's drawable pool is not a frame clock:
//! acquisition may return immediately even with FIFO presentation enabled.
#![allow(deprecated)] // CVDisplayLink supports the same macOS versions as winit.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use objc2_core_foundation::CFRetained;
use objc2_core_video::{CVDisplayLink, CVOptionFlags, CVReturn, CVTimeStamp};
use winit::{platform::macos::MonitorHandleExtMacOS, window::Window};

struct Tick {
    at: Option<Instant>,
    period: Duration,
}

struct Callback {
    tick: Mutex<Tick>,
    window: Weak<Window>,
}

pub(crate) struct DisplayLink {
    link: CFRetained<CVDisplayLink>,
    // Stable allocation, retained until stop has joined the callback thread.
    callback: Box<Callback>,
    last_present: Option<Instant>,
    display: Option<u32>,
    running: bool,
}

unsafe extern "C-unwind" fn tick(
    _: NonNull<CVDisplayLink>, _: NonNull<CVTimeStamp>, output: NonNull<CVTimeStamp>,
    _: CVOptionFlags, _: NonNull<CVOptionFlags>, context: *mut c_void,
) -> CVReturn {
    // SAFETY: CoreVideo borrows the boxed context until DisplayLink::drop stops the link.
    let callback = unsafe { &*context.cast::<Callback>() };
    let output = unsafe { output.as_ref() };
    let mut tick = callback.tick.lock().unwrap();
    let pending = tick.at.is_some();
    tick.at = Some(Instant::now());
    if output.videoTimeScale > 0 && output.videoRefreshPeriod > 0 {
        tick.period = Duration::from_secs_f64(output.videoRefreshPeriod as f64 / output.videoTimeScale as f64);
    }
    drop(tick);
    // Coalesce a backed-up event loop; missed refreshes never become a catch-up burst.
    if !pending && let Some(window) = callback.window.upgrade() { window.request_redraw(); }
    0
}

impl DisplayLink {
    pub(crate) fn running(&self) -> bool { self.running }
    pub(crate) fn refresh_hz(&self) -> f64 { 1.0 / self.callback.tick.lock().unwrap().period.as_secs_f64() }

    pub(crate) fn new(window: &Arc<Window>) -> Option<Self> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: valid out pointer; the create rule transfers one retained reference.
        let status = unsafe { CVDisplayLink::create_with_active_cg_displays(NonNull::from(&mut raw)) };
        if status != 0 { eprintln!("display link creation failed: {status}"); return None; }
        let link = unsafe { CFRetained::from_raw(NonNull::new(raw)?) };
        let mut callback = Box::new(Callback {
            tick: Mutex::new(Tick { at: None, period: Duration::from_secs_f64(1.0 / 60.0) }),
            window: Arc::downgrade(window),
        });
        let status = unsafe { link.set_output_callback(Some(tick), (&mut *callback as *mut Callback).cast()) };
        if status != 0 { eprintln!("display link callback failed: {status}"); return None; }
        Some(Self { link, callback, last_present: None, display: None, running: false })
    }

    pub(crate) fn ready(&mut self, window: &Window) -> bool {
        let display = window.current_monitor().map(|m| m.native_id());
        if display != self.display {
            if let Some(display) = display { self.link.set_current_cg_display(display); }
            self.display = display;
        }
        if !self.running {
            let status = self.link.start();
            if status != 0 { eprintln!("display link start failed: {status}"); return true; }
            self.running = true;
        }
        let mut tick = self.callback.tick.lock().unwrap();
        let Some(at) = tick.at else { return false };
        let now = Instant::now();
        // A late render followed by an on-time tick must skip, not submit two frames
        // together. Also discard stale callbacks after main-thread stalls.
        let ready = permits(self.last_present, at, now, tick.period);
        if !ready { tick.at = None; }
        ready
    }

    pub(crate) fn presented(&mut self, at: Instant) {
        self.last_present = Some(at);
        self.retry();
    }

    pub(crate) fn retry(&mut self) { self.callback.tick.lock().unwrap().at = None; }

    pub(crate) fn stop(&mut self) {
        if self.running { self.link.stop(); self.running = false; }
        self.callback.tick.lock().unwrap().at = None;
    }
}

impl Drop for DisplayLink {
    fn drop(&mut self) { self.stop(); }
}

fn permits(last: Option<Instant>, tick: Instant, now: Instant, period: Duration) -> bool {
    let half = period / 2;
    now.duration_since(tick) < half && last.is_none_or(|last| now.duration_since(last) >= half)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn late_frames_and_idle_restarts_cannot_catch_up_in_a_burst() {
        let t = Instant::now();
        let ms = |n| t + Duration::from_millis(n);
        let period = Duration::from_millis(16);
        assert!(permits(None, ms(0), ms(1), period));
        assert!(!permits(Some(ms(15)), ms(16), ms(17), period));
        assert!(!permits(Some(ms(15)), ms(16), ms(30), period));
        assert!(permits(Some(ms(15)), ms(32), ms(33), period));
        assert!(!permits(Some(ms(100)), ms(101), ms(102), period));
    }
}

//! One drawable submission per display tick. Metal's drawable pool is not a frame clock:
//! acquisition may return immediately even with FIFO presentation enabled.
#![allow(deprecated)] // CVDisplayLink supports the same macOS versions as winit.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use objc2_core_foundation::CFRetained;
use objc2_core_video::{CVDisplayLink, CVGetCurrentHostTime, CVGetHostClockFrequency, CVOptionFlags, CVReturn, CVTimeStamp, CVTimeStampFlags};
use winit::{platform::macos::MonitorHandleExtMacOS, window::Window};

struct Tick {
    target: Option<Instant>,
    period: Duration,
}

struct Callback {
    tick: Mutex<Tick>,
    wake: crate::runtime::Waker,
    host_frequency: f64,
}

pub(crate) struct DisplayLink {
    link: CFRetained<CVDisplayLink>,
    // Stable allocation, retained until stop has joined the callback thread.
    callback: Box<Callback>,
    last_present: Option<Instant>,
    last_target: Option<Instant>,
    display: Option<u32>,
    running: bool,
}

unsafe extern "C-unwind" fn tick(
    _: NonNull<CVDisplayLink>, _: NonNull<CVTimeStamp>, output: NonNull<CVTimeStamp>,
    _: CVOptionFlags, _: NonNull<CVOptionFlags>, context: *mut c_void,
) -> CVReturn {
    // SAFETY: CoreVideo borrows the boxed context until DisplayLink::drop stops the link.
    let callback = unsafe { &*context.cast::<Callback>() };
    // SAFETY: CoreVideo provides a valid timestamp for this callback invocation.
    let output = unsafe { output.as_ref() };
    let now = Instant::now();
    let host_now = CVGetCurrentHostTime();
    let mut tick = callback.tick.lock().unwrap();
    let pending = tick.target.is_some();
    if output.videoTimeScale > 0 && output.videoRefreshPeriod > 0 {
        tick.period = Duration::from_secs_f64(output.videoRefreshPeriod as f64 / output.videoTimeScale as f64);
    }
    tick.target = Some(if output.flags & CVTimeStampFlags::HostTimeValid.0 != 0 {
        target_time(now, host_now, output.hostTime, callback.host_frequency)
    } else { now + tick.period });
    drop(tick);
    // Coalesce a backed-up event loop; missed refreshes never become a catch-up burst.
    // Never call Window::request_redraw here: winit synchronously waits on the main
    // thread on macOS, which can itself be waiting in CVDisplayLinkStop.
    if !pending { (callback.wake)(); }
    0
}

impl DisplayLink {
    pub(crate) fn running(&self) -> bool { self.running }
    pub(crate) fn refresh_hz(&self) -> f64 { 1.0 / self.callback.tick.lock().unwrap().period.as_secs_f64() }

    pub(crate) fn new(wake: crate::runtime::Waker) -> Option<Self> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: valid out pointer; the create rule transfers one retained reference.
        let status = unsafe { CVDisplayLink::create_with_active_cg_displays(NonNull::from(&mut raw)) };
        if status != 0 { eprintln!("display link creation failed: {status}"); return None; }
        let link = unsafe { CFRetained::from_raw(NonNull::new(raw)?) };
        let mut callback = Box::new(Callback {
            tick: Mutex::new(Tick { target: None, period: Duration::from_secs_f64(1.0 / 60.0) }),
            wake,
            host_frequency: CVGetHostClockFrequency(),
        });
        let status = unsafe { link.set_output_callback(Some(tick), (&mut *callback as *mut Callback).cast()) };
        if status != 0 { eprintln!("display link callback failed: {status}"); return None; }
        Some(Self { link, callback, last_present: None, last_target: None, display: None, running: false })
    }

    /// Claims one display opportunity and returns its target presentation time. Consume
    /// before rendering so a tick arriving during rendering can queue the next opportunity.
    pub(crate) fn next_frame(&mut self, window: &Window) -> Option<Instant> {
        let display = window.current_monitor().map(|m| m.native_id());
        if display != self.display {
            if let Some(display) = display { self.link.set_current_cg_display(display); }
            self.display = display;
        }
        if !self.running {
            let status = self.link.start();
            if status != 0 { eprintln!("display link start failed: {status}"); return Some(Instant::now()); }
            self.running = true;
        }
        let mut tick = self.callback.tick.lock().unwrap();
        let target = tick.target.take()?;
        let now = Instant::now();
        // A late render followed by an on-time tick must skip, not submit two frames
        // together. Also discard stale callbacks after main-thread stalls.
        if !permits(self.last_present, self.last_target, target, now, tick.period) { return None; }
        self.last_target = Some(target);
        Some(target)
    }

    pub(crate) fn presented(&mut self, at: Instant) {
        self.last_present = Some(at);
    }

    pub(crate) fn stop(&mut self) {
        if self.running { self.link.stop(); self.running = false; }
        self.callback.tick.lock().unwrap().target = None;
    }
}

impl Drop for DisplayLink {
    fn drop(&mut self) { self.stop(); }
}

fn target_time(now: Instant, host_now: u64, host_target: u64, frequency: f64) -> Instant {
    let delta = Duration::from_secs_f64(host_target.abs_diff(host_now) as f64 / frequency);
    if host_target >= host_now { now + delta } else { now - delta }
}

fn permits(last: Option<Instant>, last_target: Option<Instant>, target: Instant, now: Instant, period: Duration) -> bool {
    let half = period / 2;
    now < target && last_target.is_none_or(|last| target > last)
        && last.is_none_or(|last| now.saturating_duration_since(last) >= half)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn late_frames_and_idle_restarts_cannot_catch_up_in_a_burst() {
        let t = Instant::now();
        let ms = |n| t + Duration::from_millis(n);
        let period = Duration::from_millis(16);
        assert!(permits(None, None, ms(16), ms(1), period));
        assert!(!permits(Some(ms(15)), Some(ms(16)), ms(32), ms(17), period));
        assert!(!permits(Some(ms(15)), Some(ms(16)), ms(32), ms(33), period));
        assert!(!permits(Some(ms(15)), Some(ms(32)), ms(32), ms(30), period));
        assert!(permits(Some(ms(15)), Some(ms(16)), ms(32), ms(30), period));
        assert!(!permits(Some(ms(100)), Some(ms(112)), ms(128), ms(102), period));
        assert_eq!(target_time(t, 100, 116, 1000.0), ms(16));
        assert_eq!(target_time(ms(32), 132, 116, 1000.0), ms(16));
    }
}

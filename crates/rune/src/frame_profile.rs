//! Opt-in headless phase timings. No clocks or allocator hooks in ordinary builds.
//! Commit and Layout include their nested phases; these overlapping times must not be summed.
#[derive(Clone, Copy)]
pub(crate) enum Phase { View, Commit, Layout, Text, Paint, Apply }

/// Phases timed, each with a wall and a CPU column.
#[cfg_attr(not(feature = "frame-profile"), allow(dead_code))]
pub const PHASES: usize = 6;

#[cfg(feature = "frame-profile")]
mod enabled {
    use super::{PHASES, Phase};
    use std::{cell::Cell, time::Instant};

    thread_local! {
        static TIMES: Cell<[f64; 2 * PHASES]> = const { Cell::new([0.0; 2 * PHASES]) };
    }

    /// CPU consumed by this thread, excluding time the scheduler runs other work.
    pub(crate) fn cpu_ms() -> Option<f64> {
        #[cfg(unix)]
        {
            let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
            // SAFETY: `t` is a live, writable timespec and the clock needs no handle.
            let ok = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) };
            assert_eq!(ok, 0, "thread CPU clock unavailable");
            Some(t.tv_sec as f64 * 1000.0 + t.tv_nsec as f64 / 1e6)
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::{Foundation::FILETIME, System::Threading::{GetCurrentThread, GetThreadTimes}};
            let mut created = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
            let mut exited = created;
            let mut kernel = created;
            let mut user = created;
            // SAFETY: the current-thread pseudo-handle is valid and all four outputs are
            // live, writable FILETIMEs. The pseudo-handle is borrowed, not closed.
            let ok = unsafe { GetThreadTimes(GetCurrentThread(), &mut created, &mut exited, &mut kernel, &mut user) };
            assert_ne!(ok, 0, "thread CPU clock unavailable");
            let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
            Some((ticks(kernel) + ticks(user)) as f64 / 10_000.0)
        }
        #[cfg(not(any(unix, windows)))]
        { None }
    }

    pub(crate) struct Scope(Phase, Instant, Option<f64>);
    impl Scope {
        pub(crate) fn new(phase: Phase) -> Self { Self(phase, Instant::now(), cpu_ms()) }
    }
    impl Drop for Scope {
        fn drop(&mut self) {
            TIMES.with(|times| {
                let mut row = times.get();
                row[self.0 as usize] += self.1.elapsed().as_secs_f64() * 1000.0;
                if let Some(start) = self.2 { row[self.0 as usize + PHASES] += cpu_ms().unwrap() - start; }
                times.set(row);
            });
        }
    }
    pub(crate) fn take() -> [f64; 2 * PHASES] { TIMES.replace([0.0; 2 * PHASES]) }
}

#[cfg(feature = "frame-profile")]
pub(crate) use enabled::{Scope, cpu_ms, take};

/// Wall then CPU milliseconds per phase (view, commit, layout, text, paint, apply) on this thread
/// since the last call, for benches that drive a `Runtime` themselves.
#[cfg(feature = "frame-profile")]
pub fn take_frame_phases() -> [f64; 2 * PHASES] { take() }

/// This thread's CPU clock in milliseconds.
#[cfg(feature = "frame-profile")]
pub fn thread_cpu_ms() -> f64 { cpu_ms().unwrap_or(0.0) }

#[cfg(not(feature = "frame-profile"))]
pub(crate) struct Scope;
#[cfg(not(feature = "frame-profile"))]
impl Scope {
    #[inline(always)]
    pub(crate) fn new(_: Phase) -> Self { Self }
}

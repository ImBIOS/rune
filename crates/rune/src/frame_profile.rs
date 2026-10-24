//! Opt-in headless phase timings. No clocks or allocator hooks in ordinary builds.
#[derive(Clone, Copy)]
pub(crate) enum Phase { View, Commit, Layout, Text, Paint }

#[cfg(feature = "frame-profile")]
mod enabled {
    use super::Phase;
    use std::{cell::Cell, time::Instant};

    thread_local! {
        static TIMES: Cell<[f64; 5]> = const { Cell::new([0.0; 5]) };
    }

    pub(crate) struct Scope(Phase, Instant);
    impl Scope {
        pub(crate) fn new(phase: Phase) -> Self { Self(phase, Instant::now()) }
    }
    impl Drop for Scope {
        fn drop(&mut self) {
            TIMES.with(|times| {
                let mut row = times.get();
                row[self.0 as usize] += self.1.elapsed().as_secs_f64() * 1000.0;
                times.set(row);
            });
        }
    }
    pub(crate) fn take() -> [f64; 5] { TIMES.replace([0.0; 5]) }
}

#[cfg(feature = "frame-profile")]
pub(crate) use enabled::{Scope, take};

#[cfg(not(feature = "frame-profile"))]
pub(crate) struct Scope;
#[cfg(not(feature = "frame-profile"))]
impl Scope {
    #[inline(always)]
    pub(crate) fn new(_: Phase) -> Self { Self }
}

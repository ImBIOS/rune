use super::*;

#[test]
fn updates_that_change_nothing_ask_no_window_for_a_frame() {
    use crate::{App, AppSpec, el};
    let app = App::new(AppSpec::new("t", (10.0, 10.0), || 0u32, |_, _| el()));
    let (handle, shared) = (app.handle(), app.shared.clone());
    let mut rt = Runtime::new(0u32, |_, _| el(), (10.0, 10.0), crate::Env::default());
    rt.frame(0.0);
    // As `Host::drain`, for 100 timer ticks that found nothing new.
    for _ in 0..100 {
        handle.update_if(|_, _| false);
        let mut requests = shared.take();
        let mut updated = false;
        for f in std::mem::take(&mut requests.updates) { updated |= rt.update_if(f); }
        let quiet = !updated && requests.only_updates() && !rt.patch_pending() && !rt.needs_commit();
        for (launcher, visible) in [(false, true), (true, true), (true, false), (false, false)] {
            assert!(!drain_frame(launcher, visible, visible, updated, quiet), "launcher {launcher} visible {visible}");
        }
    }
    handle.update_if(|_, _| false);
    handle.wake();
    assert!(!shared.take().only_updates(), "`wake` still runs a shown window's poll");
    handle.update_if(|_, _| false);
    setup_waker(&shared)();
    assert!(!shared.take().only_updates(), "so does `setup`'s waker");
    assert!(drain_frame(false, true, true, false, false) && drain_frame(true, false, false, true, false));
}

/// A stream of updates that change nothing, faster than a frame, on a host that neither polls
/// nor draws for them, doesn't push back a due timer, and one's own `wake_after(ZERO)` after
/// idle is due at once.
#[test]
fn updates_that_change_nothing_do_not_delay_due_timers() {
    let mut rt = Runtime::new(0u32, |_, _| crate::el(), (10.0, 10.0), crate::Env::default());
    rt.wall_start = Some(Instant::now());
    rt.frame(0.0);
    // A second idle since that frame.
    rt.wall_start = rt.wall_start.map(|t| t - Duration::from_secs(1));
    let polled = rt.polled_at();
    rt.update_if(|_, tx| { tx.wake_after(Duration::ZERO); false });
    rt.schedule_pending_wakes();
    let due = timer_deadline(&rt).expect("armed");
    assert!(due <= Instant::now(), "due now, not a frame interval from now");
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(2));
        rt.update_if(|_, _| false);
        rt.schedule_pending_wakes();
        assert_eq!(timer_deadline(&rt), Some(due));
    }
    assert_eq!(rt.polled_at(), polled, "the clock stays at the last poll");
}

/// `wake_after(Duration::ZERO)` from the poll: a running display link takes it as the next
/// frame; any host without one waits a frame interval from the poll, never `now`.
#[test]
fn a_timer_asking_for_the_next_frame_waits_a_frame_without_a_display_link() {
    fn poll(polls: &mut u32, _: f64, tx: &mut Tx) -> bool { *polls += 1; tx.wake_after(Duration::ZERO); false }
    let mut rt = Runtime::new(0u32, |_, _| crate::el(), (10.0, 10.0), crate::Env::default());
    rt.poll = Some(poll);
    let start = Instant::now() - Duration::from_secs(1);
    rt.wall_start = Some(start);
    rt.frame(0.0);
    assert_eq!(rt.state, 1);
    let due = wall_wake_deadline(&rt).expect("armed");
    let polled = start + Duration::from_secs_f64(rt.polled_at());
    assert!(due <= polled + Duration::from_micros(1), "due at once on the runtime's clock");
    let wait = timer_deadline(&rt).expect("armed");
    assert!(wait >= polled + TIMER_FRAME - Duration::from_micros(1) && wait <= polled + TIMER_FRAME + Duration::from_micros(1));
    assert!(timer_wants_frame(&rt, Some(polled + Duration::from_millis(8))), "a paced surface draws it next tick");
    assert!(!timer_wants_frame(&rt, None), "an unpaced one doesn't keep the loop turning");
    // A timer longer than a frame keeps its own time.
    let mut rt = Runtime::new(0u32, |_, _| crate::el(), (10.0, 10.0), crate::Env::default());
    rt.wall_start = Some(start);
    rt.update(|_, tx| tx.wake_after(Duration::from_millis(100)));
    rt.frame(0.0);
    assert_eq!(timer_deadline(&rt), wall_wake_deadline(&rt));
}

#[test]
fn a_frame_that_draws_the_same_pixels_is_the_same() {
    use crate::{Color, el};
    fn view(n: &u32, _: &mut crate::Cx<u32>) -> crate::El {
        el().key("root").size(200.0, 100.0).child(el().key("box").size(40.0 + *n as f32, 40.0).fill(Color { r: 0.3, g: 0.5, b: 0.9, a: 1.0 }))
    }
    let mut rt = Runtime::new(0u32, view, (200.0, 100.0), crate::Env::new(false, 2.0));
    rt.frame(0.0);
    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: wgpu::TextureFormat::Bgra8UnormSrgb,
        width: 400,
        height: 200,
        present_mode: wgpu::PresentMode::AutoVsync,
        desired_maximum_frame_latency: 2,
        alpha_mode: wgpu::CompositeAlphaMode::Opaque,
        view_formats: vec![],
        color_space: Default::default(),
    };
    let mut presented = None;
    Presented::record(&mut presented, &rt, &config);
    let p = presented.as_ref().unwrap();

    // A poll that says it changed state, but the view draws the same.
    rt.update(|_, _| {});
    rt.frame(0.0);
    assert!(p.same(&rt, &config), "a rebuild that draws the same pixels presents nothing");

    let resized = wgpu::SurfaceConfiguration { width: 402, ..config.clone() };
    assert!(!p.same(&rt, &resized), "a resized surface draws again");
    rt.text.atlas.dirty = Some((0, 1));
    assert!(!p.same(&rt, &config), "a glyph upload draws again");
    rt.text.atlas.dirty = None;

    rt.update(|n, _| *n = 8);
    rt.settle_all();
    rt.frame(0.0);
    assert!(!p.same(&rt, &config), "a change on screen draws again");
}

#[cfg(target_os = "macos")]
#[test]
fn launch_frames_hold_two_drawables_until_the_window_rests() {
    let mut config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: wgpu::TextureFormat::Bgra8UnormSrgb,
        width: 1960,
        height: 1520,
        present_mode: wgpu::PresentMode::AutoVsync,
        desired_maximum_frame_latency: 2,
        alpha_mode: wgpu::CompositeAlphaMode::Opaque,
        view_formats: vec![],
        color_space: Default::default(),
    };
    let t0 = Instant::now();
    let mut launch = LaunchDrawables::cap(&mut config);
    assert_eq!(config.desired_maximum_frame_latency, 1, "Metal makes latency + 1 drawables");
    assert_eq!(launch.settle(false, t0), None);
    assert_eq!(launch.settle(false, t0 + Duration::from_millis(500)), None, "frames back to back stay capped");
    assert_eq!(launch.settle(true, t0 + Duration::from_millis(600)), Some(2), "resting restores the configured latency");
    assert_eq!(launch.settle(true, t0 + Duration::from_millis(700)), None, "once");
    assert_eq!(launch.settle(false, t0 + Duration::from_secs(5)), None);

    // A window that never rests gets its third drawable after its first second on screen.
    config.desired_maximum_frame_latency = 2;
    let mut launch = LaunchDrawables::cap(&mut config);
    let shown = t0 + Duration::from_secs(3);
    assert_eq!(launch.settle(false, shown), None, "the second counts from the first frame on screen");
    assert_eq!(launch.settle(false, shown + Duration::from_millis(999)), None);
    assert_eq!(launch.settle(false, shown + LaunchDrawables::LAUNCH), Some(2));

    // Nothing to restore when the configuration already asks for two.
    config.desired_maximum_frame_latency = 1;
    let mut launch = LaunchDrawables::cap(&mut config);
    assert_eq!(config.desired_maximum_frame_latency, 1);
    assert_eq!(launch.settle(true, t0), None);
}

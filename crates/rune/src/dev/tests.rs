use super::*;
use crate::{Cx, El, Env, Spring, el};

/// A runtime over `spec`'s view with the panel attached on a free port.
fn attached<S: 'static>(spec: &AppSpec<S>, state: S, wake: Waker) -> Runtime<S> {
    let mut config = Config::default();
    config.bind.set_port(0);
    let mut rt = Runtime::new(state, spec.view, spec.size, Env { system_dark: true, scale: 1.0 });
    attach(&mut rt, spec, &config, wake).unwrap();
    rt
}

/// Queues `message` as if the socket had received it; replies arrive on the receiver.
fn enqueue<S>(rt: &Runtime<S>, message: Value) -> std::sync::mpsc::Receiver<Value> {
    let (reply, replies) = std::sync::mpsc::channel();
    rt.dev.as_ref().unwrap().bridge.lock().unwrap().incoming.push_back(transport::Request { message, reply });
    replies
}

#[test]
fn scaled_timer_deadlines_pause_step_and_headless_subdivision_share_one_clock() {
    fn view(_: &Vec<f64>, _: &mut Cx<Vec<f64>>) -> El { el() }
    let mut rt = Runtime::new(Vec::new(), view, (100.0, 100.0), Env { system_dark: true, scale: 1.0 });
    rt.dev_clock = Some(crate::dev_clock::DevClock::new(0.0, 0.0));
    rt.dev_clock.as_mut().unwrap().scale = 0.1;
    rt.poll = Some(|times, now, tx| {
        times.push(now);
        if times.len() == 1 { tx.wake_after(std::time::Duration::from_millis(250)); }
        false
    });
    rt.frame(0.0);
    assert_eq!(rt.wake_deadline(), Some(2.5));
    rt.frame(2.0);
    assert_eq!(rt.clock, 0.2);
    rt.dev_clock.as_mut().unwrap().paused = true;
    assert_eq!(rt.wake_deadline(), None);
    rt.frame(20.0);
    assert_eq!(rt.clock, 0.2);
    for _ in 0..3 { rt.dev_clock.as_mut().unwrap().step(); }
    rt.frame(0.0);
    assert!((rt.clock - 0.25).abs() < 1e-12);
    assert_eq!(rt.wake_deadline(), None);
    rt.dev_clock.as_mut().unwrap().paused = false;
    rt.dev_clock.as_mut().unwrap().scale = 0.5;
    rt.update(|_, tx| tx.wake_after(std::time::Duration::from_millis(250)));
    rt.frame(0.0);
    assert_eq!(rt.wake_deadline(), Some(22.5));
    rt.frame(1.0);
    assert!(rt.state.iter().any(|t| (*t - 0.5).abs() < 1e-12), "coarse steps visit the logical timer deadline");
    assert!((rt.clock - 0.75).abs() < 1e-12);
}

#[test]
fn scopes_isolate_apps_restore_on_unwind_and_accept_css_colors() {
    let first = Arc::new(Mutex::new(BTreeMap::new()));
    let second = Arc::new(Mutex::new(BTreeMap::new()));
    scope(first.clone(), || {
        assert_eq!(tune("x", 2.0_f32, None), 2.0);
        first.lock().unwrap().get_mut("x").unwrap().value = json!(3);
        let panic = std::panic::catch_unwind(|| scope(second.clone(), || {
            assert_eq!(tune("x", 2.0_f32, None), 2.0);
            panic!("unwind nested scope");
        }));
        assert!(panic.is_err());
        assert_eq!(tune("x", 2.0_f32, None), 3.0);
    });
    assert_eq!(tune("x", 2.0_f32, None), 2.0);
    assert!(crate::Color::decode(&json!("oklch(65% 0.1 250 / 0.5)")).is_some());
    // knobs' colour control reads #rrggbbaa and writes oklch, with alpha when translucent.
    assert_eq!(crate::color::hex("#4a94db80").unwrap().encode(), json!("#4a94db80"));
    assert!(crate::Color::decode(&json!("oklch(0.65102 0.12977 249.632)")).is_some());
    assert!(crate::Color::decode(&json!("oklch(0.65102 0.12977 249.632 / 0.502)")).is_some());
    assert!(Spring::decode(&json!({"duration":1,"bounce":1})).is_none());
}

#[test]
fn a_command_after_idle_starts_at_its_arrival_and_cancelled_replays_stay_cancelled() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static ACTIONS: AtomicUsize = AtomicUsize::new(0);
    fn view(n: &f32, _: &mut Cx<f32>) -> El {
        crate::col().child(el().key("box").absolute(*n, 0.0).size(10.0, 10.0))
    }
    let spec = AppSpec {
        scenes: &[("left", |_| {}), ("right", |n| *n = 80.0)],
        ..AppSpec::new("test", (100.0, 100.0), || 0.0, view)
    };
    let mut config = Config::default().transitions([Transition::new("move", "left", |n, _| {
        ACTIONS.fetch_add(1, Ordering::Relaxed);
        *n = 80.0;
    })]);
    config.bind.set_port(0);
    let mut rt = Runtime::new(0.0, view, spec.size, Env { system_dark: true, scale: 1.0 });
    attach(&mut rt, &spec, &config, Arc::new(|| {})).unwrap();
    rt.frame(0.0);
    enqueue(&rt, json!({"type":"action","key":"rune.transition.move"}));
    rt.frame(100.0);
    assert_eq!(rt.state, 0.0);
    rt.frame(100.0);
    assert_eq!(rt.state, 80.0);
    assert_eq!(rt.presented("box").unwrap().0[0], 0.0, "idle time precedes the new trajectory");
    rt.frame(0.05);
    assert!(rt.presented("box").unwrap().0[0] > 0.0);
    enqueue(&rt, json!({"type":"action","key":"rune.transition.move"}));
    rt.frame(0.0);
    enqueue(&rt, json!({"type":"set","key":"rune.state","value":"left"}));
    rt.frame(0.0);
    rt.frame(1.0);
    assert_eq!(rt.state, 0.0);
    assert_eq!(ACTIONS.load(Ordering::Relaxed), 1, "a cancelled action must not run its side effects");
}

#[test]
fn a_busy_port_moves_on_and_a_full_range_turns_the_panel_off_without_panicking() {
    fn view(_: &f32, _: &mut Cx<f32>) -> El { el() }
    let spec = AppSpec::new("test", (100.0, 100.0), || 0.0, view);
    let runtime = || Runtime::new(0.0, view, spec.size, Env { system_dark: true, scale: 1.0 });
    let mut config = Config::default();
    config.bind.set_port(0);
    let mut first = runtime();
    let taken = launch(&mut first, &spec, config, Arc::new(|| {})).unwrap();
    let mut config = Config::default();
    config.bind = taken;
    let mut second = runtime();
    let next = launch(&mut second, &spec, config, Arc::new(|| {})).unwrap();
    assert!(next.port() > taken.port() && next.port() < taken.port() + 16);
    assert_eq!(second.dev_address(), Some(next));

    // Hold the whole fallback range; ports someone else holds are just as busy.
    let base = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let start = base.local_addr().unwrap();
    let _held: Vec<_> = (1..16).filter_map(|i| std::net::TcpListener::bind((start.ip(), start.port().checked_add(i)?)).ok()).collect();
    let mut config = Config::default();
    config.bind = start;
    let mut third = runtime();
    assert_eq!(launch(&mut third, &spec, config, Arc::new(|| {})), None);
    assert!(third.dev.is_none() && third.dev_clock.is_none());
}

#[test]
fn tunables_read_between_frames_reach_the_panel_and_follow_its_values() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    fn view(_: &f32, _: &mut Cx<f32>) -> El { el() }
    let spec = AppSpec::new("test", (100.0, 100.0), || 0.0, view);
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = wakes.clone();
    let mut rt = attached(&spec, 0.0, Arc::new(move || { counter.fetch_add(1, Ordering::Relaxed); }));
    rt.frame(0.0);
    // Like `Compositor::render`: after the frame, outside its scope.
    assert_eq!(tune("glow.radius", 2.5_f32, Some([0.0, 12.0, 0.5])), 2.5);
    assert_eq!(wakes.swap(0, Ordering::Relaxed), 1, "a control registered between frames asks for a snapshot");
    assert_eq!(tune("glow.radius", 2.5_f32, Some([0.0, 12.0, 0.5])), 2.5);
    assert_eq!(wakes.load(Ordering::Relaxed), 0);
    rt.frame(0.0);
    let snapshot = rt.dev.as_ref().unwrap().bridge.lock().unwrap().latest.clone();
    assert!(snapshot["descriptors"].as_array().unwrap().iter().any(|d| d["key"] == "glow.radius"));
    enqueue(&rt, json!({"type":"set","key":"glow.radius","value":8}));
    rt.frame(0.0);
    assert_eq!(tune("glow.radius", 2.5_f32, Some([0.0, 12.0, 0.5])), 8.0);
    drop(rt);
    assert_eq!(tune("glow.radius", 2.5_f32, Some([0.0, 12.0, 0.5])), 2.5, "a dropped session leaves no registry behind");
}

#[test]
fn hidden_hosts_keep_motion_in_real_time_and_the_tree_reports_opacity() {
    fn view(n: &f32, _: &mut Cx<f32>) -> El {
        crate::col().child(el().key("box").absolute(*n, 0.0).size(10.0, 10.0).opacity(0.5))
    }
    let spec = AppSpec::new("test", (100.0, 100.0), || 0.0, view);
    let mut config = Config::default();
    config.bind.set_port(0);
    let mut rt = Runtime::new(0.0, view, spec.size, Env { system_dark: true, scale: 1.0 });
    rt.wall_start = Some(std::time::Instant::now());
    attach(&mut rt, &spec, &config, Arc::new(|| {})).unwrap();
    rt.dev_hidden_frame();
    assert_eq!(rt.dev_hidden_deadline(), None, "at rest a hidden host sleeps");
    rt.update(|n, tx| {
        *n = 80.0;
        tx.motion(Spring::new(0.3, 0.0));
    });
    rt.dev_hidden_frame();
    assert!(rt.dev_hidden_deadline().is_some(), "running motion schedules the next hidden tick");
    std::thread::sleep(std::time::Duration::from_millis(40));
    rt.dev_hidden_frame();
    let x = rt.presented("box").unwrap().0[0];
    assert!(x > 0.0 && x < 80.0, "springs advance without presented frames: {x}");

    let replies = enqueue(&rt, json!({"type":"tree","id":1}));
    rt.dev_hidden_frame();
    let tree = replies.try_recv().unwrap();
    let card = tree["elements"].as_array().unwrap().iter().find(|e| e["key"] == "box").unwrap();
    assert_eq!(card["opacity"], 0.5);
    assert_eq!(card["moving"], true);

    for _ in 0..200 {
        if rt.dev_hidden_deadline().is_none() { break; }
        std::thread::sleep(std::time::Duration::from_millis(10));
        rt.dev_hidden_frame();
    }
    assert_eq!(rt.dev_hidden_deadline(), None);
    assert_eq!(rt.presented("box").unwrap().0[0], 80.0);
}

#[test]
fn a_windowless_timer_frame_keeps_the_motion_it_starts_running() {
    fn view(s: &(f32, u32), _: &mut Cx<(f32, u32)>) -> El {
        crate::col().child(el().key("box").absolute(s.0, 0.0).size(10.0, 10.0))
    }
    let spec = AppSpec::new("test", (100.0, 100.0), || (0.0, 0), view);
    let mut rt = attached(&spec, (0.0, 0), Arc::new(|| {}));
    rt.wall_start = Some(std::time::Instant::now());
    rt.poll = Some(|s, _, tx| {
        s.1 += 1;
        if s.1 == 1 { tx.wake_after(std::time::Duration::from_millis(5)); }
        if s.1 == 2 { s.0 = 80.0; tx.motion(Spring::new(0.3, 0.0)); }
        s.1 == 2
    });
    rt.frame(0.0);
    std::thread::sleep(std::time::Duration::from_millis(10));
    rt.hidden_timer_frame();
    assert_eq!(rt.state.0, 80.0, "the timer's poll ran");
    assert!(rt.dev_hidden_deadline().is_some(), "motion started by a timer schedules the next hidden tick");
    std::thread::sleep(std::time::Duration::from_millis(40));
    rt.dev_hidden_frame();
    let x = rt.presented("box").unwrap().0[0];
    assert!(x > 0.0 && x < 80.0, "the spring advances in real time: {x}");
}

#[test]
fn transition_closures_take_their_state_from_the_config() {
    #[allow(dead_code)]
    struct Ui { joined: bool }
    impl Ui {
        fn join(&mut self, tx: &mut Tx) { self.joined = true; tx.settle(); }
    }
    // Compiling is the proof: method calls and field writes need `S` before the closure is checked.
    let _ = Config::<Ui>::default()
        .transition("Join", "lobby", |s, tx| s.join(tx))
        .transition("Leave", "room", |s, _| s.joined = false);
}

#[test]
fn transitions_can_capture_scene_data() {
    fn view(_: &f32, _: &mut Cx<f32>) -> El { el() }
    let spec = AppSpec { scenes: &[("start", |_| {})], ..AppSpec::new("test", (100.0, 100.0), || 0.0, view) };
    let target = std::rc::Rc::new(std::cell::Cell::new(42.0_f32));
    let fixture = target.clone();
    let mut config = Config::<f32>::default().transition("Jump", "start", move |n, _| *n = fixture.get());
    config.bind.set_port(0);
    let mut rt = Runtime::new(0.0, view, spec.size, Env { system_dark: true, scale: 1.0 });
    attach(&mut rt, &spec, &config, Arc::new(|| {})).unwrap();
    rt.frame(0.0);
    target.set(7.0);
    enqueue(&rt, json!({"type":"action","key":"rune.transition.Jump"}));
    rt.frame(0.1);
    rt.frame(0.2);
    assert_eq!(rt.state, 7.0, "the action reads its captured data when it runs");
}

#[test]
fn scene_fns_build_against_the_running_clock() {
    fn view(_: &(f64, f64), _: &mut Cx<(f64, f64)>) -> El { el() }
    let spec = AppSpec {
        scenes: &[("start", |s| {
            let clock = crate::scene_clock();
            *s = (clock.now, clock.dt);
        })],
        ..AppSpec::new("test", (100.0, 100.0), || (-1.0, -1.0), view)
    };
    let mut rt = attached(&spec, (spec.init)(), Arc::new(|| {}));
    rt.dev_clock.as_mut().unwrap().scale = 0.5;
    rt.frame(0.0);
    rt.frame(1.0);
    enqueue(&rt, json!({"type":"set","key":"rune.state","value":"start"}));
    rt.frame(0.1);
    assert!((rt.state.0 - 0.55).abs() < 1e-12 && (rt.state.0 - rt.clock).abs() < 1e-12, "{:?} at {}", rt.state, rt.clock);
    assert!((rt.state.1 - 0.05).abs() < 1e-12, "the panel's speed scales dt: {:?}", rt.state);
    assert_eq!(crate::scene_clock(), crate::SceneClock::default(), "outside a scene the clock reads zero");
}

#[test]
fn the_embedded_page_matches_its_source() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/dev-panel");
    let mut hash = 0xcbf29ce484222325_u64;
    for name in ["index.ts", "package.json"] {
        for byte in std::fs::read(dir.join(name)).unwrap().into_iter().chain([0]) {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
    }
    let header = transport::PANEL_JS.lines().next().unwrap();
    assert!(header.ends_with(&format!("source {hash:016x}")), "stale embedded page ({header}); run bun scripts/dev-panel/build.ts");
}

#[test]
fn the_socket_port_serves_the_page_over_http() {
    use std::io::{Read, Write};
    fn view(_: &f32, _: &mut Cx<f32>) -> El { el() }
    let spec = AppSpec::new("test", (100.0, 100.0), || 0.0, view);
    let mut rt = attached(&spec, 0.0, Arc::new(|| {}));
    let address = rt.dev_address().unwrap();
    rt.frame(0.0);
    let send = |request: &[u8]| {
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream.write_all(request).unwrap();
        stream
    };
    let get = |request: &str| {
        let mut stream = send(request.as_bytes());
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        (head.lines().next().unwrap().to_owned(), head.to_owned(), body.to_owned())
    };
    let (status, _, page) = get(&format!("GET / HTTP/1.1\r\nHost: {address}\r\n\r\n"));
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(page.contains(r#"src="/panel.js""#));
    let (status, head, script) = get(&format!("GET /panel.js?v=1 HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\n\r\n"));
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(head.contains("text/javascript"));
    assert_eq!(script, transport::PANEL_JS);
    assert_eq!(get("GET /nope HTTP/1.1\r\nHost: localhost:7863\r\n\r\n").0, "HTTP/1.1 404 Not Found");
    assert_eq!(get("POST / HTTP/1.1\r\nHost: localhost:7863\r\nContent-Length: 0\r\n\r\n").0, "HTTP/1.1 405 Method Not Allowed");

    let status = |request: &[u8]| {
        let mut stream = send(request);
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let mut response = Vec::new();
        let mut byte = [0];
        while !response.ends_with(b"\r\n") && matches!(stream.read(&mut byte), Ok(1)) { response.push(byte[0]); }
        String::from_utf8(response).unwrap().trim_end().to_owned()
    };
    // A head that isn't UTF-8 is dropped rather than half-consumed.
    assert_eq!(status(b"GET /\xff HTTP/1.1\r\nHost: localhost\r\n\r\n"), "");

    // Another site's tab, or a rebound DNS name, gets nothing: no page and no socket.
    let upgrade = "GET /ws HTTP/1.1\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n";
    assert_eq!(status(format!("{upgrade}Host: {address}\r\nOrigin: https://evil.example\r\n\r\n").as_bytes()), "HTTP/1.1 403 Forbidden");
    assert_eq!(status(format!("{upgrade}Host: {address}\r\nOrigin: null\r\n\r\n").as_bytes()), "HTTP/1.1 403 Forbidden");
    assert_eq!(status(b"GET / HTTP/1.1\r\nHost: evil.example:7863\r\n\r\n"), "HTTP/1.1 403 Forbidden");
    assert_eq!(status(b"GET / HTTP/1.1\r\n\r\n"), "HTTP/1.1 403 Forbidden");
    assert_eq!(status(format!("{upgrade}Host: {address}\r\nOrigin: http://{address}\r\n\r\n").as_bytes()), "HTTP/1.1 101 Switching Protocols");
    for (head, allowed) in [
        ("Host: [::1]:7863", true),
        ("Host: host.example.ts.net:7863\r\nOrigin: https://host.example.ts.net:7863", true),
        ("Host: 100.64.0.1:4795\r\nOrigin: http://100.64.0.1:4795", true),
        ("Host: 100.128.0.1:7863", false),
        ("Host: 127.0.0.1.evil.example", false),
        ("Host: ts.net.evil.example", false),
        ("Host: 127.0.0.1:7863\r\nOrigin: http://127.0.0.1.evil.example", false),
    ] {
        assert_eq!(transport::same_machine_or_tailnet(&format!("GET / HTTP/1.1\r\n{head}\r\n\r\n")), allowed, "{head}");
    }

    // Native clients send no Origin.
    let stream = std::net::TcpStream::connect(address).unwrap();
    let (mut socket, _) = tungstenite::client(format!("ws://{address}/ws"), stream).unwrap();
    let tungstenite::Message::Text(text) = socket.read().unwrap() else { panic!("expected a state message") };
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["type"], "state");
}

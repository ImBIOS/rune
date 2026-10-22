use std::{
    cell::Cell,
    io::{self, Write},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[repr(C)]
struct Panel {
    width: u64,
    height: u64,
}

// repr(C) makes the append-only layout probe deterministic. It does NOT make
// accessing a new field on an existing old-layout instance safe.
#[repr(C)]
struct AppState {
    counter: u64,
    values: Vec<u64>,
    panel: Panel,
    // FIELD_PROBE
}

static GLOBAL: AtomicU64 = AtomicU64::new(1000);
thread_local! {
    static LOCAL: Cell<u64> = const { Cell::new(2000) };
}

trait Label {
    fn label(&self) -> String;
}

impl Label for Panel {
    fn label(&self) -> String {
        format!("t0:{}x{}", self.width, self.height)
    }
}

fn generic<T: std::fmt::Display>(value: T) -> String {
    format!("g0:{value}")
}

fn render(state: &AppState) -> String {
    let revision = 0; // REVISION
    let closure = |value: u64| value + 10;
    let global = GLOBAL.fetch_add(1, Ordering::Relaxed);
    let local = LOCAL.with(|cell| {
        let value = cell.get();
        cell.set(value + 1);
        value
    });
    format!(
        "rev={revision};value={};generic={};closure={};trait={};static={global};tls={local};layout={}",
        state.values[0],
        generic(state.counter),
        closure(state.counter),
        state.panel.label(),
        std::mem::size_of::<AppState>(),
    )
}

fn epoch_ns() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
}

fn main() {
    // The devtools connection includes the ASLR reference and PID handshake.
    // This binary never creates a Dioxus VirtualDom, UI, renderer, or window.
    subsecond::register_handler(std::sync::Arc::new(|| println!("PATCH ns={}", epoch_ns())));
    dioxus_devtools::connect_subsecond();
    let mut state = AppState {
        counter: 0,
        values: vec![73, 19, 41],
        panel: Panel { width: 800, height: 600 },
        // FIELD_INIT
    };
    let period = Duration::from_nanos(1_000_000_000 / 120);
    let mut next_tick = Instant::now();
    let capture_guard = 123_u64;
    loop {
        state.counter += 1;
        state.values[0] += 1;
        // 0.7.10 mistakes pointer-sized closures for function pointers. Capture
        // two references so this closure follows the closure dispatch path.
        let output = subsecond::call(|| {
            std::hint::black_box(&capture_guard);
            render(&state)
        });
        println!(
            "FRAME {{\"ns\":{},\"pid\":{},\"counter\":{},\"vec0\":{},\"vec_ptr\":{},\"state_addr\":{},\"panel_width\":{},\"output\":\"{}\"}}",
            epoch_ns(), std::process::id(), state.counter, state.values[0],
            state.values.as_ptr() as usize, &state as *const AppState as usize,
            state.panel.width, output,
        );
        io::stdout().flush().unwrap();
        next_tick += period;
        std::thread::sleep(next_tick.saturating_duration_since(Instant::now()));
    }
}

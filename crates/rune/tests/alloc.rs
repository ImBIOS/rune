//! Count the whole runtime frame, not only the spring evaluator. No GPU/driver allocations.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use rune::{Cx, El, Env, GrowthSpec, Runtime, ScrollSpec, col, text};

struct Counting;
thread_local! { static ALLOCS: Cell<usize> = const { Cell::new(0) }; }
fn bump() { let _ = ALLOCS.try_with(|c| c.set(c.get() + 1)); }
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 { bump(); unsafe { System.alloc(l) } }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 { bump(); unsafe { System.alloc_zeroed(l) } }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) { unsafe { System.dealloc(p, l) } }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 { bump(); unsafe { System.realloc(p, l, n) } }
}
#[global_allocator]
static A: Counting = Counting;
fn allocs() -> usize { ALLOCS.with(Cell::get) }

fn view(body: &String, _: &mut Cx<String>) -> El {
    col().w_full().h(400.0).scroll(ScrollSpec { stick_bottom: true }).child(
        col().w_full().virtualize(80.0).growth(GrowthSpec::default()).child(
            col().key("row").w_full().streaming(true).child(text(body.clone()).key("body").wrap().w_full().reveal(0.18))
        )
    )
}

#[test]
fn streaming_presentation_allocates_nothing() {
    let mut rt = Runtime::new("hello ".repeat(120), view, (400.0, 400.0), Env { system_dark: true, scale: 2.0 });
    rt.frame(0.0);
    rt.state.truncate(360);
    rt.settle_all();
    rt.frame(0.0);
    rt.state.push_str(&"hello ".repeat(60));
    rt.invalidate();
    // Commit/shaping is separately budgeted; warm glyphs and scratch capacity before measuring.
    for _ in 0..4 { rt.frame(1.0 / 120.0); }
    let before = allocs();
    for _ in 0..30 { assert!(rt.frame(1.0 / 120.0)); }
    let count = allocs() - before;
    assert_eq!(count, 0, "presentation allocations across 30 streaming frames");
}

//! Fixed headless fixtures for scripts/bench-ui.py --frame-cpu.
use rune::{AppSpec, Color, Cx, El, ScrollSpec, Spring, Tx, col, el, row, text};
use rune_kit::markdown::{Markdown, MarkdownStyle};
#[cfg(feature = "frame-profile")]
#[global_allocator]
static ALLOC: procwatch::alloc::Counting = procwatch::alloc::Counting::system();

#[derive(Default)]
struct State { mode: u8, tick: usize, moved: bool, markdown: String }

fn poll(s: &mut State, now: f64, tx: &mut Tx) -> bool {
    match s.mode {
        1 if !s.moved && now >= 0.01 => { s.moved = true; true }
        2 => {
            s.tick += 1;
            s.markdown.push_str(if s.tick % 20 == 0 { "\n\n" } else { "some **streamed** text " });
            true
        }
        3 => {
            s.tick += 1;
            tx.resize_window_instant((600.0 + (s.tick % 120) as f32 * 2.0, 700.0));
            false
        }
        _ => false,
    }
}

fn view(s: &State, _: &mut Cx<State>) -> El {
    let mut content = col().key("rows").w_full();
    if s.mode == 2 {
        content = content.child(Markdown::new(s.markdown.clone()).render(&MarkdownStyle::neutral(true)));
    } else {
        for i in 0..500 {
            content = content.child(row().key(format!("row-{i}")).w_full().h(28.0)
                .hover(Color::WHITE.alpha(0.1)).on_press(|_: &mut State| {})
                .child(text(format!("Setting {i}: a label that wraps when the viewport becomes narrow.")).wrap().font(rune::tok::type_::BODY)));
        }
    }
    col().fill(Color::BLACK)
        .child(el().key("spring").size(24.0, 24.0).ml(if s.moved { 400.0 } else { 0.0 })
            .fill(Color::WHITE).motion(Spring::new(8.0, 0.3)))
        .child(col().key("scroll").w_full().grow(1.0).scroll(ScrollSpec { stick_bottom: false }).child(content))
}

fn main() {
    rune::run(AppSpec::<State> {
        scenes: &[("idle", |_| {}), ("spring", |s| s.mode = 1),
            ("stream", |s| s.mode = 2), ("resize", |s| s.mode = 3)],
        poll: Some(poll),
        ..AppSpec::new("Frame CPU", (800.0, 700.0), State::default, view)
    });
}

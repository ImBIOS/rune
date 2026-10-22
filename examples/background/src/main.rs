//! An app that outlives its window: it starts with none, a background thread feeds it, and
//! commands on stdin open, focus, close and quit it. Closing the window keeps the app running;
//! reopening shows the same state (the ticks kept counting, the note kept its text).
//!
//! ```sh
//! cargo run -p background            # then type: open | activate TOKEN | close | quit
//! cargo run -p background -- --open  # start with the window
//! cargo run -p background -- --release-gpu  # closing also drops the GPU device
//! cargo run -p background -- --snap /tmp/background.png
//! ```

use std::io::BufRead;
use std::time::Duration;

use rune::tok::{self, radius, space, type_};
use rune::{App, Cx, El, Key, KeyEvent, TextEdit, Tx, col, el, row, text, text_input};

struct Background {
    ticks: u64,
    closes: u32,
    note: TextEdit,
}

fn init() -> Background {
    Background {
        ticks: 0,
        closes: 0,
        note: TextEdit::new("Type here, close, reopen"),
    }
}

fn button(key: &'static str, label: &'static str, t: tok::Theme, f: impl Fn(&mut Background, &mut Tx) + 'static) -> El {
    row()
        .key(key)
        .role("button")
        .label(label)
        .h(28.0)
        .px(space::S3)
        .center()
        .radius(radius::MD)
        .fill(t.card)
        .border(1.0, t.card_border)
        .on_press_tx(f)
        .child(text(label).font(type_::LABEL).color(t.text))
}

fn view(s: &Background, cx: &mut Cx<Background>) -> El {
    let t = tok::theme(cx.system_dark());
    col()
        .key("window")
        .fill(t.bg)
        .px(space::S6)
        .py(space::S6)
        .gap(space::S3)
        .child(text("Background app").key("title").font(type_::TITLE).color(t.text))
        .child(
            text(format!(
                "{} ticks from a background thread, window closed {} times",
                s.ticks, s.closes
            ))
            .key("ticks")
            .font(type_::BODY)
            .color(t.text_secondary),
        )
        .child(
            el().key("note-field")
                .w_full()
                .h(32.0)
                .px(space::S2)
                .radius(radius::MD)
                .fill(t.card)
                .border(1.0, t.card_border)
                .center_y()
                .child(
                    text_input(&s.note, type_::BODY)
                        .key("note")
                        .color(t.text)
                        .on_edit(|s: &mut Background| &mut s.note),
                ),
        )
        .child(
            row()
                .key("actions")
                .gap(space::S2)
                .child(button("close", "Close window", t, |_, tx| tx.close_window()))
                .child(button("quit", "Quit", t, |_, tx| tx.quit())),
        )
}

/// Cmd/Ctrl-W closes the window, Cmd/Ctrl-Q quits.
fn on_key(_: &mut Background, ev: &KeyEvent, tx: &mut Tx) -> bool {
    match ev.key {
        Key::Command('w') => tx.close_window(),
        Key::Command('q') => tx.quit(),
        _ => return false,
    }
    true
}

fn on_close(s: &mut Background, _: &mut Tx) {
    s.closes += 1;
    eprintln!("background: window closed; still running");
}

fn main() {
    let mut app = App::new(rune::AppSpec {
        title: "Background",
        size: (480.0, 420.0),
        init,
        view,
        scenes: &[("ticked", |s| s.ticks = 42)],
        fonts: &[],
        families: ("Inter", "Inter"),
        setup: None,
        poll: None,
        on_key: Some(on_key),
    })
    .keep_running()
    .on_close(on_close);
    if !std::env::args().any(|a| a == "--open") {
        app = app.windowless();
    }
    if std::env::args().any(|a| a == "--release-gpu") {
        app = app.release_gpu_when_closed();
    }
    let ticker = app.handle();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(1));
            ticker.update(|s, _| s.ticks += 1);
        }
    });
    let commands = app.handle();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            let mut words = line.split_whitespace();
            match (words.next(), words.next()) {
                (Some("open"), _) => commands.open(),
                (Some("activate"), Some(token)) => commands.activate(token),
                (Some("close"), _) => commands.close(),
                (Some("quit"), _) => commands.quit(),
                (Some(other), _) => {
                    eprintln!("background: unknown command {other:?} (open | activate TOKEN | close | quit)")
                }
                (None, _) => {}
            }
        }
    });
    app.run();
}

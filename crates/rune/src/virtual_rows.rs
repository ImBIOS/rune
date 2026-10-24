//! Index-built virtual rows: [`El::virtual_rows`](crate::El::virtual_rows) gives a virtual
//! list a row count, a key per index and a builder per index. Layout builds only the rows in
//! the mounted window, so a commit costs a key check per row plus the rows it mounts, however
//! long the list is.

use std::any::Any;
use std::borrow::Cow;

use crate::El;

pub(crate) type KeyFn = dyn for<'a> Fn(&'a dyn Any, usize) -> Cow<'a, str>;
pub(crate) type BuildFn = dyn Fn(&dyn Any, usize) -> El;

/// The rows of an index-built list, built at layout time from the window's state.
pub(crate) struct RowSource {
    pub(crate) count: usize,
    pub(crate) key: Box<KeyFn>,
    pub(crate) build: Box<BuildFn>,
}

fn key_fn<F: for<'a> Fn(&'a dyn Any, usize) -> Cow<'a, str>>(f: F) -> F {
    f
}

fn state<S: 'static>(s: &dyn Any) -> &S {
    s.downcast_ref::<S>().expect("virtual rows registered for a different state type than the window's")
}

impl RowSource {
    pub(crate) fn new<S: 'static>(
        count: usize,
        key: impl for<'a> Fn(&'a S, usize) -> Cow<'a, str> + 'static,
        build: impl Fn(&S, usize) -> El + 'static,
    ) -> Self {
        RowSource {
            count,
            key: Box::new(key_fn(move |s, i| key(state::<S>(s), i))),
            build: Box::new(move |s, i| build(state::<S>(s), i)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::time::Instant;

    use crate::{Cx, El, Env, GrowthSpec, Runtime, ScrollSpec, col, memo, text};

    thread_local! {
        static VIEWS: Cell<usize> = const { Cell::new(0) };
        static VIEW_NS: Cell<u128> = const { Cell::new(0) };
        static BUILT: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        /// `virtualize` with every row a `memo` the view makes.
        Memo,
        /// `virtual_rows`, each built row a `memo`.
        Index,
    }

    struct Chat {
        ids: Vec<u64>,
        keys: Vec<String>,
        tick: u64,
        markdown: bool,
        mode: Mode,
    }

    impl Chat {
        fn new(n: u64, markdown: bool, mode: Mode) -> Self {
            let mut chat = Chat { ids: Vec::new(), keys: Vec::new(), tick: 0, markdown, mode };
            chat.splice(0, (0..n).map(|i| 100_000 + i));
            chat
        }

        fn splice(&mut self, at: usize, ids: impl Iterator<Item = u64>) {
            let ids: Vec<u64> = ids.collect();
            self.keys.splice(at..at, ids.iter().map(|id| format!("m{id}")));
            self.ids.splice(at..at, ids);
        }
    }

    fn body(id: u64, markdown: bool) -> El {
        if !markdown {
            return col().w_full().h(72.0).padding(8.0).gap(4.0).children([
                text(format!("Message {id}")).key("title"),
                text(format!("Body of message {id}, a line of text that is long enough to wrap twice")).wrap().w_full().key("body"),
                text(format!("{id} min ago")).key("time"),
            ]);
        }
        let para = |n: u64| format!("Paragraph {n} of message {id}: {}", "some prose with `code`, **bold** words and links to wrap ".repeat((id % 4 + 1) as usize));
        col().w_full().padding(8.0).gap(6.0).children([
            text(format!("## Heading for message {id}")).key("h"),
            text(para(1)).wrap().w_full().key("p1"),
            text(para(2)).wrap().w_full().key("p2"),
            text(format!("fn message_{id}() -> u64 {{ {id} }}")).key("code"),
        ])
    }

    fn view(s: &Chat, _: &mut Cx<Chat>) -> El {
        let start = Instant::now();
        let stack = col().key("rows").w_full().growth(GrowthSpec::default());
        let stack = match s.mode {
            Mode::Memo => stack.virtualize(72.0).children(s.ids.iter().map(|&id| memo(format!("m{id}"), 0, || body(id, s.markdown)))),
            Mode::Index => stack.virtual_rows(s.ids.len(), 72.0, |s: &Chat, i| s.keys[i].as_str().into(), |s: &Chat, i| {
                BUILT.with_borrow_mut(|b| b.push(i));
                memo(s.keys[i].as_str(), 0, || body(s.ids[i], s.markdown))
            }),
        };
        let root = col().w_full().h_full().children([
            text(format!("tick {}", s.tick)).key("counter"),
            col().key("scroll").w_full().grow(1.0).scroll(ScrollSpec { stick_bottom: true }).child(stack),
        ]);
        VIEWS.set(VIEWS.get() + 1);
        VIEW_NS.set(VIEW_NS.get() + start.elapsed().as_nanos());
        root
    }

    fn commit(rt: &mut Runtime<Chat>) {
        rt.invalidate();
        rt.frame(1.0 / 120.0);
    }

    fn chat(n: u64, markdown: bool, mode: Mode) -> Runtime<Chat> {
        let mut rt = Runtime::new(Chat::new(n, markdown, mode), view, (600.0, 800.0), Env::new(true, 2.0));
        rt.frame(0.0);
        rt
    }

    fn built() -> Vec<usize> {
        let mut b = BUILT.with_borrow_mut(std::mem::take);
        b.sort_unstable();
        b.dedup();
        b
    }

    /// Visible rows' keys and rects.
    fn visible(rt: &Runtime<Chat>) -> Vec<(String, [f32; 4])> {
        rt.elements().into_iter()
            .filter(|e| e.visible[3] > 0.0)
            .filter_map(|e| Some((e.path.strip_prefix("#0/scroll/rows/").filter(|k| !k.contains('/'))?.to_owned(), e.rect)))
            .collect()
    }

    #[test]
    fn only_the_window_builds_and_scrolling_builds_what_comes_in() {
        let mut rt = chat(5000, false, Mode::Index);
        let first = built();
        assert!(!first.is_empty() && first.len() < 60, "built {} rows", first.len());
        assert!(first.iter().all(|&i| i >= 4900), "pinned to the bottom, the tail builds: {first:?}");
        assert!(rt.nodes.len() < 400, "{} nodes", rt.nodes.len());
        let (count, commits) = (first.len(), rt.stats.commits);
        for _ in 0..5 {
            rt.state.tick += 1;
            commit(&mut rt);
        }
        let ticks = built();
        assert!(rt.stats.commits >= commits + 5);
        assert!(ticks.len() <= count, "a tick builds the window only: {}", ticks.len());

        for _ in 0..80 {
            rt.wheel(300.0, 400.0, 400.0);
            rt.frame(1.0 / 120.0);
        }
        assert!(built().iter().any(|&i| i < 4900), "scrolling builds rows it brings in");
        rt.state.tick += 1;
        commit(&mut rt);
        let top = rt.state.keys.iter().position(|k| visible(&rt)[0].0 == *k).unwrap();
        assert!(top < 4600, "{top}");
        // The window is the viewport and two viewports either side; rows here are 72 points.
        let now = built();
        assert!(now.iter().all(|&i| i + 25 >= top && i <= top + 36), "only rows near the viewport build: {now:?} vs top {top}");
    }

    #[test]
    fn index_rows_lay_out_as_virtualize_does_while_paging_and_prepending() {
        let mut a = chat(600, true, Mode::Memo);
        let mut b = chat(600, true, Mode::Index);
        let same = |a: &Runtime<Chat>, b: &Runtime<Chat>, step: &str| {
            let (va, vb) = (visible(a), visible(b));
            assert!(!va.is_empty(), "{step}");
            assert_eq!(va.len(), vb.len(), "{step}");
            for ((ka, ra), (kb, rb)) in va.iter().zip(&vb) {
                assert_eq!(ka, kb, "{step}");
                assert!(ra.iter().zip(rb).all(|(x, y)| (x - y).abs() < 0.05), "{step}: {ka} {ra:?} vs {rb:?}");
            }
        };
        for step in 0..120 {
            same(&a, &b, &format!("step {step}"));
            let dy = if step < 80 { 211.5 } else { -97.0 };
            for rt in [&mut a, &mut b] {
                rt.wheel(300.0, 400.0, dy);
                rt.frame(1.0 / 120.0);
            }
        }
        let before = visible(&b);
        for rt in [&mut a, &mut b] {
            let first = rt.state.ids[0];
            rt.state.splice(0, first - 60..first);
            commit(rt);
        }
        same(&a, &b, "prepended");
        let after = visible(&b);
        assert_eq!(before[0].0, after[0].0, "prepending keeps the reader's row");
        assert!((before[0].1[1] - after[0].1[1]).abs() < 0.05, "in place: {:?} vs {:?}", before[0], after[0]);
        for _ in 0..40 {
            for rt in [&mut a, &mut b] {
                rt.wheel(300.0, 400.0, 3000.0);
                rt.frame(1.0 / 120.0);
            }
        }
        same(&a, &b, "scrolled into the prepended page");
        assert_eq!(visible(&b)[0].0, "m99940", "reached the top");
    }

    #[test]
    fn a_page_prepended_above_the_reader_at_the_top_keeps_them_in_place() {
        let mut rt = chat(300, true, Mode::Index);
        for _ in 0..20 {
            rt.wheel(300.0, 400.0, 100_000.0);
            rt.frame(1.0 / 120.0);
        }
        let before = visible(&rt);
        assert_eq!(before[0].0, "m100000", "at the top");
        built();
        let first = rt.state.ids[0];
        rt.state.splice(0, first - 60..first);
        commit(&mut rt);
        let after = visible(&rt);
        assert_eq!(before.len(), after.len());
        for ((kb, rb), (ka, ra)) in before.iter().zip(&after) {
            assert_eq!(kb, ka);
            assert!(rb.iter().zip(ra).all(|(x, y)| (x - y).abs() < 0.05), "{kb} moved: {rb:?} vs {ra:?}");
        }
        let page = built();
        assert!(page.iter().all(|&i| i < 60 + 40), "{page:?}");
        // The page above mounts a little per frame, not two viewports of new rows at once.
        assert!(page.iter().filter(|&&i| i < 60).count() <= 10, "{page:?}");
        for _ in 0..10 {
            rt.frame(1.0 / 120.0);
        }
        assert_eq!(visible(&rt), after, "and nothing drifts after");
    }

    #[test]
    fn scroll_into_view_reaches_a_row_never_built() {
        let mut rt = chat(5000, true, Mode::Index);
        built();
        rt.update(|s, tx| tx.scroll_into_view(s.keys[10].clone()));
        for _ in 0..3 {
            rt.frame(1.0 / 120.0);
        }
        let shown = visible(&rt);
        assert!(shown.iter().any(|(k, _)| *k == rt.state.keys[10]), "{:?}", shown.iter().map(|(k, _)| k).collect::<Vec<_>>());
        // The update's own commit, before the reveal, builds the tail it was at.
        let now = built();
        assert!(now.iter().all(|&i| i < 60 || i >= 4900), "only rows near it build: {now:?}");
    }

    #[test]
    fn heights_persist_for_rows_scrolled_away() {
        let mut rt = chat(800, true, Mode::Index);
        let key = rt.state.keys[795].clone();
        let measured = visible(&rt).into_iter().find(|(k, _)| *k == key).expect("near the tail").1[3];
        assert!((measured - 72.0).abs() > 1.0, "a markdown row isn't the estimate: {measured}");
        for _ in 0..10 {
            rt.wheel(300.0, 400.0, 2000.0);
            rt.frame(1.0 / 120.0);
        }
        assert!(rt.find(&key).is_none(), "scrolled away, unbuilt");
        let v = &rt.virtuals["#0/scroll/rows"];
        assert_eq!(v.height(&key), measured, "its height stays measured");
        assert_eq!(v.height(&rt.state.keys[400]), 72.0, "a row never seen is estimated");
    }

    #[test]
    fn a_focused_field_in_a_row_scrolled_out_keeps_typing() {
        use crate::{Key, KeyEvent, TextEdit, TextStyle, text_input};
        fn view(s: &Vec<TextEdit>, _: &mut Cx<Vec<TextEdit>>) -> El {
            col().key("viewport").size(200.0, 200.0).scroll(ScrollSpec { stick_bottom: false }).clip_soft(0.0)
                .child(col().key("rows").w_full().growth(GrowthSpec::default()).virtual_rows(s.len(), 40.0,
                    |_: &Vec<TextEdit>, i| format!("row-{i}").into(),
                    |s: &Vec<TextEdit>, i| col().size(200.0, 40.0).child(text_input(&s[i], TextStyle::new(14.0, 400.0)).key("field")
                        .size(200.0, 40.0).on_edit(move |s: &mut Vec<TextEdit>| &mut s[i]))))
        }
        let mut state: Vec<_> = (0..1000).map(|_| TextEdit::new("")).collect();
        state[0].focused = true;
        let mut rt = Runtime::new(state, view, (200.0, 200.0), Env::new(true, 1.0));
        rt.frame(0.0);
        rt.wheel(100.0, 100.0, -20_000.0);
        rt.frame(0.0);
        assert!(rt.find("row-0/field").is_none(), "the focused row scrolled out");
        rt.key(KeyEvent { key: Key::Text("offscreen".into()), mods: Default::default() });
        rt.frame(0.0);
        assert_eq!(rt.state[0].text, "offscreen");
        assert!(rt.state[0].focused);
        rt.wheel(100.0, 100.0, 20_000.0);
        rt.frame(0.0);
        assert!(rt.find("row-0/field").is_some(), "back in view");
    }

    #[test]
    fn memo_rows_build_once_while_they_stay_in_the_window() {
        thread_local! { static BUILDS: Cell<usize> = const { Cell::new(0) }; }
        fn view(s: &(u32, u32), _: &mut Cx<(u32, u32)>) -> El {
            col().w_full().h_full().children([
                text(format!("tick {}", s.0)).key("counter"),
                col().key("scroll").w_full().grow(1.0).scroll(ScrollSpec { stick_bottom: true }).child(
                    col().key("rows").w_full().growth(GrowthSpec::default()).virtual_rows(s.1 as usize, 40.0,
                        |_: &(u32, u32), i| format!("r{i}").into(),
                        |_: &(u32, u32), i| memo(format!("r{i}"), 0, || {
                            BUILDS.set(BUILDS.get() + 1);
                            text(format!("row {i}")).h(40.0)
                        }))),
            ])
        }
        let mut rt = Runtime::new((0, 2000), view, (300.0, 400.0), Env::default());
        rt.frame(0.0);
        let first = BUILDS.get();
        assert!(first > 0 && first < 60, "{first}");
        for _ in 0..10 {
            rt.state.0 += 1;
            rt.invalidate();
            rt.frame(1.0 / 60.0);
        }
        assert_eq!(BUILDS.get(), first, "ticks reuse every mounted row");
        rt.state.1 += 1;
        rt.invalidate();
        rt.frame(1.0 / 60.0);
        assert!(rt.find("r2000").is_some(), "an appended row mounts at the pinned tail");
        assert_eq!(BUILDS.get(), first + 1, "and only it builds");
    }

    /// `cargo test -p rune --profile perf --lib virtual_rows_time -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn virtual_rows_time() {
        for mode in [Mode::Memo, Mode::Index] {
            let name = if mode == Mode::Memo { "memo " } else { "index" };
            for n in [500, 2000, 5000] {
                let mut rt = chat(n, false, mode);
                for _ in 0..30 {
                    rt.state.tick += 1;
                    commit(&mut rt);
                }
                let frames = 300;
                VIEW_NS.set(0);
                let start = Instant::now();
                for _ in 0..frames {
                    rt.state.tick += 1;
                    commit(&mut rt);
                }
                let per = start.elapsed().as_secs_f64() * 1e6 / f64::from(frames);
                let view = VIEW_NS.get() as f64 / 1e3 / f64::from(frames);
                println!("{name} tick rows {n:5}: {per:8.1} µs per frame ({view:7.1} µs in the view)");
            }

            // Older messages load at the top while the reader is there.
            for _ in 0..3 {
                let mut rt = chat(3700, true, mode);
                for _ in 0..10 {
                    rt.wheel(300.0, 400.0, 100_000.0);
                    rt.frame(1.0 / 120.0);
                }
                let (views, shaped) = (VIEWS.get(), rt.text.shaped);
                VIEW_NS.set(0);
                let first = rt.state.ids[0];
                rt.state.splice(0, first - 60..first);
                let start = Instant::now();
                commit(&mut rt);
                let ms = start.elapsed().as_secs_f64() * 1e3;
                // An index-built list mounts the rest of the page above over the next frames.
                let mut after = 0.0f64;
                for _ in 0..30 {
                    let start = Instant::now();
                    rt.frame(1.0 / 120.0);
                    after = after.max(start.elapsed().as_secs_f64() * 1e3);
                }
                println!("{name} prepend 60 to 3700: {ms:6.2} ms, {} view runs ({:.2} ms), {} shaped, {} nodes; slowest frame after {after:.2} ms",
                    VIEWS.get() - views, VIEW_NS.get() as f64 / 1e6, rt.text.shaped - shaped, rt.nodes.len());
            }

            // Reading up through history: which frames are slow, and do they shape?
            let mut rt = chat(3700, true, mode);
            let mut slow = Vec::new();
            let mut times = Vec::new();
            for f in 0..400 {
                rt.wheel(300.0, 400.0, 30.0);
                let (shaped, commits, views) = (rt.text.shaped, rt.stats.commits, VIEWS.get());
                VIEW_NS.set(0);
                let start = Instant::now();
                rt.frame(1.0 / 120.0);
                let us = start.elapsed().as_secs_f64() * 1e6;
                times.push(us);
                if us > 1000.0 {
                    slow.push(format!("f{f}: {us:.0} µs, {} commits, {} views ({:.0} µs), {} shaped",
                        rt.stats.commits - commits, VIEWS.get() - views, VIEW_NS.get() as f64 / 1e3, rt.text.shaped - shaped));
                }
            }
            times.sort_by(f64::total_cmp);
            println!("{name} scroll 400 frames: p50 {:.0} µs, p90 {:.0} µs, max {:.0} µs; {} over 1 ms:", times[200], times[360], times[399], slow.len());
            for s in slow.iter().take(6) { println!("  {s}"); }
        }
    }
}

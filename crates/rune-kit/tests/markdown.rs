#![cfg(feature = "markdown")]

use rune::{Cx, El, Env, Runtime, col};
use rune_kit::markdown::{Block, Style, parse, parse_cached};
use rune_kit::{Kit, Markdown, MarkdownStyle};
use std::rc::Rc;

fn view(src: &String, _: &mut Cx<String>) -> El {
    col().w_full().child(Markdown::new(src.clone()).build(&Kit::neutral(true)))
}

fn laid_out(src: &str) -> Runtime<String> {
    let mut rt = Runtime::new(src.to_owned(), view, (480.0, 800.0), Env::new(true, 1.0));
    rt.frame(0.0);
    rt
}

fn by_key(rt: &Runtime<String>, key: &str) -> rune::runtime::ElementInfo {
    rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap_or_else(|| panic!("no {key}"))
}

fn keys(rt: &Runtime<String>) -> Vec<String> {
    rt.elements().into_iter().filter_map(|e| e.key).collect()
}

const DOC: &str = "# Title\n\nSome `code`, *em*, **bold** and [a link](https://x.dev/a).\n\n#### Small\n\n- one\n- two\n\n3. three\n\n> quoted\n\n---\n\n```rust\nfn main() {}\n```\n";

#[test]
fn parses_each_block_type() {
    let b = parse(DOC);
    assert!(matches!(&b[0], Block::Heading(1, i) if i.text == "Title"));
    let Block::Para(p) = &b[1] else { panic!("{b:?}") };
    assert_eq!(p.text, "Some \u{202f}code\u{202f}, em, bold and a link.");
    let styles: Vec<Style> = p.spans.iter().map(|s| s.2).collect();
    assert_eq!(styles, [Style::CodePad, Style::Code, Style::CodePad, Style::Em, Style::Strong, Style::Link]);
    assert_eq!(p.links.len(), 1);
    let (a, z, url) = &p.links[0];
    assert_eq!((&p.text[*a..*z], url.as_str()), ("a link", "https://x.dev/a"));
    assert!(matches!(&b[2], Block::Heading(4, _)));
    assert!(matches!(&b[3], Block::List { start: None, items } if items.len() == 2));
    assert!(matches!(&b[4], Block::List { start: Some(3), items } if items.len() == 1));
    assert!(matches!(&b[5], Block::Quote(q) if matches!(&q[0], Block::Para(i) if i.text == "quoted")));
    assert!(matches!(&b[6], Block::Rule));
    assert!(matches!(&b[7], Block::Code(c) if c == "fn main() {}"));
}

#[test]
fn an_unclosed_fence_is_a_growing_code_block() {
    let b = parse("Look:\n\n```ts\nconst a = 1\n");
    assert!(matches!(&b[1], Block::Code(text) if text == "const a = 1"));
}

#[test]
fn renders_each_block_type_with_kit_tokens() {
    let k = Kit::neutral(true);
    let rt = laid_out(DOC);
    let title = by_key(&rt, "b0");
    assert_eq!(title.text.as_deref(), Some("Title"));
    assert_eq!(title.text_color, Some(k.c.text_base));
    assert_eq!(by_key(&rt, "b1").text_color, Some(k.c.text_base));
    assert_eq!(by_key(&rt, "b2").text_color, Some(k.c.text_muted), "level 4+ headings are muted");
    assert!(keys(&rt).iter().filter(|k| *k == "i0").count() == 2 && keys(&rt).iter().filter(|k| *k == "i1").count() == 1);
    let texts: Vec<_> = rt.elements().into_iter().filter_map(|e| e.text).collect();
    for want in ["•", "one", "two", "3.", "three", "quoted", "fn main() {}"] {
        assert!(texts.iter().any(|t| t == want), "{want} in {texts:?}");
    }
    assert_eq!(by_key(&rt, "b6").rect[3], 0.5, "a rule is a half-point line");
    let code = by_key(&rt, "b7");
    assert_eq!(code.fill, k.c.layer_01);
    assert!(title.rect[1] < by_key(&rt, "b1").rect[1] && by_key(&rt, "b1").rect[1] < code.rect[1]);
}

#[test]
fn style_builders_override_kit_tokens() {
    let red = rune::Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };
    let k = Kit::neutral(true);
    fn styled(src: &String, _: &mut Cx<String>) -> El {
        let k = Kit::neutral(true);
        let red = rune::Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };
        Markdown::new(src.clone()).style(MarkdownStyle::from_kit(&k).text(red).code_block(red, red)).build(&k)
    }
    let mut rt = Runtime::new(DOC.to_owned(), styled, (480.0, 800.0), Env::new(true, 1.0));
    rt.frame(0.0);
    assert_eq!(by_key(&rt, "b0").text_color, Some(red));
    assert_eq!(by_key(&rt, "b7").fill, red);
    assert_ne!(k.c.text_base, red);
}

#[test]
fn a_streaming_append_keeps_earlier_blocks_mounted() {
    let steps = ["# Plan\n\nFirst para", "# Plan\n\nFirst para is done.\n\n- a", "# Plan\n\nFirst para is done.\n\n- a\n- b\n\n```sh\nls", "# Plan\n\nFirst para is done.\n\n- a\n- b\n\n```sh\nls -la\n"];
    let parsed: Vec<_> = steps.iter().map(|s| parse(s)).collect();
    assert_eq!(parsed[2][0], parsed[3][0]);
    assert_eq!(parsed[1][0], parsed[3][0], "the heading block never changes while later text arrives");

    let mut rt = laid_out(steps[0]);
    let mut seen = keys(&rt);
    let head = by_key(&rt, "b0").path.clone();
    for step in &steps[1..] {
        rt.state = (*step).to_owned();
        rt.invalidate();
        rt.frame(1.0 / 60.0);
        let now = keys(&rt);
        for k in &seen {
            assert!(now.contains(k), "{k} survives {step:?}");
        }
        assert_eq!(by_key(&rt, "b0").path, head);
        seen = now;
    }
    assert!(seen.iter().any(|k| k == "b3"), "{seen:?}");
}

/// Every prefix of a nested document parses to the same blocks as before the parser's
/// container handling was shared (a hash of the debug output, captured on the old code).
#[test]
fn every_streaming_prefix_parses_like_before() {
    let doc = "# T\n\n> quote *em [link](https://x.dev) **bold\n>\n> - in `quote`\n>   1. deep\n\n- a\n  > b\n- c **d\n\n```\ncode\n";
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for end in (0..=doc.len()).filter(|&i| doc.is_char_boundary(i)) {
        for byte in format!("{:?}", parse(&doc[..end])).bytes() {
            h ^= u64::from(byte);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    assert_eq!(h, 3_224_808_071_872_330_980, "golden hash");
}

#[test]
fn identical_source_parses_once() {
    let a = parse_cached("some **cached** text");
    let b = parse_cached("some **cached** text");
    assert!(Rc::ptr_eq(&a, &b));
    let c = parse_cached("some other text");
    assert!(!Rc::ptr_eq(&a, &c));
}

struct Reader {
    src: String,
    handled: bool,
    opened: Vec<String>,
}

fn reader(s: &Reader, _: &mut Cx<Reader>) -> El {
    let md = Markdown::new(s.src.clone());
    let md = if s.handled { md.on_link(|s: &mut Reader, url| s.opened.push(url.to_owned())) } else { md };
    col().w_full().padding(10.0).child(md.build(&Kit::neutral(true)))
}

const LINKS: &str = "See [one](https://a.dev) and [two](https://b.dev).\n\n- an item with [three](https://c.dev)\n";

#[test]
fn links_press_through_on_link() {
    let mut rt = Runtime::new(Reader { src: LINKS.into(), handled: true, opened: Vec::new() }, reader, (480.0, 400.0), Env::new(true, 1.0));
    rt.frame(0.0);
    let find = |rt: &Runtime<Reader>, key: &str| {
        rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap_or_else(|| panic!("no {key}"))
    };
    let (one, two, three) = (find(&rt, "https://a.dev"), find(&rt, "https://b.dev"), find(&rt, "https://c.dev"));
    assert_eq!((one.role, one.label.as_deref()), ("link", Some("one")));
    assert_eq!(three.label.as_deref(), Some("three"));
    assert!(one.rect[0] + one.rect[2] < two.rect[0] && two.rect[1] < three.rect[1], "{one:?} {two:?} {three:?}");

    // A click on the link's text opens its destination; the plain words beside it don't.
    let centre = |e: &rune::runtime::ElementInfo| (e.rect[0] + e.rect[2] / 2.0, e.rect[1] + e.rect[3] / 2.0);
    let click = |rt: &mut Runtime<Reader>, (x, y): (f32, f32)| {
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(1.0 / 60.0);
    };
    rt.pointer_move(centre(&two).0, centre(&two).1);
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.cursor(), rune::Cursor::Pointer);
    click(&mut rt, centre(&two));
    click(&mut rt, centre(&three));
    click(&mut rt, ((one.rect[0] + one.rect[2] + two.rect[0]) / 2.0, centre(&one).1));
    assert_eq!(rt.state.opened, ["https://b.dev", "https://c.dev"]);
    // `--script` taps by key through `find`, which resolves a link by its destination.
    let at = rt.find("https://c.dev").expect("a link is found by its destination");
    click(&mut rt, at);
    assert_eq!(rt.state.opened.pop().as_deref(), Some("https://c.dev"));

    // Links are Tab stops in reading order, opened with Enter.
    rt.key(rune::KeyEvent { key: rune::Key::Tab, mods: Default::default() });
    rt.frame(1.0 / 60.0);
    rt.key(rune::KeyEvent { key: rune::Key::Enter, mods: Default::default() });
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.opened, ["https://b.dev", "https://c.dev", "https://a.dev"]);

    // Without a handler, links are styled text: no stops, no hand.
    let mut rt = Runtime::new(Reader { src: LINKS.into(), handled: false, opened: Vec::new() }, reader, (480.0, 400.0), Env::new(true, 1.0));
    rt.frame(0.0);
    assert!(rt.elements().iter().all(|e| !e.path.contains("#link-") && !e.focusable));
    rt.pointer_move(centre(&two).0, centre(&two).1);
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.cursor(), rune::Cursor::Default);
}

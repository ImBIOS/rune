//! Markdown to blocks of styled text (pulldown-cmark). Partial input while streaming is fine:
//! an unclosed fence is a code block that keeps growing.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Style {
    Code,
    /// The narrow no-break spaces around inline code (CSS's `padding: 0 4px`).
    CodePad,
    Strong,
    Em,
    Link,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inline {
    pub text: String,
    /// Byte ranges into `text`.
    pub spans: Vec<(usize, usize, Style)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Para(Inline),
    Heading(u8, Inline),
    Code(String),
    List { start: Option<u64>, items: Vec<Vec<Block>> },
    Quote(Vec<Block>),
    Rule,
}

struct Builder {
    /// Open containers: (kind, blocks so far).
    stack: Vec<(Container, Vec<Block>)>,
    inline: Option<(InlineKind, Inline, Vec<(usize, Style)>)>,
    code: Option<String>,
}

enum Container {
    Root,
    Quote,
    List(Option<u64>, Vec<Vec<Block>>),
    Item,
}

#[derive(Clone, Copy)]
enum InlineKind {
    Para,
    Heading(u8),
}

impl Builder {
    fn push(&mut self, b: Block) {
        self.stack.last_mut().unwrap().1.push(b);
    }

    fn inline(&mut self) -> &mut (InlineKind, Inline, Vec<(usize, Style)>) {
        // Tight list items have text without a paragraph.
        self.inline.get_or_insert_with(|| (InlineKind::Para, Inline::default(), Vec::new()))
    }

    fn flush_inline(&mut self) {
        if let Some((kind, mut inl, open)) = self.inline.take() {
            for (start, st) in open {
                inl.spans.push((start, inl.text.len(), st));
            }
            let trimmed = inl.text.trim_end().len();
            inl.text.truncate(trimmed);
            inl.spans.retain_mut(|s| {
                s.1 = s.1.min(trimmed);
                s.0 < s.1
            });
            if inl.text.is_empty() {
                return;
            }
            self.push(match kind {
                InlineKind::Para => Block::Para(inl),
                InlineKind::Heading(l) => Block::Heading(l, inl),
            });
        }
    }
}

const PAD: &str = "\u{202f}";

pub fn parse(src: &str) -> Vec<Block> {
    let mut b = Builder { stack: vec![(Container::Root, Vec::new())], inline: None, code: None };
    let opts = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    for ev in Parser::new_ext(src, opts) {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    b.flush_inline();
                    b.inline = Some((InlineKind::Para, Inline::default(), Vec::new()));
                }
                Tag::Heading { level, .. } => {
                    b.flush_inline();
                    let l = match level {
                        HeadingLevel::H1 => 1,
                        HeadingLevel::H2 => 2,
                        HeadingLevel::H3 => 3,
                        _ => 4,
                    };
                    b.inline = Some((InlineKind::Heading(l), Inline::default(), Vec::new()));
                }
                Tag::CodeBlock(_) => {
                    b.flush_inline();
                    b.code = Some(String::new());
                }
                Tag::BlockQuote(_) => {
                    b.flush_inline();
                    b.stack.push((Container::Quote, Vec::new()));
                }
                Tag::List(start) => {
                    b.flush_inline();
                    b.stack.push((Container::List(start, Vec::new()), Vec::new()));
                }
                Tag::Item => {
                    b.flush_inline();
                    b.stack.push((Container::Item, Vec::new()));
                }
                Tag::Strong => open(&mut b, Style::Strong),
                Tag::Emphasis => open(&mut b, Style::Em),
                Tag::Link { .. } => open(&mut b, Style::Link),
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph | TagEnd::Heading(_) => b.flush_inline(),
                TagEnd::CodeBlock => {
                    if let Some(mut text) = b.code.take() {
                        if text.ends_with('\n') {
                            text.pop();
                        }
                        b.push(Block::Code(text));
                    }
                }
                TagEnd::BlockQuote(_) => {
                    b.flush_inline();
                    if let Some((_, blocks)) = b.stack.pop() {
                        b.push(Block::Quote(blocks));
                    }
                }
                TagEnd::Item => {
                    b.flush_inline();
                    if let Some((_, blocks)) = b.stack.pop()
                        && let Some((Container::List(_, items), _)) = b.stack.last_mut()
                    {
                        items.push(blocks);
                    }
                }
                TagEnd::List(_) => {
                    b.flush_inline();
                    if let Some((Container::List(start, items), _)) = b.stack.pop() {
                        b.push(Block::List { start, items });
                    }
                }
                TagEnd::Strong | TagEnd::Emphasis | TagEnd::Link => close(&mut b),
                _ => {}
            },
            Event::Text(t) => {
                if let Some(code) = &mut b.code {
                    code.push_str(&t);
                } else {
                    b.inline().1.text.push_str(&t);
                }
            }
            Event::Code(t) => {
                let inl = &mut b.inline().1;
                let a = inl.text.len();
                inl.text.push_str(PAD);
                let start = inl.text.len();
                inl.text.push_str(&t);
                let end = inl.text.len();
                inl.text.push_str(PAD);
                inl.spans.push((a, start, Style::CodePad));
                inl.spans.push((start, end, Style::Code));
                inl.spans.push((end, inl.text.len(), Style::CodePad));
            }
            Event::SoftBreak => b.inline().1.text.push(' '),
            Event::HardBreak => b.inline().1.text.push('\n'),
            Event::Rule => {
                b.flush_inline();
                b.push(Block::Rule);
            }
            _ => {}
        }
    }
    b.flush_inline();
    if let Some(text) = b.code.take() {
        b.push(Block::Code(text.trim_end_matches('\n').to_owned()));
    }
    // Unwind containers left open by a partial stream.
    while b.stack.len() > 1 {
        let (c, blocks) = b.stack.pop().unwrap();
        match c {
            Container::Item => {
                if let Some((Container::List(_, items), _)) = b.stack.last_mut() {
                    items.push(blocks);
                }
            }
            Container::List(start, items) => b.push(Block::List { start, items }),
            Container::Quote => b.push(Block::Quote(blocks)),
            Container::Root => {}
        }
    }
    b.stack.pop().unwrap().1
}

fn open(b: &mut Builder, st: Style) {
    let inl = b.inline();
    let at = inl.1.text.len();
    inl.2.push((at, st));
}

fn close(b: &mut Builder) {
    let inl = b.inline();
    if let Some((start, st)) = inl.2.pop() {
        let end = inl.1.text.len();
        inl.1.spans.push((start, end, st));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks() {
        let b = parse("# Title\n\nSome `code` and **bold**.\n\n- one\n- two\n\n```rust\nfn main() {}\n```\n");
        assert!(matches!(&b[0], Block::Heading(1, i) if i.text == "Title"));
        let Block::Para(p) = &b[1] else { panic!("{b:?}") };
        assert_eq!(p.text, "Some \u{202f}code\u{202f} and bold.");
        assert_eq!(p.spans, vec![(5, 8, Style::CodePad), (8, 12, Style::Code), (12, 15, Style::CodePad), (20, 24, Style::Strong)]);
        assert!(matches!(&b[2], Block::List { items, .. } if items.len() == 2));
        assert!(matches!(&b[3], Block::Code(text) if text == "fn main() {}"));
    }

    #[test]
    fn partial_fence_is_code() {
        let b = parse("Look:\n\n```ts\nconst a = 1\n");
        assert!(matches!(&b[1], Block::Code(text) if text == "const a = 1"));
    }
}

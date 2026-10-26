//! Rich-text composer with an inline and block formatting bar and per-frame syntax decorations.
//!
//! Run headless to capture the demo still:
//! `cargo run -p rune-kit --example rich_editor -- --snap demo/kit/rich-editor.png`

use rune::tok::type_;
use rune::{AppSpec, Block, Cx, El, Mark, Span, TextEdit, col, el, row, text};
use rune_kit::syntax::{SyntaxTheme, highlight};
use rune_kit::{Button, ButtonVariant, ControlSize, Divider, Kit, TextArea};

struct State {
    edit: TextEdit,
}

impl Default for State {
    fn default() -> Self {
        let html = concat!(
            "<p>Ship the <b>attributed</b> composer with <i>inline marks</i>, ",
            "<u>underline</u>, <del>legacy markdown</del>, <code>Span</code> ",
            "decorations, and <a href=\"https://opencode.ai\">opencode.ai</a>.</p>",
            "<ul>",
            "<li>Byte-range marks shift and split on edits",
            "<ul><li>Nested list items indent with <code>Tab</code></li></ul>",
            "</li>",
            "</ul>",
            "<ol><li>Paste safe HTML or plain text</li><li>Export marks and blocks</li></ol>",
            "<blockquote>Formatting lives alongside the string, never inside it.</blockquote>",
            "<pre><code>fn count(lang: &amp;str, src: &amp;str) -&gt; usize {\n",
            "\tlet spans = highlight(Some(lang), src);\n",
            "\tspans.len()\n",
            "}</code></pre>",
        );
        let mut edit = TextEdit::from_html(html);
        edit.focused = true;
        // Place the caret at the end of "attributed" so the Bold button shows active.
        edit.set_caret(19);
        Self { edit }
    }
}

/// Syntax-highlight every `Block::Code` paragraph run using `rune_kit::syntax::highlight`.
fn code_block_spans(edit: &TextEdit, theme: &SyntaxTheme) -> Vec<Span> {
    let mut out = Vec::new();
    let paras = edit.paragraphs();
    let mut i = 0;
    while i < paras.len() {
        if paras[i].block == Block::Code {
            let start = paras[i].range.start;
            let mut end = paras[i].range.end;
            while i + 1 < paras.len() && paras[i + 1].block == Block::Code {
                i += 1;
                end = paras[i].range.end;
            }
            if let Some(code) = edit.text.get(start..end) {
                let ranges = highlight(Some("rust"), code);
                out.extend(theme.spans(&ranges, start));
            }
        }
        i += 1;
    }
    out
}

fn bar_btn(
    k: &Kit,
    key: &str,
    label: &str,
    active: bool,
    on_press: impl Fn(&mut State) + 'static,
) -> El {
    Button::new(label)
        .control_size(ControlSize::Small)
        .variant(if active { ButtonVariant::Contrast } else { ButtonVariant::Ghost })
        .on_press(on_press)
        .build(k)
        .key(key)
}

fn view(s: &State, _: &mut Cx<State>) -> El {
    let k = Kit::neutral(true);
    let syntax = SyntaxTheme::from_kit(&k);
    let spans = code_block_spans(&s.edit, &syntax);
    let blk = s.edit.current_block();

    let bar = row()
        .key("toolbar")
        .gap(4.0)
        .center_y()
        .child(bar_btn(&k, "bold", "B", s.edit.has_mark(&Mark::Bold), |s| {
            s.edit.toggle_mark(Mark::Bold);
        }))
        .child(bar_btn(&k, "italic", "I", s.edit.has_mark(&Mark::Italic), |s| {
            s.edit.toggle_mark(Mark::Italic);
        }))
        .child(bar_btn(&k, "underline", "U", s.edit.has_mark(&Mark::Underline), |s| {
            s.edit.toggle_mark(Mark::Underline);
        }))
        .child(bar_btn(&k, "strike", "S", s.edit.has_mark(&Mark::Strike), |s| {
            s.edit.toggle_mark(Mark::Strike);
        }))
        .child(bar_btn(&k, "code", "<>", s.edit.has_mark(&Mark::Code), |s| {
            s.edit.toggle_mark(Mark::Code);
        }))
        .child(bar_btn(&k, "link", "Link", s.edit.active_link().is_some(), |s| {
            let next = if s.edit.active_link().is_some() { None } else { Some("https://opencode.ai") };
            s.edit.set_link(next);
        }))
        .child(el().w(4.0))
        .child(bar_btn(&k, "bullet", "• List", matches!(blk, Block::Bullet(_)), |s| {
            s.edit.toggle_block(Block::Bullet(0));
        }))
        .child(bar_btn(&k, "numbered", "1. List", matches!(blk, Block::Numbered(_)), |s| {
            s.edit.toggle_block(Block::Numbered(0));
        }))
        .child(bar_btn(&k, "quote", "Quote", blk == Block::Quote, |s| {
            s.edit.toggle_block(Block::Quote);
        }))
        .child(bar_btn(&k, "codeblock", "Code", blk == Block::Code, |s| {
            s.edit.toggle_block(Block::Code);
        }));

    let composer = col()
        .w_full()
        .radius(k.radius.lg)
        .fill(k.c.layer_01)
        .border(1.0, k.c.border_base)
        .padding(10.0)
        .gap(8.0)
        .child(bar)
        .child(Divider::horizontal().build(&k))
        .child(
            TextArea::new(&s.edit)
                .width(None)
                .rows(11)
                .decorations(spans)
                .on_edit(|s: &mut State| &mut s.edit)
                .build(&k),
        );

    col()
        .w_full()
        .h_full()
        .fill(k.c.page)
        .padding(20.0)
        .gap(10.0)
        .child(text("Rich composer").font(type_::TITLE).color(k.c.text_base))
        .child(composer)
}

fn main() {
    // Code marks and code blocks draw in the mono slot.
    rune::run(AppSpec {
        fonts: &[
            include_bytes!("../../rune/assets/Inter-Regular.ttf"),
            include_bytes!("../../rune/assets/Inter-Medium.ttf"),
            include_bytes!("../../rune/assets/Inter-SemiBold.ttf"),
            include_bytes!("../../rune/assets/IBMPlexMono-Regular.ttf"),
        ],
        families: ("Inter", "IBM Plex Mono"),
        ..AppSpec::new("Rich editor", (560.0, 380.0), State::default, view)
    });
}

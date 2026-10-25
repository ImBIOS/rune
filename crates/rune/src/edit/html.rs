//! Safe HTML subset import and export for [`TextEdit`].
//!
//! Supported elements:
//! - Inline: `<b>`, `<strong>`, `<i>`, `<em>`, `<u>`, `<ins>`, `<s>`, `<del>`, `<strike>`,
//!   `<a href="...">`, `<code>`, and CSS `style` attributes (`font-weight`, `font-style`,
//!   `text-decoration`, `text-decoration-line`).
//! - Block: `<p>`, `<div>`, `<br>`, `<ul>`, `<ol>`, `<li>`, `<blockquote>`, `<pre>`.
//! - Stripped with content: `<script>`, `<style>`, `<head>`.
//! - Unknown tags drop their wrapper and keep their text. Nothing is executed, and dangerous
//!   link schemes (`javascript:`, `vbscript:`, `data:`) are rejected.

use std::ops::Range;

use super::{Block, MAX_LIST_LEVEL, Mark, MarkRange, TextEdit};

#[derive(Clone, Copy, PartialEq, Eq)]
enum ListKind {
    Bullet,
    Numbered,
}

#[derive(Clone)]
struct ActiveMark {
    mark: Mark,
    start: usize,
}

/// Safe URL check for `<a href="...">`: rejects executable or script schemes.
fn is_safe_href(url: &str) -> bool {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower: String = trimmed
        .chars()
        .filter(|c| !c.is_control() && !c.is_whitespace())
        .take(24)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    !lower.starts_with("javascript:")
        && !lower.starts_with("vbscript:")
        && !lower.starts_with("data:")
}

/// Decodes HTML entities (`&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;`, `&nbsp;`, `&#123;`, `&#x1F600;`).
pub(crate) fn decode_entities(input: &str) -> String {
    if !input.contains('&') {
        return input.to_owned();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp + 1..];
        let Some(semi) = rest.find(';') else {
            out.push('&');
            continue;
        };
        let entity = &rest[..semi];
        if entity.is_empty() || entity.len() > 32 || entity.contains(char::is_whitespace) {
            out.push('&');
            continue;
        }
        let decoded = if let Some(hex) = entity.strip_prefix("#x").or_else(|| entity.strip_prefix("#X")) {
            u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
        } else if let Some(dec) = entity.strip_prefix('#') {
            dec.parse::<u32>().ok().and_then(char::from_u32)
        } else {
            match entity {
                "amp" | "AMP" => Some('&'),
                "lt" | "LT" => Some('<'),
                "gt" | "GT" => Some('>'),
                "quot" | "QUOT" => Some('"'),
                "apos" | "#39" => Some('\''),
                "nbsp" | "NBSP" => Some('\u{00a0}'),
                "copy" => Some('©'),
                "mdash" => Some('—'),
                "ndash" => Some('–'),
                "hellip" => Some('…'),
                "lsquo" => Some('‘'),
                "rsquo" => Some('’'),
                "ldquo" => Some('“'),
                "rdquo" => Some('”'),
                "bull" => Some('•'),
                _ => None,
            }
        };
        if let Some(ch) = decoded {
            out.push(ch);
            rest = &rest[semi + 1..];
        } else {
            out.push('&');
        }
    }
    out.push_str(rest);
    out
}

fn escape_html(s: &str, out: &mut String) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
}

/// Extracts an HTML attribute's value from a tag tail (`href="..."`, `style='...'`, `href=foo`).
fn extract_attr(attrs: &str, name: &str) -> Option<String> {
    let bytes = attrs.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let key_start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'-' | b'_' | b':')) {
            i += 1;
        }
        if i == key_start {
            i += 1;
            continue;
        }
        let key = &attrs[key_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let val = if bytes[i] == b'"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            i += 1;
            let val_start = i;
            while i < bytes.len() && bytes[i] != quote {
                i += 1;
            }
            let v = &attrs[val_start..i];
            if i < bytes.len() {
                i += 1;
            }
            v
        } else {
            let val_start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' && bytes[i] != b'/' {
                i += 1;
            }
            &attrs[val_start..i]
        };
        if key.eq_ignore_ascii_case(name) {
            return Some(decode_entities(val));
        }
    }
    None
}

/// Parses inline CSS `style="..."` for `font-weight`, `font-style`, and `text-decoration`.
fn parse_style_marks(style: &str, add: &mut Vec<Mark>, remove: &mut Vec<Mark>) {
    for decl in style.split(';') {
        let Some((prop, val)) = decl.split_once(':') else { continue };
        let prop = prop.trim();
        let val = val.trim();
        if prop.eq_ignore_ascii_case("font-weight") {
            let v = val.split_whitespace().next().unwrap_or("");
            if v.eq_ignore_ascii_case("bold")
                || v.eq_ignore_ascii_case("bolder")
                || v.parse::<f32>().is_ok_and(|w| w >= 600.0)
            {
                add.push(Mark::Bold);
            } else if v.eq_ignore_ascii_case("normal")
                || v.eq_ignore_ascii_case("lighter")
                || v.parse::<f32>().is_ok_and(|w| w < 600.0)
            {
                remove.push(Mark::Bold);
            }
        } else if prop.eq_ignore_ascii_case("font-style") {
            let v = val.split_whitespace().next().unwrap_or("");
            if v.eq_ignore_ascii_case("italic") || v.eq_ignore_ascii_case("oblique") {
                add.push(Mark::Italic);
            } else if v.eq_ignore_ascii_case("normal") {
                remove.push(Mark::Italic);
            }
        } else if prop.eq_ignore_ascii_case("text-decoration")
            || prop.eq_ignore_ascii_case("text-decoration-line")
        {
            for tok in val.split_whitespace() {
                if tok.eq_ignore_ascii_case("underline") {
                    add.push(Mark::Underline);
                } else if tok.eq_ignore_ascii_case("line-through") {
                    add.push(Mark::Strike);
                } else if tok.eq_ignore_ascii_case("none") {
                    remove.push(Mark::Underline);
                    remove.push(Mark::Strike);
                }
            }
        }
    }
}

struct TagFrame {
    tag: String,
    added_marks: Vec<Mark>,
    suppressed_marks: Vec<Mark>,
}

pub(crate) fn parse_html(html: &str) -> TextEdit {
    let mut text = String::new();
    let mut marks: Vec<MarkRange> = Vec::new();
    let mut blocks: Vec<Block> = vec![Block::Paragraph];
    let mut soft_breaks: Vec<u32> = Vec::new();

    let mut stack: Vec<TagFrame> = Vec::new();
    let mut active_marks: Vec<ActiveMark> = Vec::new();
    let mut list_stack: Vec<ListKind> = Vec::new();
    let mut in_pre: usize = 0;
    let mut in_quote: usize = 0;
    let mut in_li: usize = 0;
    // Whether the current line has been claimed by a block tag or text, whether that block is
    // still open awaiting its first content, and whether text/br has been written on this line.
    let mut line_has_block = false;
    let mut line_open = false;
    let mut line_has_content = false;

    let current_default_block = |list_stack: &[ListKind], in_pre: usize, in_quote: usize, in_li: usize| -> Block {
        if in_pre > 0 {
            Block::Code
        } else if in_li > 0 && let Some(&kind) = list_stack.last() {
            let level = (list_stack.len() - 1).min(MAX_LIST_LEVEL as usize) as u8;
            match kind {
                ListKind::Bullet => Block::Bullet(level),
                ListKind::Numbered => Block::Numbered(level),
            }
        } else if in_quote > 0 {
            Block::Quote
        } else {
            Block::Paragraph
        }
    };

    let flush_active_marks = |active: &mut [ActiveMark], marks: &mut Vec<MarkRange>, pos: usize, in_pre: usize| {
        if in_pre == 0 {
            for a in active.iter_mut() {
                if a.start < pos {
                    marks.push(MarkRange {
                        start: a.start,
                        end: pos,
                        mark: a.mark.clone(),
                    });
                }
                a.start = pos;
            }
        } else {
            for a in active.iter_mut() {
                a.start = pos;
            }
        }
    };

    let start_new_paragraph = |text: &mut String,
                               blocks: &mut Vec<Block>,
                               active: &mut [ActiveMark],
                               marks: &mut Vec<MarkRange>,
                               line_has_block: &mut bool,
                               line_open: &mut bool,
                               line_has_content: &mut bool,
                               block: Block,
                               in_pre: usize| {
        if (!*line_has_block || *line_open) && !*line_has_content {
            if let Some(last) = blocks.last_mut() {
                *last = block;
            }
            *line_has_block = true;
            *line_open = true;
            return;
        }
        let pos = text.len();
        flush_active_marks(active, marks, pos, in_pre);
        text.push('\n');
        blocks.push(block);
        let new_pos = text.len();
        for a in active.iter_mut() {
            a.start = new_pos;
        }
        *line_has_block = true;
        *line_open = true;
        *line_has_content = false;
    };

    let mut i = 0;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'<' {
            // Comment `<!-- ... -->`
            if html[i..].starts_with("<!--") {
                if let Some(end) = html[i + 4..].find("-->") {
                    i += 4 + end + 3;
                } else {
                    break;
                }
                continue;
            }
            // Processing instruction or doctype `<!...>` / `<?...>`
            if html[i..].starts_with("<!") || html[i..].starts_with("<?") {
                if let Some(end) = html[i + 2..].find('>') {
                    i += 2 + end + 1;
                } else {
                    break;
                }
                continue;
            }
            let Some(close_angle) = html[i + 1..].find('>') else {
                // Unclosed `<`, treat as literal text
                let decoded = decode_entities(&html[i..]);
                text.push_str(&decoded);
                break;
            };
            let raw_tag = html[i + 1..i + 1 + close_angle].trim();
            i += 1 + close_angle + 1;

            if raw_tag.is_empty() {
                continue;
            }
            let is_closing = raw_tag.starts_with('/');
            let self_closing = raw_tag.ends_with('/');
            let body = raw_tag.trim_start_matches('/').trim_end_matches('/').trim();
            let tag_end = body
                .find(|c: char| c.is_whitespace() || c == '/')
                .unwrap_or(body.len());
            let tag = body[..tag_end].to_ascii_lowercase();
            let attrs = &body[tag_end..];

            if !is_closing && matches!(tag.as_str(), "script" | "style" | "head") {
                if !self_closing {
                    // Skip until matching `</tag>` (case-insensitive)
                    let pat = format!("</{tag}");
                    let mut search = i;
                    let mut found = None;
                    while search + pat.len() <= bytes.len() {
                        if html[search..].len() >= pat.len()
                            && html.is_char_boundary(search)
                            && html.is_char_boundary(search + pat.len())
                            && html[search..search + pat.len()].eq_ignore_ascii_case(&pat)
                        {
                            if let Some(gt) = html[search + pat.len()..].find('>') {
                                found = Some(search + pat.len() + gt + 1);
                                break;
                            }
                        }
                        search += 1;
                    }
                    i = found.unwrap_or(bytes.len());
                }
                continue;
            }

            if !is_closing && tag == "br" {
                let pos = text.len();
                flush_active_marks(&mut active_marks, &mut marks, pos, in_pre);
                let blk = blocks.last().copied().unwrap_or(Block::Paragraph);
                text.push('\n');
                blocks.push(blk);
                if in_pre == 0 {
                    soft_breaks.push((blocks.len() - 1) as u32);
                }
                let new_pos = text.len();
                for a in &mut active_marks {
                    a.start = new_pos;
                }
                line_has_block = true;
                line_open = true;
                line_has_content = false;
                continue;
            }

            if !is_closing {
                match tag.as_str() {
                    "ul" => {
                        list_stack.push(ListKind::Bullet);
                        if !line_has_content {
                            line_open = true;
                        } else {
                            line_open = false;
                        }
                    }
                    "ol" => {
                        list_stack.push(ListKind::Numbered);
                        if !line_has_content {
                            line_open = true;
                        } else {
                            line_open = false;
                        }
                    }
                    "li" => {
                        in_li += 1;
                        let blk = current_default_block(&list_stack, in_pre, in_quote, in_li);
                        start_new_paragraph(
                            &mut text,
                            &mut blocks,
                            &mut active_marks,
                            &mut marks,
                            &mut line_has_block,
                            &mut line_open,
                            &mut line_has_content,
                            blk,
                            in_pre,
                        );
                    }
                    "blockquote" => {
                        in_quote += 1;
                        let blk = current_default_block(&list_stack, in_pre, in_quote, in_li);
                        start_new_paragraph(
                            &mut text,
                            &mut blocks,
                            &mut active_marks,
                            &mut marks,
                            &mut line_has_block,
                            &mut line_open,
                            &mut line_has_content,
                            blk,
                            in_pre,
                        );
                    }
                    "pre" => {
                        in_pre += 1;
                        start_new_paragraph(
                            &mut text,
                            &mut blocks,
                            &mut active_marks,
                            &mut marks,
                            &mut line_has_block,
                            &mut line_open,
                            &mut line_has_content,
                            Block::Code,
                            in_pre,
                        );
                    }
                    "p" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        let blk = current_default_block(&list_stack, in_pre, in_quote, in_li);
                        start_new_paragraph(
                            &mut text,
                            &mut blocks,
                            &mut active_marks,
                            &mut marks,
                            &mut line_has_block,
                            &mut line_open,
                            &mut line_has_content,
                            blk,
                            in_pre,
                        );
                    }
                    _ => {}
                }

                let mut to_add = Vec::new();
                let mut to_suppress = Vec::new();
                match tag.as_str() {
                    "b" | "strong" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => to_add.push(Mark::Bold),
                    "i" | "em" => to_add.push(Mark::Italic),
                    "u" | "ins" => to_add.push(Mark::Underline),
                    "s" | "del" | "strike" => to_add.push(Mark::Strike),
                    "code" if in_pre == 0 => to_add.push(Mark::Code),
                    "a" => {
                        if let Some(href) = extract_attr(attrs, "href")
                            && is_safe_href(&href)
                        {
                            to_add.push(Mark::Link(href.trim().to_owned()));
                        }
                    }
                    _ => {}
                }
                if let Some(style) = extract_attr(attrs, "style") {
                    parse_style_marks(&style, &mut to_add, &mut to_suppress);
                }

                let pos = text.len();
                let mut frame = TagFrame {
                    tag: tag.clone(),
                    added_marks: Vec::new(),
                    suppressed_marks: Vec::new(),
                };

                for rem in to_suppress {
                    if let Some(idx) = active_marks.iter().position(|a| a.mark.same_kind(&rem)) {
                        let popped = active_marks.remove(idx);
                        if in_pre == 0 && popped.start < pos {
                            marks.push(MarkRange {
                                start: popped.start,
                                end: pos,
                                mark: popped.mark.clone(),
                            });
                        }
                        frame.suppressed_marks.push(popped.mark);
                    }
                }
                for m in to_add {
                    if let Some(idx) = active_marks.iter().position(|a| a.mark.same_kind(&m)) {
                        if active_marks[idx].mark != m {
                            let old = active_marks.remove(idx);
                            if in_pre == 0 && old.start < pos {
                                marks.push(MarkRange {
                                    start: old.start,
                                    end: pos,
                                    mark: old.mark.clone(),
                                });
                            }
                            frame.suppressed_marks.push(old.mark);
                            active_marks.push(ActiveMark { mark: m.clone(), start: pos });
                            frame.added_marks.push(m);
                        }
                    } else {
                        active_marks.push(ActiveMark { mark: m.clone(), start: pos });
                        frame.added_marks.push(m);
                    }
                }

                if !self_closing {
                    stack.push(frame);
                }
            } else {
                // Closing tag
                let pos = text.len();
                if let Some(frame_idx) = stack.iter().rposition(|f| f.tag == tag) {
                    while stack.len() > frame_idx {
                        let frame = stack.pop().unwrap();
                        for m in frame.added_marks.into_iter().rev() {
                            if let Some(idx) = active_marks.iter().rposition(|a| a.mark == m) {
                                let ended = active_marks.remove(idx);
                                if in_pre == 0 && ended.start < pos {
                                    marks.push(MarkRange {
                                        start: ended.start,
                                        end: pos,
                                        mark: ended.mark,
                                    });
                                }
                            }
                        }
                        for sup in frame.suppressed_marks {
                            if !active_marks.iter().any(|a| a.mark.same_kind(&sup)) {
                                active_marks.push(ActiveMark { mark: sup, start: pos });
                            }
                        }
                        match frame.tag.as_str() {
                            "ul" | "ol" => {
                                list_stack.pop();
                                line_open = false;
                            }
                            "li" => {
                                in_li = in_li.saturating_sub(1);
                                line_open = false;
                            }
                            "blockquote" => {
                                in_quote = in_quote.saturating_sub(1);
                                line_open = false;
                            }
                            "pre" => {
                                in_pre = in_pre.saturating_sub(1);
                                line_open = false;
                            }
                            "p" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                                line_open = false;
                            }
                            _ => {}
                        }
                    }
                }
            }
        } else {
            // Plain text node up to next `<`
            let next_lt = html[i..].find('<').map_or(bytes.len(), |off| i + off);
            let raw_slice = &html[i..next_lt];
            i = next_lt;
            let decoded = decode_entities(raw_slice);
            if in_pre > 0 {
                // Inside `<pre>`, preserve whitespace and split newlines into `Block::Code` paragraphs.
                let mut chars = decoded.chars().peekable();
                while let Some(c) = chars.next() {
                    if c == '\r' {
                        if chars.peek() == Some(&'\n') {
                            continue;
                        }
                        text.push('\n');
                        blocks.push(Block::Code);
                        line_has_block = true;
                        line_open = false;
                        line_has_content = false;
                    } else if c == '\n' {
                        // Ignore a single leading newline immediately after `<pre>`
                        if line_open && !line_has_content {
                            line_open = false;
                            continue;
                        }
                        text.push('\n');
                        blocks.push(Block::Code);
                        line_has_block = true;
                        line_open = false;
                        line_has_content = false;
                    } else {
                        text.push(c);
                        line_has_block = true;
                        line_open = false;
                        line_has_content = true;
                    }
                }
            } else {
                // Outside `<pre>`: ignore pure inter-block whitespace if we're between closed blocks and a `<` follows
                if !line_open && decoded.chars().all(char::is_whitespace) && i < bytes.len() && (!line_has_block || line_has_content) {
                    // Wait: if we just closed `<p>foo</p>\n  <p>bar</p>`, `line_open` is false and `decoded` is `\n  `.
                    continue;
                }
                if line_open && !line_has_content && decoded.chars().all(char::is_whitespace) && i < bytes.len() && html[i..].starts_with('<') {
                    // Check if the next tag is a block child like `<p>` or `<li>` inside `<blockquote>` or `<ul>`
                    let rest_tag = html[i + 1..].trim_start();
                    let next_is_block = ["p", "div", "ul", "ol", "li", "blockquote", "pre", "h1", "h2", "h3", "h4", "h5", "h6"]
                        .iter()
                        .any(|b| {
                            rest_tag.len() >= b.len()
                                && rest_tag.is_char_boundary(b.len())
                                && rest_tag[..b.len()].eq_ignore_ascii_case(b)
                                && rest_tag[b.len()..].starts_with(|c: char| c.is_whitespace() || c == '>' || c == '/')
                        });
                    if next_is_block {
                        continue;
                    }
                }
                if !line_open && !decoded.is_empty() && (!line_has_block || line_has_content) {
                    let blk = current_default_block(&list_stack, in_pre, in_quote, in_li);
                    if line_has_block {
                        // Loose text after a closed block tag (e.g. `<p>A</p>B`)
                        // Wait: if `active_marks` is non-empty, we are inside an inline tag like `<b>A</b> B`!
                        // Only start a new paragraph if we're NOT inside an inline tag and the previous block closed.
                        if stack.iter().all(|f| matches!(f.tag.as_str(), "ul" | "ol" | "li" | "blockquote" | "body" | "html")) {
                            let pos = text.len();
                            let line_start = text.rfind('\n').map_or(0, |k| k + 1);
                            if pos > line_start && !line_has_content {
                                // shouldn't happen
                            }
                        }
                    } else if let Some(last) = blocks.last_mut() {
                        *last = blk;
                        line_has_block = true;
                        line_open = true;
                    }
                }
                // Collapse ASCII whitespace runs (newlines/tabs/spaces) to single spaces in HTML flow
                let line_start = text.rfind('\n').map_or(0, |k| k + 1);
                let mut prev_space = text.len() == line_start || text.ends_with(' ');
                for ch in decoded.chars() {
                    if ch == '\u{00a0}' {
                        text.push('\u{00a0}');
                        prev_space = false;
                        line_has_block = true;
                        line_open = true;
                        line_has_content = true;
                    } else if ch.is_whitespace() {
                        if !prev_space {
                            text.push(' ');
                            prev_space = true;
                            line_has_block = true;
                            line_open = true;
                            line_has_content = true;
                        }
                    } else {
                        text.push(ch);
                        prev_space = false;
                        line_has_block = true;
                        line_open = true;
                        line_has_content = true;
                    }
                }
            }
        }
    }

    let end_pos = text.len();
    flush_active_marks(&mut active_marks, &mut marks, end_pos, in_pre);

    // Strip a single trailing newline inside `<pre>` if it left an empty trailing line
    if blocks.len() > 1
        && blocks.last() == Some(&Block::Code)
        && text.ends_with('\n')
        && !soft_breaks.contains(&((blocks.len() - 1) as u32))
    {
        text.pop();
        blocks.pop();
    }

    let mut edit = TextEdit::from_parts(text, marks, blocks);
    edit.soft_breaks = soft_breaks;
    edit
}

/// Serializes byte range `range` of `edit` to HTML.
pub(crate) fn serialize_html(edit: &TextEdit, range: Range<usize>) -> String {
    let start = range.start.min(edit.text.len());
    let end = range.end.min(edit.text.len()).max(start);
    if start == end && edit.blocks().iter().all(|&b| b == Block::Paragraph) {
        return String::new();
    }

    // Collect paragraphs intersecting `start..end`
    let mut paras: Vec<(usize, Range<usize>, Block, bool)> = Vec::new();
    for p in edit.paragraphs() {
        let p_end_with_nl = if p.range.end < edit.text.len() { p.range.end + 1 } else { p.range.end };
        if p.range.start <= end && p_end_with_nl >= start {
            let sub_start = p.range.start.max(start);
            let sub_end = p.range.end.min(end);
            if sub_start <= sub_end
                && (sub_start < sub_end || start == end || (p.range.start >= start && p.range.end <= end))
            {
                paras.push((p.index, sub_start..sub_end, p.block, p.soft_break && p.range.start > start));
            }
        }
    }

    let mut out = String::new();
    let mut idx = 0;
    while idx < paras.len() {
        let (_, _, block, _) = paras[idx];
        match block {
            Block::Code => {
                out.push_str("<pre><code>");
                let mut first = true;
                while idx < paras.len() && paras[idx].2 == Block::Code {
                    if !first {
                        out.push('\n');
                    }
                    first = false;
                    let r = &paras[idx].1;
                    escape_html(&edit.text[r.clone()], &mut out);
                    idx += 1;
                }
                out.push_str("</code></pre>");
            }
            Block::Bullet(_) | Block::Numbered(_) => {
                // Group consecutive list items and emit properly nested `<ul>` / `<ol>`
                let mut open_lists: Vec<ListKind> = Vec::new();
                while idx < paras.len() && paras[idx].2.is_list() {
                    let (_, ref r, blk, _) = paras[idx];
                    let (kind, level) = match blk {
                        Block::Bullet(l) => (ListKind::Bullet, l as usize),
                        Block::Numbered(l) => (ListKind::Numbered, l as usize),
                        _ => unreachable!(),
                    };
                    let target_depth = level + 1;
                    while open_lists.len() > target_depth {
                        match open_lists.pop().unwrap() {
                            ListKind::Bullet => out.push_str("</ul>"),
                            ListKind::Numbered => out.push_str("</ol>"),
                        }
                    }
                    if open_lists.len() == target_depth && open_lists.last() != Some(&kind) {
                        match open_lists.pop().unwrap() {
                            ListKind::Bullet => out.push_str("</ul>"),
                            ListKind::Numbered => out.push_str("</ol>"),
                        }
                    }
                    while open_lists.len() < target_depth {
                        let k = if open_lists.len() + 1 == target_depth {
                            kind
                        } else {
                            *open_lists.last().unwrap_or(&kind)
                        };
                        match k {
                            ListKind::Bullet => out.push_str("<ul>"),
                            ListKind::Numbered => out.push_str("<ol>"),
                        }
                        open_lists.push(k);
                    }
                    out.push_str("<li>");
                    serialize_inline(edit, r.clone(), &mut out);
                    idx += 1;
                    while idx < paras.len() && paras[idx].3 && paras[idx].2 == blk {
                        out.push_str("<br>");
                        serialize_inline(edit, paras[idx].1.clone(), &mut out);
                        idx += 1;
                    }
                    out.push_str("</li>");
                }
                while let Some(k) = open_lists.pop() {
                    match k {
                        ListKind::Bullet => out.push_str("</ul>"),
                        ListKind::Numbered => out.push_str("</ol>"),
                    }
                }
            }
            Block::Quote => {
                out.push_str("<blockquote>");
                serialize_inline(edit, paras[idx].1.clone(), &mut out);
                idx += 1;
                while idx < paras.len() && paras[idx].3 && paras[idx].2 == Block::Quote {
                    out.push_str("<br>");
                    serialize_inline(edit, paras[idx].1.clone(), &mut out);
                    idx += 1;
                }
                out.push_str("</blockquote>");
            }
            Block::Paragraph => {
                out.push_str("<p>");
                serialize_inline(edit, paras[idx].1.clone(), &mut out);
                idx += 1;
                while idx < paras.len() && paras[idx].3 && paras[idx].2 == Block::Paragraph {
                    out.push_str("<br>");
                    serialize_inline(edit, paras[idx].1.clone(), &mut out);
                    idx += 1;
                }
                out.push_str("</p>");
            }
        }
    }
    out
}

/// Serializes inline marks within `range` (a single line/paragraph slice) to HTML.
fn serialize_inline(edit: &TextEdit, range: Range<usize>, out: &mut String) {
    if range.is_empty() {
        return;
    }
    let overlapping: Vec<&MarkRange> = edit
        .marks()
        .iter()
        .filter(|m| m.start < range.end && m.end > range.start)
        .collect();
    if overlapping.is_empty() {
        escape_html(&edit.text[range], out);
        return;
    }
    let mut bounds = vec![range.start, range.end];
    for m in &overlapping {
        bounds.push(m.start.clamp(range.start, range.end));
        bounds.push(m.end.clamp(range.start, range.end));
    }
    bounds.sort_unstable();
    bounds.dedup();

    for w in bounds.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a >= b {
            continue;
        }
        let mut active: Vec<&Mark> = overlapping
            .iter()
            .filter(|m| m.start <= a && b <= m.end)
            .map(|m| &m.mark)
            .collect();
        // Canonical nesting order: Link, Bold, Italic, Underline, Strike, Code
        active.sort_by_key(|m| match m {
            Mark::Link(_) => 0u8,
            Mark::Bold => 1,
            Mark::Italic => 2,
            Mark::Underline => 3,
            Mark::Strike => 4,
            Mark::Code => 5,
        });
        for m in &active {
            match m {
                Mark::Bold => out.push_str("<strong>"),
                Mark::Italic => out.push_str("<em>"),
                Mark::Underline => out.push_str("<u>"),
                Mark::Strike => out.push_str("<s>"),
                Mark::Code => out.push_str("<code>"),
                Mark::Link(url) => {
                    out.push_str("<a href=\"");
                    escape_html(url, out);
                    out.push_str("\">");
                }
            }
        }
        escape_html(&edit.text[a..b], out);
        for m in active.iter().rev() {
            match m {
                Mark::Bold => out.push_str("</strong>"),
                Mark::Italic => out.push_str("</em>"),
                Mark::Underline => out.push_str("</u>"),
                Mark::Strike => out.push_str("</s>"),
                Mark::Code => out.push_str("</code>"),
                Mark::Link(_) => out.push_str("</a>"),
            }
        }
    }
}

//! Strips active content from HTML that came from another app.
//!
//! A small tokenizer, not a parser: it keeps text and ordinary tags and drops anything that
//! can run code or load something. Where it can't tell, it drops more, never less. What it
//! returns is still untrusted input to parse, not markup to hand to a web view as is.

/// Elements dropped with everything inside them.
const DROP_WITH_CONTENT: &[&str] = &[
    "script", "iframe", "object", "applet", "noscript", "noembed", "noframes", "template", "frameset",
    "svg", "math", "xml", "portal", "fencedframe",
];
/// Elements dropped on their own; their content (if any) stays.
const DROP_TAG: &[&str] = &["base", "link", "meta", "embed", "frame", "param"];
/// Attributes that hold a URL (or, for `srcset`, a list of them).
const URL_ATTRIBUTES: &[&str] = &[
    "href", "src", "action", "formaction", "xlink:href", "background", "poster", "cite", "longdesc", "lowsrc",
    "dynsrc", "data", "codebase", "ping", "srcset", "manifest", "icon", "profile", "classid", "archive",
];
const DROP_ATTRIBUTES: &[&str] = &["srcdoc", "http-equiv"];

/// HTML from outside the app with scripts, event handlers, frames, embeds, `javascript:` and
/// other non-web URLs, and CSS that imports or loads anything removed. Comments, doctypes and
/// processing instructions go too. Text and ordinary formatting (tags, `class`, inline `style`
/// and `<style>` that only style) stay.
pub fn sanitize_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while let Some(lt) = html[i..].find('<').map(|n| i + n) {
        out.push_str(&html[i..lt]);
        let rest = &html[lt..];
        let at = |n: usize| rest.as_bytes().get(n).copied().unwrap_or(0);
        if rest.starts_with("<!--") {
            let Some(end) = rest[4..].find("-->") else { return finish(out) };
            i = lt + 4 + end + 3;
        } else if rest.starts_with("<!") || rest.starts_with("<?") || (at(1) == b'/' && !at(2).is_ascii_alphabetic()) {
            let Some(end) = rest.find('>') else { return finish(out) };
            i = lt + end + 1;
        } else if at(1) == b'/' {
            let Some(tag) = Tag::parse(&rest[2..]) else { return finish(out) };
            i = lt + 2 + tag.len;
            if !dropped(&tag.name) && tag.name != "style" {
                out.push_str("</");
                out.push_str(&tag.name);
                out.push('>');
            }
        } else if at(1).is_ascii_alphabetic() {
            let Some(tag) = Tag::parse(&rest[1..]) else { return finish(out) };
            i = lt + 1 + tag.len;
            if DROP_WITH_CONTENT.contains(&tag.name.as_str()) || tag.name == "style" {
                let Some((inner, close)) = until_close(&html[i..], &tag.name) else { return finish(out) };
                if tag.name == "style" && !tag.closed && css_is_inert(inner) {
                    tag.write(&mut out);
                    out.push_str(inner);
                    out.push_str("</style>");
                }
                i += close;
            } else if !DROP_TAG.contains(&tag.name.as_str()) {
                tag.write(&mut out);
            }
        } else {
            out.push_str("&lt;");
            i = lt + 1;
        }
    }
    out.push_str(&html[i..]);
    finish(out)
}

fn finish(mut out: String) -> String {
    out.retain(|c| c != '\0');
    out
}

fn dropped(name: &str) -> bool { DROP_WITH_CONTENT.contains(&name) || DROP_TAG.contains(&name) }

/// The text before `</name`, and the length through that end tag's `>`.
fn until_close<'a>(html: &'a str, name: &str) -> Option<(&'a str, usize)> {
    let lower = html.to_ascii_lowercase();
    let needle = format!("</{name}");
    let mut from = 0;
    loop {
        let at = from + lower[from..].find(&needle)?;
        let after = lower.as_bytes().get(at + needle.len()).copied().unwrap_or(b'>');
        if !after.is_ascii_alphanumeric() && after != b'-' {
            let end = lower[at..].find('>')?;
            return Some((&html[..at], at + end + 1));
        }
        from = at + needle.len();
    }
}

struct Tag {
    name: String,
    attributes: Vec<(String, Option<String>)>,
    closed: bool,
    /// Bytes from the tag name through `>`.
    len: usize,
}

impl Tag {
    /// A tag starting at its name; `None` when it never ends.
    fn parse(s: &str) -> Option<Tag> {
        let b = s.as_bytes();
        let name_len = b.iter().position(|c| !(c.is_ascii_alphanumeric() || matches!(c, b'-' | b':' | b'_'))).unwrap_or(b.len());
        let name = s[..name_len].to_ascii_lowercase();
        let (mut i, mut attributes, mut closed) = (name_len, Vec::new(), false);
        loop {
            while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
                closed = b[i] == b'/';
                i += 1;
            }
            match b.get(i) {
                None => return None,
                Some(b'>') => return Some(Tag { name, attributes, closed, len: i + 1 }),
                _ => closed = false,
            }
            let start = i;
            while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'/' | b'>' | b'=') { i += 1; }
            if i == start { i += 1; } // A stray `=`.
            let attr = s[start..i].to_ascii_lowercase();
            while i < b.len() && b[i].is_ascii_whitespace() { i += 1; }
            let mut value = None;
            if b.get(i) == Some(&b'=') {
                i += 1;
                while i < b.len() && b[i].is_ascii_whitespace() { i += 1; }
                match b.get(i) {
                    Some(&q) if q == b'"' || q == b'\'' => {
                        let end = s[i + 1..].find(q as char)? + i + 1;
                        value = Some(s[i + 1..end].to_owned());
                        i = end + 1;
                    }
                    _ => {
                        let start = i;
                        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' { i += 1; }
                        value = Some(s[start..i].to_owned());
                    }
                }
            }
            if keep_attribute(&attr, value.as_deref()) { attributes.push((attr, value)); }
        }
    }

    fn write(&self, out: &mut String) {
        out.push('<');
        out.push_str(&self.name);
        for (name, value) in &self.attributes {
            out.push(' ');
            out.push_str(name);
            if let Some(value) = value {
                out.push_str("=\"");
                out.push_str(&value.replace('"', "&quot;"));
                out.push('"');
            }
        }
        if self.closed { out.push_str(" /"); }
        out.push('>');
    }
}

fn keep_attribute(name: &str, value: Option<&str>) -> bool {
    if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b':' | b'_' | b'.')) {
        return false;
    }
    if name.starts_with("on") || DROP_ATTRIBUTES.contains(&name) { return false; }
    let value = value.unwrap_or("");
    if name == "style" { return css_is_inert(&decode_entities(value)); }
    if name == "srcset" { return value.split(',').all(|part| url_is_safe(part.split_whitespace().next().unwrap_or(""), true)); }
    if URL_ATTRIBUTES.contains(&name) { return url_is_safe(value, name == "src"); }
    true
}

/// No imports, loads, bindings or scripts; nothing a CSS escape could hide.
fn css_is_inert(css: &str) -> bool {
    let mut plain = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        plain.push_str(&rest[..start]);
        rest = rest[start + 2..].find("*/").map_or("", |end| &rest[start + 2 + end + 2..]);
    }
    plain.push_str(rest);
    let plain: String = plain.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
    !["expression(", "javascript:", "vbscript:", "@import", "url(", "image-set(", "behavior:", "-moz-binding", "</", "\\", "<"]
        .iter()
        .any(|bad| plain.contains(bad))
}

/// A relative URL, or one whose scheme is web, mail, phone or an attachment; `image` also
/// allows raster `data:` images.
fn url_is_safe(raw: &str, image: bool) -> bool {
    let url: String = decode_entities(raw).chars().filter(|c| !c.is_control() && !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
    let Some(colon) = url.find(':') else { return true };
    if url[..colon].contains(['/', '?', '#']) { return true; }
    match &url[..colon] {
        "http" | "https" | "mailto" | "tel" | "cid" => true,
        "data" => image && ["png", "jpeg", "jpg", "gif", "webp"].iter().any(|t| url.starts_with(&format!("data:image/{t}"))),
        _ => false,
    }
}

/// Numeric references and the named ones that can hide a scheme or CSS function.
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let body: String = rest[1..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '#').take(32).collect();
        let decoded = if let Some(num) = body.strip_prefix('#') {
            let (digits, radix) = match num.strip_prefix(['x', 'X']) { Some(hex) => (hex, 16), None => (num, 10) };
            let digits: String = digits.chars().take_while(|c| c.is_digit(radix)).collect();
            let used = 1 + (num.len() - num.trim_start_matches(['x', 'X']).len()) + digits.len();
            u32::from_str_radix(&digits, radix).ok().map(|n| (char::from_u32(n).unwrap_or('\u{fffd}'), used))
        } else {
            let named = match body.to_ascii_lowercase().as_str() {
                "colon" => Some(':'), "tab" => Some('\t'), "newline" => Some('\n'), "lpar" => Some('('), "rpar" => Some(')'),
                "sol" => Some('/'), "bsol" => Some('\\'), "amp" => Some('&'), "lt" => Some('<'), "gt" => Some('>'),
                "quot" => Some('"'), "apos" => Some('\''), "nbsp" => Some(' '), "num" => Some('#'), "period" => Some('.'),
                _ => None,
            };
            named.map(|c| (c, body.len()))
        };
        match decoded {
            Some((c, used)) => {
                out.push(c);
                rest = &rest[1 + used..];
                if rest.starts_with(';') { rest = &rest[1..]; }
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::sanitize_html as clean;

    #[test]
    fn formatting_survives() {
        let html = r#"<p class="p1"><b>Bold</b> <i>it</i> <a href="https://example.com/a?b=1">link</a><br/><span style="font-weight: 700; color: #333">x</span></p>"#;
        assert_eq!(clean(html), r#"<p class="p1"><b>Bold</b> <i>it</i> <a href="https://example.com/a?b=1">link</a><br /><span style="font-weight: 700; color: #333">x</span></p>"#);
        assert_eq!(clean("<ul><li>1 &lt; 2 &amp; 3 > 0</li></ul>"), "<ul><li>1 &lt; 2 &amp; 3 > 0</li></ul>");
        assert_eq!(clean("<style>p.p1 {margin: 0; font: 12px Helvetica}</style><p>a</p>"), "<style>p.p1 {margin: 0; font: 12px Helvetica}</style><p>a</p>");
        assert_eq!(clean("a < b"), "a &lt; b");
        assert_eq!(clean(r#"<img src="data:image/png;base64,AAAA" alt='a "q"'>"#), r#"<img src="data:image/png;base64,AAAA" alt="a &quot;q&quot;">"#);
    }

    #[test]
    fn active_content_goes() {
        let cases = [
            ("<p>a<script>alert(1)</script>b</p>", "<p>ab</p>"),
            ("<p>a<SCRIPT type=x>alert(1)</ScRiPt >b</p>", "<p>ab</p>"),
            ("<script/src=x></script>ok", "ok"),
            ("<img src=x onerror=alert(1)>", "<img src=\"x\">"),
            ("<img src=x OnError = 'alert(1)' >", "<img src=\"x\">"),
            ("<a href=\"javascript:alert(1)\">x</a>", "<a>x</a>"),
            ("<a href=\" JaVa&#x09;Script&colon;alert(1)\">x</a>", "<a>x</a>"),
            ("<a href=\"jav&#97;script:alert(1)\">x</a>", "<a>x</a>"),
            ("<a href='vbscript:x'>x</a>", "<a>x</a>"),
            ("<img src=\"data:image/svg+xml;base64,AAAA\">", "<img>"),
            ("<a href=\"data:text/html,<script>\">x</a>", "<a>x</a>"),
            ("<iframe srcdoc=\"<script>\"></iframe>t", "t"),
            ("<svg><a xlink:href=\"javascript:x\"/><script>x</script></svg>t", "t"),
            ("<math><mi href=javascript:x>x</mi></math>t", "t"),
            ("<meta http-equiv=refresh content=\"0;url=javascript:x\"><base href=//evil><link rel=import href=x>t", "t"),
            ("<embed src=x.swf><object data=x>fallback</object>t", "t"),
            ("<div style=\"background:url(javascript:x)\">t</div>", "<div>t</div>"),
            ("<div style=\"width: expression(alert(1))\">t</div>", "<div>t</div>"),
            ("<div style=\"x: ex/**/pression(1)\">t</div>", "<div>t</div>"),
            ("<div style=\"background:u\\72l(x)\">t</div>", "<div>t</div>"),
            ("<style>@import 'x.css';</style>t", "t"),
            ("<style>p{}</style><script>x</script>t", "<style>p{}</style>t"),
            ("<style>p{}</style ><b>t</b>", "<style>p{}</style><b>t</b>"),
            ("<!--<script>x</script>-->t", "t"),
            ("<!--[if gte mso 9]><xml>x</xml><![endif]-->t", "t"),
            ("<![if !supportLists]>t<![endif]>", "t"),
            ("<!DOCTYPE html><html><body>t</body></html>", "<html><body>t</body></html>"),
            ("<title><!--</title><script>x</script>-->t", "<title>t"),
            ("<form action=javascript:x><button formaction=javascript:y>b</button></form>", "<form><button>b</button></form>"),
            ("<details open ontoggle=x>t</details>", "<details open>t</details>"),
            ("<scr<script>ipt>alert(1)</script>", "<scr>ipt>alert(1)"),
            ("<img srcset=\"a.png 1x, javascript:x 2x\">", "<img>"),
            ("t<script>never closed", "t"),
            ("t<img src=x", "t"),
            ("t<a href=\"unterminated>", "t"),
            ("a\0b", "ab"),
        ];
        for (dirty, want) in cases { assert_eq!(clean(dirty), want, "{dirty}"); }
    }

    #[test]
    fn clean_is_stable() {
        for html in ["<p><b>x</b><img src=y onload=z></p>", "<a href='mailto:a@b.c'>m</a>", "<style>a{}</style>"] {
            let once = clean(html);
            assert_eq!(clean(&once), once);
        }
    }
}

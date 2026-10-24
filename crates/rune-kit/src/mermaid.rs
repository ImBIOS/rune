//! Native ```` ```mermaid ```` diagrams: a common subset of mermaid's flowchart and sequence
//! syntax, parsed and laid out in Rust and drawn with [`rune::Shape`]s and text, so they theme
//! with the app and stay crisp at any scale. No JavaScript, web view or network.
//!
//! **Flowcharts** (`graph` or `flowchart`, directions `TD`/`TB`/`BT`/`LR`/`RL`): node shapes
//! `[rect]`, `(rounded)`, `([stadium])`, `[[subroutine]]`, `[(cylinder)]`, `((circle))`,
//! `>asymmetric]`, `{diamond}`, `{{hexagon}}`, `[/parallelogram/]` and `[/trapezoid\]`; links
//! `-->`, `---`, `-.->`, `==>`, `~~~`, `<-->`, `--o`, `--x`, longer forms for longer links,
//! labels as `-- text -->` or `-->|text|`; chains (`A --> B --> C`), `&` groups, quoted labels,
//! `<br>` breaks, and nested `subgraph … end`. `classDef`, `class`, `style`, `linkStyle`,
//! `click`, `direction` and `%%` comments are ignored.
//!
//! **Sequence diagrams** (`sequenceDiagram`): `participant` and `actor` with `as` aliases;
//! messages `->`, `-->`, `->>`, `-->>`, `-x`, `--x`, `-)`, `--)`, `<<->>` and `<<-->>`, with
//! `+`/`-` activation; `activate`/`deactivate`; `Note left of|right of|over A[,B]`;
//! `autonumber`; and `loop`, `alt`/`else`, `opt`, `par`/`and`, `critical`/`option` and `break`
//! frames (`rect` and `box` groups are accepted and not drawn).
//!
//! Anything else (other diagram types, unknown statements, unbalanced brackets) is unsupported:
//! [`render`] returns `None` and [`crate::Markdown`] shows the source as a code block.
//!
//! **Cost.** Parsing, measuring and layout happen once per source and style, on the first
//! build that sees them; later builds reuse the laid-out scene from a small cache. Work is
//! bounded for untrusted input: charts over 400 nodes, edges and subgraphs (or 40
//! participants), subgraphs nested more than 8 deep, and flowcharts whose long edges need
//! more than 3,000 bend points fall back to source; links longer than `--------->` draw at
//! that length.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use rune::{Align, Color, El, LineCap, Shape, TextStyle, canvas, col, el, text};

use crate::markdown::{DiagramPaint, MarkdownStyle};

mod flowchart;
mod layered;
mod sequence;

/// `src` drawn as a diagram card with `style`'s diagram colours, or `None` when it is outside
/// the supported subset.
pub fn render(src: &str, style: &MarkdownStyle) -> Option<El> {
    diagram(src, &style.diagram)
}

/// Whether `src` parses as a supported diagram.
pub fn supported(src: &str) -> bool {
    parse(src).is_some()
}

pub(crate) enum Diagram {
    Flowchart(flowchart::Flowchart),
    Sequence(sequence::Sequence),
}

/// Diagrams bigger than this are left as source: they would be unreadable at markdown width.
const MAX_ITEMS: usize = 400;

pub(crate) fn parse(src: &str) -> Option<Diagram> {
    let mut lines = src.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("%%")).peekable();
    // A front-matter block (`---` … `---`) carries a title and config; skip it.
    if lines.peek() == Some(&"---") {
        lines.next();
        for l in lines.by_ref() {
            if l == "---" {
                break;
            }
        }
    }
    let header = lines.next()?;
    let (kind, rest) = header.split_once(|c: char| c.is_whitespace() || c == ';').unwrap_or((header, ""));
    let body: Vec<&str> = lines.collect();
    match kind {
        "graph" | "flowchart" => flowchart::parse(rest, &body).map(Diagram::Flowchart),
        "sequenceDiagram" => sequence::parse(rest, &body).map(Diagram::Sequence),
        _ => None,
    }
}

const CACHE_LEN: usize = 16;

#[derive(Default)]
struct Cache {
    map: HashMap<u64, Option<Rc<Scene>>>,
    order: VecDeque<u64>,
}

thread_local! {
    static SCENES: RefCell<Cache> = RefCell::new(Cache::default());
}

pub(crate) fn diagram(src: &str, paint: &DiagramPaint) -> Option<El> {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    format!("{paint:?}").hash(&mut h);
    let key = h.finish();
    let cached = SCENES.with_borrow(|c| c.map.get(&key).cloned());
    let scene = match cached {
        Some(s) => s,
        None => {
            // Defensive: a bug in layout must cost a diagram, not the app.
            let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| scene(src, paint))).ok().flatten().map(Rc::new);
            SCENES.with_borrow_mut(|c| {
                if c.order.len() >= CACHE_LEN
                    && let Some(old) = c.order.pop_front()
                {
                    c.map.remove(&old);
                }
                c.order.push_back(key);
                c.map.insert(key, built.clone());
            });
            built
        }
    };
    scene.map(|s| s.el(paint))
}

fn scene(src: &str, paint: &DiagramPaint) -> Option<Scene> {
    match parse(src)? {
        Diagram::Flowchart(f) => flowchart::draw(&f, paint),
        Diagram::Sequence(s) => sequence::draw(&s, paint),
    }
}

/// The card's padding on each side.
const CARD_PAD: f32 = 16.0;

/// The widest a diagram's drawing can be for its card to fit `paint.max_width`.
fn room(paint: &DiagramPaint) -> f32 {
    paint.max_width - 2.0 * CARD_PAD - 2.0
}

/// How much a canvas `width` points wide is drawn at to fit its card in `paint.max_width`.
fn fit(width: f32, paint: &DiagramPaint) -> f32 {
    if width <= room(paint) { 1.0 } else { (room(paint) / width).max(0.6) }
}

/// Text measured for a diagram: its size, and whether it wraps at [`WRAP`].
#[derive(Clone)]
struct Measured {
    text: String,
    size: [f32; 2],
    wrap: bool,
}

/// Labels wider than this wrap.
const WRAP: f32 = 200.0;

fn measure(s: &str, font: TextStyle) -> Measured {
    if s.is_empty() {
        return Measured { text: String::new(), size: [0.0, 0.0], wrap: false };
    }
    let (w, h) = rune::text::measure(s, &[], font, None);
    if w <= WRAP {
        return Measured { text: s.to_owned(), size: [w, h], wrap: false };
    }
    let (w, h) = rune::text::measure(s, &[], font, Some(WRAP));
    Measured { text: s.to_owned(), size: [w, h], wrap: true }
}

/// Mermaid label markup to plain text: `<br>` breaks, entity codes, markdown-string backticks.
fn clean(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix('`').and_then(|s| s.strip_suffix('`')).unwrap_or(s);
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(['<', '#', '&']) {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let lower = rest.get(..6).unwrap_or(rest).to_ascii_lowercase();
        let (rep, len) = if lower.starts_with("<br>") {
            ("\n", 4)
        } else if lower.starts_with("<br/>") {
            ("\n", 5)
        } else if lower.starts_with("<br />") {
            ("\n", 6)
        } else if rest.starts_with("#quot;") {
            ("\"", 6)
        } else if rest.starts_with("#59;") {
            (";", 4)
        } else if rest.starts_with("#35;") {
            ("#", 4)
        } else if rest.starts_with("&amp;") {
            ("&", 5)
        } else if rest.starts_with("&lt;") {
            ("<", 4)
        } else if rest.starts_with("&gt;") {
            (">", 4)
        } else if rest.starts_with("&quot;") {
            ("\"", 6)
        } else {
            (&rest[..1], 1)
        };
        out.push_str(rep);
        rest = &rest[len..];
    }
    out.push_str(rest);
    out.lines().map(str::trim).collect::<Vec<_>>().join("\n")
}

enum Op {
    Fill(Vec<[f32; 2]>, Color),
    Stroke { points: Vec<[f32; 2]>, closed: bool, width: f32, cap: LineCap, color: Color },
}

struct Label {
    text: String,
    at: [f32; 2],
    size: [f32; 2],
    font: TextStyle,
    color: Color,
    wrap: bool,
    align: Align,
}

/// Drawing in a diagram's own points; [`Canvas::finish`] moves it to the origin and scales it.
#[derive(Default)]
struct Canvas {
    ops: Vec<Op>,
    labels: Vec<Label>,
}

impl Canvas {
    fn fill(&mut self, points: Vec<[f32; 2]>, color: Color) {
        if points.len() >= 3 {
            self.ops.push(Op::Fill(points, color));
        }
    }

    fn stroke(&mut self, points: Vec<[f32; 2]>, closed: bool, width: f32, color: Color) {
        if points.len() >= 2 {
            self.ops.push(Op::Stroke { points, closed, width, cap: LineCap::Round, color });
        }
    }

    /// A filled outline.
    fn shape(&mut self, points: Vec<[f32; 2]>, fill: Color, border: Color, width: f32) {
        self.fill(points.clone(), fill);
        self.stroke(points, true, width, border);
    }

    fn dashed(&mut self, points: &[[f32; 2]], width: f32, color: Color, on: f32, off: f32) {
        let mut dash: Vec<[f32; 2]> = Vec::new();
        let mut drawing = true;
        let mut left = on;
        // Bounded so a huge or non-finite length (where `t += step` stops advancing) can't spin.
        let mut budget = 20_000usize;
        for w in points.windows(2) {
            let (a, b) = (w[0], w[1]);
            let len = dist(a, b);
            if !len.is_finite() {
                continue;
            }
            let mut t = 0.0;
            while t < len {
                let Some(rest) = budget.checked_sub(1) else { return };
                budget = rest;
                let step = left.min(len - t);
                let p0 = lerp(a, b, t / len);
                let p1 = lerp(a, b, (t + step) / len);
                if drawing {
                    if dash.is_empty() {
                        dash.push(p0);
                    }
                    dash.push(p1);
                }
                t += step;
                left -= step;
                if left <= 1e-3 {
                    if drawing && dash.len() >= 2 {
                        self.ops.push(Op::Stroke { points: std::mem::take(&mut dash), closed: false, width, cap: LineCap::Butt, color });
                    }
                    dash.clear();
                    drawing = !drawing;
                    left = if drawing { on } else { off };
                }
            }
        }
        if dash.len() >= 2 {
            self.ops.push(Op::Stroke { points: dash, closed: false, width, cap: LineCap::Butt, color });
        }
    }

    /// `m` centred on `center`.
    fn label(&mut self, m: &Measured, center: [f32; 2], font: TextStyle, color: Color, align: Align) {
        if m.text.is_empty() {
            return;
        }
        let at = [center[0] - m.size[0] / 2.0, center[1] - m.size[1] / 2.0];
        self.label_at(m, at, font, color, align);
    }

    fn label_at(&mut self, m: &Measured, at: [f32; 2], font: TextStyle, color: Color, align: Align) {
        if !m.text.is_empty() {
            self.labels.push(Label { text: m.text.clone(), at, size: m.size, font, color, wrap: m.wrap, align });
        }
    }

    /// Draws `other` over this.
    fn append(&mut self, other: Canvas) {
        self.ops.extend(other.ops);
        self.labels.extend(other.labels);
    }

    /// The bounding box of everything drawn: `[x0, y0, x1, y1]`.
    fn bounds(&self) -> [f32; 4] {
        let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        let mut add = |p: [f32; 2], r: f32| {
            b = [b[0].min(p[0] - r), b[1].min(p[1] - r), b[2].max(p[0] + r), b[3].max(p[1] + r)];
        };
        for op in &self.ops {
            match op {
                Op::Fill(points, _) => points.iter().for_each(|&p| add(p, 0.0)),
                Op::Stroke { points, width, .. } => points.iter().for_each(|&p| add(p, width / 2.0)),
            }
        }
        for l in &self.labels {
            add(l.at, 0.0);
            add([l.at[0] + l.size[0], l.at[1] + l.size[1]], 0.0);
        }
        if b[0] > b[2] { [0.0; 4] } else { b }
    }

    fn width(&self) -> f32 {
        let b = self.bounds();
        b[2] - b[0]
    }

    fn finish(self, scale: f32) -> Option<Scene> {
        if self.ops.is_empty() && self.labels.is_empty() {
            return None;
        }
        let b = self.bounds();
        let pad = 1.0;
        let map = |p: [f32; 2]| [(p[0] - b[0]) * scale + pad, (p[1] - b[1]) * scale + pad];
        let mut shape = Shape::new();
        for op in self.ops {
            shape = match op {
                Op::Fill(points, color) => shape.fill(points.into_iter().map(map).collect::<Vec<_>>(), color),
                Op::Stroke { points, closed, width, cap, color } => {
                    shape.stroke_with(points.into_iter().map(map).collect::<Vec<_>>(), closed, width * scale.max(0.8), cap, color)
                }
            };
        }
        let labels = self
            .labels
            .into_iter()
            .map(|l| Label {
                at: map(l.at),
                size: [l.size[0] * scale, l.size[1] * scale],
                font: TextStyle { size: l.font.size * scale, line_height: l.font.line_height * scale, ..l.font },
                ..l
            })
            .collect();
        let size = [(b[2] - b[0]) * scale + 2.0 * pad, (b[3] - b[1]) * scale + 2.0 * pad];
        Some(Scene { size, shape, labels })
    }
}

/// A laid-out diagram, drawn the same way every build.
pub(crate) struct Scene {
    size: [f32; 2],
    shape: Shape,
    labels: Vec<Label>,
}

impl Scene {
    fn el(&self, paint: &DiagramPaint) -> El {
        let [w, h] = self.size;
        let mut body = el().key("diagram").size(w, h).child(canvas(self.shape.clone()).key("shape").absolute(0.0, 0.0).size(w, h));
        for (i, l) in self.labels.iter().enumerate() {
            // A point of slack so rounding never breaks a measured line.
            let mut t = text(l.text.clone())
                .key(format!("t{i}"))
                .font(l.font)
                .color(l.color)
                .align(l.align)
                .w(l.size[0] + 2.0)
                .absolute(l.at[0] - 1.0, l.at[1]);
            if l.wrap {
                t = t.wrap();
            }
            body = body.child(t);
        }
        col()
            .w_full()
            .items_center()
            .padding(CARD_PAD)
            .radius(6.0)
            .fill(paint.surface)
            .border(0.5, paint.surface_border)
            .clip_rounded(6.0)
            .child(body)
    }
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt()
}

fn lerp(a: [f32; 2], b: [f32; 2], t: f32) -> [f32; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

fn unit(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    let d = dist(a, b).max(1e-6);
    [(b[0] - a[0]) / d, (b[1] - a[1]) / d]
}

/// A rectangle with corners of radius `r`, as a polygon.
fn rounded_rect(x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> Vec<[f32; 2]> {
    let r = r.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0).max(0.0);
    if r < 0.5 {
        return vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
    }
    let steps = 6;
    let mut pts = Vec::with_capacity(4 * (steps + 1));
    let corners = [([x1 - r, y0 + r], -90.0f32), ([x1 - r, y1 - r], 0.0), ([x0 + r, y1 - r], 90.0), ([x0 + r, y0 + r], 180.0)];
    for (c, start) in corners {
        for i in 0..=steps {
            let a = (start + 90.0 * i as f32 / steps as f32).to_radians();
            pts.push([c[0] + r * a.cos(), c[1] + r * a.sin()]);
        }
    }
    pts
}

/// An ellipse's arc from `a0` to `a1` degrees (clockwise on screen), as points.
fn arc(c: [f32; 2], rx: f32, ry: f32, a0: f32, a1: f32, steps: usize) -> Vec<[f32; 2]> {
    (0..=steps)
        .map(|i| {
            let a = (a0 + (a1 - a0) * i as f32 / steps as f32).to_radians();
            [c[0] + rx * a.cos(), c[1] + ry * a.sin()]
        })
        .collect()
}

fn ellipse(c: [f32; 2], rx: f32, ry: f32) -> Vec<[f32; 2]> {
    let mut pts = arc(c, rx, ry, 0.0, 360.0, 48);
    pts.pop();
    pts
}

/// A filled arrowhead with its tip at `tip`, pointing along `dir` (a unit vector).
fn arrowhead(tip: [f32; 2], dir: [f32; 2], len: f32, width: f32) -> Vec<[f32; 2]> {
    let base = [tip[0] - dir[0] * len, tip[1] - dir[1] * len];
    let n = [-dir[1] * width / 2.0, dir[0] * width / 2.0];
    vec![tip, [base[0] + n[0], base[1] + n[1]], [base[0] - n[0], base[1] - n[1]]]
}

/// A smooth curve through `pts` (Catmull-Rom, as flattened cubics).
fn smooth(pts: &[[f32; 2]]) -> Vec<[f32; 2]> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let n = pts.len();
    let tangent = |i: usize| {
        let (a, b) = if i == 0 {
            (pts[0], pts[1])
        } else if i == n - 1 {
            (pts[n - 2], pts[n - 1])
        } else {
            (pts[i - 1], pts[i + 1])
        };
        let k = if i == 0 || i == n - 1 { 1.0 } else { 0.5 };
        [(b[0] - a[0]) * k, (b[1] - a[1]) * k]
    };
    let mut out = vec![pts[0]];
    for i in 0..n - 1 {
        let (a, b) = (pts[i], pts[i + 1]);
        let (ta, tb) = (tangent(i), tangent(i + 1));
        let c1 = [a[0] + ta[0] / 3.0, a[1] + ta[1] / 3.0];
        let c2 = [b[0] - tb[0] / 3.0, b[1] - tb[1] / 3.0];
        let steps = ((dist(a, c1) + dist(c1, c2) + dist(c2, b)) / 4.0).ceil().clamp(2.0, 48.0) as usize;
        for s in 1..=steps {
            let t = s as f32 / steps as f32;
            let u = 1.0 - t;
            let (b0, b1, b2, b3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            out.push([
                b0 * a[0] + b1 * c1[0] + b2 * c2[0] + b3 * b[0],
                b0 * a[1] + b1 * c1[1] + b2 * c2[1] + b3 * b[1],
            ]);
        }
    }
    out
}

/// A byte cursor over one statement.
struct Cur<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Cur<'a> {
    fn new(s: &'a str) -> Self {
        Cur { s, i: 0 }
    }
    fn rest(&self) -> &'a str {
        &self.s[self.i..]
    }
    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }
    fn peek2(&self) -> Option<char> {
        self.rest().chars().nth(1)
    }
    fn bump(&mut self) {
        if let Some(c) = self.peek() {
            self.i += c.len_utf8();
        }
    }
    fn eat(&mut self, t: &str) -> bool {
        let hit = self.rest().starts_with(t);
        if hit {
            self.i += t.len();
        }
        hit
    }
    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }
    fn done(&self) -> bool {
        self.i >= self.s.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_turns_markup_into_text() {
        assert_eq!(clean("a<br>b<br/>c<BR />d"), "a\nb\nc\nd");
        assert_eq!(clean("#quot;hi#quot; &amp; &lt;3"), "\"hi\" & <3");
        assert_eq!(clean("`**md**`"), "**md**");
        assert_eq!(clean("# not an entity & also not"), "# not an entity & also not");
    }

    #[test]
    fn other_diagram_types_and_garbage_are_unsupported() {
        for src in ["", "pie title Pets\n\"Dogs\" : 386", "classDiagram\nA <|-- B", "gantt\ntitle x", "hello", "%% only a comment"] {
            assert!(!supported(src), "{src:?}");
        }
        assert!(supported("---\ntitle: Hi\n---\ngraph TD\nA-->B"), "front matter is skipped");
    }

    #[test]
    fn dashes_cover_the_line_in_on_off_runs() {
        let mut c = Canvas::default();
        c.dashed(&[[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]], 1.0, Color::WHITE, 3.0, 1.0);
        let dashes: Vec<&Vec<[f32; 2]>> = c.ops.iter().map(|op| match op { Op::Stroke { points, .. } => points, Op::Fill(..) => unreachable!() }).collect();
        assert_eq!(dashes.len(), 5, "20 points of line in 4-point periods");
        assert_eq!(dashes[0].first(), Some(&[0.0, 0.0]));
        let corner = dashes[2];
        assert_eq!(corner.len(), 3, "a dash turns the corner: {corner:?}");
        assert!(dist(corner[0], [8.0, 0.0]) < 1e-4 && dist(corner[1], [10.0, 0.0]) < 1e-4 && dist(corner[2], [10.0, 1.0]) < 1e-4);
    }

    #[test]
    fn smooth_passes_through_its_points() {
        let pts = [[0.0, 0.0], [20.0, 40.0], [0.0, 80.0]];
        let curve = smooth(&pts);
        assert_eq!(curve.first(), Some(&pts[0]));
        assert_eq!(curve.last(), Some(&pts[2]));
        assert!(curve.iter().any(|p| dist(*p, pts[1]) < 0.01));
    }

    /// Each source falls back (`false`) or draws (`true`), in well under a second at full speed;
    /// the bound here is loose for debug builds on a busy machine.
    fn hostile(src: &str, draws: bool) {
        let paint = crate::MarkdownStyle::neutral(true).diagram;
        let t = std::time::Instant::now();
        let scene = scene(src, &paint);
        let took = t.elapsed();
        assert_eq!(scene.is_some(), draws, "{}…", &src[..src.len().min(80)]);
        assert!(took.as_secs_f32() < 5.0, "{took:?} for {}…", &src[..src.len().min(80)]);
    }

    fn ids(prefix: &str, n: usize) -> String {
        (1..=n).map(|i| format!("{prefix}{i}")).collect::<Vec<_>>().join(" & ")
    }

    #[test]
    fn deep_subgraphs_fall_back() {
        hostile(&format!("graph TD\n{}A\n{}", "subgraph s\n".repeat(5000), "end\n".repeat(5000)), false);
        hostile(&format!("graph TD\n{}A\n{}", "subgraph s\n".repeat(9), "end\n".repeat(9)), false);
        hostile(&format!("graph TD\n{}A\n{}", "subgraph s\n".repeat(8), "end\n".repeat(8)), true);
        // Many flat subgraphs count toward the item cap.
        hostile(&format!("graph TD\nA\n{}", "subgraph s\nend\n".repeat(5000)), false);
    }

    #[test]
    fn a_huge_cross_product_falls_back_before_it_is_built() {
        hostile(&format!("graph TD\n{} --> {}", ids("A", 500), ids("B", 500)), false);
        hostile(&format!("graph TD\n{}", ids("A", 5000)), false);
        hostile(&format!("graph TD\n{} --> {}", ids("A", 10), ids("B", 10)), true);
    }

    #[test]
    fn a_huge_dash_count_draws_at_the_longest_link() {
        let paint = crate::MarkdownStyle::neutral(true).diagram;
        hostile(&format!("graph TD\nA {}> B", "-".repeat(10_000)), true);
        hostile(&format!("graph TD\nA -{}-> B", ".".repeat(10_000)), true);
        let longest = scene(&format!("graph TD\nA {}> B", "-".repeat(10_000)), &paint).unwrap();
        let eight = scene(&format!("graph TD\nA {}> B", "-".repeat(9)), &paint).unwrap();
        assert_eq!(longest.size, eight.size);
    }

    #[test]
    fn long_chords_over_a_chain_fall_back() {
        let chain: Vec<String> = (0..129).map(|i| format!("N{i} --> N{}", i + 1)).collect();
        let chords: Vec<String> = (0..140).map(|k| format!("N{} --> N{}", k % 10, 120 + k % 10)).collect();
        hostile(&format!("graph TD\n{}\n{}", chain.join("\n"), chords.join("\n")), false);
        // A chain with a few long edges still draws.
        let short: Vec<String> = (0..4).map(|k| format!("N{k} --> N{}", 30 + k)).collect();
        hostile(&format!("graph TD\n{}\n{}", chain[..40].join("\n"), short.join("\n")), true);
    }

    #[test]
    fn the_biggest_allowed_charts_draw_in_bounded_time() {
        // Dense: every item spent on nodes and short cross edges.
        let mut lines = vec!["graph LR".to_owned()];
        for i in 0..130 {
            lines.push(format!("N{i} --> N{} & N{}", (i * 7 + 1) % 130, (i * 13 + 5) % 130));
        }
        let src = lines.join("\n");
        assert!(parse(&src).is_some());
        let paint = crate::MarkdownStyle::neutral(true).diagram;
        let t = std::time::Instant::now();
        let drawn = scene(&src, &paint).is_some();
        println!("dense chart: drawn {drawn} in {:?}", t.elapsed());
        assert!(t.elapsed().as_secs_f32() < 5.0, "{:?}", t.elapsed());
        // Just under the long-edge budget: 40 long edges across a 60-node chain, half of it grouped.
        let mut big = String::from("graph TD\nsubgraph left\n");
        for i in 0..59 {
            big.push_str(&format!("N{i} --> N{}\n", i + 1));
            if i == 30 {
                big.push_str("end\nsubgraph right\n");
            }
        }
        big.push_str("end\n");
        for k in 0..40 {
            big.push_str(&format!("N{} --> N{}\n", k % 20, k % 20 + 37));
        }
        let t = std::time::Instant::now();
        assert!(scene(&big, &paint).is_some());
        println!("chart at the long-edge budget: {:?}", t.elapsed());
        assert!(t.elapsed().as_secs_f32() < 5.0, "{:?}", t.elapsed());
        // Nested subgraphs at the depth cap, each with a few nodes.
        let mut nested = String::from("graph TD\n");
        for d in 0..8 {
            nested.push_str(&format!("subgraph s{d}\nA{d} --> B{d} --> C{d}\n"));
        }
        nested.push_str(&"end\n".repeat(8));
        nested.push_str("A0 --> C7\nC7 --> A0");
        hostile(&nested, true);
    }

    #[test]
    fn repeated_create_falls_back_without_recursing() {
        hostile(&format!("sequenceDiagram\n{}participant A\nA->>B: hi", "create ".repeat(100_000)), false);
        hostile("sequenceDiagram\ncreate loop x\nA->>B: hi\nend", false);
        hostile("sequenceDiagram\ncreate participant A\nA->>B: hi", true);
        hostile("sequenceDiagram\nA->>B: hi\ncreate actor C as Carol\nB->>C: hello", true);
    }

    #[test]
    fn dashes_stop_on_huge_and_non_finite_lengths() {
        let ink = Color::BLACK;
        for end in [1e9, f32::INFINITY, f32::NAN] {
            let mut c = Canvas::default();
            c.dashed(&[[0.0, 0.0], [end, 0.0]], 1.0, ink, 4.0, 3.0);
            assert!(c.ops.len() <= 20_000);
        }
    }
}

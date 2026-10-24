//! Flowcharts: the parser for `graph`/`flowchart` sources and their drawing on top of
//! [`super::layered`].

use std::collections::HashMap;

use rune::{Align, TextStyle};

use super::layered::{GCluster, GEdge, Graph, MAX_MINLEN, Spacing, layout};
use super::{Canvas, Cur, DiagramPaint, MAX_ITEMS, Measured, Scene, arc, arrowhead, clean, dist, ellipse, fit, measure, room, rounded_rect, smooth, unit};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dir {
    Down,
    Up,
    Right,
    Left,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NodeShape {
    Rect,
    Round,
    Stadium,
    Subroutine,
    Cylinder,
    Circle,
    Asymmetric,
    Diamond,
    Hexagon,
    Parallelogram,
    Trapezoid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Line {
    Solid,
    Dotted,
    Thick,
    Invisible,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Head {
    None,
    Arrow,
    Circle,
    Cross,
}

#[derive(Debug)]
pub(crate) struct Node {
    #[cfg_attr(not(test), allow(dead_code, reason = "tests check parsing by id"))]
    pub id: String,
    pub label: String,
    pub shape: NodeShape,
    pub cluster: Option<usize>,
}

#[derive(Debug)]
pub(crate) struct Edge {
    pub from: usize,
    pub to: usize,
    pub label: Option<String>,
    pub line: Line,
    pub start: Head,
    pub end: Head,
    pub minlen: usize,
}

#[derive(Debug)]
pub(crate) struct Subgraph {
    pub title: String,
    pub parent: Option<usize>,
}

#[derive(Debug)]
pub(crate) struct Flowchart {
    pub dir: Dir,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub subgraphs: Vec<Subgraph>,
}

/// `header` is what follows `graph`/`flowchart` on its line; `body` the remaining lines.
pub(crate) fn parse(header: &str, body: &[&str]) -> Option<Flowchart> {
    let (dir, rest) = header.trim_start().split_once(|c: char| c.is_whitespace() || c == ';').unwrap_or((header.trim(), ""));
    let dir = match dir {
        "" | "TD" | "TB" => Dir::Down,
        "BT" => Dir::Up,
        "LR" => Dir::Right,
        "RL" => Dir::Left,
        _ => return None,
    };
    let mut p = Parser { f: Flowchart { dir, nodes: Vec::new(), edges: Vec::new(), subgraphs: Vec::new() }, ids: HashMap::new(), stack: Vec::new() };
    for line in std::iter::once(rest).chain(body.iter().copied()) {
        for stmt in statements(line) {
            p.statement(stmt)?;
        }
    }
    (!p.f.nodes.is_empty()).then_some(p.f)
}

/// A line split at `;` outside quotes.
fn statements(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut quoted) = (0, false);
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ';' if !quoted => {
                out.push(&line[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&line[start..]);
    out.into_iter().map(str::trim).filter(|s| !s.is_empty() && !s.starts_with("%%")).collect()
}

struct Parser {
    f: Flowchart,
    ids: HashMap<String, usize>,
    stack: Vec<usize>,
}

/// Subgraphs nest at most this deep.
const MAX_DEPTH: usize = 8;

struct Link {
    line: Line,
    start: Head,
    end: Head,
    minlen: usize,
    label: Option<String>,
}

impl Parser {
    /// `None` once the chart already holds [`MAX_ITEMS`] nodes, edges and subgraphs, checked
    /// before each one is added so no single statement can build an unbounded amount.
    fn budget(&self) -> Option<()> {
        (self.f.nodes.len() + self.f.edges.len() + self.f.subgraphs.len() < MAX_ITEMS).then_some(())
    }

    fn statement(&mut self, s: &str) -> Option<()> {
        let word = s.split(|c: char| c.is_whitespace() || c == ':').next().unwrap_or("");
        match word {
            "subgraph" => {
                let rest = s["subgraph".len()..].trim();
                let title = match rest.find('[') {
                    Some(i) if rest.ends_with(']') => clean(rest[i + 1..rest.len() - 1].trim().trim_matches('"')),
                    _ => clean(rest.trim_matches('"')),
                };
                self.budget()?;
                if self.stack.len() >= MAX_DEPTH {
                    return None;
                }
                self.f.subgraphs.push(Subgraph { title, parent: self.stack.last().copied() });
                self.stack.push(self.f.subgraphs.len() - 1);
            }
            "end" if s == "end" => {
                self.stack.pop()?;
            }
            "direction" | "classDef" | "class" | "style" | "linkStyle" | "click" | "accTitle" | "accDescr" => {}
            _ => self.chain(s)?,
        }
        Some(())
    }

    fn chain(&mut self, s: &str) -> Option<()> {
        let mut c = Cur::new(s);
        let mut left = self.group(&mut c)?;
        loop {
            c.skip_ws();
            if c.done() {
                return Some(());
            }
            let link = link(&mut c)?;
            c.skip_ws();
            let right = self.group(&mut c)?;
            for &from in &left {
                for &to in &right {
                    self.budget()?;
                    self.f.edges.push(Edge {
                        from,
                        to,
                        label: link.label.clone(),
                        line: link.line,
                        start: link.start,
                        end: link.end,
                        minlen: link.minlen,
                    });
                }
            }
            left = right;
        }
    }

    fn group(&mut self, c: &mut Cur) -> Option<Vec<usize>> {
        let mut v = vec![self.node(c)?];
        loop {
            c.skip_ws();
            if !c.eat("&") {
                return Some(v);
            }
            c.skip_ws();
            v.push(self.node(c)?);
        }
    }

    fn node(&mut self, c: &mut Cur) -> Option<usize> {
        let id = ident(c)?;
        let shape = shape(c)?;
        if c.eat(":::") {
            ident(c)?;
        }
        let cluster = self.stack.last().copied();
        let i = match self.ids.get(&id) {
            Some(&i) => i,
            None => {
                self.budget()?;
                self.f.nodes.push(Node { label: id.clone(), id: id.clone(), shape: NodeShape::Rect, cluster });
                self.ids.insert(id, self.f.nodes.len() - 1);
                self.f.nodes.len() - 1
            }
        };
        let n = &mut self.f.nodes[i];
        // A node first used outside a subgraph joins the first subgraph that mentions it.
        if n.cluster.is_none() {
            n.cluster = cluster;
        }
        if let Some((label, shape)) = shape {
            n.label = label;
            n.shape = shape;
        }
        Some(i)
    }
}

fn ident(c: &mut Cur) -> Option<String> {
    let start = c.i;
    while let Some(ch) = c.peek() {
        let word = |ch: char| ch.is_alphanumeric() || ch == '_';
        if word(ch) || (ch == '-' && c.i > start && c.peek2().is_some_and(word)) {
            c.bump();
        } else {
            break;
        }
    }
    (c.i > start).then(|| c.s[start..c.i].to_owned())
}

/// Openers and closers, longest first.
const SHAPES: &[(&str, &str, NodeShape)] = &[
    ("(((", ")))", NodeShape::Circle),
    ("([", "])", NodeShape::Stadium),
    ("[[", "]]", NodeShape::Subroutine),
    ("[(", ")]", NodeShape::Cylinder),
    ("((", "))", NodeShape::Circle),
    ("{{", "}}", NodeShape::Hexagon),
    (">", "]", NodeShape::Asymmetric),
    ("[", "]", NodeShape::Rect),
    ("(", ")", NodeShape::Round),
    ("{", "}", NodeShape::Diamond),
];

/// A node's bracketed label: `Some(None)` when there is none, `None` when it is malformed.
fn shape(c: &mut Cur) -> Option<Option<(String, NodeShape)>> {
    if c.rest().starts_with("@{") {
        return None;
    }
    // `[/…/]`, `[\…\]` lean; `[/…\]`, `[\…/]` are trapezoids.
    for open in ["[/", "[\\"] {
        if c.rest().starts_with(open) {
            c.eat(open);
            let rest = c.rest();
            let end = rest.find("/]").into_iter().chain(rest.find("\\]")).min()?;
            let same = rest[end..].starts_with(&open[1..]);
            let label = clean(rest[..end].trim().trim_matches('"'));
            c.i += end + 2;
            return Some(Some((label, if same { NodeShape::Parallelogram } else { NodeShape::Trapezoid })));
        }
    }
    let Some(&(open, close, kind)) = SHAPES.iter().find(|(open, ..)| c.rest().starts_with(open)) else {
        return Some(None);
    };
    c.eat(open);
    c.skip_ws();
    let label = if c.peek() == Some('"') {
        c.bump();
        let end = c.rest().find('"')?;
        let label = c.rest()[..end].to_owned();
        c.i += end + 1;
        c.skip_ws();
        if !c.eat(close) {
            return None;
        }
        label
    } else {
        let end = c.rest().find(close)?;
        let label = c.rest()[..end].to_owned();
        c.i += end + close.len();
        label
    };
    Some(Some((clean(&label), kind)))
}

fn run(c: &mut Cur) -> String {
    let start = c.i;
    while c.peek().is_some_and(|ch| matches!(ch, '-' | '=' | '.' | '~')) {
        c.bump();
    }
    c.s[start..c.i].to_owned()
}

/// A head right after a link's body: `>`, or `o`/`x` when no identifier follows.
fn end_head(c: &mut Cur) -> Head {
    match c.peek() {
        Some('>') => {
            c.bump();
            Head::Arrow
        }
        Some(ch @ ('o' | 'x')) if c.peek2().is_none_or(|n| n.is_whitespace() || n == '|') => {
            c.bump();
            if ch == 'o' { Head::Circle } else { Head::Cross }
        }
        _ => Head::None,
    }
}

fn link(c: &mut Cur) -> Option<Link> {
    let start = if c.eat("<") {
        Head::Arrow
    } else if matches!(c.peek(), Some('o' | 'x')) && matches!(c.peek2(), Some('-' | '=')) {
        let ch = c.peek();
        c.bump();
        if ch == Some('o') { Head::Circle } else { Head::Cross }
    } else {
        Head::None
    };
    let first = run(c);
    if first.len() < 2 {
        return None;
    }
    let mut end = end_head(c);
    let mut body = first.clone();
    let mut label = None;
    if end == Head::None && matches!(first.as_str(), "--" | "==" | "-.") {
        // `-- text -->`: the text runs to the closing body.
        let close = if first == "-." { ".-" } else { &first[..] };
        let at = c.rest().find(close)?;
        let text = clean(&c.rest()[..at]);
        if text.is_empty() {
            return None;
        }
        label = Some(text);
        c.i += at;
        body = run(c);
        end = end_head(c);
    }
    let line = if body.contains('~') {
        Line::Invisible
    } else if body.contains('=') {
        Line::Thick
    } else if body.contains('.') {
        Line::Dotted
    } else {
        Line::Solid
    };
    let minlen = match line {
        Line::Dotted => body.matches('.').count(),
        _ => {
            let n = body.chars().filter(|&ch| ch == '-' || ch == '=' || ch == '~').count();
            if end == Head::None { n.saturating_sub(2) } else { n.saturating_sub(1) }
        }
    }
    .clamp(1, MAX_MINLEN);
    let save = c.i;
    c.skip_ws();
    if c.eat("|") {
        let at = c.rest().find('|')?;
        let text = clean(&c.rest()[..at]);
        c.i += at + 1;
        label = (!text.is_empty()).then_some(text).or(label);
    } else {
        c.i = save;
    }
    Some(Link { line, start, end, minlen, label })
}

// Drawing.

const PAD: [f32; 2] = [30.0, 22.0];
const SPACING: Spacing = Spacing { node: 36.0, rank: 44.0, cluster: 14.0 };

struct Sized {
    label: Measured,
    size: [f32; 2],
}

fn node_size(shape: NodeShape, t: [f32; 2]) -> [f32; 2] {
    let [w, h] = [t[0] + PAD[0], (t[1] + PAD[1]).max(40.0)];
    match shape {
        NodeShape::Rect | NodeShape::Round => [w.max(60.0), h],
        NodeShape::Stadium | NodeShape::Hexagon => [w + h / 2.0, h],
        NodeShape::Subroutine => [w + 16.0, h],
        NodeShape::Asymmetric => [w + h / 3.0, h],
        NodeShape::Cylinder => [w.max(60.0), h + 16.0],
        NodeShape::Parallelogram | NodeShape::Trapezoid => [w + h * 0.6, h],
        NodeShape::Circle => {
            let d = (t[0] * t[0] + t[1] * t[1]).sqrt() + 20.0;
            [d.max(48.0), d.max(48.0)]
        }
        NodeShape::Diamond => {
            // A rhombus holding the label's box: |x|/a + |y|/b ≤ 1 at its corners.
            let a = t[0] / 2.0 + t[1] + 12.0;
            let b = t[1] / 2.0 * a / (t[1] + 12.0) + 10.0;
            [2.0 * a, (2.0 * b).max(40.0)]
        }
    }
}

/// Where the line from `c` toward `toward` leaves a node of `shape` and `size` centred at `c`.
fn boundary(shape: NodeShape, c: [f32; 2], size: [f32; 2], toward: [f32; 2]) -> [f32; 2] {
    let (dx, dy) = (toward[0] - c[0], toward[1] - c[1]);
    if dx.abs() < 1e-3 && dy.abs() < 1e-3 {
        return c;
    }
    let (a, b) = (size[0] / 2.0, size[1] / 2.0);
    let t = match shape {
        NodeShape::Circle => 1.0 / ((dx / a).powi(2) + (dy / b).powi(2)).sqrt(),
        NodeShape::Diamond => 1.0 / (dx.abs() / a + dy.abs() / b),
        _ => {
            let tx = if dx.abs() > 1e-3 { a / dx.abs() } else { f32::MAX };
            let ty = if dy.abs() > 1e-3 { b / dy.abs() } else { f32::MAX };
            tx.min(ty)
        }
    };
    let t = t.min(1.0);
    [c[0] + dx * t, c[1] + dy * t]
}

fn outline(shape: NodeShape, c: [f32; 2], s: [f32; 2]) -> Vec<[f32; 2]> {
    let (x0, y0, x1, y1) = (c[0] - s[0] / 2.0, c[1] - s[1] / 2.0, c[0] + s[0] / 2.0, c[1] + s[1] / 2.0);
    let h = s[1];
    match shape {
        NodeShape::Rect | NodeShape::Subroutine => rounded_rect(x0, y0, x1, y1, 3.0),
        NodeShape::Round => rounded_rect(x0, y0, x1, y1, 10.0),
        NodeShape::Stadium => rounded_rect(x0, y0, x1, y1, h / 2.0),
        NodeShape::Circle => ellipse(c, s[0] / 2.0, s[1] / 2.0),
        NodeShape::Diamond => vec![[c[0], y0], [x1, c[1]], [c[0], y1], [x0, c[1]]],
        NodeShape::Hexagon => {
            let i = h / 4.0;
            vec![[x0 + i, y0], [x1 - i, y0], [x1, c[1]], [x1 - i, y1], [x0 + i, y1], [x0, c[1]]]
        }
        NodeShape::Asymmetric => vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0 + h / 3.0, c[1]]],
        NodeShape::Parallelogram => {
            let k = h * 0.3;
            vec![[x0 + k, y0], [x1, y0], [x1 - k, y1], [x0, y1]]
        }
        NodeShape::Trapezoid => {
            let k = h * 0.3;
            vec![[x0 + k, y0], [x1 - k, y0], [x1, y1], [x0, y1]]
        }
        NodeShape::Cylinder => {
            let ry = 8.0;
            let rx = s[0] / 2.0;
            let mut pts = arc([c[0], y0 + ry], rx, ry, 180.0, 360.0, 20);
            pts.extend(arc([c[0], y1 - ry], rx, ry, 0.0, 180.0, 20));
            pts
        }
    }
}

pub(crate) fn draw(f: &Flowchart, paint: &DiagramPaint) -> Option<Scene> {
    let nodes: Vec<Sized> = f
        .nodes
        .iter()
        .map(|n| {
            let label = measure(&n.label, paint.font);
            Sized { size: node_size(n.shape, label.size), label }
        })
        .collect();
    let edge_font = TextStyle { size: paint.font.size - 1.0, line_height: paint.font.line_height - 2.0, ..paint.font };
    let labels: Vec<Option<Measured>> = f.edges.iter().map(|e| e.label.as_ref().map(|l| measure(l, edge_font))).collect();
    let title_font = TextStyle { weight: 600.0, ..edge_font };
    let titles: Vec<Measured> = f.subgraphs.iter().map(|s| measure(&s.title, title_font)).collect();
    let fonts = Fonts { node: paint.font, edge: edge_font, title: title_font };
    let mut canvas = draw_in(f, f.dir, &nodes, &labels, &titles, fonts, paint)?;
    if canvas.width() > room(paint) && matches!(f.dir, Dir::Right | Dir::Left) {
        let down = draw_in(f, if f.dir == Dir::Right { Dir::Down } else { Dir::Up }, &nodes, &labels, &titles, fonts, paint)?;
        if down.width() < canvas.width() {
            canvas = down;
        }
    }
    let scale = fit(canvas.width(), paint);
    canvas.finish(scale)
}

#[derive(Clone, Copy)]
struct Fonts {
    node: TextStyle,
    edge: TextStyle,
    title: TextStyle,
}

fn draw_in(f: &Flowchart, dir: Dir, nodes: &[Sized], labels: &[Option<Measured>], titles: &[Measured], fonts: Fonts, paint: &DiagramPaint) -> Option<Canvas> {
    let across = matches!(dir, Dir::Right | Dir::Left);
    // Layout axes are [along a layer, across layers]; up and left lay out reversed edges.
    let ax = |s: [f32; 2]| if across { [s[1], s[0]] } else { s };
    let flip = matches!(dir, Dir::Up | Dir::Left);
    let label_box = |m: &Measured| [m.size[0] + 10.0, m.size[1] + 4.0];
    let graph = Graph {
        sizes: nodes.iter().map(|n| ax(n.size)).collect(),
        edges: f
            .edges
            .iter()
            .zip(labels)
            .map(|(e, l)| {
                let (from, to) = if flip { (e.to, e.from) } else { (e.from, e.to) };
                GEdge { from, to, label: l.as_ref().map(|m| ax(label_box(m))), minlen: e.minlen }
            })
            .collect(),
        node_cluster: f.nodes.iter().map(|n| n.cluster).collect(),
        clusters: f
            .subgraphs
            .iter()
            .zip(titles)
            .map(|(s, t)| {
                let lead = if t.text.is_empty() { 0.0 } else { t.size[1] + 8.0 };
                GCluster { parent: s.parent, lead: if across { [lead, 0.0] } else { [0.0, lead] } }
            })
            .collect(),
    };
    let placed = layout(&graph, &SPACING)?;
    let pt = |p: [f32; 2]| if across { [p[1], p[0]] } else { p };
    let mut c = Canvas::default();

    // Subgraphs, outermost first.
    let depth = |mut i: usize| {
        let mut d = 0;
        while let Some(p) = f.subgraphs[i].parent {
            i = p;
            d += 1;
            if d > f.subgraphs.len() {
                break;
            }
        }
        d
    };
    let mut order: Vec<usize> = (0..f.subgraphs.len()).collect();
    order.sort_by_key(|&i| depth(i));
    for i in order {
        let b = placed.clusters[i];
        if b == [0.0; 4] {
            continue;
        }
        let (p0, p1) = (pt([b[0], b[1]]), pt([b[2], b[3]]));
        c.shape(rounded_rect(p0[0], p0[1], p1[0], p1[1], 6.0), paint.cluster_fill, paint.surface_border, 1.0);
        c.label_at(&titles[i], [p0[0] + 10.0, p0[1] + 7.0], fonts.title, paint.muted, Align::Start);
    }

    // Edges.
    let centers: Vec<[f32; 2]> = placed.centers.iter().map(|&p| pt(p)).collect();
    for (i, e) in f.edges.iter().enumerate() {
        let mut path: Vec<[f32; 2]> = placed.paths[i].iter().map(|&p| pt(p)).collect();
        if flip {
            path.reverse();
        }
        let (from, to) = (e.from, e.to);
        let mid = if path.len() <= 1 {
            let s = labels[i].as_ref().map_or(0.0, |m| label_box(m)[0]);
            self_loop(&mut c, e, centers[from], nodes[from].size, paint).map(|at| [at[0] + s / 2.0, at[1]])
        } else {
            let n = path.len();
            path[0] = boundary(f.nodes[from].shape, centers[from], nodes[from].size, path[1]);
            path[n - 1] = boundary(f.nodes[to].shape, centers[to], nodes[to].size, path[n - 2]);
            connector(&mut c, &mut path, e, paint);
            placed.labels[i].map(pt)
        };
        if let (Some(m), Some(at)) = (&labels[i], mid) {
            let s = label_box(m);
            c.fill(rounded_rect(at[0] - s[0] / 2.0, at[1] - s[1] / 2.0, at[0] + s[0] / 2.0, at[1] + s[1] / 2.0, 3.0), paint.surface);
            c.label(m, at, fonts.edge, paint.text, Align::Center);
        }
    }

    // Nodes.
    for (i, n) in f.nodes.iter().enumerate() {
        let (ctr, size) = (centers[i], nodes[i].size);
        c.shape(outline(n.shape, ctr, size), paint.node_fill, paint.node_border, 1.0);
        let (x0, y0, x1, y1) = (ctr[0] - size[0] / 2.0, ctr[1] - size[1] / 2.0, ctr[0] + size[0] / 2.0, ctr[1] + size[1] / 2.0);
        match n.shape {
            NodeShape::Subroutine => {
                c.stroke(vec![[x0 + 8.0, y0], [x0 + 8.0, y1]], false, 1.0, paint.node_border);
                c.stroke(vec![[x1 - 8.0, y0], [x1 - 8.0, y1]], false, 1.0, paint.node_border);
            }
            NodeShape::Cylinder => c.stroke(arc([ctr[0], y0 + 8.0], size[0] / 2.0, 8.0, 0.0, 180.0, 20), false, 1.0, paint.node_border),
            _ => {}
        }
        let center = if n.shape == NodeShape::Cylinder { [ctr[0], ctr[1] + 4.0] } else { ctr };
        c.label(&nodes[i].label, center, fonts.node, paint.text, Align::Center);
    }
    Some(c)
}

const ARROW: f32 = 8.0;

/// Draws `e` along `path` (endpoints on the node outlines), heads included.
fn connector(c: &mut Canvas, path: &mut [[f32; 2]], e: &Edge, paint: &DiagramPaint) {
    let n = path.len();
    let (tip_end, tip_start) = (path[n - 1], path[0]);
    let (dir_end, dir_start) = (unit(path[n - 2], path[n - 1]), unit(path[1], path[0]));
    let inset = |h: Head| match h {
        Head::None => 0.0,
        Head::Arrow => ARROW - 1.0,
        Head::Circle => 8.0,
        Head::Cross => 4.0,
    };
    let shorten = |p: [f32; 2], d: [f32; 2], by: f32| [p[0] - d[0] * by, p[1] - d[1] * by];
    if dist(path[n - 2], path[n - 1]) > inset(e.end) + 1.0 {
        path[n - 1] = shorten(path[n - 1], dir_end, inset(e.end));
    }
    if dist(path[0], path[1]) > inset(e.start) + 1.0 {
        path[0] = shorten(path[0], dir_start, inset(e.start));
    }
    let width = match e.line {
        Line::Thick => 2.5,
        _ => 1.25,
    };
    let curve = smooth(path);
    match e.line {
        Line::Invisible => return,
        Line::Dotted => c.dashed(&curve, width, paint.line, 3.0, 3.0),
        _ => c.stroke(curve, false, width, paint.line),
    }
    head(c, e.end, tip_end, dir_end, width, paint);
    head(c, e.start, tip_start, dir_start, width, paint);
}

fn head(c: &mut Canvas, h: Head, tip: [f32; 2], d: [f32; 2], width: f32, paint: &DiagramPaint) {
    let back = |by: f32| [tip[0] - d[0] * by, tip[1] - d[1] * by];
    match h {
        Head::None => {}
        Head::Arrow => c.fill(arrowhead(tip, d, ARROW + width, 7.0 + width), paint.line),
        Head::Circle => c.shape(ellipse(back(4.0), 4.0, 4.0), paint.node_fill, paint.line, width),
        Head::Cross => {
            let m = back(4.0);
            let (a, b) = ([d[0] + d[1], d[1] - d[0]], [d[0] - d[1], d[1] + d[0]]);
            let k = 3.5 / std::f32::consts::SQRT_2;
            c.stroke(vec![[m[0] - a[0] * k, m[1] - a[1] * k], [m[0] + a[0] * k, m[1] + a[1] * k]], false, width, paint.line);
            c.stroke(vec![[m[0] - b[0] * k, m[1] - b[1] * k], [m[0] + b[0] * k, m[1] + b[1] * k]], false, width, paint.line);
        }
    }
}

/// A loop off the node's right side; returns the left middle of where its label goes.
fn self_loop(c: &mut Canvas, e: &Edge, ctr: [f32; 2], size: [f32; 2], paint: &DiagramPaint) -> Option<[f32; 2]> {
    let x = ctr[0] + size[0] / 2.0;
    let (a, b) = ([x, ctr[1] - size[1] / 4.0], [x, ctr[1] + size[1] / 4.0]);
    let mut path = [a, [x + 18.0, a[1] - 4.0], [x + 26.0, ctr[1]], [x + 18.0, b[1] + 4.0], b];
    connector(c, &mut path, e, paint);
    Some([x + 30.0, ctr[1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow(src: &str) -> Flowchart {
        match super::super::parse(src) {
            Some(super::super::Diagram::Flowchart(f)) => f,
            _ => panic!("not a flowchart: {src:?}"),
        }
    }

    fn edges(f: &Flowchart) -> Vec<(&str, &str)> {
        f.edges.iter().map(|e| (f.nodes[e.from].id.as_str(), f.nodes[e.to].id.as_str())).collect()
    }

    #[test]
    fn directions_shapes_and_labels() {
        let f = flow(
            "flowchart LR\n  A[Start] --> B(Round)\n  B --> C([Stadium]) --> D[[Sub]]\n  D --> E[(Database)] & F((Circle))\n  F --> G>Flag] --> H{Decide?} --> I{{Hex}}\n  I --> J[/Lean/] --> K[/Trap\\]",
        );
        assert_eq!(f.dir, Dir::Right);
        let shapes: Vec<(&str, &str, NodeShape)> = f.nodes.iter().map(|n| (n.id.as_str(), n.label.as_str(), n.shape)).collect();
        assert_eq!(
            shapes,
            vec![
                ("A", "Start", NodeShape::Rect),
                ("B", "Round", NodeShape::Round),
                ("C", "Stadium", NodeShape::Stadium),
                ("D", "Sub", NodeShape::Subroutine),
                ("E", "Database", NodeShape::Cylinder),
                ("F", "Circle", NodeShape::Circle),
                ("G", "Flag", NodeShape::Asymmetric),
                ("H", "Decide?", NodeShape::Diamond),
                ("I", "Hex", NodeShape::Hexagon),
                ("J", "Lean", NodeShape::Parallelogram),
                ("K", "Trap", NodeShape::Trapezoid),
            ]
        );
        assert_eq!(edges(&f)[3..5], [("D", "E"), ("D", "F")], "`&` fans out");
    }

    #[test]
    fn link_styles_heads_lengths_and_labels() {
        let f = flow(
            "graph TD\nA --> B\nA --- C\nA -.-> D\nA ==> E\nA -- yes --> F\nA -->|no| G\nA <--> H\nA --o I\nA --x J\nA ---> K\nA -. maybe .-> L\nA == sure ==> M\nA ~~~ N\nA-->O;O-->P",
        );
        let e = |to: &str| f.edges.iter().find(|e| f.nodes[e.to].id == to).expect(to);
        assert_eq!((e("B").line, e("B").end, e("B").minlen), (Line::Solid, Head::Arrow, 1));
        assert_eq!((e("C").line, e("C").end), (Line::Solid, Head::None));
        assert_eq!((e("D").line, e("D").end), (Line::Dotted, Head::Arrow));
        assert_eq!((e("E").line, e("E").end), (Line::Thick, Head::Arrow));
        assert_eq!(e("F").label.as_deref(), Some("yes"));
        assert_eq!(e("G").label.as_deref(), Some("no"));
        assert_eq!((e("H").start, e("H").end), (Head::Arrow, Head::Arrow));
        assert_eq!(e("I").end, Head::Circle);
        assert_eq!(e("J").end, Head::Cross);
        assert_eq!(e("K").minlen, 2);
        assert_eq!((e("L").line, e("L").label.as_deref()), (Line::Dotted, Some("maybe")));
        assert_eq!((e("M").line, e("M").label.as_deref()), (Line::Thick, Some("sure")));
        assert_eq!(e("N").line, Line::Invisible);
        assert_eq!(e("P").from, f.nodes.iter().position(|n| n.id == "O").unwrap(), "`;` separates statements");
    }

    #[test]
    fn subgraphs_nest_and_claim_nodes() {
        let f = flow(
            "graph TB\n  c1-->a2\n  subgraph one [The first]\n    a1-->a2\n    subgraph inner\n      x1\n    end\n  end\n  subgraph \"two\"\n    b1-->b2\n  end\n  classDef hot fill:#f00\n  class a1 hot\n  style b1 fill:#0f0\n  click b2 callback\n  a1:::hot --> b1",
        );
        let titles: Vec<&str> = f.subgraphs.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["The first", "inner", "two"]);
        assert_eq!(f.subgraphs[1].parent, Some(0));
        let cluster = |id: &str| f.nodes.iter().find(|n| n.id == id).unwrap().cluster;
        assert_eq!(cluster("c1"), None);
        assert_eq!(cluster("a2"), Some(0), "first used outside, then inside");
        assert_eq!(cluster("x1"), Some(1));
        assert_eq!(cluster("b2"), Some(2));
    }

    #[test]
    fn quoted_labels_breaks_and_dashed_ids() {
        let f = flow("graph TD\n  web-server[\"Web (edge) server\"] --> db[(\"Postgres<br/>primary\")]");
        assert_eq!(f.nodes[0].id, "web-server");
        assert_eq!(f.nodes[0].label, "Web (edge) server");
        assert_eq!(f.nodes[1].label, "Postgres\nprimary");
    }

    #[test]
    fn malformed_and_unsupported_input_is_rejected() {
        for src in [
            "graph XY\nA-->B",
            "graph TD\nA[unclosed --> B",
            "graph TD\nA -- --> B",
            "graph TD\nA -->|unclosed B",
            "graph TD\nA - B",
            "graph TD\nA@{ shape: rect } --> B",
            "graph TD\nend",
            "graph TD\nA --> ",
            "graph TD",
            "flowchart TD\n  A{\"quote never closes} --> B",
        ] {
            assert!(super::super::parse(src).is_none(), "{src:?}");
        }
    }

    #[test]
    fn every_prefix_of_a_source_parses_or_falls_back_without_panicking() {
        let src = "flowchart TD\n  A[Start] -->|go| B{Ok?}\n  B -- yes --> C([Done])\n  B -.->|no| A\n  subgraph s [Group]\n    C --> D[(Store)]\n  end";
        let paint = crate::MarkdownStyle::neutral(false).diagram;
        for (i, _) in src.char_indices() {
            let _ = super::super::scene(&src[..i], &paint);
        }
        assert!(super::super::scene(src, &paint).is_some());
    }

    #[test]
    fn boundary_points_sit_on_the_outline() {
        let c = [0.0, 0.0];
        assert_eq!(boundary(NodeShape::Rect, c, [100.0, 40.0], [0.0, 100.0]), [0.0, 20.0]);
        assert_eq!(boundary(NodeShape::Rect, c, [100.0, 40.0], [100.0, 0.0]), [50.0, 0.0]);
        let d = boundary(NodeShape::Diamond, c, [100.0, 40.0], [100.0, 40.0]);
        assert!((d[0].abs() / 50.0 + d[1].abs() / 20.0 - 1.0).abs() < 1e-4);
        let e = boundary(NodeShape::Circle, c, [40.0, 40.0], [30.0, 40.0]);
        assert!((dist(c, e) - 20.0).abs() < 1e-4);
    }
}

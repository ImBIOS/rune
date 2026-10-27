//! GFM tables: columns sized from their content with [`rune::text::measure`], then shared out by
//! flex growth so every row's cells line up without a grid.

use super::*;

/// Estimated bytes of column widths kept beyond the tables on screen.
const WIDTHS_BYTES: usize = 1 << 20;

/// Per column: the content's width on one line and its longest word (capped), padding included.
#[derive(Debug)]
struct Columns {
    natural: Vec<f32>,
    min: Vec<f32>,
}

thread_local! {
    static WIDTHS: RefCell<Budget<u64, Rc<Columns>>> = RefCell::new(Budget::new(WIDTHS_BYTES));
}

pub(super) fn font_key(f: TextStyle) -> (u32, u32, u32, u32, String) {
    (f.size.to_bits(), f.weight.to_bits(), f.line_height.to_bits(), f.tracking.to_bits(), format!("{:?}{}{}{}", f.family, f.variations, f.features, f.italic))
}

/// `features` with tabular figures on, interned so the result is `'static` like a
/// [`TextStyle`]'s features (a style has few distinct feature lists).
fn with_tnum(features: &'static str) -> &'static str {
    if features.is_empty() {
        return "'tnum' 1";
    }
    if features.contains("tnum") {
        return features;
    }
    static INTERNED: std::sync::Mutex<Vec<(&'static str, &'static str)>> = std::sync::Mutex::new(Vec::new());
    let mut interned = INTERNED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(&(_, out)) = interned.iter().find(|(f, _)| *f == features) {
        return out;
    }
    let out: &'static str = Box::leak(format!("{features}, 'tnum' 1").into_boxed_str());
    interned.push((features, out));
    out
}

impl MarkdownStyle {
    /// A table's outline and row rules, and its header row's fill.
    pub fn table(mut self, border: Color, header_fill: Color) -> Self {
        self.table_outline = Some(border);
        self.table_rule = border;
        self.table_head_fill = header_fill;
        self
    }
    /// The rounded outline around a table, or `None` for none; a rule then runs under the
    /// last row too, as on a web page.
    pub fn table_outline(mut self, outline: Option<Color>) -> Self {
        self.table_outline = outline;
        self
    }
    /// The hairline under the header and between rows.
    pub fn table_rules(mut self, c: Color) -> Self {
        self.table_rule = c;
        self
    }
    /// The header row's text colour (the body text colour by default).
    pub fn table_header_text(mut self, c: Color) -> Self {
        self.table_head_text = c;
        self
    }
    /// The header row's fill alone (`table` sets it with the border).
    pub fn table_header_fill(mut self, c: Color) -> Self {
        self.table_head_fill = c;
        self
    }
    /// The hairline under the header row, when it should stand out from the row rules, as a
    /// web `th { border-bottom }` in a stronger colour than the `td`s'. The row rule colour
    /// by default.
    pub fn table_header_rule(mut self, c: Color) -> Self {
        self.table_head_rule = Some(c);
        self
    }
    /// The header row's weight (the strong weight by default), for a medium-weight `th` beside
    /// bold `strong` text.
    pub fn table_header_weight(mut self, w: f32) -> Self {
        self.table_head_weight = Some(w);
        self
    }
    /// Whether cells set figures at one width (OpenType `tnum`) so numbers line up down a
    /// column. On by default; only digits change.
    pub fn table_tabular_figures(mut self, on: bool) -> Self {
        self.table_tabular = on;
        self
    }
    /// The narrowest a column shrinks to when its longest word is wider (120 points by default).
    pub fn table_min_column(mut self, points: f32) -> Self {
        self.table_min_column = points.max(0.0);
        self
    }
    /// The cells' font (the body font by default). The header takes it at the strong weight,
    /// and inline code scales from its size. A web table at `font-size: 0.93em` is the body
    /// style at 0.93 of its size.
    pub fn table_font(mut self, f: TextStyle) -> Self {
        self.table_font = Some(f);
        self
    }
    /// Each cell's padding: `x` on the left and right, `y` above and below (10 and 6 points by
    /// default).
    pub fn table_cell_padding(mut self, x: f32, y: f32) -> Self {
        self.table_pad = [x.max(0.0), y.max(0.0)];
        self
    }
    /// Whether the first column starts at the table's left edge, with no left padding, so its
    /// text lines up with the paragraphs around it, as a web table whose first cells have
    /// `padding-left: 0`. Off by default.
    pub fn table_flush_left(mut self, on: bool) -> Self {
        self.table_flush_left = on;
        self
    }
    /// Tables that read like a data [`Table`](crate::Table) beside them: its cell font, cell
    /// padding (sized so a one-line row is the data table's row height), rules and muted header
    /// text. The outline, when there is one, takes the rule colour.
    pub fn table_like(mut self, t: &crate::TableStyle) -> Self {
        self.table_font = Some(t.font);
        self.table_pad = [t.cell_pad, ((t.row_height - t.font.line_height) / 2.0).max(0.0)];
        self.table_rule = t.rule;
        self.table_head_text = t.header_text;
        self.table_outline = self.table_outline.map(|_| t.rule);
        self
    }
}

impl Renderer {
    fn head_font(&self) -> TextStyle {
        self.cell_font(TextStyle { weight: self.s.table_head_weight.unwrap_or(self.s.strong_weight), ..self.s.table_font.unwrap_or(self.s.body) })
    }

    fn body_font(&self) -> TextStyle {
        self.cell_font(self.s.table_font.unwrap_or(self.s.body))
    }

    /// Column `j`'s left padding.
    fn pad_left(&self, j: usize) -> f32 {
        if j == 0 && self.s.table_flush_left { 0.0 } else { self.s.table_pad[0] }
    }

    fn cell_font(&self, f: TextStyle) -> TextStyle {
        if self.s.table_tabular { TextStyle { features: with_tnum(f.features), ..f } } else { f }
    }

    pub(super) fn table(&self, t: &Table, head_maps: &[SpanMap], row_maps: &[Vec<SpanMap>]) -> El {
        let s = &self.s;
        let n = t.align.len().max(t.head.len()).max(1);
        let cols = self.columns(t, n);
        let total: f32 = cols.natural.iter().sum();
        // Without an outline the corners stay square, so the rules run the full width, and no
        // border inset moves the cells off the text around the table. Either way columns too
        // wide to fit stop at the frame.
        let border = if s.table_outline.is_some() { 0.5 } else { 0.0 };
        let mut frame = col().w_full().max_w(total + 2.0 * border).padding(border);
        frame = match s.table_outline {
            Some(outline) => frame.radius(6.0).border(0.5, outline).clip_rounded(6.0),
            None => frame.clip_soft(0.0),
        };
        frame = frame.child(self.table_row(&t.head, head_maps, t, &cols, self.head_font(), s.table_head_text).key("th").fill(s.table_head_fill));
        let rule = |key: String| el().key(key).w_full().h(0.5).fill(s.table_rule);
        for (i, cells) in t.rows.iter().enumerate() {
            let rmap = row_maps.get(i).map_or(&[][..], Vec::as_slice);
            let sep = rule(format!("s{i}"));
            let sep = match s.table_head_rule { Some(c) if i == 0 => sep.fill(c), _ => sep };
            frame = frame.child(sep).child(self.table_row(cells, rmap, t, &cols, self.body_font(), s.text).key(format!("r{i}")));
        }
        if s.table_outline.is_none() {
            let end = rule("s-end".into());
            frame = frame.child(match s.table_head_rule { Some(c) if t.rows.is_empty() => end.fill(c), _ => end });
        }
        if self.scroll_wide {
            // Every column at its natural width, scrolling when the table is wider than the text.
            return col().key("scroll").w_full().scroll_x().child(frame.w(total + 2.0 * border));
        }
        frame
    }

    fn table_row(&self, cells: &[Inline], maps: &[SpanMap], t: &Table, cols: &Columns, font: TextStyle, color: Color) -> El {
        let empty = Inline::default();
        let [pad_x, pad_y] = self.s.table_pad;
        let mut r = row().w_full();
        for (j, (natural, min)) in cols.natural.iter().zip(&cols.min).enumerate() {
            let inl = cells.get(j).unwrap_or(&empty);
            let align = t.align.get(j).copied().unwrap_or_default();
            let content = rich(inl.text.clone(), self.spans(inl, font.size))
                .font(font)
                .color(color)
                .align(align)
                .reveal(self.reveal)
                .w_full();
            r = r.child(
                // Clipped: a word wider than a shrunk column (or a row still streaming in) must
                // not run into the next column.
                col().key(format!("c{j}")).w(*natural).shrink().min_w(*min).pl(self.pad_left(j)).pr(pad_x).py(pad_y).clip_soft(0.0).child(self.select_inline(content, inl, maps.get(j))),
            );
        }
        r
    }

    /// Column widths from the header and every finished row, cached by content and font. A last
    /// row without a newline is still arriving only while the text streams; otherwise the
    /// document is whole and it counts too.
    fn columns(&self, t: &Table, n: usize) -> Rc<Columns> {
        let (head, body) = (self.head_font(), self.body_font());
        let settled = if t.open && self.streaming { t.rows.len().saturating_sub(1) } else { t.rows.len() };
        let rows = &t.rows[..settled];
        let cap = self.s.table_min_column;
        let link_styles: Vec<_> = if let Some(look) = &self.link_look {
            t.head
                .iter()
                .chain(rows.iter().flatten())
                .flat_map(|inl| &inl.links)
                .map(|(_, _, url)| look(url).map(|(sp, _)| (sp.weight.map(f32::to_bits), sp.size.map(f32::to_bits), sp.mono, sp.italic)))
                .collect()
        } else {
            Vec::new()
        };
        let key = hash_of((
            n,
            &t.head,
            rows,
            font_key(head),
            font_key(body),
            cap.to_bits(),
            self.s.inline_code_scale.to_bits(),
            self.s.inline_code_weight.to_bits(),
            self.s.table_pad.map(f32::to_bits),
            self.s.table_flush_left,
            link_styles,
        ));
        if let Some(hit) = WIDTHS.with_borrow_mut(|c| c.get(&key).cloned()) {
            return hit;
        }
        let mut natural = vec![0.0f32; n];
        let mut min = vec![0.0f32; n];
        let lines = std::iter::once((&t.head, head)).chain(rows.iter().map(|r| (r, body)));
        for (cells, font) in lines {
            for (j, inl) in cells.iter().take(n).enumerate() {
                if inl.text.is_empty() {
                    continue;
                }
                let spans = self.spans(inl, font.size);
                natural[j] = natural[j].max(rune::text::measure(&inl.text, &spans, font, None).0);
                min[j] = min[j].max(rune::text::measure(&inl.text, &spans, font, Some(0.0)).0.min(cap));
            }
        }
        let pad = |(j, w): (usize, f32)| w + self.pad_left(j) + self.s.table_pad[0];
        let cols = Rc::new(Columns {
            natural: natural.into_iter().enumerate().map(pad).collect(),
            min: min.into_iter().enumerate().map(pad).collect(),
        });
        WIDTHS.with_borrow_mut(|c| c.insert(key, cols.clone(), 8 * n + 64));
        cols
    }
}

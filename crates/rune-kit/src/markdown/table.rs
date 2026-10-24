//! GFM tables: columns sized from their content with [`rune::text::measure`], then shared out by
//! flex growth so every row's cells line up without a grid.

use super::*;

const PAD_X: f32 = 10.0;
const PAD_Y: f32 = 6.0;
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

impl Renderer {
    fn head_font(&self) -> TextStyle {
        self.cell_font(TextStyle { weight: self.s.strong_weight, ..self.s.body })
    }

    fn body_font(&self) -> TextStyle {
        self.cell_font(self.s.body)
    }

    fn cell_font(&self, f: TextStyle) -> TextStyle {
        if self.s.table_tabular { TextStyle { features: with_tnum(f.features), ..f } } else { f }
    }

    pub(super) fn table(&self, t: &Table) -> El {
        let s = &self.s;
        let n = t.align.len().max(t.head.len()).max(1);
        let cols = self.columns(t, n);
        let total: f32 = cols.natural.iter().sum();
        let mut frame = col().w_full().max_w(total + 1.0).padding(0.5);
        // Without an outline the corners stay square, so the rules run the full width. Either way
        // columns too wide to fit stop at the frame.
        frame = match s.table_outline {
            Some(outline) => frame.radius(6.0).border(0.5, outline).clip_rounded(6.0),
            None => frame.clip_soft(0.0),
        };
        frame = frame.child(self.table_row(&t.head, t, &cols, self.head_font(), s.table_head_text).key("th").fill(s.table_head_fill));
        let rule = |key: String| el().key(key).w_full().h(0.5).fill(s.table_rule);
        for (i, cells) in t.rows.iter().enumerate() {
            frame = frame.child(rule(format!("s{i}"))).child(self.table_row(cells, t, &cols, self.body_font(), s.text).key(format!("r{i}")));
        }
        if s.table_outline.is_none() {
            frame = frame.child(rule("s-end".into()));
        }
        frame
    }

    fn table_row(&self, cells: &[Inline], t: &Table, cols: &Columns, font: TextStyle, color: Color) -> El {
        let empty = Inline::default();
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
                col().key(format!("c{j}")).w(*natural).shrink().min_w(*min).px(PAD_X).py(PAD_Y).clip_soft(0.0).child(self.wire_link(content, inl)),
            );
        }
        r
    }

    /// Column widths from the header and every finished row, cached by content and font. A last
    /// row without a newline is still arriving only while the text reveals; otherwise the
    /// document is whole and it counts too.
    fn columns(&self, t: &Table, n: usize) -> Rc<Columns> {
        let (head, body) = (self.head_font(), self.body_font());
        let settled = if t.open && self.reveal > 0.0 { t.rows.len().saturating_sub(1) } else { t.rows.len() };
        let rows = &t.rows[..settled];
        let cap = self.s.table_min_column;
        let key = hash_of((
            n,
            &t.head,
            rows,
            font_key(head),
            font_key(body),
            cap.to_bits(),
            self.s.inline_code_scale.to_bits(),
            self.s.inline_code_weight.to_bits(),
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
        let pad = |w: f32| w + 2.0 * PAD_X;
        let cols = Rc::new(Columns { natural: natural.into_iter().map(pad).collect(), min: min.into_iter().map(pad).collect() });
        WIDTHS.with_borrow_mut(|c| c.insert(key, cols.clone(), 8 * n + 64));
        cols
    }
}

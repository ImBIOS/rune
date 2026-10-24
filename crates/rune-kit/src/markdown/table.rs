//! GFM tables: columns sized from their content with [`rune::text::measure`], then shared out by
//! flex growth so every row's cells line up without a grid.

use super::*;

const PAD_X: f32 = 10.0;
const PAD_Y: f32 = 6.0;
const WIDTHS_LEN: usize = 32;

/// Per column: the content's width on one line and its longest word (capped), padding included.
#[derive(Debug)]
struct Columns {
    natural: Vec<f32>,
    min: Vec<f32>,
}

#[derive(Default)]
struct WidthCache {
    map: HashMap<u64, Rc<Columns>>,
    order: VecDeque<u64>,
}

thread_local! {
    static WIDTHS: RefCell<WidthCache> = RefCell::new(WidthCache::default());
}

pub(super) fn font_key(f: TextStyle) -> (u32, u32, u32, u32, String) {
    (f.size.to_bits(), f.weight.to_bits(), f.line_height.to_bits(), f.tracking.to_bits(), format!("{:?}{}{}{}", f.family, f.variations, f.features, f.italic))
}

impl Renderer {
    fn head_font(&self) -> TextStyle {
        TextStyle { weight: self.s.strong_weight, ..self.s.body }
    }

    pub(super) fn table(&self, t: &Table) -> El {
        let s = &self.s;
        let n = t.align.len().max(t.head.len()).max(1);
        let cols = self.columns(t, n);
        let total: f32 = cols.natural.iter().sum();
        let mut frame = col()
            .w_full()
            .max_w(total + 1.0)
            .padding(0.5)
            .radius(6.0)
            .border(0.5, s.table_border)
            .clip_rounded(6.0)
            .child(self.table_row(&t.head, t, &cols, self.head_font()).key("th").fill(s.table_head_fill));
        for (i, cells) in t.rows.iter().enumerate() {
            frame = frame
                .child(el().key(format!("s{i}")).w_full().h(0.5).fill(s.table_border))
                .child(self.table_row(cells, t, &cols, s.body).key(format!("r{i}")));
        }
        frame
    }

    fn table_row(&self, cells: &[Inline], t: &Table, cols: &Columns, font: TextStyle) -> El {
        let empty = Inline::default();
        let mut r = row().w_full();
        for (j, (natural, min)) in cols.natural.iter().zip(&cols.min).enumerate() {
            let inl = cells.get(j).unwrap_or(&empty);
            let align = t.align.get(j).copied().unwrap_or_default();
            let content = rich(inl.text.clone(), self.spans(inl, font.size))
                .font(font)
                .color(self.s.text)
                .align(align)
                .reveal(self.reveal)
                .w_full();
            r = r.child(
                col().key(format!("c{j}")).grow(*natural).min_w(*min).px(PAD_X).py(PAD_Y).child(self.wire_link(content, inl)),
            );
        }
        r
    }

    /// Column widths from the header and every finished row, cached by content and font.
    fn columns(&self, t: &Table, n: usize) -> Rc<Columns> {
        let (head, body) = (self.head_font(), self.s.body);
        let settled = if t.open { t.rows.len().saturating_sub(1) } else { t.rows.len() };
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
        if let Some(hit) = WIDTHS.with_borrow(|c| c.map.get(&key).cloned()) {
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
        WIDTHS.with_borrow_mut(|c| {
            if c.order.len() >= WIDTHS_LEN
                && let Some(old) = c.order.pop_front()
            {
                c.map.remove(&old);
            }
            c.order.push_back(key);
            c.map.insert(key, cols.clone());
        });
        cols
    }
}

# 28. Resizing a window with a long Markdown note

Reader: someone changing text layout or its cache, or deciding how a notes app shows a long
note. Re-run the bench below before and after changing either.

The question: can a window showing a long Markdown note (`rune_kit::Markdown` in a scroll
view) be resized at 120 Hz (8.3 ms a frame)? Before this change it couldn't at 50 KB: every
frame of a live resize shaped every wrapped text of the note again, 16 ms a frame. Now a width
change only re-breaks lines, and a frame costs 4.6 ms.

## How it was measured

`crates/rune-kit/examples/markdown_resize.rs`, headless, no window:

```
cargo run --profile perf -p rune-kit --example markdown_resize --features markdown,frame-profile -- \
  --sizes 5,50,200 [--memo]
```

The note is generated from fictional words: headings, wrapping paragraphs with bold, italic,
inline code and links, bullet, numbered and task lists, quotes and fenced code, at 5.4 KB
(71 texts), 52 KB (645 texts, 1,364 layout nodes) and 202 KB (2,501 texts, 5,381 nodes). Only
32 texts are on screen at a time. The window is 900 pt tall at 2x; `resize` steps its width
from 900 to 600 pt and back in 1 pt steps, one frame per step (600 frames), as a live resize
does. Each frame is timed in thread CPU, split into the runtime's phases. Numbers are medians
(p95 in brackets) in ms from an M4 Pro Mac mini, the before and after builds run back to back.

## Results

| Resize frame | 5 KB | 50 KB | 200 KB |
|---|---|---|---|
| CPU, before | 1.57 (1.67) | 16.1 (17.1) | 70.6 (75.4) |
| CPU, after | 0.55 (0.59) | 4.6 (5.1) | 23.2 (24.0) |
| Text (shaping and line breaking), before | 1.08 | 11.9 | 49.2 |
| Text, after | 0.06 | 0.70 | 3.2 |
| Texts shaped per frame, before → after | 42 → 0 | 401 → 0 | 1,565 → 0 |
| Texts re-broken per frame, after | 42 | 401 | 1,565 |

Scrolling (0.24 ms a frame at 50 KB) and opening the note (17 ms at 50 KB, 70 ms at 200 KB,
one shaping per distinct text) are unchanged.

## What was slow

`TextSystem` caches each layout under a hash of its text, spans, style, scale *and wrap width*.
A width it hasn't seen misses, and Parley lays the text out from scratch: font selection,
text analysis, HarfRust shaping, then line breaking. During a live resize every width is new,
so every frame shaped every wrapped text in the note, on screen or not (the column has to know
each block's height for its scroll extent).

Only line breaking depends on the width. Parley's `LayoutBuilder::build` shapes; a built
`Layout` can be broken again with `break_all_lines` (or Rune's block-indent and pill breaker)
and aligned again, which clears its old lines and leaves the shaped runs alone. Alignment and
justification are stored per line, so nothing from an earlier width carries over.

## What changed

The cache key splits in two. A *shape key* hashes what shaping reads: text, spans, style,
scale, `wrap_anywhere` and pills. The *layout key* adds the width, alignment and block indents. Beside the
layout cache, an index maps each shape key to the layout key of the latest layout of that
shaping. A miss on the layout key whose shape key has a cached layout clones that layout
(a flat copy of its glyph and run arrays) and breaks and aligns the copy at the new width,
about 2 µs for a paragraph against about 30 µs to shape it.

- Memory: the index holds no layouts, only keys, and drops an entry when its layout is
  evicted. Layouts are kept and evicted exactly as before (one per text and width, two commits
  and then the stale-text budget), so a resize holds what it held before.
- Secure text still bypasses the shared cache entirely: it neither reads nor feeds the index.
- Everything that reads a layout (carets, hit testing, selection boxes, decorations and span
  rings, visible-line culling, `max_lines` and elision, alignment and justification) gets a
  layout with the same lines and glyph positions as one built from scratch at that width. The
  test `a_new_width_rebreaks_without_shaping_and_matches_a_fresh_layout` checks this bit for
  bit across eight widths (zero and unbounded included) for spans of every kind, all four
  alignments, `wrap_anywhere`, right-to-left and justified right-to-left text, CJK, emoji and math fallbacks,
  monospace tabs, indented blocks and pills, and that none of it shapes.
- Measuring: Taffy measures a wrapped text at its definite width only (`w_full` texts in a
  column are stretched, so min- and max-content aren't asked for), and the final pass asks for
  the same width, so each wrapped text costs one re-break per resize frame and nothing else.
  A `max_lines` text's trial cuts go through the same cache, so a cut tried at the last width
  is re-broken rather than shaped.

## What's left

At 50 KB a resize frame is now 4.6 ms, comfortably inside 120 Hz. What it spends, all of it in
proportion to the note's 1,364 layout nodes, since none of the note is virtualized:

| 50 KB resize frame, after | ms |
|---|---|
| View (building the Markdown tree again) | 0.78 |
| Flattening the tree into layout nodes, and the pass after layout | about 0.85 |
| Taffy layout | about 1.15 |
| Text line breaking | 0.70 |
| Apply (mounting, layout animation targets) | 0.81 |
| Paint | 0.22 |

At 200 KB the same split is 23 ms: view 3.4, Taffy 5.8, line breaking 3.2, apply 4.8, the
rest flattening. A sample of it puts 11% in moving node structs (flatten and apply) and 5% in
hashing whole texts again in Taffy's measure callback, which asks about three times per text
per layout.

A plan, most to least effect:

1. **Virtualize long Markdown.** Build and lay out only the blocks near the viewport, keeping
   measured heights per width for the rest (`El::virtualize`). That makes a resize frame
   independent of note length. It needs an estimate for blocks never measured at the current
   width, which a cheap re-break of their cached shapes can now supply.
2. **Skip the view on a viewport-only commit.** A resize rebuilds the view and flattens it even
   when nothing the view read changed. Reusing the last flattened tree when no view read the
   viewport or a layout would save view and flatten: about 1.6 ms at 50 KB, 8 ms at 200 KB.
3. **Hash each text once per commit.** Flattening already hashes every text for Taffy's cache;
   passing that hash to the measure callback and the final pass saves the repeats, about 5%.

An app can take some of the view cost today by memoizing the note's subtree on its revision
(`rune::memo`, `--memo` in the bench): view drops from 0.78 to 0.54 ms at 50 KB.

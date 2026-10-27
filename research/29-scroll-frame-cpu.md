# Scroll-frame CPU

Fixtures: 1,000 plain rows, a long Markdown document, 40 sticky sidebar sections,
and a 1,000-row growth timeline receiving a row every 12 frames. Each drives
480 wheel deltas at virtual 120 Hz, reversing halfway, with the pointer inside.

Metric: median thread CPU from wheel dispatch through draw-list construction;
GPU submission is excluded. One warmup and nine alternating before/after pairs.
Build instructions are in `scripts/bench-ui.py`; run with
`python3 scripts/bench-ui.py --frame-cpu --scroll --out target/scroll --runs 9`.

Kept: skip unchanged default hover targets and gate interaction lookups on styles.
Improvement is the median paired ratio; CPU columns are medians in milliseconds.

| Fixture | Before | After | Improvement |
| --- | ---: | ---: | ---: |
| Plain list | 0.401 | 0.306 | 22.1% |
| Markdown | 0.187 | 0.177 | 4.30% |
| Sticky sidebar | 0.333 | 0.276 | 18.9% |
| Growth timeline | 0.473 | 0.416 | 13.2% |

Next proposals:
- Retain scroll-subtree geometry and conservatively cull branches.
- Commit and measure only changed growth rows, retaining keyed row structure.
- Index hover/press candidates instead of scanning every node twice per frame.

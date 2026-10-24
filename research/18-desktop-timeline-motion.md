# 18. Desktop timeline motion: what the DOM branch learned

Scope: the OpenCode Desktop session timeline on branch `timeline-motion` (PR #57224, head `39eb86d`), for the Rune
rebuild of the session page. Written 2026-10-22. Constants come from source unless marked *measured* (the
branch's `.jank` harness) or *not in code*. Bare file names are under `packages/app/src/session/timeline/`.

## Summary

- One spring, `timeline.growth` at 0.32 s and bounce 0 (`row-growth.ts:6-14 @ 39eb86d`), drives row growth, gap
  closing, Working's glide and the send flight. Springs are analytic and keep velocity when retuned
  (`packages/ui/src/motion/spring.ts:59-91 @ 39eb86d`), but per-frame pixel caps make growth frame-rate dependent.
- The streaming edge is still an open decision, and "coming back lands at the bottom" is *not in code*.

## 1. Streaming edge and word reveal

The four edges are one table (`row-growth.ts:31-40 @ 39eb86d`):

| Edge | mask max | floor | shrink/frame | catch-up | duration × | clip |
|---|---|---|---|---|---|---|
| Feather | 28 px | 18 px | 1.8 px | 1 | 1 | yes |
| Fast Feather | 12 px | 6 px | 4 px | 2.4 | 0.55 | yes |
| No Mask, Word Fade | 0 | 0 | 28 px | 1 | 1 | no |

The mask aims for `min(max, max(floor, 1.5 × leftToGrow))`, growing at most 5 px per frame (`row-growth.ts:311-317
@ 39eb86d`). Its gradient has 72% opacity at 0.62 of its height and 28% at 0.28, rounded to whole pixels so
unchanged masks aren't rewritten (`row-growth.ts:586-597 @ 39eb86d`).

The "15 px mask, 40 px trail" figures are *measured* means: Feather showed 15.3 px of mask with content 39.5 px
ahead of the shown height, Fast Feather 5.7 and 19.8, No Mask 0 and 37.2, Word Fade 0 and 34.7.

Words fade in over 180 ms ease-out (`packages/session-ui/src/components/markdown.css:54-56 @ 39eb86d`); No Mask
disables the fade and Word Fade makes it 360 ms (`markdown.css:470-480 @ 39eb86d`). The stagger between words is
20 ms, shrinking to 5.5 ms as the backlog grows (`packages/session-ui/src/components/markdown-solid.tsx:22-33 @
39eb86d`).

Paced text shows backlogs under 512 characters at once and reveals larger ones in chunks every 24 ms, snapped to
word or punctuation boundaries (`packages/session-ui/src/message/message-content.tsx:118-222 @ 39eb86d`). A live
row also holds back a trailing partial word of up to 24 characters
(`packages/session-ui/src/timeline/session-timeline-row.tsx:80-95 @ 39eb86d`).

## 2. Row growth

Growing rows share one spring whose step is a budget split between them (`row-growth.ts:163-339 @ 39eb86d`). Each
frame's take is capped (`row-growth.ts:222-239 @ 39eb86d`): it starts at 1.875 px, ramps up by at most 2.2× per
frame, and is capped at 11.5 px, rising to 16 px for long distances, or 7.5 px while carrying Working. Streaming
rows slow further near their target (`(0.085 × rem + 0.65) × catchUp` within 36 px).

Only the ramps scale with `dt`, and `dt` is clamped to 1/30 s (`row-growth.ts:356 @ 39eb86d`), so the 16 px cap is
per frame. A 457 px bubble took about 1.5 s at 30 fps headless and about 0.5 s at 60 fps (*measured*). A growth
bent toward a nearer target lands there instead of overshooting (`row-growth.ts:177-178 @ 39eb86d`).

Growth joins follow in `settle`: rows resize without TanStack's per-row scroll correction, then one `pinEnd` write
holds the end (`virtualizer.tsx:483-540 @ 39eb86d`). Space freed by the composer or a removed row is held under the
last row and closes on the same spring after 120 ms without growth (`row-growth.ts:359-366 @ 39eb86d`). A
streaming row never shrinks below what it shows (`virtualizer.tsx:709-719 @ 39eb86d`).

## 3. Send flight

In `send-flight.ts @ 39eb86d`, only the composer's visible lines are cloned, clipped with `clip-path` (lines
38-58). When the bubble mounts, a clone flies on `growthSpring` with `dt` capped at 1/60 (line 189). The measure
phase re-reads the live bubble rect every frame, so flights retarget as earlier bubbles land (lines 175-180). The
clone scales uniformly by width and reveals its height through a `clip-path` inset from where the composer had
scrolled (lines 200-209). If the wraps differ by over 2 px (line 166), composer text is gone by p = 0.25 and the
clone fades in over p = 0.1-0.4; otherwise they finish at 0.4 and 0.5 (lines 210-213).

## 4. Working tail row

Working is its own 36 px `Activity` row at the end of the active turn (`message-timeline.tsx:229-250 @ 39eb86d`),
inside an 82 px reserved tail (`virtualizer.tsx:50-52 @ 39eb86d`). At turn end it hands off over 0.34 s, blurring
to 6 px and scaling to 0.5 while the copy button sharpens from scale 0.6 (`row-growth.ts:53-102 @ 39eb86d`,
`virtualizer.tsx:937-995 @ 39eb86d`).

## 5. Steer promotion

Admission used to add a 24 px "Steering" label and move Working. Now the client predicts the status a steer sent
now will get (`packages/client/src/solid/data.ts:294-305,1491-1494 @ 39eb86d`), and the just-sent bubble sits where
admission will put it (`controller.tsx:89-114 @ 39eb86d`). On promotion, Working glides on the growth spring while
the gaps it leaves shrink in proportion (`virtualizer.tsx:775-847 @ 39eb86d`). A row shrinking mid-glide joins the
glide unless it is past 75% (`virtualizer.tsx:736-752 @ 39eb86d`).

## 6. Width-change easing

`holdLandingWidth` gives rows their landing width when a side panel starts moving, so text wraps once
(`packages/app/src/shell/panel-motion.ts:151-200 @ 39eb86d`). Heights wait for 120 ms of stable width, then growing
rows ease to their new heights; for 400 ms a streaming row may shrink (`virtualizer.tsx:672-693 @ 39eb86d`).

## 7. Pretext sub-line measurement

Rows are measured with `@chenglou/pretext` where possible. While streaming, the last line's fill is measured
(`packages/session-ui/src/timeline/text-layout.ts:156-170 @ 39eb86d`) and the target height lags by up to 0.52
(one line) or 0.82 line heights, less as the line fills, rounded to 1/64
(`packages/session-ui/src/timeline/markdown-layout.ts:168-180 @ 39eb86d`). A new line's height arrives as it
fills. The followed row uses this `streamHeight` (`virtualizer.tsx:466 @ 39eb86d`).

## 8. The 1/64 px grid

Heights and offsets are rounded to 1/64 px (`row-growth.ts:16 @ 39eb86d`, `virtualizer.tsx:46-48 @ 39eb86d`).
Chromium rounds `scrollTop` to whole pixels, so `writeScroll` writes the floor, draws the remainder as a
`translate`, and defers the commit until growth settles (`virtualizer.tsx:569-636 @ 39eb86d`). Sub-pixel reversals
went from 3163 to 0 (PR body).

## 9. Follow and coming back

Following needs pinned, a finished cold mount, no touch and no reduced motion (`virtualizer.tsx:769-771 @
39eb86d`). Losing it lands every growth (`virtualizer.tsx:1137-1145 @ 39eb86d`). Cold opens stay hidden until
fonts load and the bottom is pinned (`virtualizer.tsx:1421-1445 @ 39eb86d`).

A newer requirement is *not in code*: returning to a session, tab or window paints the bottom in the first
frame, with no animated scroll, nothing that arrived while away streaming or growing, and no remembered position.
While visible and following, a target more than about a viewport away should cut instead of gliding. Today a hidden tab pauses rAF, the first frame back clamps `dt` to 0.1 s (`packages/ui/src/motion/loop.ts:61 @
39eb86d`), growth clamps it to 1/30, and the backlog animates at the per-frame cap. Paced text doesn't flush.

## What was hardest in the DOM

- **Reads vs writes.** Per-frame work goes through `frameLoop.addMeasured`, which runs all reads before any writes
  (`loop.ts:68-99,136 @ 39eb86d`). One stray rect read after a write forces layout.
- **Caps per frame.** Headless 30 fps looks about 2× slower than 60 fps.
- **Reflow mid-animation**, handled by landing widths and the width hold.
- **Unknown heights.** TanStack doesn't cache a measurement equal to its estimate, which looped
  (`virtualizer.tsx:1069-1070 @ 39eb86d`); pretext avoids DOM measurement.
- **Main-thread stalls.** SSE now flushes on rAF (commit `31c63cef78`); a ~170 ms render task on session open
  remains.
- **Masks and clip-path.** Writes are rounded and deduplicated; paint cost was never measured.

## What Rune should guarantee

1. Springs evaluated in time, with speed limits per second, so 30 fps and 120 Hz give the same curves.
2. Retargeting that keeps position and velocity (`spring.ts:85-86 @ 39eb86d`).
3. Synchronous text measurement, including last-line fill.
4. A measure phase before a mutate phase, enforced by the API.
5. Fractional scroll offsets and wrap-once width animation.
6. No one-frame disappearance; leaving nodes fade where they stand.
7. A global reduced-motion mode that keeps 0.11-0.2 s opacity fades.
8. A settle-all for resume that lands every track and flushes paced text before the first frame.

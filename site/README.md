# Rune site

The docs site for Rune: what it is, its principles, its architecture and its roadmap. Vite and React, prerendered to
static HTML.

- `bun run dev`: dev server on `127.0.0.1:4241`.
- `bun run build`: typecheck, client and SSR builds, then prerender every route into `dist/`.

Content lives in `src/content.tsx` (principles, layers, decisions, milestones, risks) and `src/pages.tsx`. The
source of truth for all of it is `PLAN.md` at the repo root; update the plan first, then the site.

## Design system

`src/system/` (`tokens.css`, `primitives.css`, `Frame.tsx`, `useActiveId.ts`) and the Geist fonts in
`public/fonts/` are copied unchanged from Abattoir's design system (Anomaly's docs system, direction "Workbench").
`src/site.css` sets Rune's accent by overriding `--ink` and adds only what the system lacks: the wordmark, the
architecture diagram, the decisions table and the roadmap rows. These files should become a shared package that both
sites import; until then, copy changes across by hand rather than editing the copies here.

The screenshots in `public/shots/` are Rune's own headless renders from `demo/`, resized.

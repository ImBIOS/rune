#!/usr/bin/env bun
// `bun run build`: typecheck, client build, SSR build, then prerender every route to static HTML in dist/.
import { $ } from "bun"

await $`tsc --noEmit`
await $`vite build`
await $`vite build --ssr src/entry-server.tsx --outDir dist-ssr`
await $`bun scripts/prerender.ts`

// Shared helpers for the kit scripts: serve a static Storybook build and open stories in
// headless Chromium through the OpenCode checkout's own Playwright.
//
// OPENCODE (env) points at an opencode checkout with `bun install` done and
// `packages/storybook/storybook-static` built (`bun run --cwd packages/storybook build`).

import fs from "node:fs"
import http from "node:http"
import path from "node:path"
import { createRequire } from "node:module"

export const opencode = process.env.OPENCODE ?? path.resolve(import.meta.dirname, "../../../opencode")
const storybookPkg = path.join(opencode, "packages/storybook/package.json")
export const staticDir = process.env.STORYBOOK_STATIC ?? path.join(opencode, "packages/storybook/storybook-static")

const types = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".mjs": "text/javascript",
  ".css": "text/css",
  ".json": "application/json",
  ".svg": "image/svg+xml",
  ".woff2": "font/woff2",
  ".woff": "font/woff",
  ".ttf": "font/ttf",
  ".png": "image/png",
}

export async function serve() {
  if (!fs.existsSync(path.join(staticDir, "iframe.html"))) {
    throw new Error(`no Storybook build at ${staticDir}; run \`bun run --cwd packages/storybook build\` in ${opencode}`)
  }
  const server = http.createServer((req, res) => {
    const url = decodeURIComponent(req.url.split("?")[0])
    const file = path.join(staticDir, url === "/" ? "index.html" : url)
    if (!file.startsWith(staticDir) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) {
      res.statusCode = 404
      return res.end()
    }
    res.setHeader("content-type", types[path.extname(file)] ?? "application/octet-stream")
    fs.createReadStream(file).pipe(res)
  })
  await new Promise((ok) => server.listen(0, "127.0.0.1", ok))
  return { url: `http://127.0.0.1:${server.address().port}`, close: () => server.close() }
}

export function playwright() {
  return createRequire(storybookPkg)("@playwright/test")
}

/** Open a story with the given scheme; the decorator's padding stays, Storybook's own body padding goes. */
export async function openStory(page, base, id, scheme) {
  await page.goto(`${base}/iframe.html?id=${id}&viewMode=story&globals=theme:${scheme}`)
  await page.waitForSelector("#storybook-root > *", { timeout: 15000 })
  await page.addStyleTag({
    content: `body.sb-main-padded, body { padding: 0 !important; margin: 0 !important; }
      html, body { background: var(--background-base) !important; }
      *, *::before, *::after { transition: none !important; animation-play-state: paused !important; caret-color: transparent !important; }`,
  })
  await page.evaluate(() => document.fonts.ready)
  await page.waitForTimeout(250)
}

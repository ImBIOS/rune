// Renders Storybook stories to PNG in headless Chromium, for scripts/kit-diff.py.
//
// stdin: JSON [{ id, scheme, width, height?, out, actions? }]
//   actions: [{ click | hover | focus: selector, nth?: n }] run before the shot.
//   Without `height`, the viewport is fitted to the story (frame padding included).
// stdout: JSON [{ id, scheme, width, height }]

import fs from "node:fs"
import { openStory, playwright, serve } from "./storybook.mjs"

const jobs = JSON.parse(fs.readFileSync(0, "utf8"))
const server = await serve()
const browser = await playwright().chromium.launch()
const results = []
for (const job of jobs) {
  const page = await browser.newPage({ viewport: { width: job.width, height: 1600 }, deviceScaleFactor: job.scale ?? 2 })
  await page.emulateMedia({ reducedMotion: "reduce" })
  await openStory(page, server.url, job.id, job.scheme)
  for (const a of job.actions ?? []) {
    const [kind, selector] = Object.entries(a).find(([k]) => ["click", "hover", "focus", "press"].includes(k))
    const loc = page.locator(selector).nth(a.nth ?? 0)
    try {
      if (kind === "click") await loc.click({ timeout: 5000 })
      if (kind === "hover") await loc.hover({ timeout: 5000 })
      if (kind === "focus") await loc.focus({ timeout: 5000 })
      if (kind === "press") await page.keyboard.press(selector)
    } catch (e) {
      process.stderr.write(`${job.id}: ${kind} ${selector} failed: ${e.message.split("\n")[0]}\n`)
    }
    await page.waitForTimeout(a.wait ?? 400)
  }
  // Pause every animation at its end so overlays are fully in.
  await page.evaluate(() => {
    for (const a of document.getAnimations()) {
      try {
        const t = a.effect?.getComputedTiming()
        if (t && Number.isFinite(t.endTime)) a.currentTime = t.endTime
        else a.currentTime = 0
        a.pause()
      } catch {}
    }
  })
  const height =
    job.height ??
    (await page.evaluate(() => {
      const frame = document.querySelector("#storybook-root > div")
      let bottom = 0
      for (const el of frame?.children ?? []) bottom = Math.max(bottom, el.getBoundingClientRect().bottom)
      for (const el of document.querySelectorAll("[data-popper-positioner], [role=dialog], [role=tooltip], .toast-v2-region"))
        bottom = Math.max(bottom, el.getBoundingClientRect().bottom)
      return Math.ceil(bottom + 24)
    }))
  await page.setViewportSize({ width: job.width, height })
  await page.waitForTimeout(100)
  await page.screenshot({ path: job.out })
  results.push({ id: job.id, scheme: job.scheme, width: job.width, height })
  await page.close()
}
await browser.close()
server.close()
process.stdout.write(JSON.stringify(results))

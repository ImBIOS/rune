// Prints the layout rects (points) of a story's slotted elements, for matching Rune layouts.
// usage: node scripts/kit/rects.mjs <story-id> [light|dark] [click-selector]
import { openStory, playwright, serve } from "./storybook.mjs"

const [id, scheme = "light", click] = process.argv.slice(2)
const server = await serve()
const browser = await playwright().chromium.launch()
const page = await browser.newPage({ viewport: { width: 720, height: 900 } })
await openStory(page, server.url, id, scheme)
if (click) {
  await page.locator(click).first().click()
  await page.waitForTimeout(500)
}
const rows = await page.evaluate(() => {
  const out = []
  for (const el of document.querySelectorAll("[data-component], [data-slot], [role], button, input, label, span, svg")) {
    const r = el.getBoundingClientRect()
    if (!r.width || !r.height) continue
    const cs = getComputedStyle(el)
    const name = el.getAttribute("data-component") ?? el.getAttribute("data-slot") ?? el.getAttribute("role") ?? el.tagName.toLowerCase()
    const t = el.children.length === 0 ? (el.textContent ?? "").trim().slice(0, 24) : ""
    out.push(`${name.padEnd(32)} ${[r.x, r.y, r.width, r.height].map((v) => v.toFixed(2).padStart(8)).join(" ")}  ${cs.fontSize}/${cs.lineHeight} ${cs.fontWeight} ${t}`)
  }
  return out
})
console.log(rows.join("\n"))
await browser.close()
server.close()

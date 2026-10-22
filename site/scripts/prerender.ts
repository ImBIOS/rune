import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { join, relative, resolve } from "node:path"
import { getWorkerAssetsDir } from "@cloudflare/build-output-utils"
import { BASE_PATH } from "../src/base"
import type { Route } from "../src/routes"

const root = resolve(import.meta.dirname, "..")
const distDir = getWorkerAssetsDir(root)
const distRel = relative(root, distDir)
const ssrDir = join(root, "dist-ssr")
const templatePath = join(distDir, "index.html")

if (!existsSync(templatePath)) throw new Error(`${distRel}/index.html not found; run \`vite build\` first`)

let template = readFileSync(templatePath, "utf8")
if (!template.includes("<!--app-->")) throw new Error(`${distRel}/index.html is missing the <!--app--> marker`)

// Inline the one stylesheet so the first paint needs no extra request.
template = template.replace(new RegExp(`<link rel="stylesheet"[^>]*href="${BASE_PATH}/(assets/[^"]+\\.css)"[^>]*>`, "g"), (full, href: string) => {
  const cssPath = join(distDir, href)
  if (!existsSync(cssPath)) return full
  return `<style>${readFileSync(cssPath, "utf8").replace(/<\/style/gi, "<\\/style")}</style>`
})

const escape = (s: string) => s.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;").replace(/>/g, "&gt;")

const ssr = (await import(join(ssrDir, "entry-server.js"))) as {
  render: (pathname: string) => string
  routes: Route[]
}

for (const route of ssr.routes) {
  const html = template
    .replace(/<title>[\s\S]*?<\/title>/, `<title>${escape(route.title)}</title>`)
    .replace(/<meta name="description" content="[^"]*" \/>/, `<meta name="description" content="${escape(route.description)}" />`)
    .replace("<!--app-->", ssr.render(route.path))
  if (route.path === "/") {
    writeFileSync(templatePath, html)
  } else {
    // Both files, so /principles serves directly instead of redirecting to /principles/.
    const dir = join(distDir, route.path.slice(1))
    mkdirSync(dir, { recursive: true })
    writeFileSync(join(dir, "index.html"), html)
    writeFileSync(`${dir}.html`, html)
  }
}

rmSync(ssrDir, { recursive: true, force: true })
console.log(`Prerendered ${ssr.routes.length} routes into ${distRel}/`)

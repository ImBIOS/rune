#!/usr/bin/env bun
// For working on the page: serves a development build per start. Open it with
// `?ws=ws://127.0.0.1:7863/ws` to drive a running app. Apps serve the embedded build (build.ts)
// themselves. Every control belongs to @kitlangton/knobs.
import { bundle } from "./build.ts"

const port = Number(process.env.PORT ?? 7862)
const js = await bundle({ conditions: ["development"] })

const server = Bun.serve({
  hostname: "127.0.0.1", port,
  fetch(request) {
    const url = new URL(request.url)
    if (url.pathname === "/panel.js") return new Response(js, { headers: { "Content-Type": "text/javascript" } })
    if (url.pathname !== "/") return new Response("Not found", { status: 404 })
    return new Response(`<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Rune</title></head><body><script type="module" src="/panel.js"></script></body></html>`, { headers: { "Content-Type": "text/html" } })
  },
})
console.log(`Rune knobs page: ${server.url}?ws=ws://127.0.0.1:7863/ws`)

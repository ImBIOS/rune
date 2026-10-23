#!/usr/bin/env bun
// For working on the page: rebuilds it per start and proxies an app's socket. Apps serve the
// embedded build (build.ts) themselves. Every control belongs to @kitlangton/knobs.
import { bundle } from "./build.ts"

const port = Number(process.env.PORT ?? 7862)
const upstream = process.env.RUNE_DEV_WS ?? "ws://127.0.0.1:7863"
const js = await bundle({ conditions: ["development"] })

// Same rule as the app's own port: another site's tab, or a rebound DNS name, gets nothing.
function local(authority: string | null) {
  if (!authority) return false
  let host: string
  try { host = new URL(`http://${authority}`).hostname.replace(/^\[|\]$/g, "").toLowerCase() } catch { return false }
  if (host === "localhost" || host.endsWith(".ts.net") || host === "::1" || /^127\./.test(host)) return true
  const [a, b] = host.split(".").map(Number)
  return /^\d+\.\d+\.\d+\.\d+$/.test(host) && a === 100 && b! >= 64 && b! < 128
}
function allowed(request: Request) {
  const origin = request.headers.get("origin")
  if (origin === null) return local(request.headers.get("host"))
  try {
    const url = new URL(origin)
    return (url.protocol === "http:" || url.protocol === "https:") && local(url.host) && local(request.headers.get("host"))
  } catch { return false }
}
const server = Bun.serve<{ upstream?: WebSocket }>({
  hostname: "127.0.0.1", port,
  fetch(request, server) {
    if (!allowed(request)) return new Response("Forbidden", { status: 403 })
    const url = new URL(request.url)
    if (url.pathname === "/ws") {
      // Proxy only this configured Rune runtime; clients never select an upstream address.
      if (server.upgrade(request, { data: {} })) return
      return new Response("Upgrade required", { status: 426 })
    }
    if (url.pathname === "/panel.js") return new Response(js, { headers: { "Content-Type": "text/javascript" } })
    if (url.pathname !== "/") return new Response("Not found", { status: 404 })
    return new Response(`<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Rune</title></head><body><script type="module" src="/panel.js"></script></body></html>`, { headers: { "Content-Type": "text/html" } })
  },
  websocket: {
    maxPayloadLength: 64 * 1024,
    open(client) {
      const peer = new WebSocket(upstream)
      client.data.upstream = peer
      peer.addEventListener("message", event => client.send(String(event.data)))
      peer.addEventListener("close", () => client.close())
      peer.addEventListener("error", () => client.close())
    },
    message(client, message) {
      const peer = client.data.upstream
      if (peer?.readyState === WebSocket.OPEN) peer.send(message)
    },
    close(client) { client.data.upstream?.close() },
  },
})
console.log(`Rune knobs page: ${server.url} → ${upstream}`)

import { cloudflare } from "@cloudflare/vite-plugin"
import react from "@vitejs/plugin-react"
import { defineConfig } from "vite"
import { BASE_PATH } from "./src/base"

// The Cloudflare plugin only shapes the client build into Build Output for `cf deploy`.
// Dev stays plain Vite, and the SSR build only feeds the prerender.
export default defineConfig(({ command, isSsrBuild }) => ({
  plugins: [react(), ...(command === "build" && !isSsrBuild ? [cloudflare()] : [])],
  base: `${BASE_PATH}/`,
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
  server: {
    host: "127.0.0.1",
    port: 4241,
    strictPort: true,
    allowedHosts: [".ts.net"],
  },
  preview: {
    host: "127.0.0.1",
    port: 4241,
    strictPort: true,
    allowedHosts: [".ts.net"],
  },
}))

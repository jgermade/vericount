import { defineConfig } from "vite"
import { jq79 } from "jq79/vite"

export default defineConfig({
  // Components are precompiled at build time, so the page runs under
  // `script-src 'self'` without `unsafe-eval`.
  plugins: [jq79({ safeEval: true })],
  build: {
    // No inlined assets: `data:` URLs are not allowed by the CSP.
    assetsInlineLimit: 0,
    modulePreload: { polyfill: false },
  },
  server: {
    proxy: {
      "/api": "http://127.0.0.1:8080",
      "/healthz": "http://127.0.0.1:8080",
    },
  },
})

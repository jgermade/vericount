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
  // The dependency scan does not understand the `.html` components, and
  // there is nothing to pre-bundle.
  optimizeDeps: { noDiscovery: true },
  server: {
    // The Host header is kept: the server refuses state-changing requests
    // whose Origin does not match it (CSRF), and Vite's string shorthand
    // rewrites it to the target's.
    proxy: {
      "/api": { target: "http://127.0.0.1:8080", changeOrigin: false },
      "/healthz": { target: "http://127.0.0.1:8080", changeOrigin: false },
    },
  },
})

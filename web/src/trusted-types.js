// Trusted Types default policy. It must be created before jq79 parses any
// component, so main.js imports this module first.
//
// jq79 parses component sources with DOMParser and `innerHTML`, and has no
// Trusted Types policy of its own, so HTML sinks are allowed through this
// default policy. Script sinks stay closed: script text is always refused
// and script URLs must be same-origin. Inline scripts and event handlers
// that injected HTML could carry are still blocked by `script-src 'self'`.
//
// Never use jq79's `:html` with data that is not already trusted.
if (window.trustedTypes) {
  window.trustedTypes.createPolicy("default", {
    createHTML: html => html,
    createScript: () => null,
    createScriptURL: url =>
      new URL(url, document.baseURI).origin === window.location.origin ? url : null,
  })
}

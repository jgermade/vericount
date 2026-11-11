// Entry point of the web client. The code is organised by domain: app/
// (the shell, the API client and what every screen shares), access/,
// home/, invoices/, contacts/, company/, aeat/ and settings/; the texts are
// in i18n/es.ts and the design system in theme/.
import "./app/trusted-types"
import App from "./app/App.html"
import { api } from "./app/api"
import { describeError } from "./app/errors"
import { es } from "./i18n/es"
import { webauthn } from "./access/webauthn"

App.mount("#app", { api, t: es, describeError, webauthn })

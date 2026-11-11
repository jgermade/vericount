import "./trusted-types"
import App from "./App.html"
import { api } from "./api"
import { describeError } from "./errors"
import { es } from "./i18n/es"

App.mount("#app", { api, t: es, describeError })

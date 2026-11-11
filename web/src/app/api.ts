// The typed client of the vericount API: every domain's endpoints in one
// object, which the screens receive as `api`. The UI only talks to the
// server through it.

import { accessApi } from "../access/api"
import { aeatApi } from "../aeat/api"
import { companyApi } from "../company/api"
import { contactsApi } from "../contacts/api"
import { invoicesApi } from "../invoices/api"

export { ApiError } from "./http"

export const api = {
  ...accessApi,
  ...invoicesApi,
  ...contactsApi,
  ...companyApi,
  ...aeatApi,
}

export type Api = typeof api

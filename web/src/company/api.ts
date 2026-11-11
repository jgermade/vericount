// API endpoints for the company and its numbering series.

import { request } from "../app/http"
import type { Company, Series } from "./types"

export const companyApi = {
  company: () => request<Company>("GET", "/company"),
  saveCompany: (company: Company) => request<Company>("PUT", "/company", company),
  listSeries: () => request<Series[]>("GET", "/series"),
  createSeries: (code: string, description: string) =>
    request<Series>("POST", "/series", { code, description }),
  updateSeries: (code: string, description: string, active: boolean) =>
    request<Series>("PUT", `/series/${encodeURIComponent(code)}`, { description, active }),
}

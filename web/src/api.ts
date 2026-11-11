// Typed client for the vericounts API. The UI only talks to the server
// through this module.

import type {
  AeatQueue,
  ChainReport,
  Company,
  Contact,
  ContactInput,
  ErrorBody,
  Invoice,
  InvoiceInput,
  InvoicePreview,
  InvoiceSummary,
  Series,
  Session,
} from "./types"

export class ApiError extends Error {
  readonly status: number
  readonly code: string
  readonly field: string | null

  constructor(status: number, error?: ErrorBody) {
    super(error?.message ?? `Request failed (${status})`)
    this.status = status
    this.code = error?.code ?? "unknown"
    this.field = error?.field ?? null
  }
}

type Method = "GET" | "POST" | "PUT" | "DELETE"

async function request<T>(method: Method, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = {}
  // The server refuses state-changing requests without this header, which
  // other sites cannot send.
  if (method !== "GET") headers["X-CSRF-Protection"] = "1"
  if (body !== undefined) headers["Content-Type"] = "application/json"
  const response = await fetch(`/api${path}`, {
    method,
    headers,
    credentials: "same-origin",
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  const json: unknown = response.status === 204 ? null : await response.json().catch(() => null)
  if (!response.ok) {
    throw new ApiError(response.status, (json as { error?: ErrorBody } | null)?.error)
  }
  return json as T
}

export const api = {
  session: () => request<Session>("GET", "/session"),
  signIn: (username: string, password: string) =>
    request<Session>("POST", "/session", { username, password }),
  signOut: () => request<null>("DELETE", "/session"),
  aeatQueue: () => request<AeatQueue>("GET", "/aeat"),
  company: () => request<Company>("GET", "/company"),
  saveCompany: (company: Company) => request<Company>("PUT", "/company", company),
  listSeries: () => request<Series[]>("GET", "/series"),
  createSeries: (code: string, description: string) =>
    request<Series>("POST", "/series", { code, description }),
  updateSeries: (code: string, description: string, active: boolean) =>
    request<Series>("PUT", `/series/${encodeURIComponent(code)}`, { description, active }),
  listContacts: () => request<Contact[]>("GET", "/contacts"),
  createContact: (contact: ContactInput) => request<Contact>("POST", "/contacts", contact),
  updateContact: (id: number, contact: ContactInput) =>
    request<Contact>("PUT", `/contacts/${id}`, contact),
  listInvoices: () => request<InvoiceSummary[]>("GET", "/invoices"),
  getInvoice: (id: number) => request<Invoice>("GET", `/invoices/${id}`),
  previewInvoice: (invoice: InvoiceInput) =>
    request<InvoicePreview>("POST", "/invoices/preview", invoice),
  createInvoice: (invoice: InvoiceInput) => request<Invoice>("POST", "/invoices", invoice),
  cancelInvoice: (id: number) => request<Invoice>("POST", `/invoices/${id}/cancellation`, {}),
  verifyChain: () => request<ChainReport>("GET", "/chain/verification"),
}

export type Api = typeof api

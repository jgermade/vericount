// Typed client for the vericounts API. The UI only talks to the server
// through this module.

import type {
  ChainReport,
  ErrorBody,
  Invoice,
  InvoiceInput,
  InvoicePreview,
  InvoiceSummary,
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

async function request<T>(method: "GET" | "POST", path: string, body?: unknown): Promise<T> {
  const response = await fetch(`/api${path}`, {
    method,
    headers: body === undefined ? {} : { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  const json: unknown = await response.json().catch(() => null)
  if (!response.ok) {
    throw new ApiError(response.status, (json as { error?: ErrorBody } | null)?.error)
  }
  return json as T
}

export const api = {
  listInvoices: () => request<InvoiceSummary[]>("GET", "/invoices"),
  getInvoice: (id: number) => request<Invoice>("GET", `/invoices/${id}`),
  previewInvoice: (invoice: InvoiceInput) =>
    request<InvoicePreview>("POST", "/invoices/preview", invoice),
  createInvoice: (invoice: InvoiceInput) => request<Invoice>("POST", "/invoices", invoice),
  cancelInvoice: (id: number) => request<Invoice>("POST", `/invoices/${id}/cancellation`, {}),
  verifyChain: () => request<ChainReport>("GET", "/chain/verification"),
}

export type Api = typeof api

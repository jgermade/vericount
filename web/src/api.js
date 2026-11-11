// Typed-by-convention client for the vericounts API. The UI only talks to
// the server through this module.

export class ApiError extends Error {
  constructor(status, error) {
    super(error?.message ?? `Request failed (${status})`)
    this.status = status
    this.code = error?.code ?? "unknown"
    this.field = error?.field ?? null
  }
}

async function request(method, path, body) {
  const response = await fetch(`/api${path}`, {
    method,
    headers: body === undefined ? {} : { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  const json = await response.json().catch(() => null)
  if (!response.ok) throw new ApiError(response.status, json?.error)
  return json
}

export const api = {
  listInvoices: () => request("GET", "/invoices"),
  getInvoice: id => request("GET", `/invoices/${id}`),
  previewInvoice: invoice => request("POST", "/invoices/preview", invoice),
  createInvoice: invoice => request("POST", "/invoices", invoice),
  cancelInvoice: id => request("POST", `/invoices/${id}/cancellation`, {}),
  verifyChain: () => request("GET", "/chain/verification"),
}

// Typed client for the vericount API. The UI only talks to the server
// through this module.

import type {
  AeatQueue,
  Certificate,
  ChainReport,
  Company,
  Contact,
  ContactInput,
  ErrorBody,
  Invoice,
  InvoiceInput,
  InvoicePreview,
  InvoiceSummary,
  PasskeyChallenge,
  PasskeyInfo,
  PeriodSummary,
  Series,
  Session,
  UserInfo,
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

/// Base64 of a file's bytes, to send it in JSON.
async function base64(file: File): Promise<string> {
  const bytes = new Uint8Array(await file.arrayBuffer())
  let binary = ""
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000))
  }
  return btoa(binary)
}

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
  /// Emails a sign-in code. The answer is the same whether the address
  /// has an account or not.
  requestCode: (email: string) => request<null>("POST", "/session/code", { email }),
  signIn: (email: string, code: string) => request<Session>("POST", "/session", { email, code }),
  passkeySignInOptions: () => request<PasskeyChallenge>("POST", "/session/passkey/options", {}),
  passkeySignIn: (ceremony: string, credential: unknown) =>
    request<Session>("POST", "/session/passkey", { ceremony, credential }),
  listPasskeys: () => request<PasskeyInfo[]>("GET", "/passkeys"),
  passkeyOptions: () => request<PasskeyChallenge>("POST", "/passkeys/options", {}),
  addPasskey: (ceremony: string, name: string, credential: unknown) =>
    request<PasskeyInfo>("POST", "/passkeys", { ceremony, name, credential }),
  removePasskey: (id: number) => request<null>("DELETE", `/passkeys/${id}`),
  listUsers: () => request<UserInfo[]>("GET", "/users"),
  inviteUser: (email: string) => request<null>("POST", "/users", { email }),
  removeUser: (id: number) => request<null>("DELETE", `/users/${id}`),
  signOut: () => request<null>("DELETE", "/session"),
  aeatQueue: () => request<AeatQueue>("GET", "/aeat"),
  /// Uploads the AEAT certificate: a .p12/.pfx file with its password, or
  /// a PEM file with the certificate and its key.
  uploadCertificate: async (file: File, password: string) =>
    request<Certificate>("PUT", "/aeat/certificate", { file: await base64(file), password }),
  removeCertificate: () => request<null>("DELETE", "/aeat/certificate"),
  /// Turns live mode on, for good.
  goLive: () => request<AeatQueue>("POST", "/aeat/live", {}),
  summary: () => request<PeriodSummary>("GET", "/summary"),
  /// URL of the QR image of an invoice, for an `<img>`.
  qrImageUrl: (id: number) => `/api/invoices/${id}/qr.svg`,
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
  /// Replaces the rejected record of an invoice with a new one to send.
  correctInvoice: (id: number) => request<Invoice>("POST", `/invoices/${id}/correction`, {}),
  verifyChain: () => request<ChainReport>("GET", "/chain/verification"),
}

export type Api = typeof api

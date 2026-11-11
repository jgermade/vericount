// API endpoints for invoices, their QR code, the chain check and the quarter's totals.

import { request } from "../app/http"
import type { ChainReport, Invoice, InvoiceInput, InvoicePreview, InvoiceSummary, PeriodSummary } from "./types"

export const invoicesApi = {
  summary: () => request<PeriodSummary>("GET", "/summary"),
  /// URL of the QR image of an invoice, for an `<img>`.
  qrImageUrl: (id: number) => `/api/invoices/${id}/qr.svg`,
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

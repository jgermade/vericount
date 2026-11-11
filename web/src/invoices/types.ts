// Shapes of invoices and their records. Amounts are decimal strings, never
// numbers, so that no precision is lost on the way.

import type { AeatResult, AeatState } from "../aeat/types"

export type Amount = string

export interface LineInput {
  description: string
  quantity: Amount
  unit_price: Amount
  vat_rate: Amount
}

export interface InvoiceInput {
  series: string
  issue_date?: string
  contact_id: number
  description: string
  lines: LineInput[]
}

export interface Line extends LineInput {
  base_amount: Amount
}

export interface TaxBreakdown {
  vat_rate: Amount
  base_amount: Amount
  tax_amount: Amount
}

export interface Totals {
  total_base: Amount
  total_tax: Amount
  total_amount: Amount
}

export interface InvoicePreview extends Totals {
  lines: Line[]
  taxes: TaxBreakdown[]
}

export interface InvoiceSummary {
  id: number
  aeat_state: AeatState
  series_number: string
  issue_date: string
  recipient_name: string
  total_amount: Amount
  cancelled: boolean
}

export interface BillingRecord {
  id: number
  aeat: AeatResult
  record_type: "registration" | "cancellation"
  correction: boolean
  previous_hash: string
  hash: string
  generated_at: string
}

export interface Invoice extends Totals {
  id: number
  issuer_nif: string
  issuer_name: string
  series: string
  number: number
  series_number: string
  issue_date: string
  invoice_type: string
  recipient_name: string
  recipient_nif: string
  issuer_address: string
  recipient_address: string
  contact_id: number | null
  description: string
  cancelled: boolean
  lines: Line[]
  taxes: TaxBreakdown[]
  records: BillingRecord[]
  qr_url: string
  /// False for a practice invoice, which is not real.
  live: boolean
}

export interface ChainReport {
  valid: boolean
  records: number
  last_hash: string | null
  error: { record_id: number; reason: string } | null
}

export interface PeriodSummary extends Totals {
  year: number
  quarter: number
  from: string
  to: string
  invoices: number
  cancelled: number
  taxes: TaxBreakdown[]
}

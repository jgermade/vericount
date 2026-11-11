// Shapes of the vericount API. Amounts are decimal strings, never
// numbers, so that no precision is lost on the way.

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

export type AeatState =
  | "pending"
  | "accepted"
  | "accepted_with_errors"
  | "rejected"
  // Rejected and replaced by a later record of the same invoice.
  | "superseded"

export interface AeatResult {
  state: AeatState
  error_code: string | null
  error_description: string | null
}

export interface AeatQueue {
  enabled: boolean
  pending: number
  rejected: number
  accepted_with_errors: number
  last_outcome: string | null
  last_error: string | null
  certificate: Certificate | null
  certificate_matches_company: boolean | null
  representative_nif: string | null
}

export interface Certificate {
  subject: string
  nif: string | null
  of_legal_person: boolean
  expires_at: string
}

export interface Session {
  email: string
  /// How many passkeys the user has.
  passkeys: number
  /// Present and true when this sign-in was the user's sign-up.
  first_sign_in?: boolean
}

/// A WebAuthn challenge: `options` goes to the browser, `ceremony` comes
/// back to the server with its answer.
export interface PasskeyChallenge {
  ceremony: string
  options: unknown
}

export interface PasskeyInfo {
  id: number
  name: string
  created_at: string
  last_used_at: string | null
}

export interface UserInfo {
  id: number
  email: string
  invited_by: string | null
  /// When the user signed up; null while the invitation is pending.
  activated_at: string | null
  passkeys: number
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
}

export interface ChainReport {
  valid: boolean
  records: number
  last_hash: string | null
  error: { record_id: number; reason: string } | null
}

export interface ErrorBody {
  code: string
  message: string
  field?: string
}

export interface Company {
  nif: string
  name: string
  address: string
  postal_code: string
  city: string
}

export interface Series {
  code: string
  description: string
  active: boolean
  invoices: number
}

export interface ContactInput {
  name: string
  nif: string
  address: string
  postal_code: string
  city: string
  email: string
  is_customer: boolean
  is_supplier: boolean
  archived: boolean
}

export interface Contact extends ContactInput {
  id: number
  invoices: number
}

/// What every screen receives from the application shell.
export interface Shell {
  /// Shows an API error to the user, or the sign-in screen if the session ended.
  fail: (error: unknown) => void
  clear: () => void
  /// Opens another screen.
  go: (screen: "home" | "invoices" | "contacts" | "settings") => void
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

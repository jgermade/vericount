// Shapes of the AEAT sending: record states, the queue, the mode and the
// certificate.

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

/// "practice": invoices are not real and go to a mock of the AEAT.
/// "live": real invoices, sent to the AEAT.
export type Mode = "practice" | "live"

export type LiveBlocker = "certificate_missing" | "certificate_expired" | "producer_missing"

export interface AeatQueue {
  mode: Mode
  /// Where live records go: AEAT pre-production ("test") or production.
  aeat_environment: "test" | "production"
  /// Whether records are being sent.
  enabled: boolean
  pending: number
  rejected: number
  accepted_with_errors: number
  last_outcome: string | null
  last_error: string | null
  certificate: Certificate | null
  certificate_matches_company: boolean | null
  /// Whether the server can store a certificate (it has a secret key).
  certificate_upload: boolean
  representative_nif: string | null
  /// Empty when live mode can be turned on, and in live mode.
  live_blockers: LiveBlocker[]
}

export interface Certificate {
  subject: string
  nif: string | null
  of_legal_person: boolean
  expires_at: string
}

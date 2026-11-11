// Shapes shared by every screen: API errors and the application shell.

export interface ErrorBody {
  code: string
  message: string
  field?: string
}

/// What every screen receives from the application shell.
export interface Shell {
  /// Shows an API error to the user, or the sign-in screen if the session ended.
  fail: (error: unknown) => void
  clear: () => void
  /// Opens another screen.
  go: (screen: "home" | "invoices" | "contacts" | "settings") => void
}

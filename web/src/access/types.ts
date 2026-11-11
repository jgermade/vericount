// Shapes of access: the session, passkeys and users.

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

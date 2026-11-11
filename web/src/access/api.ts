// API endpoints for signing in and out, passkeys and users.

import { request } from "../app/http"
import type { PasskeyChallenge, PasskeyInfo, Session, UserInfo } from "./types"

export const accessApi = {
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
}

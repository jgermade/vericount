// Passkeys in the browser. The server sends WebAuthn options as JSON,
// with binary fields in base64url; this module turns them into what
// `navigator.credentials` expects, and its answers back into JSON.

import type { Api } from "../app/api"
import type { PasskeyInfo, Session } from "./types"

/// An error the interface can describe, like the API's.
export class PasskeyError extends Error {
  readonly code: string

  constructor(code: "passkey_cancelled" | "passkey_unsupported") {
    super(code)
    this.code = code
  }
}

type Json = Record<string, unknown>

const toBytes = (base64url: string): ArrayBuffer => {
  const base64 = base64url.replace(/-/g, "+").replace(/_/g, "/")
  const binary = atob(base64.padEnd(Math.ceil(base64.length / 4) * 4, "="))
  const bytes = new Uint8Array(binary.length)
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i)
  return bytes.buffer
}

const toBase64url = (buffer: ArrayBuffer): string => {
  let binary = ""
  for (const byte of new Uint8Array(buffer)) binary += String.fromCharCode(byte)
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "")
}

const descriptors = (list: unknown): PublicKeyCredentialDescriptor[] | undefined =>
  (list as Json[] | undefined)?.map(item => ({
    ...(item as unknown as PublicKeyCredentialDescriptor),
    id: toBytes(item.id as string),
  }))

export const passkeysSupported = (): boolean =>
  typeof window.PublicKeyCredential === "function" && "credentials" in navigator

/// Whether the browser can offer passkeys in the email field's autofill.
export async function autofillAvailable(): Promise<boolean> {
  if (!passkeysSupported()) return false
  const check = (PublicKeyCredential as { isConditionalMediationAvailable?: () => Promise<boolean> })
    .isConditionalMediationAvailable
  return check ? check.call(PublicKeyCredential).catch(() => false) : false
}

const rethrow = (error: unknown): never => {
  const name = (error as { name?: string }).name
  if (name === "NotAllowedError" || name === "AbortError") throw new PasskeyError("passkey_cancelled")
  // No authenticator here can keep passkeys.
  if (name === "NotSupportedError") throw new PasskeyError("passkey_unsupported")
  throw error
}

/// Creates a passkey with the options of `POST /api/passkeys/options`.
export async function createPasskey(options: unknown): Promise<Json> {
  if (!passkeysSupported()) throw new PasskeyError("passkey_unsupported")
  const publicKey = (options as { publicKey: Json }).publicKey
  const user = publicKey.user as Json
  const credential = (await navigator.credentials
    .create({
      publicKey: {
        ...(publicKey as unknown as PublicKeyCredentialCreationOptions),
        challenge: toBytes(publicKey.challenge as string),
        user: { ...(user as unknown as PublicKeyCredentialUserEntity), id: toBytes(user.id as string) },
        excludeCredentials: descriptors(publicKey.excludeCredentials),
      },
    })
    .catch(rethrow)) as PublicKeyCredential | null
  if (!credential) throw new PasskeyError("passkey_cancelled")
  const response = credential.response as AuthenticatorAttestationResponse
  return {
    id: credential.id,
    rawId: toBase64url(credential.rawId),
    type: credential.type,
    response: {
      attestationObject: toBase64url(response.attestationObject),
      clientDataJSON: toBase64url(response.clientDataJSON),
      transports: response.getTransports?.() ?? [],
    },
    extensions: credential.getClientExtensionResults(),
  }
}

/// Signs in with a passkey, using the options of
/// `POST /api/session/passkey/options`. With `autofill`, the browser waits
/// until the user picks a passkey in the email field; `signal` stops it.
export async function getPasskey(
  options: unknown,
  autofill = false,
  signal?: AbortSignal,
): Promise<Json> {
  if (!passkeysSupported()) throw new PasskeyError("passkey_unsupported")
  const publicKey = (options as { publicKey: Json }).publicKey
  const credential = (await navigator.credentials
    .get({
      mediation: autofill ? "conditional" : "optional",
      signal,
      publicKey: {
        ...(publicKey as unknown as PublicKeyCredentialRequestOptions),
        challenge: toBytes(publicKey.challenge as string),
        allowCredentials: descriptors(publicKey.allowCredentials),
      },
    })
    .catch(rethrow)) as PublicKeyCredential | null
  if (!credential) throw new PasskeyError("passkey_cancelled")
  const response = credential.response as AuthenticatorAssertionResponse
  return {
    id: credential.id,
    rawId: toBase64url(credential.rawId),
    type: credential.type,
    response: {
      authenticatorData: toBase64url(response.authenticatorData),
      clientDataJSON: toBase64url(response.clientDataJSON),
      signature: toBase64url(response.signature),
      userHandle: response.userHandle ? toBase64url(response.userHandle) : null,
    },
    extensions: credential.getClientExtensionResults(),
  }
}

/// Registers a new passkey for the signed-in user.
export async function registerPasskey(api: Api, name: string): Promise<PasskeyInfo> {
  const { ceremony, options } = await api.passkeyOptions()
  return api.addPasskey(ceremony, name, await createPasskey(options))
}

/// Opens a session with a passkey.
export async function signInWithPasskey(
  api: Api,
  autofill = false,
  signal?: AbortSignal,
): Promise<Session> {
  const { ceremony, options } = await api.passkeySignInOptions()
  return api.passkeySignIn(ceremony, await getPasskey(options, autofill, signal))
}

/// A rough name for this device, to tell passkeys apart.
export function deviceName(): string {
  const agent = navigator.userAgent
  const known: [RegExp, string][] = [
    [/iPhone/, "iPhone"],
    [/iPad/, "iPad"],
    [/Android/, "Android"],
    [/Macintosh/, "Mac"],
    [/Windows/, "Windows"],
    [/CrOS/, "ChromeOS"],
    [/Linux/, "Linux"],
  ]
  return known.find(([pattern]) => pattern.test(agent))?.[1] ?? ""
}

/// What the screens use, given to them as a prop: component scripts
/// cannot import modules.
export const webauthn = {
  supported: passkeysSupported,
  autofillAvailable,
  register: registerPasskey,
  signIn: signInWithPasskey,
  deviceName,
}

export type WebAuthn = typeof webauthn

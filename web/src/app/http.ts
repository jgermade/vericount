// The client's only way to the server: one request to the JSON API, and
// the error it throws when the answer is not a success. Each domain's
// endpoints are in its own api.ts.

import type { ErrorBody } from "./types"

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

export async function request<T>(method: Method, path: string, body?: unknown): Promise<T> {
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

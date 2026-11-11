import type { ApiError } from "./api"
import type { Texts } from "./i18n/es"

/// Turns an API error into a message in the user's language.
export function describeError(t: Texts, error: unknown): string {
  const { code, field } = error as Partial<ApiError>
  if (code === undefined) return t.errors.network
  if (code === "validation" && field) {
    const inLine = /^lines\[\d+\]\./.test(field)
    const name = field.replace(/^lines\[\d+\]\./, "")
    const key = inLine && name === "description" ? "description_line" : name
    return t.errors.validation(t.fields[key] ?? field)
  }
  const message = (t.errors as unknown as Record<string, unknown>)[code]
  return typeof message === "string" ? message : t.errors.unknown
}

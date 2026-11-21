import { GatewayError } from "./errors.js"

/**
 * Validate and normalize an operator-entered Gateway address.
 *
 * The result is an origin plus an optional path prefix, with no trailing slash.
 * Credentials, query strings, and fragments are rejected so the caller token
 * and secrets cannot be smuggled into a URL.
 */
export function normalizeGatewayUrl(raw: string): string {
  const value = raw.trim()
  if (!value) {
    throw new GatewayError({
      kind: "invalid-url",
      message: "Enter the Gateway URL.",
    })
  }

  let url: URL
  try {
    url = new URL(value)
  } catch {
    throw new GatewayError({
      kind: "invalid-url",
      message: "Gateway URL must start with http:// or https://.",
    })
  }

  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new GatewayError({
      kind: "invalid-url",
      message: "Gateway URL must use http:// or https://.",
    })
  }
  if (url.username || url.password) {
    throw new GatewayError({
      kind: "invalid-url",
      message: "Gateway URL must not contain credentials.",
    })
  }
  if (url.search) {
    throw new GatewayError({
      kind: "invalid-url",
      message: "Gateway URL must not contain a query string.",
    })
  }
  if (url.hash) {
    throw new GatewayError({
      kind: "invalid-url",
      message: "Gateway URL must not contain a fragment.",
    })
  }

  const path = url.pathname.replace(/\/+$/, "")
  return `${url.origin}${path}`
}

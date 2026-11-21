/**
 * Bounded, redaction-aware rendering helpers for operator evidence.
 *
 * Diagnostics and audit payloads are already redacted by the Gateway, but the
 * browser never trusts that alone: host paths, credential-shaped values, and
 * credential-named keys are removed before they reach the DOM. Nested objects
 * are never serialized as JSON; only flat scalars become rows.
 */

export const redactedPath = "[redacted path]"
export const redactedCredential = "[redacted credential]"
export const redactedValue = "[redacted]"

const sensitiveKey =
  /secret|token|passw|credential|authorization|cookie|api[-_]?key|private[-_]?key|bearer|session[-_]?key/i

const pathPattern =
  /(^|[\s"'(=:[,])((?:\/(?:home|Users|root|var|tmp|etc|opt|srv|mnt|private|proc|sys|usr|run|Volumes)\b)[^\s"'()[\],;]*)/g
const windowsPathPattern =
  /(^|[\s"'(=:[,])([A-Za-z]:\\[^\s"'()[\],;]*)/g
const bearerPattern =
  /(^|[\s"'(=:[,])(?:bearer\s+)[A-Za-z0-9._~+/=-]{8,}/gi
const keyedCredentialPattern =
  /(^|[\s"'(=:[,])((?:token|secret|password|passwd|api[-_]?key|authorization|credential)\s*[:=]\s*)\S+/gi
const providerKeyPattern = /(^|[\s"'(=:[,])(?:sk-|ghp_|xox[baprs]-)[A-Za-z0-9_-]{8,}/g

export interface RedactedText {
  text: string
  redacted: boolean
}

/** Redact and bound one scalar value. Non-scalars render as an empty string. */
export function redactText(value: unknown, limit = 400): RedactedText {
  if (
    typeof value !== "string" &&
    typeof value !== "number" &&
    typeof value !== "boolean"
  ) {
    return { text: "", redacted: false }
  }
  let text = String(value)
  let redacted = false

  const before = text
  text = text
    .replace(pathPattern, `$1${redactedPath}`)
    .replace(windowsPathPattern, `$1${redactedPath}`)
    .replace(bearerPattern, `$1${redactedCredential}`)
    .replace(keyedCredentialPattern, `$1$2${redactedValue}`)
    .replace(providerKeyPattern, `$1${redactedCredential}`)
  if (text !== before) {
    redacted = true
  }
  if (text.length > limit) {
    text = `${text.slice(0, limit)}…`
  }
  return { text, redacted }
}

export interface SafeRow {
  label: string
  value: string
  redacted: boolean
}

export interface SafeRows {
  rows: SafeRow[]
  /** Number of scalar fields found before the row cap. */
  total: number
  redacted: boolean
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

/**
 * Flatten a record into plain scalar rows. Credential-named keys are skipped
 * and reported as redacted; nested values are omitted rather than dumped.
 */
export function safeRows(value: unknown, limit = 12): SafeRows {
  if (!isRecord(value)) {
    return { rows: [], total: 0, redacted: false }
  }
  const rows: SafeRow[] = []
  let total = 0
  let redacted = false
  for (const [key, entry] of Object.entries(value)) {
    if (sensitiveKey.test(key)) {
      total += 1
      redacted = true
      continue
    }
    if (
      typeof entry !== "string" &&
      typeof entry !== "number" &&
      typeof entry !== "boolean"
    ) {
      continue
    }
    total += 1
    if (rows.length >= limit) {
      continue
    }
    const label = redactText(key, 80)
    const text = redactText(entry)
    redacted = redacted || label.redacted || text.redacted
    rows.push({ label: label.text, value: text.text, redacted: text.redacted })
  }
  return { rows, total, redacted }
}

/** A bounded, read-only list. `items` is already capped. */
export function bounded<T>(items: T[], limit: number): T[] {
  return items.slice(0, Math.max(0, limit))
}

export interface RedactionScan {
  redacted: boolean
  bounded: boolean
}

const scanNodeLimit = 400

/**
 * Whether a payload would lose any detail to redaction or bounding.
 *
 * The scan is capped so a hostile or unexpectedly large payload cannot make
 * rendering slow.
 */
export function scanRedaction(value: unknown): RedactionScan {
  let visited = 0
  let redacted = false
  let bounded = false

  function visit(entry: unknown): void {
    if (visited >= scanNodeLimit) {
      bounded = true
      return
    }
    visited += 1
    if (
      typeof entry === "string" ||
      typeof entry === "number" ||
      typeof entry === "boolean"
    ) {
      if (redactText(entry).redacted) {
        redacted = true
      }
      return
    }
    if (Array.isArray(entry)) {
      for (const item of entry) {
        visit(item)
      }
      return
    }
    if (isRecord(entry)) {
      for (const [key, item] of Object.entries(entry)) {
        if (sensitiveKey.test(key)) {
          redacted = true
          continue
        }
        visit(item)
      }
    }
  }

  visit(value)
  return { redacted, bounded }
}

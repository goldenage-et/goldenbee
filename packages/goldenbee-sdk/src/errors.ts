/**
 * Gateway client error taxonomy.
 *
 * Every failure crosses the SDK boundary as one {@link GatewayError}. UI code
 * switches on `kind` for presentation and on `code` for protocol conditions; it
 * never inspects raw HTTP responses. Messages are safe to render: the SDK
 * redacts the caller token from transport messages before constructing one.
 */
export type GatewayErrorKind =
  | "invalid-url"
  | "network"
  | "timeout"
  | "aborted"
  | "unauthorized"
  | "forbidden"
  | "not-found"
  | "conflict"
  | "rate-limited"
  | "server"
  | "http"
  | "invalid-response"
  | "uncertain-submission"
  | "cursor-expired"
  | "cursor-invalid"
  | "protocol"
  | "stream"
  | "socket"
  | "socket-closed"

/** The documented Gateway error envelope (`schema/gateway-v1.json#/definitions/error`). */
export interface GatewayErrorEnvelope {
  code: string
  message: string
  retryable: boolean
  recovery: string
  details?: Record<string, string>
}

/** Backwards-compatible name for the decoded envelope. */
export type GatewayErrorDetails = GatewayErrorEnvelope

export interface GatewayErrorOptions {
  kind: GatewayErrorKind
  message: string
  /** HTTP status, when the failure came from a response. */
  status?: number
  /** Stable Gateway error code, when present. */
  code?: string
  retryable?: boolean
  recovery?: string
  details?: Record<string, string>
  /** Server-requested retry delay, when present. */
  retryAfterMs?: number
}

/**
 * A typed Gateway failure.
 *
 * The required operator-facing fields are `code`, `message`, `retryable`,
 * `recovery`, and `status`; `details` carries the documented string map.
 */
export class GatewayError extends Error {
  readonly kind: GatewayErrorKind
  readonly status: number | undefined
  readonly code: string | undefined
  readonly retryable: boolean
  readonly recovery: string | undefined
  readonly details: Record<string, string> | undefined
  readonly retryAfterMs: number | undefined

  constructor(options: GatewayErrorOptions) {
    super(options.message)
    this.name = "GatewayError"
    this.kind = options.kind
    this.status = options.status
    this.code = options.code
    this.retryable = options.retryable ?? false
    this.recovery = options.recovery
    this.details = options.details
    this.retryAfterMs = options.retryAfterMs
  }

  /**
   * HTTP 202 `task.unknown_submission` is a success-family response that the
   * Gateway could not resolve into a definitive acceptance or rejection.
   */
  get isUncertainSubmission(): boolean {
    return this.code === "task.unknown_submission" || this.kind === "uncertain-submission"
  }

  /** Older durable events were trimmed before the requested cursor. */
  get isCursorExpired(): boolean {
    return this.kind === "cursor-expired" || this.code === "stream.cursor_expired"
  }
}

export function isGatewayError(value: unknown): value is GatewayError {
  return value instanceof GatewayError
}

const statusKinds: Record<number, GatewayErrorKind> = {
  401: "unauthorized",
  403: "forbidden",
  404: "not-found",
  409: "conflict",
  429: "rate-limited",
  202: "uncertain-submission",
}

const codeKinds: Array<[prefix: string, kind: GatewayErrorKind]> = [
  ["stream.cursor_expired", "cursor-expired"],
  ["stream.cursor_invalid", "cursor-invalid"],
  ["task.unknown_submission", "uncertain-submission"],
  ["sdk.stream", "stream"],
  ["sdk.socket", "socket"],
  ["sdk.protocol", "protocol"],
]

export function kindForStatus(status: number): GatewayErrorKind {
  if (statusKinds[status]) {
    return statusKinds[status]
  }
  if (status >= 500) {
    return "server"
  }
  return "http"
}

export function kindForCode(
  code: string,
  status: number,
): GatewayErrorKind {
  for (const [prefix, kind] of codeKinds) {
    if (code === prefix || code.startsWith(`${prefix}.`)) {
      return kind
    }
  }
  return kindForStatus(status)
}

export function isRetryableStatus(status: number): boolean {
  return status === 429 || status >= 500
}

/** Recognize the documented error envelope without assuming a status. */
export function isErrorEnvelope(value: unknown): value is GatewayErrorEnvelope {
  if (typeof value !== "object" || value === null) {
    return false
  }
  const record = value as Record<string, unknown>
  return (
    typeof record.code === "string" &&
    typeof record.message === "string" &&
    typeof record.retryable === "boolean" &&
    typeof record.recovery === "string" &&
    (record.details === undefined ||
      (typeof record.details === "object" && record.details !== null))
  )
}

function parseRetryAfter(body: GatewayErrorEnvelope): number | undefined {
  const raw = body.details?.retry_after_ms
  if (!raw) {
    return undefined
  }
  const parsed = Number(raw)
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : undefined
}

/**
 * Decode a Gateway error body. `status` may be a success-family status: the
 * Gateway returns HTTP 202 with an error envelope for an unknown submission.
 */
export function errorFromResponse(
  status: number,
  body: unknown,
  fallbackMessage: string,
): GatewayError {
  if (isErrorEnvelope(body)) {
    return new GatewayError({
      kind: kindForCode(body.code, status),
      status,
      message: body.message,
      code: body.code,
      retryable: body.retryable,
      recovery: body.recovery,
      details: body.details,
      retryAfterMs: parseRetryAfter(body),
    })
  }
  return new GatewayError({
    kind: kindForStatus(status),
    status,
    message: fallbackMessage,
    retryable: isRetryableStatus(status),
  })
}

/** Build a local (non-HTTP) SDK failure. */
export function sdkError(
  kind: GatewayErrorKind,
  message: string,
  options: Omit<GatewayErrorOptions, "kind" | "message"> = {},
): GatewayError {
  return new GatewayError({ kind, message, ...options })
}

/**
 * Transport retries are deliberately narrow: connection loss, timeouts,
 * Gateway availability, storage unavailability, and explicitly retryable rate
 * limits. Validation, auth, scope, digest, idempotency, and protocol failures
 * are never retried.
 */
export function isRetryableTransportError(error: GatewayError): boolean {
  if (error.status === 401 || error.status === 403) {
    return false
  }
  if (error.kind === "network" || error.kind === "timeout") {
    return true
  }
  if (error.code === "sdk.transport") {
    return true
  }
  if (error.code === "storage.unavailable" || error.code === "route.worker_unavailable") {
    return true
  }
  if (error.status === 429) {
    return error.retryable
  }
  if (error.status !== undefined && error.status >= 500) {
    return error.retryable
  }
  return false
}

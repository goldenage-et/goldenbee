/** Gateway client error taxonomy. UI code switches on `kind`, never on raw HTTP. */
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

export interface GatewayErrorDetails {
  code: string
  message: string
  retryable: boolean
  recovery: string
  details?: Record<string, string>
}

export interface GatewayErrorOptions {
  kind: GatewayErrorKind
  message: string
  status?: number
  code?: string
  retryable?: boolean
  recovery?: string
  details?: Record<string, string>
}

/**
 * A typed Gateway failure. Messages are safe to render: they never include the
 * caller token or request headers.
 */
export class GatewayError extends Error {
  readonly kind: GatewayErrorKind
  readonly status: number | undefined
  readonly code: string | undefined
  readonly retryable: boolean
  readonly recovery: string | undefined
  readonly details: Record<string, string> | undefined

  constructor(options: GatewayErrorOptions) {
    super(options.message)
    this.name = "GatewayError"
    this.kind = options.kind
    this.status = options.status
    this.code = options.code
    this.retryable = options.retryable ?? false
    this.recovery = options.recovery
    this.details = options.details
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
}

export function kindForStatus(status: number): GatewayErrorKind {
  if (statusKinds[status]) {
    return statusKinds[status]
  }
  if (status >= 500) {
    return "server"
  }
  return "http"
}

export function isRetryableStatus(status: number): boolean {
  return status === 429 || status >= 500
}

function isDetails(value: unknown): value is GatewayErrorDetails {
  if (typeof value !== "object" || value === null) {
    return false
  }
  const record = value as Record<string, unknown>
  return (
    typeof record.code === "string" &&
    typeof record.message === "string" &&
    typeof record.retryable === "boolean" &&
    typeof record.recovery === "string"
  )
}

/** Decode the documented Gateway error envelope, falling back to the status. */
export function errorFromResponse(
  status: number,
  body: unknown,
  fallbackMessage: string,
): GatewayError {
  const kind = kindForStatus(status)
  if (isDetails(body)) {
    return new GatewayError({
      kind,
      status,
      message: body.message,
      code: body.code,
      retryable: body.retryable,
      recovery: body.recovery,
      details: body.details,
    })
  }
  return new GatewayError({
    kind,
    status,
    message: fallbackMessage,
    retryable: isRetryableStatus(status),
  })
}

import { isGatewayError, type GatewayErrorKind } from "@goldenbee/sdk"

export interface ProviderErrorView {
  kind: GatewayErrorKind | "unknown"
  code?: string
  message: string
  recovery?: string
  retryable: boolean
  status?: number
  details?: Record<string, string>
  /** Operator-facing headline for this failure class. */
  title: string
}

function titleFor(
  kind: ProviderErrorView["kind"],
  code: string | undefined,
): string {
  if (code?.startsWith("scope.")) {
    return "Not permitted"
  }
  switch (kind) {
    case "forbidden":
      return "Not permitted"
    case "unauthorized":
      return "Caller token rejected"
    case "not-found":
      return "Provider not found"
    case "conflict":
      return "Provider changed elsewhere"
    case "rate-limited":
      return "Gateway busy"
    case "server":
      return "Gateway failure"
    case "network":
      return "Gateway unreachable"
    case "timeout":
      return "Gateway timed out"
    case "invalid-response":
      return "Unexpected Gateway response"
    default:
      return "Provider action failed"
  }
}

/**
 * Present a typed Gateway failure without exposing raw transport detail.
 *
 * The SDK already redacts the caller token and the credential value, so the
 * message and recovery text are safe to render. UI code never inspects HTTP.
 */
export function providerErrorView(cause: unknown): ProviderErrorView {
  if (isGatewayError(cause)) {
    return {
      kind: cause.kind,
      code: cause.code,
      message: cause.message,
      recovery: cause.recovery,
      retryable: cause.retryable,
      status: cause.status,
      details: cause.details,
      title: titleFor(cause.kind, cause.code),
    }
  }
  return {
    kind: "unknown",
    message:
      cause instanceof Error && cause.message
        ? cause.message
        : "The provider action failed.",
    retryable: false,
    title: "Provider action failed",
  }
}

export function isRevisionConflict(cause: unknown): boolean {
  return (
    isGatewayError(cause) &&
    (cause.code === "provider.revision_conflict" || cause.kind === "conflict")
  )
}

/** Discovery failure codes are stable protocol values, not free text. */
export function isDiscoveryCode(code: string | undefined): boolean {
  return Boolean(code?.startsWith("provider.discovery"))
}

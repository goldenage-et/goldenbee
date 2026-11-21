import { isGatewayError, type GatewayErrorKind } from "@goldenbee/sdk"

export interface ProfileErrorView {
  kind: GatewayErrorKind | "unknown"
  code?: string
  message: string
  recovery?: string
  retryable: boolean
  status?: number
  title: string
  /** The caller token cannot perform this profile action. */
  denied: boolean
}

function titleFor(
  kind: ProfileErrorView["kind"],
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
      return code === "profile.not_found"
        ? "Profile not found"
        : "Worker not found"
    case "conflict":
      return "Profile changed elsewhere"
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
      return "Profile action failed"
  }
}

/** Present a typed Gateway failure without exposing raw transport detail. */
export function profileErrorView(cause: unknown): ProfileErrorView {
  if (isGatewayError(cause)) {
    return {
      kind: cause.kind,
      code: cause.code,
      message: cause.message,
      recovery: cause.recovery,
      retryable: cause.retryable,
      status: cause.status,
      title: titleFor(cause.kind, cause.code),
      denied:
        cause.kind === "forbidden" ||
        cause.kind === "unauthorized" ||
        Boolean(cause.code?.startsWith("scope.")),
    }
  }
  return {
    kind: "unknown",
    message:
      cause instanceof Error && cause.message
        ? cause.message
        : "The profile action failed.",
    retryable: false,
    title: "Profile action failed",
    denied: false,
  }
}

export function isProfileRevisionConflict(cause: unknown): boolean {
  return (
    isGatewayError(cause) &&
    (cause.code === "profile.revision_conflict" || cause.kind === "conflict")
  )
}

export function isProfileDenied(cause: unknown): boolean {
  return profileErrorView(cause).denied
}

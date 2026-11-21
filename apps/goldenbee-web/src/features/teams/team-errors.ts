import { isGatewayError, type GatewayErrorKind } from "@goldenbee/sdk"

export interface TeamErrorView {
  kind: GatewayErrorKind | "unknown"
  code?: string
  message: string
  recovery?: string
  retryable: boolean
  status?: number
  /** Operator-facing headline for this failure class. */
  title: string
  /** The caller token cannot perform this team action. */
  denied: boolean
}

function titleFor(
  kind: TeamErrorView["kind"],
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
      return "Team not found"
    case "conflict":
      return "Team changed elsewhere"
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
      return "Team action failed"
  }
}

/** Present a typed Gateway failure without exposing raw transport detail. */
export function teamErrorView(cause: unknown): TeamErrorView {
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
        : "The team action failed.",
    retryable: false,
    title: "Team action failed",
    denied: false,
  }
}

export function isTeamVersionConflict(cause: unknown): boolean {
  return (
    isGatewayError(cause) &&
    (cause.code === "team.version_conflict" || cause.kind === "conflict")
  )
}

export function isTeamDenied(cause: unknown): boolean {
  return teamErrorView(cause).denied
}

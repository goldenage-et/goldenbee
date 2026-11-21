import { isGatewayError } from "@goldenbee/sdk"

export type HostErrorKind =
  | "not-configured"
  | "denied"
  | "not-found"
  | "conflict"
  | "unavailable"
  | "uncertain"
  | "generic"

export interface HostErrorView {
  kind: HostErrorKind
  title: string
  message: string
  recovery?: string
  code?: string
  retryable: boolean
}

function isOutcomeUncertain(error: unknown): boolean {
  if (!isGatewayError(error)) {
    return false
  }
  return (
    error.kind === "network" ||
    error.kind === "timeout" ||
    error.kind === "server"
  )
}

/**
 * Present one typed Gateway failure for the local-host pages.
 *
 * `write` marks transport and server failures as an uncertain outcome: the
 * Gateway may have applied the operation before the response was lost. The UI
 * must never turn that into success or a plain retry with a new identity.
 */
export function hostErrorView(
  error: unknown,
  action: "read" | "write" = "read",
): HostErrorView {
  if (!isGatewayError(error)) {
    return {
      kind: "generic",
      title: "Gateway request failed",
      message:
        error instanceof Error && error.message
          ? error.message
          : "The Gateway request did not complete.",
      retryable: false,
    }
  }

  const base = {
    message: error.message,
    recovery: error.recovery,
    code: error.code,
    retryable: error.retryable,
  }

  if (error.code === "host.not_configured") {
    return {
      ...base,
      kind: "not-configured",
      title: "Local host is not configured",
      recovery:
        error.recovery ??
        "Ask the operator to enable instances_root and packages_root on this Gateway.",
      retryable: false,
    }
  }

  if (error.code === "host.config_invalid") {
    return {
      ...base,
      kind: "not-configured",
      title: "Local host configuration is invalid",
      retryable: false,
    }
  }

  if (error.kind === "forbidden") {
    return {
      ...base,
      kind: "denied",
      title: "Access denied",
      recovery:
        error.recovery ??
        "This caller token does not have the scope this action requires. Scope cannot be widened from the browser.",
      retryable: false,
    }
  }

  if (error.kind === "unauthorized") {
    return {
      ...base,
      kind: "denied",
      title: "Caller token rejected",
      recovery: error.recovery ?? "Reconnect with a valid caller token.",
      retryable: false,
    }
  }

  if (error.kind === "not-found") {
    return {
      ...base,
      kind: "not-found",
      title: error.code === "host.not_found" ? "Host not found" : "Instance not found",
      retryable: false,
    }
  }

  if (error.kind === "conflict") {
    return {
      ...base,
      kind: "conflict",
      title: "Operation conflict",
      recovery:
        error.code === "host.operation_conflict"
          ? "This operation id was already used with a different request. Start the action again to get a new operation id."
          : error.recovery,
      retryable: false,
    }
  }

  if (action === "write" && isOutcomeUncertain(error)) {
    return {
      ...base,
      kind: "uncertain",
      title: "Outcome uncertain",
      recovery:
        "The Gateway may have applied this operation before the response was lost. Retry with the same operation id, then inspect the instance before repeating a destructive action.",
      retryable: true,
    }
  }

  if (
    error.kind === "network" ||
    error.kind === "timeout" ||
    error.code === "route.worker_unavailable"
  ) {
    return {
      ...base,
      kind: "unavailable",
      title: "Gateway or host unavailable",
      retryable: true,
    }
  }

  if (error.kind === "rate-limited") {
    return {
      ...base,
      kind: "unavailable",
      title: "Gateway is rate limiting requests",
      retryable: true,
    }
  }

  return {
    ...base,
    kind: "generic",
    title: "Gateway request failed",
  }
}

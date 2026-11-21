import type { ConnectionErrorView } from "@/features/connection/connection-store"

export interface ConnectionErrorCopy {
  title: string
  description: string
}

/** Operator-facing copy for one typed connection failure. */
export function connectionErrorCopy(
  error: ConnectionErrorView,
): ConnectionErrorCopy {
  switch (error.kind) {
    case "unauthorized":
      return {
        title: "Caller token rejected",
        description:
          "Gateway recognized the request but did not accept this caller token.",
      }
    case "forbidden":
      return {
        title: "Caller token lacks access",
        description:
          "The token is valid, but this caller cannot discover Workers.",
      }
    case "invalid-url":
      return {
        title: "Check the Gateway URL",
        description:
          "The address must be an http or https URL without credentials.",
      }
    case "network":
    case "timeout":
    case "aborted":
      return {
        title: "Gateway unreachable",
        description: "No Gateway answered at this address.",
      }
    default:
      return {
        title: "Gateway request failed",
        description:
          "Gateway answered, but the connection check did not complete.",
      }
  }
}

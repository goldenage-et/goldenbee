/**
 * Goldenbee Gateway client boundary.
 *
 * Applications import from this module only. Connection, chat streaming,
 * approvals, and task routes are added here in later tickets so every
 * transport change lands behind one package-shaped seam.
 */
export { createGatewayClient } from "./client"
export type { GatewayClient, GatewayClientOptions, GatewayFetch } from "./client"
export {
  GatewayError,
  errorFromResponse,
  isGatewayError,
  isRetryableStatus,
  kindForStatus,
} from "./errors"
export type { GatewayErrorDetails, GatewayErrorKind } from "./errors"
export type {
  ConnectionResult,
  GatewayCapability,
  GatewayHealth,
  GatewayRequestOptions,
  GatewayWorker,
} from "./types"
export { normalizeGatewayUrl } from "./url"

/**
 * Goldenbee Gateway client boundary.
 *
 * Applications import from this module only. Connection, tasks, streams,
 * approvals, workers, profiles, providers, packages, instances, diagnostics,
 * and socket frames all live behind this one package-shaped seam.
 */
export { createGatewayClient, newIdempotencyKey } from "./client.js"
export type {
  GatewayClient,
  GatewayClientOptions,
  GatewayFetch,
} from "./client.js"
export {
  GatewayError,
  errorFromResponse,
  isErrorEnvelope,
  isGatewayError,
  isRetryableStatus,
  isRetryableTransportError,
  kindForCode,
  kindForStatus,
  sdkError,
} from "./errors.js"
export type {
  GatewayErrorDetails,
  GatewayErrorEnvelope,
  GatewayErrorKind,
  GatewayErrorOptions,
} from "./errors.js"
export {
  GatewayTaskStream,
  MAX_STREAM_BLOCK_BYTES,
  SseBlockParser,
  StreamCursor,
  frameFromBlock,
  openTaskStream,
  pageHasCursorGap,
  parseSseBlock,
} from "./sse.js"
export type { ParsedSseBlock } from "./sse.js"
export {
  GatewaySocket,
  approvalCommand,
  cancelCommand,
  connectSocket,
  gatewayErrorFromFrame,
  parseServerFrame,
  socketCommandFrame,
  socketUrl,
  subscribeCommand,
  submitCommand,
  validateSocketFrame,
} from "./socket.js"
export type {
  GatewaySocketLike,
  GatewayWebSocketFactory,
} from "./socket.js"
export type { RetryOptions } from "./transport.js"
export type * from "./types.js"
export { normalizeGatewayUrl } from "./url.js"

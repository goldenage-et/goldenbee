import {
  GatewayError,
  errorFromResponse,
  isGatewayError,
} from "./errors"
import type {
  ConnectionResult,
  GatewayHealth,
  GatewayRequestOptions,
  GatewayWorker,
} from "./types"
import { normalizeGatewayUrl } from "./url"

export type GatewayFetch = (
  input: string,
  init?: RequestInit,
) => Promise<Response>

export interface GatewayClientOptions {
  baseUrl: string
  /** Caller bearer token. It is held in memory and sent only in a header. */
  token?: string
  fetch?: GatewayFetch
  timeoutMs?: number
}

export interface GatewayClient {
  readonly baseUrl: string
  readonly hasToken: boolean
  /** Public liveness plus approved and reachable Worker counts. */
  health(options?: GatewayRequestOptions): Promise<GatewayHealth>
  /** Caller-scoped Worker discovery, which also proves the token. */
  listWorkers(options?: GatewayRequestOptions): Promise<GatewayWorker[]>
  /** One connection handshake with a discriminated, renderable result. */
  checkConnection(options?: GatewayRequestOptions): Promise<ConnectionResult>
}

const defaultTimeoutMs = 15_000

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null
}

const workerStatuses = new Set<string>([
  "approved",
  "registered",
  "unreachable",
  "disabled",
  "decommissioned",
])

function isHealth(value: unknown): value is GatewayHealth {
  return (
    isRecord(value) &&
    typeof value.status === "string" &&
    Number.isInteger(value.workers) &&
    (value.workers as number) >= 0 &&
    Number.isInteger(value.reachable) &&
    (value.reachable as number) >= 0
  )
}

function isWorker(value: unknown): value is GatewayWorker {
  return (
    isRecord(value) &&
    typeof value.id === "string" &&
    typeof value.instance === "string" &&
    typeof value.package === "string" &&
    typeof value.version === "string" &&
    typeof value.status === "string" &&
    workerStatuses.has(value.status) &&
    typeof value.reachable === "boolean" &&
    (value.locality === "local" || value.locality === "remote")
  )
}

function isWorkerList(value: unknown): value is GatewayWorker[] {
  return Array.isArray(value) && value.every(isWorker)
}

function abortable(
  external: AbortSignal | undefined,
  timeoutMs: number,
): { signal: AbortSignal; timedOut: () => boolean; dispose: () => void } {
  const controller = new AbortController()
  let timedOut = false
  const timer = setTimeout(() => {
    timedOut = true
    controller.abort()
  }, timeoutMs)
  const forwardAbort = () => controller.abort()
  external?.addEventListener("abort", forwardAbort, { once: true })

  return {
    signal: controller.signal,
    timedOut: () => timedOut,
    dispose: () => {
      clearTimeout(timer)
      external?.removeEventListener("abort", forwardAbort)
    },
  }
}

function invalidResponse(message: string): GatewayError {
  return new GatewayError({
    kind: "invalid-response",
    message,
    retryable: false,
  })
}

/**
 * Create an app-local Gateway client. The returned surface is the only way UI
 * code talks to Gateway; tickets extend it here rather than in components.
 */
export function createGatewayClient(
  options: GatewayClientOptions,
): GatewayClient {
  const baseUrl = normalizeGatewayUrl(options.baseUrl)
  const token = options.token?.trim() || undefined
  const doFetch = options.fetch ?? ((input, init) => fetch(input, init))
  const timeoutMs = options.timeoutMs ?? defaultTimeoutMs

  async function requestJson(
    path: string,
    request: { authenticated: boolean; signal?: AbortSignal },
  ): Promise<unknown> {
    const abort = abortable(request.signal, timeoutMs)
    const headers: Record<string, string> = { accept: "application/json" }
    if (request.authenticated && token) {
      headers.authorization = `Bearer ${token}`
    }

    let response: Response
    try {
      response = await doFetch(`${baseUrl}${path}`, {
        method: "GET",
        headers,
        signal: abort.signal,
      })
    } catch (cause) {
      if (request.signal?.aborted) {
        throw new GatewayError({
          kind: "aborted",
          message: "The Gateway request was cancelled.",
        })
      }
      if (abort.timedOut()) {
        throw new GatewayError({
          kind: "timeout",
          message: "The Gateway did not answer in time.",
          retryable: true,
        })
      }
      throw new GatewayError({
        kind: "network",
        message:
          cause instanceof Error && cause.message
            ? `The Gateway could not be reached: ${cause.message}`
            : "The Gateway could not be reached.",
        retryable: true,
      })
    } finally {
      abort.dispose()
    }

    if (!response.ok) {
      const body = await response.json().catch(() => undefined)
      throw errorFromResponse(
        response.status,
        body,
        `Gateway returned HTTP ${response.status}.`,
      )
    }

    return response.json().catch(() => {
      throw invalidResponse("Gateway returned a response that is not JSON.")
    })
  }

  async function health(
    request: GatewayRequestOptions = {},
  ): Promise<GatewayHealth> {
    const body = await requestJson("/v1/health", {
      authenticated: false,
      signal: request.signal,
    })
    if (!isHealth(body)) {
      throw invalidResponse("Gateway health response has an unexpected shape.")
    }
    return body
  }

  async function listWorkers(
    request: GatewayRequestOptions = {},
  ): Promise<GatewayWorker[]> {
    const body = await requestJson("/v1/workers", {
      authenticated: true,
      signal: request.signal,
    })
    if (!isWorkerList(body)) {
      throw invalidResponse("Gateway Worker list has an unexpected shape.")
    }
    return body
  }

  async function checkConnection(
    request: GatewayRequestOptions = {},
  ): Promise<ConnectionResult> {
    let gatewayHealth: GatewayHealth
    try {
      gatewayHealth = await health(request)
    } catch (cause) {
      return {
        status: "unreachable",
        error: toGatewayError(cause),
      }
    }

    try {
      const workers = await listWorkers(request)
      return { status: "connected", health: gatewayHealth, workers }
    } catch (cause) {
      const error = toGatewayError(cause)
      if (error.kind === "unauthorized") {
        return { status: "unauthorized", health: gatewayHealth, error }
      }
      if (error.kind === "forbidden") {
        return { status: "forbidden", health: gatewayHealth, error }
      }
      return { status: "error", health: gatewayHealth, error }
    }
  }

  return {
    baseUrl,
    hasToken: Boolean(token),
    health,
    listWorkers,
    checkConnection,
  }
}

function toGatewayError(cause: unknown): GatewayError {
  if (isGatewayError(cause)) {
    return cause
  }
  return new GatewayError({
    kind: "network",
    message:
      cause instanceof Error && cause.message
        ? cause.message
        : "The Gateway could not be reached.",
    retryable: true,
  })
}

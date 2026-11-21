import {
  GatewayError,
  errorFromResponse,
  isErrorEnvelope,
  isGatewayError,
  isRetryableTransportError,
  sdkError,
} from "./errors.js"

export type GatewayFetch = (input: string, init?: RequestInit) => Promise<Response>

export interface RetryOptions {
  /** Total attempts, including the first. Clamped to 1-8. */
  maxAttempts?: number
  initialDelayMs?: number
  maxDelayMs?: number
}

export interface GatewayTransportOptions {
  baseUrl: string
  token?: string
  fetch: GatewayFetch
  timeoutMs: number
  retry?: RetryOptions
  /** Test seam; production waits with `setTimeout`. */
  sleep?: (milliseconds: number) => Promise<void>
}

export interface RequestSpec<T> {
  method: "GET" | "POST" | "PUT" | "PATCH" | "DELETE"
  path: string
  /** JSON body. Omitted means no request body. */
  body?: unknown
  authenticated?: boolean
  signal?: AbortSignal
  validate: (value: unknown) => value is T
  /**
   * Reconcile `task.unknown_submission`: reuse the exact body and idempotency
   * key, and keep the first truthful uncertainty if a later attempt fails.
   */
  reconcile?: boolean
}

export const MAX_ATTEMPTS = 8
export const DEFAULT_MAX_ATTEMPTS = 3
export const DEFAULT_INITIAL_DELAY_MS = 100
export const DEFAULT_MAX_DELAY_MS = 5_000

const defaultSleep = (milliseconds: number) =>
  new Promise<void>((resolve) => {
    setTimeout(resolve, milliseconds)
  })

export function encodeSegment(value: string): string {
  return encodeURIComponent(value)
}

export function queryString(
  values: Record<string, string | number | boolean | undefined | null>,
): string {
  const params = new URLSearchParams()
  for (const [key, value] of Object.entries(values)) {
    if (value === undefined || value === null) {
      continue
    }
    params.set(key, String(value))
  }
  const encoded = params.toString()
  return encoded ? `?${encoded}` : ""
}

interface AbortScope {
  signal: AbortSignal
  timedOut: () => boolean
  dispose: () => void
}

function abortable(
  external: AbortSignal | undefined,
  timeoutMs: number,
): AbortScope {
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

export class GatewayTransport {
  readonly baseUrl: string
  private readonly token: string | undefined
  private readonly doFetch: GatewayFetch
  private readonly timeoutMs: number
  private readonly maxAttempts: number
  private readonly initialDelayMs: number
  private readonly maxDelayMs: number
  private readonly wait: (milliseconds: number) => Promise<void>

  constructor(options: GatewayTransportOptions) {
    this.baseUrl = options.baseUrl.replace(/\/+$/, "")
    this.token = options.token?.trim() || undefined
    this.doFetch = options.fetch
    this.timeoutMs = options.timeoutMs
    this.maxAttempts = Math.min(
      MAX_ATTEMPTS,
      Math.max(1, Math.trunc(options.retry?.maxAttempts ?? DEFAULT_MAX_ATTEMPTS)),
    )
    this.initialDelayMs = Math.max(
      0,
      options.retry?.initialDelayMs ?? DEFAULT_INITIAL_DELAY_MS,
    )
    this.maxDelayMs = Math.max(
      this.initialDelayMs,
      options.retry?.maxDelayMs ?? DEFAULT_MAX_DELAY_MS,
    )
    this.wait = options.sleep ?? defaultSleep
  }

  /** Redact the caller token from any text that might reach the UI. */
  private redact(text: string): string {
    if (!this.token) {
      return text
    }
    return text.split(this.token).join("[redacted]")
  }

  private toGatewayError(cause: unknown, signal: AbortSignal | undefined): GatewayError {
    if (isGatewayError(cause)) {
      if (cause.message.includes(this.token ?? "\u0000")) {
        return new GatewayError({
          kind: cause.kind,
          status: cause.status,
          code: cause.code,
          message: this.redact(cause.message),
          retryable: cause.retryable,
          recovery: cause.recovery,
          details: cause.details,
          retryAfterMs: cause.retryAfterMs,
        })
      }
      return cause
    }
    if (signal?.aborted) {
      return sdkError("aborted", "The Gateway request was cancelled.")
    }
    const message =
      cause instanceof Error && cause.message
        ? cause.message
        : "The Gateway could not be reached."
    return sdkError("network", this.redact(message), { retryable: true })
  }

  private async sleepBeforeRetry(completed: number, error: GatewayError): Promise<void> {
    const exponent = Math.min(16, Math.max(0, completed - 1))
    const base = Math.min(this.initialDelayMs * 2 ** exponent, this.maxDelayMs)
    const jitter = Math.floor(Math.random() * (base / 10 + 1))
    const requested = error.retryAfterMs ?? 0
    const delay = Math.min(this.maxDelayMs, Math.max(base + jitter, requested))
    if (delay > 0) {
      await this.wait(delay)
    }
  }

  private headers(spec: RequestSpec<unknown>): Record<string, string> {
    const headers: Record<string, string> = {
      accept: "application/json",
    }
    if (spec.authenticated !== false && this.token) {
      headers.authorization = `Bearer ${this.token}`
    }
    if (spec.body !== undefined) {
      headers["content-type"] = "application/json"
    }
    return headers
  }

  private async readBody(response: Response): Promise<unknown> {
    const text = await response.text()
    if (!text.trim()) {
      return undefined
    }
    try {
      return JSON.parse(text) as unknown
    } catch {
      throw sdkError(
        "invalid-response",
        `Gateway returned a response that is not JSON (HTTP ${response.status}).`,
      )
    }
  }

  private async attempt<T>(spec: RequestSpec<T>): Promise<T> {
    const abort = abortable(spec.signal, this.timeoutMs)
    try {
      let response: Response
      try {
        response = await this.doFetch(`${this.baseUrl}${spec.path}`, {
          method: spec.method,
          headers: this.headers(spec),
          body: spec.body === undefined ? undefined : JSON.stringify(spec.body),
          signal: abort.signal,
        })
      } catch (cause) {
        if (spec.signal?.aborted) {
          throw sdkError("aborted", "The Gateway request was cancelled.")
        }
        if (abort.timedOut()) {
          throw sdkError("timeout", "The Gateway did not answer in time.", {
            retryable: true,
          })
        }
        throw this.toGatewayError(cause, abort.signal)
      }

      const body = await this.readBody(response)
      if (response.status === 202) {
        if (isErrorEnvelope(body)) {
          throw errorFromResponse(
            response.status,
            body,
            "Gateway returned HTTP 202 without a definitive task identity.",
          )
        }
        throw sdkError(
          "uncertain-submission",
          "Gateway returned HTTP 202 without a definitive task identity.",
        )
      }
      if (!response.ok) {
        const error = errorFromResponse(
          response.status,
          body,
          `Gateway returned HTTP ${response.status}.`,
        )
        const retryAfter = response.headers.get("retry-after")
        if (retryAfter && error.retryAfterMs === undefined) {
          const seconds = Number(retryAfter)
          if (Number.isFinite(seconds) && seconds >= 0) {
            throw this.redactError(
              new GatewayError({
                kind: error.kind,
                status: error.status,
                code: error.code,
                message: error.message,
                retryable: error.retryable,
                recovery: error.recovery,
                details: error.details,
                retryAfterMs: seconds * 1000,
              }),
            )
          }
        }
        throw this.redactError(error)
      }
      // A success-family status may still carry a typed error envelope. The
      // Gateway uses this for a 202 unknown submission; tolerate a proxy that
      // forwards it with the original status.
      if (isErrorEnvelope(body)) {
        throw errorFromResponse(
          response.status,
          body,
          "Gateway returned an error envelope with a success status.",
        )
      }
      if (!spec.validate(body)) {
        throw sdkError(
          "invalid-response",
          `Gateway ${spec.method} ${spec.path} returned an unexpected shape.`,
        )
      }
      return body
    } finally {
      abort.dispose()
    }
  }

  async request<T>(spec: RequestSpec<T>): Promise<T> {
    let completed = 0
    let uncertainty: GatewayError | undefined
    let last: GatewayError | undefined
    while (completed < this.maxAttempts) {
      completed += 1
      try {
        return await this.attempt(spec)
      } catch (cause) {
        const error = this.toGatewayError(cause, spec.signal)
        last = error
        if (spec.reconcile && error.isUncertainSubmission) {
          uncertainty ??= error
        }
        const retry =
          (spec.reconcile === true && error.isUncertainSubmission) ||
          isRetryableTransportError(error)
        if (completed >= this.maxAttempts || !retry) {
          break
        }
        await this.sleepBeforeRetry(completed, error)
      }
    }
    throw uncertainty ?? last ?? sdkError("network", "The Gateway request failed.")
  }

  /**
   * Open a long-lived streaming response.
   *
   * Only the connect attempt is retried and timed out. The response body is
   * owned by the caller and is closed by its `AbortSignal`.
   */
  async openStream(path: string, signal?: AbortSignal): Promise<Response> {
    return this.openResponse(path, "text/event-stream", signal)
  }

  /**
   * Open a response for byte delivery (artifact content).
   *
   * Only the connect attempt is retried and timed out.
   */
  async openResponse(
    path: string,
    accept: string,
    signal?: AbortSignal,
  ): Promise<Response> {
    let completed = 0
    let last: GatewayError | undefined
    while (completed < this.maxAttempts) {
      completed += 1
      const abort = abortable(signal, this.timeoutMs)
      let response: Response
      try {
        response = await this.doFetch(`${this.baseUrl}${path}`, {
          method: "GET",
          headers: {
            accept,
            ...(this.token ? { authorization: `Bearer ${this.token}` } : {}),
          },
          signal: abort.signal,
        })
      } catch (cause) {
        const error = abort.timedOut()
          ? sdkError("timeout", "The Gateway did not answer in time.", {
              retryable: true,
            })
          : this.toGatewayError(cause, abort.signal)
        last = error
        if (completed >= this.maxAttempts || !isRetryableTransportError(error)) {
          throw error
        }
        await this.sleepBeforeRetry(completed, error)
        continue
      } finally {
        abort.dispose()
      }
      if (response.ok) {
        return response
      }
      const body = await this.readBody(response)
      const error = errorFromResponse(
        response.status,
        body,
        `Gateway returned HTTP ${response.status} when opening the response.`,
      )
      throw this.redactError(error)
    }
    throw last ?? sdkError("network", "The Gateway response could not be opened.")
  }

  private redactError(error: GatewayError): GatewayError {
    if (!this.token || !error.message.includes(this.token)) {
      return error
    }
    return new GatewayError({
      kind: error.kind,
      status: error.status,
      code: error.code,
      message: this.redact(error.message),
      retryable: error.retryable,
      recovery: error.recovery,
      details: error.details,
      retryAfterMs: error.retryAfterMs,
    })
  }
}

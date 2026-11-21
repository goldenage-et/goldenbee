import {
  isGatewayError,
  type GatewayRequestOptions,
  type GatewayStreamFrame,
  type GatewaySubmitRequest,
  type GatewaySubmitResponse,
  type GatewayTask,
  type StreamEvent,
} from "@goldenbee/sdk"

import type {
  ChatAction,
  ChatErrorInfo,
  ChatSelection,
} from "./chat-types"

/** The minimal SDK surface the chat lifecycle needs. */
export interface ChatStream extends AsyncIterable<GatewayStreamFrame> {
  /** Highest durable sequence; the only cursor a reconnect may use. */
  readonly lastDurableSeq: number
}

export interface ChatGateway {
  submit(
    request: GatewaySubmitRequest,
    options?: GatewayRequestOptions,
  ): Promise<GatewaySubmitResponse>
  watch(
    taskId: string,
    after?: number,
    options?: GatewayRequestOptions,
  ): Promise<ChatStream>
  cancel(taskId: string, options?: GatewayRequestOptions): Promise<GatewayTask>
}

export interface ChatRunnerOptions {
  client: ChatGateway
  dispatch: (action: ChatAction) => void
  sessionId: () => string
  /** Test seam; production waits with `setTimeout`. */
  sleep?: (milliseconds: number) => Promise<void>
  /** Test seam for deterministic backoff jitter. */
  random?: () => number
  maxReconnectAttempts?: number
  initialBackoffMs?: number
  maxBackoffMs?: number
}

export interface RunTaskInput {
  text: string
  selection: ChatSelection
  assistantMessageId: string
  idempotencyKey: string
}

const terminalKinds = new Set([
  "task.succeeded",
  "task.failed",
  "task.cancelled",
  "task.unknown",
])

const defaultSleep = (milliseconds: number) =>
  new Promise<void>((resolve) => {
    setTimeout(resolve, milliseconds)
  })

function sleepUnlessAborted(
  sleep: (milliseconds: number) => Promise<void>,
  milliseconds: number,
  signal: AbortSignal,
): Promise<void> {
  if (signal.aborted) {
    return Promise.resolve()
  }
  return Promise.race([
    sleep(milliseconds),
    new Promise<void>((resolve) => {
      signal.addEventListener("abort", () => resolve(), { once: true })
    }),
  ])
}

function errorInfo(cause: unknown): ChatErrorInfo {
  if (isGatewayError(cause)) {
    return {
      code: cause.code ?? `sdk.${cause.kind}`,
      message: cause.message,
      recovery: cause.recovery,
      retryable: cause.retryable,
      details: cause.details,
    }
  }
  return {
    code: "sdk.unknown",
    message:
      cause instanceof Error && cause.message
        ? cause.message
        : "The Gateway operation failed.",
    retryable: false,
  }
}

function isRetryable(cause: unknown): boolean {
  if (!isGatewayError(cause)) {
    return false
  }
  if (cause.kind === "aborted" || cause.kind === "unauthorized" || cause.kind === "forbidden") {
    return false
  }
  if (cause.isCursorExpired || cause.kind === "cursor-invalid") {
    return false
  }
  return cause.retryable || cause.kind === "network" || cause.kind === "timeout"
}

function laggedCursor(event: StreamEvent): number | null {
  if (event.kind !== "stream.lagged") {
    return null
  }
  const payload = event.payload
  if (typeof payload !== "object" || payload === null) {
    return null
  }
  const fromSeq = (payload as Record<string, unknown>).from_seq
  return typeof fromSeq === "number" && Number.isFinite(fromSeq) ? fromSeq : null
}

/**
 * Owns the submit, stream, cancel, and reconnect lifecycle for one active task.
 *
 * The runner never dispatches `send` or `retry-started`; the session owns the
 * user turn, so a retry cannot duplicate the message or reuse an idempotency
 * key. The runner also aborts any previous subscription before opening a new
 * one, so one task can never hold two streams.
 */
export class ChatRunner {
  private readonly client: ChatGateway
  private readonly dispatch: (action: ChatAction) => void
  private readonly sessionId: () => string
  private readonly sleep: (milliseconds: number) => Promise<void>
  private readonly random: () => number
  private readonly maxReconnectAttempts: number
  private readonly initialBackoffMs: number
  private readonly maxBackoffMs: number

  private controller: AbortController | null = null
  private taskId: string | null = null
  private assistantId: string | null = null
  private disposed = false

  constructor(options: ChatRunnerOptions) {
    this.client = options.client
    this.dispatch = options.dispatch
    this.sessionId = options.sessionId
    this.sleep = options.sleep ?? defaultSleep
    this.random = options.random ?? Math.random
    this.maxReconnectAttempts = Math.max(0, options.maxReconnectAttempts ?? 5)
    this.initialBackoffMs = Math.max(1, options.initialBackoffMs ?? 400)
    this.maxBackoffMs = Math.max(
      this.initialBackoffMs,
      options.maxBackoffMs ?? 8_000,
    )
  }

  get activeTaskId(): string | null {
    return this.taskId
  }

  async runTask(input: RunTaskInput): Promise<void> {
    this.abortActive()
    if (this.disposed) {
      return
    }
    const controller = new AbortController()
    this.controller = controller
    this.assistantId = input.assistantMessageId

    const request: GatewaySubmitRequest = {
      worker: input.selection.worker,
      agent: input.selection.agent,
      session_id: this.sessionId(),
      input: input.text,
      idempotency_key: input.idempotencyKey,
    }
    const profile = input.selection.profile?.trim()
    if (profile) {
      request.profile = profile
    }

    let response: GatewaySubmitResponse
    try {
      response = await this.client.submit(request, { signal: controller.signal })
    } catch (cause) {
      if (controller.signal.aborted || this.disposed) {
        return
      }
      if (isGatewayError(cause) && cause.isUncertainSubmission) {
        this.dispatch({
          type: "submit-uncertain",
          assistantMessageId: input.assistantMessageId,
          error: errorInfo(cause),
        })
        return
      }
      this.dispatch({
        type: "submit-failed",
        assistantMessageId: input.assistantMessageId,
        error: errorInfo(cause),
      })
      return
    }

    if (controller.signal.aborted || this.disposed) {
      return
    }

    this.taskId = response.task_id
    this.dispatch({
      type: "submitted",
      assistantMessageId: input.assistantMessageId,
      taskId: response.task_id,
      state: response.state,
    })

    await this.stream(response.task_id, input.assistantMessageId, controller.signal)
  }

  /** Send the Gateway cancellation request and report the returned projection. */
  async stop(): Promise<void> {
    const taskId = this.taskId
    const assistantId = this.assistantId
    if (!taskId || !assistantId) {
      return
    }
    this.dispatch({ type: "stop-requested" })
    try {
      const task = await this.client.cancel(taskId)
      this.dispatch({
        type: "stop-applied",
        assistantMessageId: assistantId,
        task,
      })
      if (isTerminalState(task)) {
        this.finish()
      }
    } catch (cause) {
      this.dispatch({
        type: "stop-failed",
        assistantMessageId: assistantId,
        error: errorInfo(cause),
      })
    }
  }

  /** Abort the active stream and release the task identity. */
  dispose(): void {
    this.disposed = true
    this.abortActive()
  }

  /**
   * Abort only the current subscription while leaving the runner reusable.
   *
   * React development StrictMode runs effect cleanup immediately after setup;
   * unmount cleanup must abort the fetch without poisoning the session.
   */
  interrupt(): void {
    this.abortActive()
  }

  private abortActive(): void {
    this.controller?.abort()
    this.controller = null
    this.taskId = null
    this.assistantId = null
  }

  private finish(): void {
    this.controller?.abort()
    this.controller = null
    this.taskId = null
    this.assistantId = null
  }

  private backoff(attempt: number): number {
    const base = Math.min(
      this.maxBackoffMs,
      this.initialBackoffMs * 2 ** Math.max(0, attempt - 1),
    )
    return base + Math.floor(this.random() * (base / 10 + 1))
  }

  private async stream(
    taskId: string,
    assistantMessageId: string,
    signal: AbortSignal,
  ): Promise<void> {
    let after = 0
    let attempt = 0

    while (!signal.aborted && !this.disposed) {
      let stream: ChatStream
      try {
        stream = await this.client.watch(taskId, after, { signal })
      } catch (cause) {
        if (signal.aborted || this.disposed) {
          return
        }
        if (isGatewayError(cause) && cause.isCursorExpired) {
          this.dispatch({ type: "gap", reason: "cursor_expired", fromSeq: after })
          this.finish()
          return
        }
        if (isGatewayError(cause) && cause.kind === "cursor-invalid") {
          this.dispatch({ type: "gap", reason: "cursor_invalid", fromSeq: after })
          this.finish()
          return
        }
        if (!isRetryable(cause) || attempt >= this.maxReconnectAttempts) {
          this.dispatch({
            type: "stream-error",
            assistantMessageId,
            error: errorInfo(cause),
          })
          this.finish()
          return
        }
        attempt += 1
        await sleepUnlessAborted(this.sleep, this.backoff(attempt), signal)
        continue
      }

      let terminal = false
      let requiresReconnect = false
      try {
        for await (const frame of stream) {
          if (signal.aborted || this.disposed) {
            return
          }
          if (frame.type === "draining") {
            this.dispatch({
              type: "stream-draining",
              assistantMessageId,
              phase: frame.phase,
            })
            requiresReconnect = true
            break
          }
          const event = frame.event
          this.dispatch({ type: "event", assistantMessageId, event })
          if (event.durable) {
            after = Math.max(after, event.seq)
          }
          const lagged = laggedCursor(event)
          if (lagged !== null) {
            after = Math.max(after, lagged)
            requiresReconnect = true
            break
          }
          if (terminalKinds.has(event.kind)) {
            terminal = true
            break
          }
        }
      } catch (cause) {
        if (signal.aborted || this.disposed) {
          return
        }
        if (isGatewayError(cause) && cause.isCursorExpired) {
          this.dispatch({ type: "gap", reason: "cursor_expired", fromSeq: after })
          this.finish()
          return
        }
        if (isGatewayError(cause) && cause.kind === "cursor-invalid") {
          this.dispatch({ type: "gap", reason: "cursor_invalid", fromSeq: after })
          this.finish()
          return
        }
        if (!isRetryable(cause) || attempt >= this.maxReconnectAttempts) {
          this.dispatch({
            type: "stream-error",
            assistantMessageId,
            error: errorInfo(cause),
          })
          this.finish()
          return
        }
        requiresReconnect = true
      }

      after = Math.max(after, stream.lastDurableSeq)
      if (terminal) {
        this.finish()
        return
      }
      if (signal.aborted || this.disposed) {
        return
      }
      if (attempt >= this.maxReconnectAttempts) {
        this.dispatch({
          type: "stream-error",
          assistantMessageId,
          error: {
            code: "sdk.stream_interrupted",
            message:
              "The Gateway stream closed before the task reached a terminal state.",
            recovery:
              "Reconnect the chat or inspect the task on the Gateway before retrying.",
            retryable: true,
          },
        })
        this.finish()
        return
      }
      attempt += 1
      if (requiresReconnect || attempt > 0) {
        await sleepUnlessAborted(this.sleep, this.backoff(attempt), signal)
      }
    }
  }
}

function isTerminalState(task: GatewayTask): boolean {
  const state = task.task.state
  return (
    state === "succeeded" ||
    state === "failed" ||
    state === "cancelled" ||
    state === "unknown"
  )
}

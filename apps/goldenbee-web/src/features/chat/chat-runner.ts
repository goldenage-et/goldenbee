import {
  isGatewayError,
  type GatewayRequestOptions,
  type GatewayStreamFrame,
  type GatewaySubmitRequest,
  type GatewaySubmitResponse,
  type GatewayTask,
  type StreamEvent,
  type StreamPage,
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
  task(taskId: string, options?: GatewayRequestOptions): Promise<GatewayTask>
  events(
    taskId: string,
    after?: number,
    limit?: number,
    options?: GatewayRequestOptions,
  ): Promise<StreamPage>
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

export interface ResumeTaskInput {
  taskId: string
  assistantMessageId: string
  /** Last durable sequence saved before the reload. */
  after: number
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

  /**
   * Reconnect one task that was active before a reload.
   *
   * The caller has already reconciled the canonical task projection. The stream
   * always starts at the saved durable cursor; an ephemeral delta sequence is
   * never accepted across a reload.
   */
  async resumeTask(input: ResumeTaskInput): Promise<void> {
    this.abortActive()
    if (this.disposed) {
      return
    }
    const controller = new AbortController()
    this.controller = controller
    this.taskId = input.taskId
    this.assistantId = input.assistantMessageId
    await this.stream(
      input.taskId,
      input.assistantMessageId,
      controller.signal,
      Math.max(0, input.after),
    )
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
    initialAfter = 0,
  ): Promise<void> {
    let after = Math.max(0, initialAfter)
    let attempt = 0
    let gapRecoveries = 0

    while (!signal.aborted && !this.disposed) {
      let stream: ChatStream
      try {
        stream = await this.client.watch(taskId, after, { signal })
      } catch (cause) {
        if (signal.aborted || this.disposed) {
          return
        }
        if (isCursorGap(cause)) {
          if (gapRecoveries >= 2) {
            this.dispatch({
              type: "stream-error",
              assistantMessageId,
              error: {
                code: "sdk.cursor_recovery_failed",
                message:
                  "The saved event cursor could not be reconciled with the Gateway.",
                recovery: "Inspect the task on the Gateway or reset local history.",
                retryable: false,
              },
            })
            this.finish()
            return
          }
          gapRecoveries += 1
          const resumed = await this.reloadFromGap(
            taskId,
            assistantMessageId,
            signal,
            after,
            cause,
          )
          if (resumed === null) {
            return
          }
          after = resumed
          attempt = 0
          continue
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
        if (isCursorGap(cause)) {
          if (gapRecoveries >= 2) {
            this.dispatch({
              type: "stream-error",
              assistantMessageId,
              error: {
                code: "sdk.cursor_recovery_failed",
                message:
                  "The saved event cursor could not be reconciled with the Gateway.",
                recovery: "Inspect the task on the Gateway or reset local history.",
                retryable: false,
              },
            })
            this.finish()
            return
          }
          gapRecoveries += 1
          const resumed = await this.reloadFromGap(
            taskId,
            assistantMessageId,
            signal,
            after,
            cause,
          )
          if (resumed === null) {
            return
          }
          after = resumed
          attempt = 0
          continue
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

  /**
   * Recover a cursor that fell outside retention.
   *
   * The replay is explicitly incomplete: the UI keeps the gap notice, the
   * canonical task projection replaces local truth, and retained events repopulate
   * tool and approval identity. The stream then resumes at `next_seq`.
   */
  private async reloadFromGap(
    taskId: string,
    assistantMessageId: string,
    signal: AbortSignal,
    fromSeq: number,
    cause: unknown,
  ): Promise<number | null> {
    const expired = isGatewayError(cause) && cause.isCursorExpired
    this.dispatch({
      type: "gap",
      reason: expired ? "cursor_expired" : "cursor_invalid",
      fromSeq,
    })

    let task: GatewayTask
    try {
      task = await this.client.task(taskId, { signal })
    } catch (failure) {
      if (signal.aborted || this.disposed) {
        return null
      }
      if (isGatewayError(failure) && failure.kind === "not-found") {
        this.dispatch({
          type: "task-detached",
          assistantMessageId,
          taskId,
          detail:
            "The Gateway no longer exposes this task. Local history is kept, but the task cannot be resumed.",
        })
        this.finish()
        return null
      }
      this.dispatch({
        type: "stream-error",
        assistantMessageId,
        error: errorInfo(failure),
      })
      this.finish()
      return null
    }

    this.dispatch({ type: "task-projection", assistantMessageId, task })
    if (isTerminalState(task)) {
      this.finish()
      return null
    }

    // The retained page may start after the beginning of history. Reset the
    // in-memory cursor before applying it so retained task/tool/approval events
    // are not mistaken for duplicates of the expired cursor.
    this.dispatch({ type: "cursor-reset" })

    let after = 0
    try {
      const page = await this.client.events(taskId, 0, undefined, { signal })
      for (const event of page.events) {
        this.dispatch({ type: "event", assistantMessageId, event })
        if (terminalKinds.has(event.kind)) {
          this.finish()
          return null
        }
        if (event.durable) {
          after = Math.max(after, event.seq)
        }
      }
      after = Math.max(after, page.next_seq)
    } catch (failure) {
      if (signal.aborted || this.disposed) {
        return null
      }
      this.dispatch({
        type: "stream-error",
        assistantMessageId,
        error: errorInfo(failure),
      })
      this.finish()
      return null
    }

    return after
  }
}

function isCursorGap(cause: unknown): boolean {
  return (
    isGatewayError(cause) &&
    (cause.isCursorExpired || cause.kind === "cursor-invalid")
  )
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

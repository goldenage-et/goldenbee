import { describe, expect, it, vi } from "vitest"

import { createChatSession } from "@/features/chat/chat-session"
import type { ChatGateway, ChatStream } from "@/features/chat/chat-runner"
import type { ChatSelection } from "@/features/chat/chat-types"
import {
  GatewayError,
  type GatewayRequestOptions,
  type GatewayStreamFrame,
  type GatewaySubmitRequest,
  type GatewaySubmitResponse,
  type GatewayTask,
  type StreamEvent,
} from "@goldenbee/sdk"

const selection: ChatSelection = {
  worker: "general-a",
  agent: "general",
  profile: "default",
}

function event(
  seq: number,
  kind: string,
  payload: unknown,
  durable = true,
): StreamEvent {
  return {
    event_id: `evt-${seq}`,
    source: "worker",
    incarnation: "inc-1",
    stream_id: "task:exec",
    seq,
    schema_version: 1,
    kind,
    durable,
    payload,
    at: seq,
  }
}

class FakeStream implements ChatStream {
  lastDurableSeq = 0
  private readonly events: StreamEvent[]
  private readonly signal: AbortSignal | undefined
  private readonly holdOpen: boolean

  constructor(
    events: StreamEvent[],
    signal?: AbortSignal,
    holdOpen = false,
  ) {
    this.events = events
    this.signal = signal
    this.holdOpen = holdOpen
  }

  async *[Symbol.asyncIterator](): AsyncIterator<GatewayStreamFrame> {
    for (const next of this.events) {
      yield { type: "event", event: next }
      if (next.durable) {
        this.lastDurableSeq = Math.max(this.lastDurableSeq, next.seq)
      }
    }
    if (this.holdOpen && this.signal) {
      await new Promise<void>((resolve) => {
        if (this.signal?.aborted) {
          resolve()
          return
        }
        this.signal?.addEventListener("abort", () => resolve(), { once: true })
      })
    }
  }
}

interface WatchCall {
  taskId: string
  after: number
  options?: GatewayRequestOptions
}

class FakeGateway implements ChatGateway {
  submits: Array<{ request: GatewaySubmitRequest; options?: GatewayRequestOptions }> = []
  watches: WatchCall[] = []
  cancels: string[] = []
  submitError: GatewayError | null = null
  streams: Array<StreamEvent[]> = []
  cancelTask: GatewayTask | null = null

  submit(
    request: GatewaySubmitRequest,
    options?: GatewayRequestOptions,
  ): Promise<GatewaySubmitResponse> {
    this.submits.push({ request, options })
    if (this.submitError) {
      return Promise.reject(this.submitError)
    }
    const attempt = this.submits.length
    return Promise.resolve({
      task_id: `exec-${attempt}`,
      worker: request.worker ?? "general-a",
      worker_instance: "general-a",
      agent: request.agent,
      profile: "default:1",
      state: "accepted",
      replayed: false,
    })
  }

  watch(
    taskId: string,
    after = 0,
    options?: GatewayRequestOptions,
  ): Promise<ChatStream> {
    this.watches.push({ taskId, after, options })
    const events = this.streams.shift() ?? []
    const holdOpen = events.length === 0
    return Promise.resolve(new FakeStream(events, options?.signal, holdOpen))
  }

  cancel(taskId: string): Promise<GatewayTask> {
    this.cancels.push(taskId)
    return Promise.resolve(
      this.cancelTask ?? {
        task_id: taskId,
        worker: "general-a",
        worker_instance: "general-a",
        connectivity: "online",
        task: {
          id: "worker-task",
          namespace: "local",
          caller: "operator",
          agent: "general",
          session_id: "chat-test",
          state: "cancelled",
        },
      },
    )
  }
}

function createHarness(streams: StreamEvent[][]) {
  const gateway = new FakeGateway()
  gateway.streams = [...streams]
  let keys = 0
  let ids = 0
  const session = createChatSession({
    client: gateway,
    sessionId: "chat-test",
    now: () => 1,
    createId: (prefix) => `${prefix}-${(ids += 1)}`,
    createIdempotencyKey: () => `key-${(keys += 1)}`,
    sleep: () => Promise.resolve(),
    random: () => 0,
  })
  return { gateway, session }
}

function terminalReady(session: ReturnType<typeof createHarness>["session"]) {
  return vi.waitFor(() => {
    const status = session.getState().status
    if (!["succeeded", "failed", "cancelled", "unknown", "uncertain"].includes(status)) {
      throw new Error(`still ${status}`)
    }
  })
}

describe("chat session submission", () => {
  it("submits the selected target with the session and idempotency key, then streams text", async () => {
    const { gateway, session } = createHarness([
      [
        event(1, "task.accepted", { state: "accepted" }),
        event(2, "model.delta", { text: "streamed " }, false),
        event(3, "model.delta.complete", { text: "streamed answer" }, false),
        event(4, "task.succeeded", {
          state: "succeeded",
          output: "streamed answer",
          usage: { model_calls: 1 },
        }),
      ],
    ])

    session.send("hello", selection)
    await terminalReady(session)

    expect(gateway.submits[0]?.request).toEqual({
      worker: "general-a",
      agent: "general",
      profile: "default",
      session_id: "chat-test",
      input: "hello",
      idempotency_key: "key-1",
    })
    expect(gateway.watches[0]).toMatchObject({ taskId: "exec-1", after: 0 })
    expect(gateway.watches[0]?.options?.signal).toBeInstanceOf(AbortSignal)

    const state = session.getState()
    expect(state.status).toBe("succeeded")
    expect(state.messages).toHaveLength(2)
    expect(state.lastDurableSeq).toBe(4)
    const assistant = state.messages[1]
    expect(assistant?.parts.filter((part) => part.type === "text")).toEqual([
      { type: "text", text: "streamed answer", author: undefined, streaming: false },
    ])
  })

  it("treats HTTP 202 unknown submission as uncertainty, not success or rejection", async () => {
    const { gateway, session } = createHarness([])
    gateway.submitError = new GatewayError({
      kind: "uncertain-submission",
      status: 202,
      code: "task.unknown_submission",
      message: "The Gateway could not confirm the submission.",
      recovery: "Inspect the task before sending it again.",
      retryable: false,
      details: { execution_id: "exec-unknown", worker: "general-a" },
    })

    session.send("hello", selection)
    await terminalReady(session)

    const state = session.getState()
    expect(state.status).toBe("uncertain")
    expect(gateway.watches).toEqual([])
    const parts = state.messages[1]?.parts ?? []
    expect(parts).toContainEqual({
      type: "uncertainty",
      code: "task.unknown_submission",
      message: "The Gateway could not confirm the submission.",
      recovery: "Inspect the task before sending it again.",
      details: { execution_id: "exec-unknown", worker: "general-a" },
    })
    expect(parts.some((part) => part.type === "error")).toBe(false)
  })

  it("records a rejected submission as a failure", async () => {
    const { gateway, session } = createHarness([])
    gateway.submitError = new GatewayError({
      kind: "forbidden",
      status: 403,
      code: "scope.task_submit",
      message: "The caller token cannot submit tasks.",
      recovery: "Ask the operator for task:submit.",
      retryable: false,
    })

    session.send("hello", selection)
    await terminalReady(session)

    const state = session.getState()
    expect(state.status).toBe("failed")
    expect(state.messages[1]?.parts).toContainEqual({
      type: "error",
      code: "scope.task_submit",
      message: "The caller token cannot submit tasks.",
      recovery: "Ask the operator for task:submit.",
      retryable: false,
      kind: "submission",
    })
  })
})

describe("chat session stop and retry", () => {
  it("cancels the Gateway task and reports the projection truthfully", async () => {
    const { gateway, session } = createHarness([
      [
        event(1, "task.accepted", { state: "accepted" }),
        event(2, "task.running", { attempt: 1 }),
      ],
    ])

    session.send("hello", selection)
    await vi.waitFor(() => {
      expect(session.getState().status).toBe("streaming")
      expect(session.getState().activeTaskId).toBe("exec-1")
    })

    session.stop()
    await vi.waitFor(() => {
      expect(session.getState().status).toBe("cancelled")
    })

    expect(gateway.cancels).toEqual(["exec-1"])
    const task = session
      .getState()
      .messages[1]?.parts.find((part) => part.type === "task")
    expect(task).toMatchObject({ state: "cancelled" })
    expect(session.getState().activeTaskId).toBeNull()
  })

  it("retries with a new task and idempotency key without duplicating the user turn", async () => {
    const { gateway, session } = createHarness([
      [
        event(1, "task.accepted", { state: "accepted" }),
        event(2, "task.failed", {
          state: "failed",
          error: {
            code: "worker.crashed",
            message: "boom",
            retryable: true,
            recovery: "retry",
          },
        }),
      ],
      [
        event(3, "task.accepted", { state: "accepted" }),
        event(4, "task.succeeded", { state: "succeeded", output: "second try" }),
      ],
    ])

    session.send("hello", selection)
    await terminalReady(session)
    expect(session.getState().status).toBe("failed")

    session.retry("user-1")
    await vi.waitFor(() => {
      expect(session.getState().status).toBe("succeeded")
    })

    expect(gateway.submits).toHaveLength(2)
    expect(gateway.submits[0]?.request.idempotency_key).toBe("key-1")
    expect(gateway.submits[1]?.request.idempotency_key).toBe("key-2")
    expect(gateway.submits[1]?.request).toMatchObject({
      input: "hello",
      worker: "general-a",
      agent: "general",
      session_id: "chat-test",
    })
    const state = session.getState()
    expect(state.messages.filter((message) => message.role === "user")).toHaveLength(1)
    expect(state.messages).toHaveLength(2)
    expect(state.messages[1]?.taskId).toBe("exec-2")
  })

  it("does not retry an uncertain submission with a new task", () => {
    const { gateway, session } = createHarness([])
    gateway.submitError = new GatewayError({
      kind: "uncertain-submission",
      status: 202,
      code: "task.unknown_submission",
      message: "unknown",
      retryable: false,
    })

    session.send("hello", selection)
    session.retry("user-1")

    expect(gateway.submits).toHaveLength(1)
  })
})

describe("chat session reconnect and cleanup", () => {
  it("reconnects from the durable cursor when a stream closes without a terminal event", async () => {
    const { gateway, session } = createHarness([
      [
        event(1, "task.accepted", { state: "accepted" }),
        event(2, "model.delta", { text: "partial" }, false),
      ],
      [event(3, "task.succeeded", { state: "succeeded", output: "partial answer" })],
    ])

    session.send("hello", selection)
    await terminalReady(session)

    expect(gateway.watches).toHaveLength(2)
    expect(gateway.watches[1]).toMatchObject({ taskId: "exec-1", after: 1 })
    expect(session.getState().status).toBe("succeeded")
  })

  it("aborts the stream on dispose and never opens a second subscription", async () => {
    const { gateway, session } = createHarness([[]])

    session.send("hello", selection)
    await vi.waitFor(() => {
      expect(gateway.watches).toHaveLength(1)
    })
    const signal = gateway.watches[0]?.options?.signal
    expect(signal?.aborted).toBe(false)

    session.dispose()

    expect(signal?.aborted).toBe(true)
    expect(gateway.watches).toHaveLength(1)
  })

  it("starts a new chat with a new session id and no messages", async () => {
    const { session } = createHarness([
      [event(1, "task.accepted", { state: "accepted" }), event(2, "task.succeeded", { state: "succeeded" })],
    ])

    session.send("hello", selection)
    await terminalReady(session)
    const previous = session.getState().sessionId

    session.newChat()

    expect(session.getState().sessionId).not.toBe(previous)
    expect(session.getState().messages).toEqual([])
    expect(session.getState().status).toBe("idle")
  })
})

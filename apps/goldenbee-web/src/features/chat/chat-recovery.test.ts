import { IDBFactory } from "fake-indexeddb"
import { describe, expect, it, vi } from "vitest"

import { createChatSession } from "@/features/chat/chat-session"
import type { ChatGateway, ChatStream } from "@/features/chat/chat-runner"
import {
  createInitialChatState,
  type ChatMessage,
  type ChatState,
} from "@/features/chat/chat-types"
import {
  createChatHistory,
  stateFromStored,
  type StoredChat,
  type StoredMessage,
} from "@/features/chat/local-chat-history"
import {
  GatewayError,
  type GatewayRequestOptions,
  type GatewayStreamFrame,
  type GatewaySubmitRequest,
  type GatewaySubmitResponse,
  type GatewayTask,
  type StreamEvent,
  type StreamPage,
  type TaskState,
} from "@goldenbee/sdk"

const selection = {
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
    stream_id: "task:exec-1",
    seq,
    schema_version: 1,
    kind,
    durable,
    payload,
    at: seq,
  }
}

function projection(state: TaskState, extras: Partial<GatewayTask["task"]> = {}): GatewayTask {
  return {
    task_id: "exec-1",
    worker: "general-a",
    worker_instance: "general-a",
    connectivity: "online",
    task: {
      id: "worker-task-1",
      namespace: "local",
      caller: "operator",
      agent: "general",
      session_id: "chat-test",
      state,
      ...extras,
    },
  }
}

function page(events: StreamEvent[], nextSeq: number, oldestSeq = 1): StreamPage {
  return {
    stream_id: "task:exec-1",
    events,
    next_seq: nextSeq,
    oldest_seq: oldestSeq,
    cursor_expired: false,
  }
}

class HoldStream implements ChatStream {
  lastDurableSeq = 0
  private readonly events: StreamEvent[]
  private readonly signal: AbortSignal | undefined

  constructor(events: StreamEvent[], signal?: AbortSignal) {
    this.events = events
    this.signal = signal
  }

  async *[Symbol.asyncIterator](): AsyncIterator<GatewayStreamFrame> {
    for (const next of this.events) {
      yield { type: "event", event: next }
      if (next.durable) {
        this.lastDurableSeq = Math.max(this.lastDurableSeq, next.seq)
      }
    }
    if (this.events.length === 0 && this.signal) {
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

class RecoveryGateway implements ChatGateway {
  taskCalls: string[] = []
  watchCalls: Array<{ taskId: string; after: number }> = []
  eventCalls: Array<{ taskId: string; after: number }> = []
  submits: GatewaySubmitRequest[] = []
  cancels: string[] = []
  taskResults: GatewayTask[] = []
  taskError: GatewayError | null = null
  watchErrors: GatewayError[] = []
  streams: StreamEvent[][] = []
  pages: StreamPage[] = []

  task(taskId: string, _options?: GatewayRequestOptions): Promise<GatewayTask> {
    this.taskCalls.push(taskId)
    if (this.taskError) {
      return Promise.reject(this.taskError)
    }
    const result = this.taskResults.shift()
    return Promise.resolve(result ?? projection("running"))
  }

  events(
    taskId: string,
    after = 0,
    _limit?: number,
    _options?: GatewayRequestOptions,
  ): Promise<StreamPage> {
    this.eventCalls.push({ taskId, after })
    return Promise.resolve(this.pages.shift() ?? page([], after))
  }

  watch(
    taskId: string,
    after = 0,
    options?: GatewayRequestOptions,
  ): Promise<ChatStream> {
    this.watchCalls.push({ taskId, after })
    const error = this.watchErrors.shift()
    if (error) {
      return Promise.reject(error)
    }
    return Promise.resolve(new HoldStream(this.streams.shift() ?? [], options?.signal))
  }

  submit(request: GatewaySubmitRequest): Promise<GatewaySubmitResponse> {
    this.submits.push(request)
    return Promise.resolve({
      task_id: "exec-1",
      worker: request.worker ?? "general-a",
      worker_instance: "general-a",
      agent: request.agent,
      profile: "default:1",
      state: "accepted",
      replayed: false,
    })
  }

  cancel(taskId: string): Promise<GatewayTask> {
    this.cancels.push(taskId)
    return Promise.resolve(projection("cancelled"))
  }
}

function restoredState(lastDurableSeq = 5): ChatState {
  const user: ChatMessage = {
    id: "user-1",
    role: "user",
    createdAt: 1,
    parts: [{ type: "text", text: "hello", streaming: false }],
    selection,
  }
  const assistant: ChatMessage = {
    id: "assistant-1",
    role: "assistant",
    createdAt: 2,
    replyTo: "user-1",
    taskId: "exec-1",
    parts: [
      {
        type: "task",
        state: "running",
        attempt: 1,
        output: "partial",
      },
    ],
  }
  return {
    ...createInitialChatState("chat-test"),
    messages: [user, assistant],
    status: "streaming",
    activeTaskId: "exec-1",
    activeAssistantId: "assistant-1",
    lastSeq: lastDurableSeq,
    lastDurableSeq,
  }
}

function createSession(client: ChatGateway, state: ChatState, extra = {}) {
  return createChatSession({
    client,
    sessionId: state.sessionId,
    initialState: state,
    chatId: "chat-1",
    now: () => 10,
    createId: (() => {
      let next = 0
      return (prefix: string) => `${prefix}-${(next += 1)}`
    })(),
    createIdempotencyKey: (() => {
      let next = 0
      return () => `key-${(next += 1)}`
    })(),
    sleep: () => Promise.resolve(),
    random: () => 0,
    ...extra,
  })
}

describe("chat restore and reconciliation", () => {
  it("reconciles a non-terminal task with the Gateway and resumes at the durable cursor", async () => {
    const gateway = new RecoveryGateway()
    const session = createSession(gateway, restoredState(5))

    await session.recover()

    expect(gateway.taskCalls).toEqual(["exec-1"])
    expect(gateway.watchCalls).toEqual([{ taskId: "exec-1", after: 5 }])
    expect(session.getState().status).toBe("streaming")
    expect(session.getState().recovery).toMatchObject({
      kind: "reconnected",
      taskId: "exec-1",
    })
    session.dispose()
  })

  it("keeps input disabled while the saved task is being reconciled", async () => {
    const gateway = new RecoveryGateway()
    let resolveTask: ((task: GatewayTask) => void) | undefined
    gateway.task = () =>
      new Promise<GatewayTask>((resolve) => {
        resolveTask = resolve
      })
    const session = createSession(gateway, restoredState(3))

    const recovery = session.recover()
    expect(session.getState().status).toBe("recovering")
    expect(session.getState().recovery?.kind).toBe("reconciling")

    resolveTask?.(projection("running"))
    await recovery
    expect(session.getState().status).toBe("streaming")
    session.dispose()
  })

  it("applies a terminal Gateway projection without opening a stream", async () => {
    const gateway = new RecoveryGateway()
    gateway.taskResults = [
      projection("succeeded", { output: "final answer", usage: { model_calls: 2 } }),
    ]
    const session = createSession(gateway, restoredState(8))

    await session.recover()

    const state = session.getState()
    expect(state.status).toBe("succeeded")
    expect(state.activeTaskId).toBeNull()
    expect(state.recovery?.kind).toBe("reconnected")
    expect(state.lastDurableSeq).toBe(8)
    expect(gateway.watchCalls).toEqual([])
    expect(
      state.messages[1]?.parts.filter((part) => part.type === "text"),
    ).toEqual([
      { type: "text", text: "final answer", author: undefined, streaming: false },
    ])
  })

  it("marks the local chat detached when the task is gone, without deleting it", async () => {
    const gateway = new RecoveryGateway()
    gateway.taskError = new GatewayError({
      kind: "not-found",
      status: 404,
      code: "route.task.not_found",
      message: "The task is not visible.",
      recovery: "Start a new task.",
      retryable: false,
    })
    const session = createSession(gateway, restoredState(2))

    await session.recover()

    const state = session.getState()
    expect(state.status).toBe("failed")
    expect(state.recovery).toMatchObject({ kind: "detached", taskId: "exec-1" })
    expect(state.messages).toHaveLength(2)
    expect(gateway.watchCalls).toEqual([])
    session.dispose()
  })

  it("fails closed on an interrupted submission that has no task identity", async () => {
    const gateway = new RecoveryGateway()
    const state: ChatState = {
      ...createInitialChatState("chat-test"),
      messages: [
        {
          id: "user-1",
          role: "user",
          createdAt: 1,
          parts: [{ type: "text", text: "uncertain", streaming: false }],
          selection,
        },
        {
          id: "assistant-1",
          role: "assistant",
          createdAt: 2,
          replyTo: "user-1",
          parts: [],
        },
      ],
      status: "submitting",
      activeAssistantId: "assistant-1",
    }
    const session = createSession(gateway, state)

    await session.recover()

    const recovered = session.getState()
    expect(recovered.status).toBe("uncertain")
    expect(gateway.taskCalls).toEqual([])
    expect(gateway.watchCalls).toEqual([])
    expect(recovered.messages[1]?.parts).toContainEqual(
      expect.objectContaining({
        type: "uncertainty",
        code: "task.reload_interrupted",
      }),
    )
    session.dispose()
  })
})

describe("cursor expiry recovery", () => {
  it("shows a gap, reloads the canonical task and retained events, and resumes at next_seq", async () => {
    const gateway = new RecoveryGateway()
    gateway.taskResults = [projection("running")]
    gateway.watchErrors = [
      new GatewayError({
        kind: "cursor-expired",
        status: 400,
        code: "stream.cursor_expired",
        message: "The cursor is older than retained events.",
        retryable: false,
      }),
    ]
    gateway.pages = [
      page(
        [
          event(1, "task.accepted", { state: "accepted" }),
          event(4, "tool.call", { id: "tool-1", name: "exec" }),
        ],
        4,
        2,
      ),
    ]
    const session = createSession(gateway, restoredState(2))

    await session.recover()

    await vi.waitFor(() => {
      expect(gateway.watchCalls).toHaveLength(2)
    })
    expect(gateway.eventCalls).toEqual([{ taskId: "exec-1", after: 0 }])
    expect(gateway.watchCalls[1]).toEqual({ taskId: "exec-1", after: 4 })
    expect(session.getState().gap).toMatchObject({
      reason: "cursor_expired",
      fromSeq: 2,
    })
    expect(session.getState().status).toBe("streaming")
    session.dispose()
  })
})

describe("local chat session persistence", () => {
  it("persists the durable cursor but never the ephemeral delta sequence", async () => {
    const gateway = new RecoveryGateway()
    gateway.streams = [
      [
        event(1, "task.accepted", { state: "accepted" }),
        event(2, "model.delta", { text: "draft" }, false),
        event(3, "task.succeeded", { state: "succeeded", output: "draft" }),
      ],
    ]
    const history = createChatHistory({
      factory: new IDBFactory(),
      databaseName: "session-cursor",
    })
    const state = createInitialChatState("chat-test")
    const session = createChatSession({
      client: gateway,
      history,
      chatId: "chat-1",
      initialState: state,
      sessionId: "chat-test",
      sleep: () => Promise.resolve(),
      random: () => 0,
    })

    session.send("hello", selection)
    await vi.waitFor(async () => {
      expect(session.getState().status).toBe("succeeded")
      const restored = await history.loadLastChat()
      expect(restored?.chat.lastDurableSeq).toBe(3)
    })

    const restored = await history.loadLastChat()
    expect(restored?.state.lastDurableSeq).toBe(3)
    expect(restored?.state.lastSeq).toBe(3)
    expect(restored?.activeTask).toBeNull()
    session.dispose()
    await history.close()
  })

  it("starts a new local chat and session id while keeping the previous chat", async () => {
    const gateway = new RecoveryGateway()
    const history = createChatHistory({
      factory: new IDBFactory(),
      databaseName: "session-new-chat",
    })
    const session = createChatSession({
      client: gateway,
      history,
      chatId: "chat-1",
      sessionId: "chat-test",
      sleep: () => Promise.resolve(),
      random: () => 0,
    })

    await vi.waitFor(async () => {
      expect(await history.listChats()).toHaveLength(1)
    })
    const firstChatId = session.getChatId()
    const firstSessionId = session.getState().sessionId

    session.newChat()

    expect(session.getChatId()).not.toBe(firstChatId)
    expect(session.getState().sessionId).not.toBe(firstSessionId)
    await vi.waitFor(async () => {
      const chats = await history.listChats()
      expect(chats).toHaveLength(2)
      expect(chats.map((chat) => chat.id)).toContain(firstChatId)
    })
    session.dispose()
    await history.close()
  })
})

describe("restored state conversion", () => {
  it("never carries an ephemeral cursor across a reload", () => {
    const chat: StoredChat = {
      schemaVersion: 1,
      id: "chat-1",
      sessionId: "chat-test",
      title: "hello",
      createdAt: 1,
      updatedAt: 2,
      status: "streaming",
      activeTaskId: "exec-1",
      activeAssistantId: "assistant-1",
      gap: null,
      recovery: null,
      selection,
      lastDurableSeq: 4,
    }
    const messages: StoredMessage[] = [
      {
        schemaVersion: 1,
        chatId: "chat-1",
        position: 0,
        id: "user-1",
        role: "user",
        createdAt: 1,
        parts: [{ type: "text", text: "hello", streaming: false }],
      },
    ]

    const state = stateFromStored(chat, messages)

    expect(state.lastDurableSeq).toBe(4)
    expect(state.lastSeq).toBe(4)
  })
})

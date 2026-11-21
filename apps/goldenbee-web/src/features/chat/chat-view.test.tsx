import { IDBFactory } from "fake-indexeddb"
import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, describe, expect, it } from "vitest"

import { App } from "@/App"
import { ChatView } from "@/features/chat/chat-view"
import type { ChatGateway, ChatStream } from "@/features/chat/chat-runner"
import {
  createInitialChatState,
  type ChatState,
} from "@/features/chat/chat-types"
import {
  createChatHistory,
  createChatSnapshot,
  LocalHistoryError,
  type ChatHistoryStore,
} from "@/features/chat/local-chat-history"
import {
  createConnectionStore,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import { createTokenStore } from "@/features/connection/token-store"
import {
  GatewayError,
  type GatewayRequestOptions,
  type GatewayStreamFrame,
  type GatewaySubmitRequest,
  type GatewaySubmitResponse,
  type GatewayTask,
  type StreamEvent,
  type StreamPage,
  type WorkerSummary,
} from "@goldenbee/sdk"

function memoryStorage(): Storage {
  const values = new Map<string, string>()
  return {
    get length() {
      return values.size
    },
    clear: () => values.clear(),
    getItem: (key) => values.get(key) ?? null,
    key: (index) => Array.from(values.keys())[index] ?? null,
    removeItem: (key) => {
      values.delete(key)
    },
    setItem: (key, value) => {
      values.set(key, value)
    },
  }
}

const worker: WorkerSummary = {
  id: "general-a",
  instance: "general-a",
  package: "general",
  version: "0.1.0",
  status: "approved",
  reachable: true,
  locality: "local",
  capabilities: [
    { id: "general", kind: "agent", description: "General agent" },
  ],
}

function connectedStore(): ConnectionStore {
  const store = createConnectionStore({
    tokenStore: createTokenStore({ sessionStorage: memoryStorage() }),
    defaultUrl: "http://gateway.test",
    createClient: () => ({
      checkConnection: async () => ({
        status: "connected",
        health: { status: "ok", workers: 1, reachable: 1 },
        workers: [worker],
      }),
    }),
  })
  store.setState({
    status: "connected",
    gatewayUrl: "http://gateway.test",
    token: "caller-token",
    health: { status: "ok", workers: 1, reachable: 1 },
    workers: [worker],
  })
  return store
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

class TestStream implements ChatStream {
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

class ChatGatewayDouble implements ChatGateway {
  submits: GatewaySubmitRequest[] = []
  watches: Array<{ taskId: string; after: number }> = []
  taskCalls: string[] = []
  cancels: string[] = []
  streams: StreamEvent[][] = []
  submitError: GatewayError | null = null
  cancelTask: GatewayTask | null = null

  submit(
    request: GatewaySubmitRequest,
    _options?: GatewayRequestOptions,
  ): Promise<GatewaySubmitResponse> {
    this.submits.push(request)
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
    this.watches.push({ taskId, after })
    const events = this.streams.shift() ?? []
    return Promise.resolve(new TestStream(events, options?.signal))
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

  task(taskId: string): Promise<GatewayTask> {
    this.taskCalls.push(taskId)
    return Promise.resolve({
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
        state: "running",
      },
    })
  }

  events(taskId: string, after = 0): Promise<StreamPage> {
    return Promise.resolve({
      stream_id: `task:${taskId}`,
      events: [],
      next_seq: after,
      oldest_seq: 1,
      cursor_expired: false,
    })
  }
}

async function sendPrompt(user: ReturnType<typeof userEvent.setup>, text: string) {
  await user.type(screen.getByLabelText("Message"), text)
  await user.click(screen.getByRole("button", { name: "Send message" }))
}

afterEach(() => {
  window.location.hash = ""
})

describe("chat view", () => {
  it("submits the selected target and streams the answer into the reply", async () => {
    const gateway = new ChatGatewayDouble()
    gateway.streams = [
      [
        event(1, "task.accepted", { state: "accepted" }),
        event(2, "model.delta", { text: "Hello " }, false),
        event(3, "model.delta", { text: "world" }, false),
        event(4, "task.succeeded", {
          state: "succeeded",
          output: "Hello world",
        }),
      ],
    ]
    const user = userEvent.setup()
    render(<ChatView store={connectedStore()} client={gateway} />)

    await sendPrompt(user, "say hello")

    expect(await screen.findByText("Hello world")).toBeInTheDocument()
    expect(screen.getByText("Task succeeded")).toBeInTheDocument()
    expect(gateway.submits[0]).toMatchObject({
      worker: "general-a",
      agent: "general",
      profile: "default",
      input: "say hello",
      session_id: expect.stringMatching(/^chat-/),
      idempotency_key: expect.stringMatching(/^chat-/),
    })
    expect(gateway.watches[0]).toMatchObject({ taskId: "exec-1", after: 0 })
  })

  it("shows failure guidance and retries without duplicating the user turn", async () => {
    const gateway = new ChatGatewayDouble()
    gateway.streams = [
      [
        event(1, "task.accepted", { state: "accepted" }),
        event(2, "task.failed", {
          state: "failed",
          error: {
            code: "worker.crashed",
            message: "The Worker stopped before answering.",
            retryable: true,
            recovery: "Retry the task.",
          },
        }),
      ],
      [
        event(3, "task.accepted", { state: "accepted" }),
        event(4, "task.succeeded", { state: "succeeded", output: "second try" }),
      ],
    ]
    const user = userEvent.setup()
    render(<ChatView store={connectedStore()} client={gateway} />)

    await sendPrompt(user, "retry me")

    expect(await screen.findByText("Task failed")).toBeInTheDocument()
    expect(
      screen.getByText(/The Worker stopped before answering\./),
    ).toBeInTheDocument()

    await user.click(screen.getByRole("button", { name: "Retry" }))

    expect(await screen.findByText("Task succeeded")).toBeInTheDocument()
    expect(screen.getAllByText("retry me")).toHaveLength(1)
    expect(gateway.submits).toHaveLength(2)
    expect(gateway.submits[0]?.idempotency_key).not.toBe(
      gateway.submits[1]?.idempotency_key,
    )
    expect(gateway.submits[1]?.input).toBe("retry me")
  })

  it("shows a 202 unknown submission as uncertainty with no retry action", async () => {
    const gateway = new ChatGatewayDouble()
    gateway.submitError = new GatewayError({
      kind: "uncertain-submission",
      status: 202,
      code: "task.unknown_submission",
      message: "The Gateway could not confirm whether the task was accepted.",
      recovery: "Inspect the task before sending it again.",
      retryable: false,
      details: { execution_id: "exec-unknown", worker: "general-a" },
    })
    const user = userEvent.setup()
    render(<ChatView store={connectedStore()} client={gateway} />)

    await sendPrompt(user, "did it start?")

    expect(await screen.findByText("Submission uncertain")).toBeInTheDocument()
    expect(
      screen.getByText("Inspect the task before sending it again."),
    ).toBeInTheDocument()
    expect(screen.queryByText("Task failed")).not.toBeInTheDocument()
    expect(screen.queryByRole("button", { name: "Retry" })).not.toBeInTheDocument()
    expect(gateway.watches).toEqual([])
  })

  it("stops an active task through the Gateway cancellation route", async () => {
    const gateway = new ChatGatewayDouble()
    gateway.streams = [[]]
    const user = userEvent.setup()
    render(<ChatView store={connectedStore()} client={gateway} />)

    await sendPrompt(user, "long task")
    await waitFor(() => {
      expect(gateway.watches).toHaveLength(1)
    })

    await user.click(screen.getByRole("button", { name: "Stop task" }))

    expect(await screen.findByText("Task cancelled")).toBeInTheDocument()
    expect(gateway.cancels).toEqual(["exec-1"])
  })

  it("restores the last chat, reconciles the active task, then enables input", async () => {
    const history = createChatHistory({
      factory: new IDBFactory(),
      databaseName: "view-restore",
    })
    const state: ChatState = {
      ...createInitialChatState("chat-restored"),
      messages: [
        {
          id: "user-1",
          role: "user",
          createdAt: 1,
          parts: [{ type: "text", text: "restore me", streaming: false }],
          selection: { worker: "general-a", agent: "general", profile: "default" },
        },
        {
          id: "assistant-1",
          role: "assistant",
          createdAt: 2,
          replyTo: "user-1",
          taskId: "exec-restored",
          parts: [
            { type: "task", state: "running", attempt: 1 },
            { type: "text", text: "restored answer", streaming: false },
          ],
        },
      ],
      status: "streaming",
      activeTaskId: "exec-restored",
      activeAssistantId: "assistant-1",
      lastSeq: 9,
      lastDurableSeq: 6,
    }
    await history.saveChat(
      createChatSnapshot({
        chatId: "chat-restored",
        state,
        selection: { worker: "general-a", agent: "general", profile: "default" },
      }),
    )
    const gateway = new ChatGatewayDouble()
    gateway.streams = [
      [
        event(7, "task.succeeded", {
          state: "succeeded",
          output: "restored answer",
        }),
      ],
    ]
    render(
      <ChatView store={connectedStore()} history={history} client={gateway} />,
    )

    expect(await screen.findByText("restored answer")).toBeInTheDocument()
    expect(await screen.findByText("restore me")).toBeInTheDocument()
    await waitFor(() => {
      expect(gateway.taskCalls).toEqual(["exec-restored"])
      expect(gateway.watches[0]).toMatchObject({
        taskId: "exec-restored",
        after: 6,
      })
    })
    await waitFor(() => {
      expect(screen.getByLabelText("Message")).not.toBeDisabled()
    })
    await history.close()
  })

  it("fails closed on corrupt history and recovers through reset local history", async () => {
    let reset = false
    const history: ChatHistoryStore = {
      loadLastChat: async () => {
        throw new LocalHistoryError(
          "corrupt",
          "The local chat database contains an unreadable record.",
        )
      },
      reset: async () => {
        reset = true
      },
      saveChat: async () => undefined,
      saveSelection: async () => undefined,
      loadChat: async () => null,
      loadSelection: async () => null,
      listChats: async () => [],
      close: () => undefined,
    }
    const user = userEvent.setup()
    render(<ChatView store={connectedStore()} history={history} />)

    expect(
      await screen.findByText("Local chat history could not be read"),
    ).toBeInTheDocument()
    expect(screen.getByLabelText("Message")).toBeDisabled()

    await user.click(screen.getByRole("button", { name: "Reset local history" }))

    expect(reset).toBe(true)
    await waitFor(() => {
      expect(screen.getByLabelText("Message")).not.toBeDisabled()
    })
    expect(screen.getByText("Start a task")).toBeInTheDocument()
  })
})

describe("app shell chat navigation", () => {
  it("routes to chat from the primary navigation", async () => {
    window.location.hash = "#/overview"
    const store = connectedStore()
    const user = userEvent.setup()
    render(<App store={store} />)

    expect(screen.getByText("Gateway connected")).toBeInTheDocument()

    await user.click(screen.getByRole("link", { name: "Chat" }))

    expect(await screen.findByRole("heading", { name: "Chat" })).toBeInTheDocument()
    expect(screen.getByLabelText("Message")).toBeInTheDocument()
    expect(screen.getByText("Start a task")).toBeInTheDocument()
  })
})

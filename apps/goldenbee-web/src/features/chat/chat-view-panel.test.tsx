import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { render, waitFor } from "@testing-library/react"
import { describe, expect, it } from "vitest"
import type { ReactNode } from "react"

import { ChatView } from "@/features/chat/chat-view"
import type { ChatGateway, ChatStream } from "@/features/chat/chat-runner"
import { createChatSelectionStore } from "@/features/chat/chat-selection-store"
import { createInitialChatState } from "@/features/chat/chat-types"
import {
  createConnectionStore,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import { createTokenStore } from "@/features/connection/token-store"
import { MemoryChatHistory } from "@/test/memory-chat-history"
import type {
  GatewayRequestOptions,
  GatewayStreamFrame,
  GatewaySubmitRequest,
  GatewaySubmitResponse,
  GatewayTask,
  StreamEvent,
  StreamPage,
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

function connectedStore(): ConnectionStore {
  const store = createConnectionStore({
    tokenStore: createTokenStore({ sessionStorage: memoryStorage() }),
    defaultUrl: "http://gateway.test",
    createClient: () => ({
      checkConnection: async () => ({
        status: "connected",
        health: { status: "ok", workers: 1, reachable: 1 },
        workers: [],
      }),
    }),
  })
  store.setState({
    status: "connected",
    gatewayUrl: "http://gateway.test",
    token: "caller-token",
    health: { status: "ok", workers: 1, reachable: 1 },
    workers: [],
  })
  return store
}

function succeeded(seq: number): StreamEvent {
  return {
    event_id: `evt-${seq}`,
    source: "worker",
    incarnation: "inc-1",
    stream_id: "task:exec",
    seq,
    schema_version: 1,
    kind: "task.succeeded",
    durable: true,
    payload: { state: "succeeded", output: "panel answer" },
    at: seq,
  }
}

class OneEventStream implements ChatStream {
  lastDurableSeq = 0
  private readonly event: StreamEvent

  constructor(event: StreamEvent) {
    this.event = event
  }

  async *[Symbol.asyncIterator](): AsyncIterator<GatewayStreamFrame> {
    yield { type: "event", event: this.event }
    this.lastDurableSeq = this.event.seq
  }
}

class StreamGateway implements ChatGateway {
  taskCalls: string[] = []
  watches: Array<{ taskId: string; after: number }> = []

  submit(
    _request: GatewaySubmitRequest,
    _options?: GatewayRequestOptions,
  ): Promise<GatewaySubmitResponse> {
    return Promise.reject(new Error("submit is not used"))
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
        session_id: "session-panel",
        state: "running",
      },
    })
  }

  events(
    _taskId: string,
    after = 0,
  ): Promise<StreamPage> {
    return Promise.resolve({
      stream_id: "task:exec",
      events: [],
      next_seq: after,
      oldest_seq: 1,
      cursor_expired: false,
    })
  }

  watch(
    taskId: string,
    after = 0,
    _options?: GatewayRequestOptions,
  ): Promise<ChatStream> {
    this.watches.push({ taskId, after })
    return Promise.resolve(new OneEventStream(succeeded(5)))
  }

  cancel(taskId: string): Promise<GatewayTask> {
    return this.task(taskId)
  }
}

describe("panel stream lifecycle", () => {
  it("does not open a second stream when only the focused flag changes", async () => {
    const history = new MemoryChatHistory()
    history.seed("chat-panel", "Panel task", {
      ...createInitialChatState("session-panel"),
      messages: [
        {
          id: "assistant-1",
          role: "assistant",
          createdAt: 2,
          taskId: "exec-panel",
          parts: [{ type: "task", state: "running", attempt: 1 }],
        },
      ],
      status: "streaming",
      activeTaskId: "exec-panel",
      activeAssistantId: "assistant-1",
      lastDurableSeq: 4,
    })
    const gateway = new StreamGateway()
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    })
    function wrapper({ children }: { children: ReactNode }) {
      return (
        <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
      )
    }
    function view(focused: boolean) {
      return (
        <ChatView
          store={connectedStore()}
          history={history}
          chatId="chat-panel"
          title="Panel task"
          client={gateway}
          profileClient={null}
          selectionStore={createChatSelectionStore(memoryStorage())}
          focused={focused}
          panelLabel="Session Panel task"
          onFocus={() => undefined}
        />
      )
    }

    const rendered = render(view(false), { wrapper })

    await waitFor(() => {
      expect(gateway.taskCalls).toEqual(["exec-panel"])
      expect(gateway.watches).toEqual([{ taskId: "exec-panel", after: 4 }])
    })

    rendered.rerender(view(true))
    rendered.rerender(view(true))

    await waitFor(() => {
      expect(gateway.taskCalls).toHaveLength(1)
      expect(gateway.watches).toHaveLength(1)
    })
  })
})

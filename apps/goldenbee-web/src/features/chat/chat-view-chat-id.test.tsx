import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { render, screen } from "@testing-library/react"
import { describe, expect, it } from "vitest"
import type { ReactNode } from "react"

import { ChatView } from "@/features/chat/chat-view"
import { createInitialChatState, type ChatState } from "@/features/chat/chat-types"
import {
  createConnectionStore,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import { createTokenStore } from "@/features/connection/token-store"
import { MemoryChatHistory } from "@/test/memory-chat-history"
import type { WorkerSummary } from "@goldenbee/sdk"

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
  capabilities: [{ id: "general", kind: "agent", description: "General" }],
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

function answerState(sessionId: string, text: string): ChatState {
  return {
    ...createInitialChatState(sessionId),
    messages: [
      {
        id: `${sessionId}-assistant`,
        role: "assistant",
        createdAt: 1,
        parts: [{ type: "text", text, streaming: false }],
      },
    ],
  }
}

describe("controlled chat id", () => {
  it("loads the requested local chat instead of the last saved chat", async () => {
    const history = new MemoryChatHistory()
    history.seed("chat-beta", "Beta task", answerState("session-beta", "Beta answer"))
    history.seed("chat-alpha", "Alpha task", answerState("session-alpha", "Alpha answer"))

    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    })
    function wrapper({ children }: { children: ReactNode }) {
      return (
        <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
      )
    }

    render(
      <ChatView
        store={connectedStore()}
        history={history}
        chatId="chat-beta"
        client={null}
        profileClient={null}
      />,
      { wrapper },
    )

    expect(await screen.findByText("Beta answer")).toBeInTheDocument()
    expect(screen.queryByText("Alpha answer")).not.toBeInTheDocument()
    expect(history.loadChatCalls).toEqual(["chat-beta"])
    expect(history.loadLastCalls).toBe(0)
  })
})

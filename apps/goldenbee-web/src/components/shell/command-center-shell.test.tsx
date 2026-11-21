import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, describe, expect, it, vi } from "vitest"
import type { ReactNode } from "react"

import { App } from "@/App"
import {
  createInitialChatState,
  type ChatState,
} from "@/features/chat/chat-types"
import {
  createConnectionStore,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import { createTokenStore } from "@/features/connection/token-store"
import {
  clientSettingsStorageKey,
  createClientSettingsStore,
} from "@/features/workspace/client-settings"
import {
  createGatewayProfilesStore,
  gatewayProfilesStorageKey,
} from "@/features/workspace/gateway-profiles"
import {
  createWorkspaceStore,
  workspaceStorageKey,
} from "@/features/workspace/workspace-store"
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
  id: "worker-1",
  instance: "worker-a",
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

interface RenderOptions {
  history?: MemoryChatHistory
  workspace?: ReturnType<typeof createWorkspaceStore>
  route?: string
}

function renderApp(options: RenderOptions = {}) {
  const storage = memoryStorage()
  const history =
    options.history ??
    (() => {
      const seeded = new MemoryChatHistory()
      seeded.seed(
        "chat-alpha",
        "Alpha task",
        answerState("session-alpha", "Alpha answer"),
      )
      return seeded
    })()
  let counter = 0
  const workspace =
    options.workspace ??
    createWorkspaceStore({
      storage,
      createId: (prefix) => `${prefix}-${++counter}`,
    })
  const preferences = createClientSettingsStore({ storage })
  const profiles = createGatewayProfilesStore({
    storage,
    createId: (prefix) => `${prefix}-${++counter}`,
  })
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  function wrapper({ children }: { children: ReactNode }) {
    return (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    )
  }
  window.location.hash = options.route ?? "#/chat"
  const user = userEvent.setup()
  const rendered = render(
    <App
      store={connectedStore()}
      history={history}
      workspace={workspace}
      preferences={preferences}
      profiles={profiles}
    />,
    { wrapper },
  )
  return {
    history,
    workspace,
    preferences,
    profiles,
    user,
    storage,
    unmount: rendered.unmount,
  }
}

async function waitForChat(): Promise<void> {
  await waitFor(() => {
    expect(screen.getAllByLabelText("Message").length).toBeGreaterThan(0)
  })
}

afterEach(() => {
  vi.unstubAllGlobals()
  window.location.hash = ""
  window.localStorage.clear()
  Object.defineProperty(window, "innerWidth", {
    configurable: true,
    value: 1024,
  })
  window.dispatchEvent(new Event("resize"))
})

describe("command center shell", () => {
  it("creates, renames, and closes a group by backgrounding its chats", async () => {
    const { user, workspace } = renderApp()
    await waitForChat()
    expect(await screen.findByText("Alpha answer")).toBeInTheDocument()

    await user.click(
      screen.getByRole("button", { name: "New session group" }),
    )
    let dialog = await screen.findByRole("dialog")
    const name = within(dialog).getByLabelText("Group name")
    await user.clear(name)
    await user.type(name, "Research")
    await user.click(within(dialog).getByRole("button", { name: "Create group" }))

    expect(
      await screen.findByRole("tab", { name: /Research/ }),
    ).toBeInTheDocument()
    await waitFor(() => {
      expect(screen.getAllByLabelText("Message")).toHaveLength(1)
    })

    await user.click(screen.getByRole("button", { name: "Rename group" }))
    dialog = await screen.findByRole("dialog")
    const rename = within(dialog).getByLabelText("Group name")
    await user.clear(rename)
    await user.type(rename, "Research notes")
    await user.click(within(dialog).getByRole("button", { name: "Save name" }))
    expect(
      await screen.findByRole("tab", { name: /Research notes/ }),
    ).toBeInTheDocument()

    await user.click(screen.getByRole("button", { name: "Close group" }))
    dialog = await screen.findByRole("dialog")
    await user.click(
      within(dialog).getByRole("button", { name: "Close group" }),
    )

    expect(
      await screen.findByText(/1 session moved to background/i),
    ).toBeInTheDocument()
    expect(
      screen.queryByRole("tab", { name: /Research notes/ }),
    ).not.toBeInTheDocument()
    expect(workspace.getState().backgroundChatIds).toHaveLength(1)
    expect(
      await screen.findByRole("button", { name: /^Restore / }),
    ).toBeInTheDocument()
  })

  it("backgrounds the active chat, keeps it in the sidebar, and restores it", async () => {
    const { user, history, workspace } = renderApp()
    await waitForChat()
    expect(await screen.findByText("Alpha answer")).toBeInTheDocument()

    await user.click(
      screen.getByRole("button", { name: "Background session" }),
    )

    expect(
      screen.getByText("No visible session in this group"),
    ).toBeInTheDocument()
    expect(workspace.getState().backgroundChatIds).toEqual(["chat-alpha"])
    const restore = await screen.findByRole("button", {
      name: "Restore Alpha task",
    })

    await user.click(restore)

    expect(await screen.findByText("Alpha answer")).toBeInTheDocument()
    expect(screen.getAllByLabelText("Message")).toHaveLength(1)
    expect(workspace.getState().activeChatId).toBe("chat-alpha")
    expect(workspace.getState().backgroundChatIds).toEqual([])
    // One panel read plus one inspector evidence read per mount.
    expect(
      history.loadChatCalls.filter((chatId) => chatId === "chat-alpha").length,
    ).toBeGreaterThanOrEqual(3)
    // One read selected the last chat; the controlled panels load by id.
    expect(history.loadLastCalls).toBe(1)
  })

  it("switches groups without mounting a second chat view or losing a chat", async () => {
    const history = new MemoryChatHistory()
    history.seed("chat-alpha", "Alpha task", answerState("session-a", "Alpha answer"))
    history.seed("chat-beta", "Beta task", answerState("session-b", "Beta answer"))
    const storage = memoryStorage()
    let counter = 0
    const workspace = createWorkspaceStore({
      storage,
      createId: (prefix) => `${prefix}-${++counter}`,
    })
    workspace.getState().addChat("chat-alpha")
    const first = workspace.getState().activeGroupId
    const second = workspace.getState().createGroup("Second")
    workspace.getState().addChat("chat-beta")
    workspace.getState().selectGroup(first)

    const { user } = renderApp({ history, workspace })
    await waitForChat()
    expect(await screen.findByText("Alpha answer")).toBeInTheDocument()

    await user.click(screen.getByRole("tab", { name: /Second/ }))

    expect(screen.getByRole("tab", { name: /Second/ })).toHaveAttribute(
      "aria-current",
      "page",
    )
    expect(await screen.findByText("Beta answer")).toBeInTheDocument()
    expect(screen.queryByText("Alpha answer")).not.toBeInTheDocument()
    expect(screen.getAllByLabelText("Message")).toHaveLength(1)

    await user.click(screen.getByRole("tab", { name: /Sessions/ }))

    expect(await screen.findByText("Alpha answer")).toBeInTheDocument()
    // The focused panel and the inspector each read the chat once per visit.
    expect(history.loadChatCalls.filter((chatId) => chatId === "chat-alpha"))
      .toHaveLength(4)
    expect(history.loadChatCalls.filter((chatId) => chatId === "chat-beta"))
      .toHaveLength(2)
    expect(second).not.toBe(first)
  })

  it("opens the command palette, searches local entries, and navigates", async () => {
    const { user, workspace } = renderApp()
    await waitForChat()

    await user.keyboard("{Control>}k{/Control}")
    let dialog = await screen.findByRole("dialog", { name: "Command palette" })
    const input = within(dialog).getByRole("combobox", { name: "Command" })
    await waitFor(() => expect(input).toHaveFocus())

    await user.type(input, "overview")
    const option = await within(dialog).findByRole("option", {
      name: /Open Gateway overview/,
    })
    expect(option).toHaveAttribute("aria-selected", "true")
    await user.keyboard("{ArrowDown}{Enter}")

    expect(
      await screen.findByRole("heading", { name: "Gateway connected" }),
    ).toBeInTheDocument()

    fireEvent.keyDown(window, { key: "Enter", ctrlKey: true })
    await waitFor(() => expect(window.location.hash).toBe("#/chat"))
    expect(workspace.getState().activeChatId).not.toBeNull()
    await waitForChat()

    await user.keyboard("{Control>}k{/Control}")
    dialog = await screen.findByRole("dialog", { name: "Command palette" })
    await user.type(
      within(dialog).getByRole("combobox", { name: "Command" }),
      "audit",
    )
    expect(
      await within(dialog).findByRole("option", { name: /Open audit/ }),
    ).toBeInTheDocument()
    await user.keyboard("{Escape}")
    await waitFor(() => {
      expect(
        screen.queryByRole("dialog", { name: "Command palette" }),
      ).not.toBeInTheDocument()
    })

    const trigger = screen.getByRole("button", {
      name: "Open command palette",
    })
    await user.click(trigger)
    await screen.findByRole("dialog", { name: "Command palette" })
    await user.keyboard("{Escape}")
    await waitFor(() => expect(trigger).toHaveFocus())
  })

  it("separates local Client settings from read-only Gateway and Worker facts", async () => {
    const { user, preferences, storage } = renderApp()
    await waitForChat()

    await user.click(screen.getByRole("button", { name: "Client settings" }))
    const dialog = await screen.findByRole("dialog")
    expect(
      within(dialog).getByRole("tab", { name: "Client" }),
    ).toHaveAttribute("aria-selected", "true")

    await user.selectOptions(
      within(dialog).getByLabelText("Interface density"),
      "compact",
    )
    expect(preferences.getState().density).toBe("compact")
    expect(document.querySelector('[data-density="compact"]')).not.toBeNull()
    expect(storage.getItem(clientSettingsStorageKey) ?? "").not.toContain(
      "caller-token",
    )

    await user.click(within(dialog).getByRole("tab", { name: "Connection" }))
    expect(within(dialog).getByText("Local connection")).toBeInTheDocument()
    expect(within(dialog).getByLabelText("Gateway URL")).toHaveValue(
      "http://gateway.test",
    )
    expect(within(dialog).getByLabelText("Caller token")).toHaveAttribute(
      "type",
      "password",
    )
    expect(document.body.textContent ?? "").not.toContain("caller-token")
    expect(
      within(dialog).getByRole("button", { name: "Reconnect" }),
    ).toBeInTheDocument()

    await user.click(within(dialog).getByRole("tab", { name: "Gateway" }))
    expect(within(dialog).getByText("Read-only")).toBeInTheDocument()
    expect(
      within(dialog).getByText("http://gateway.test"),
    ).toBeInTheDocument()
    expect(within(dialog).queryAllByRole("textbox")).toHaveLength(0)
    expect(
      within(dialog).queryByRole("button", { name: /save/i }),
    ).not.toBeInTheDocument()

    await user.click(within(dialog).getByRole("tab", { name: "Worker" }))
    expect(
      within(dialog).getByLabelText("Worker to inspect"),
    ).toBeInTheDocument()
    expect(within(dialog).getByText("worker-1")).toBeInTheDocument()
    expect(within(dialog).getByText("0.1.0")).toBeInTheDocument()
    expect(within(dialog).getByText("Read-only")).toBeInTheDocument()

    await user.click(within(dialog).getByRole("button", { name: "Close" }))
    await waitFor(() => {
      expect(
        screen.getByRole("button", { name: "Client settings" }),
      ).toHaveFocus()
    })
  })

  it("opens the sidebar as a drawer below 780px", async () => {
    Object.defineProperty(window, "innerWidth", {
      configurable: true,
      value: 700,
    })
    const { user } = renderApp()
    await waitForChat()

    const sidebar = document.getElementById("context-sidebar")
    expect(sidebar).not.toBeNull()
    expect(sidebar).toHaveAttribute("data-state", "closed")
    expect(sidebar).toHaveAttribute("aria-hidden", "true")

    await user.click(screen.getByRole("button", { name: "Open navigation" }))
    expect(sidebar).toHaveAttribute("data-state", "open")
    expect(sidebar).not.toHaveAttribute("aria-hidden")

    await user.click(screen.getByRole("button", { name: "Close navigation" }))
    expect(sidebar).toHaveAttribute("data-state", "closed")
    expect(sidebar).toHaveAttribute("aria-hidden", "true")
  })

  it("keeps attention as an honest empty route", async () => {
    renderApp({ route: "#/attention" })
    expect(
      await screen.findByText("No approval queue"),
    ).toBeInTheDocument()
    expect(
      screen.getByText(/does not simulate a global approval queue/i),
    ).toBeInTheDocument()
    expect(
      screen.getByRole("link", { name: "Attention" }),
    ).toHaveAttribute("aria-current", "page")
  })

  it("routes to diagnostics and audit from the shell and sidebar", async () => {
    const bundle = {
      generated_at: 1_700_000_000_000,
      health: { status: "ok", workers: 1, reachable: 1 },
      metrics: [],
      storage: { metadata: "ok", artifacts: "ok", degraded: [] },
      findings: [],
      correlation: {},
      audit: [],
      traces: [],
      subsystems: { runtime: "ok" },
    }
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input)
        const body = url.includes("/audit") ? [] : bundle
        return {
          ok: true,
          status: 200,
          text: async () => JSON.stringify(body),
          headers: { get: () => null },
        } as unknown as Response
      }),
    )

    const diagnostics = renderApp({ route: "#/diagnostics" })
    expect(
      await screen.findByRole("heading", { name: "Diagnostics" }),
    ).toBeInTheDocument()
    expect(
      within(
        screen.getByRole("navigation", { name: "Primary navigation" }),
      ).getByRole("link", { name: "Diagnostics" }),
    ).toHaveAttribute("aria-current", "page")
    const sidebar = screen.getByRole("complementary", {
      name: "Gateway context",
    })
    expect(
      within(sidebar).getByRole("link", { name: "Diagnostics" }),
    ).toHaveAttribute("aria-current", "page")
    expect(within(sidebar).getByRole("link", { name: "Audit" })).toBeInTheDocument()
    expect(await screen.findByText("Gateway health")).toBeInTheDocument()
    diagnostics.unmount()

    renderApp({ route: "#/audit" })
    expect(
      await screen.findByRole("heading", { name: "Audit" }),
    ).toBeInTheDocument()
    expect(
      within(
        screen.getByRole("navigation", { name: "Primary navigation" }),
      ).getByRole("link", { name: "Audit" }),
    ).toHaveAttribute("aria-current", "page")
    expect(
      await screen.findByText("No registry decisions have been retained."),
    ).toBeInTheDocument()
  })
})

describe("local Gateway profiles", () => {
  it("saves only names and URLs and never stores the token", async () => {
    const { user, storage } = renderApp()
    await waitForChat()

    await user.click(
      screen.getByRole("button", { name: "Save current Gateway as profile" }),
    )

    const raw = storage.getItem(gatewayProfilesStorageKey) ?? ""
    expect(raw).toContain("gateway.test")
    expect(raw).not.toContain("caller-token")
    expect(raw).not.toContain("Authorization")
    expect(
      await screen.findByRole("button", {
        name: "Use profile gateway.test",
      }),
    ).toBeInTheDocument()
  })
})

describe("workspace persistence", () => {
  it("persists layout without secrets", () => {
    const storage = memoryStorage()
    const workspace = createWorkspaceStore({
      storage,
      createId: (prefix) => `${prefix}-persist`,
    })
    workspace.getState().addChat("chat-layout")
    const raw = storage.getItem(workspaceStorageKey) ?? ""
    expect(raw).toContain("chat-layout")
    expect(raw).not.toContain("token")
    expect(raw).not.toContain("Authorization")
  })
})

import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { render, screen, waitFor, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, describe, expect, it } from "vitest"
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
import { createClientSettingsStore } from "@/features/workspace/client-settings"
import { createGatewayProfilesStore } from "@/features/workspace/gateway-profiles"
import {
  createWorkspaceStore,
  type WorkspaceStore,
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

const titles: Record<string, string> = {
  "chat-alpha": "Alpha task",
  "chat-beta": "Beta task",
  "chat-gamma": "Gamma task",
  "chat-delta": "Delta task",
}

interface RenderOptions {
  chats?: string[]
  activeChatId?: string
  inspectorDefault?: "open" | "closed"
  width?: number
}

function renderWorkspace(options: RenderOptions = {}) {
  const chats = options.chats ?? ["chat-alpha"]
  if (options.width) {
    Object.defineProperty(window, "innerWidth", {
      configurable: true,
      value: options.width,
    })
  }
  const storage = memoryStorage()
  const history = new MemoryChatHistory()
  for (const chatId of chats) {
    history.seed(chatId, titles[chatId] ?? chatId, answerState(chatId, `${titles[chatId]} answer`))
  }
  let counter = 0
  const workspace: WorkspaceStore = createWorkspaceStore({
    storage,
    createId: (prefix) => `${prefix}-${++counter}`,
  })
  for (const chatId of chats) {
    workspace.getState().addChat(chatId)
  }
  if (options.activeChatId) {
    workspace.getState().focusChat(options.activeChatId)
  }
  const preferences = createClientSettingsStore({ storage })
  if (options.inspectorDefault) {
    preferences.getState().setInspectorDefault(options.inspectorDefault)
  }
  const profiles = createGatewayProfilesStore({
    storage,
    createId: (prefix) => `${prefix}-profile`,
  })
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  function wrapper({ children }: { children: ReactNode }) {
    return (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    )
  }
  window.location.hash = "#/chat"
  const user = userEvent.setup()
  render(
    <App
      store={connectedStore()}
      history={history}
      workspace={workspace}
      preferences={preferences}
      profiles={profiles}
    />,
    { wrapper },
  )
  return { history, workspace, preferences, user }
}

function panelItems(grid: HTMLElement): HTMLElement[] {
  return Array.from(
    grid.querySelectorAll<HTMLElement>("[data-panel-id]"),
  )
}

async function panelGrid(count: number): Promise<HTMLElement> {
  const grid = await screen.findByRole("list", { name: "Session panels" })
  await waitFor(() => {
    expect(grid).toHaveAttribute("data-panel-count", String(count))
    expect(panelItems(grid)).toHaveLength(count)
  })
  return grid
}

async function panelFor(label: RegExp): Promise<HTMLElement> {
  return screen.findByRole("region", { name: label })
}

afterEach(() => {
  window.location.hash = ""
  window.localStorage.clear()
  Object.defineProperty(window, "innerWidth", {
    configurable: true,
    value: 1200,
  })
  window.dispatchEvent(new Event("resize"))
})

describe("multi-panel session workspace", () => {
  it("renders one panel as a single column", async () => {
    const { workspace } = renderWorkspace({ chats: ["chat-alpha"] })

    const grid = await panelGrid(1)
    expect(grid).toHaveAttribute("data-layout", "single")
    expect(grid.querySelectorAll('[data-slot="resizable-handle"]')).toHaveLength(0)
    expect(panelItems(grid)).toHaveLength(1)
    expect(workspace.getState().activeChatId).toBe("chat-alpha")
  })

  it("renders two panels in two columns", async () => {
    renderWorkspace({ chats: ["chat-alpha", "chat-beta"] })

    const grid = await panelGrid(2)
    expect(grid).toHaveAttribute("data-layout", "double")
    expect(grid.querySelectorAll('[data-slot="resizable-handle"]')).toHaveLength(1)
    expect(panelItems(grid)).toHaveLength(2)
    expect(await panelFor(/Beta task, focused/)).toBeInTheDocument()
    expect(screen.queryByRole("region", { name: /Alpha task, focused/ }))
      .not.toBeInTheDocument()
  })

  it("renders three panels with resizable rows", async () => {
    renderWorkspace({
      chats: ["chat-alpha", "chat-beta", "chat-gamma"],
    })

    const grid = await panelGrid(3)
    expect(grid).toHaveAttribute("data-layout", "triple")
    expect(panelItems(grid)).toHaveLength(3)
    expect(grid.querySelectorAll('[data-slot="resizable-handle"]')).toHaveLength(2)
  })

  it("renders four panels as a two-by-two grid", async () => {
    renderWorkspace({
      chats: ["chat-alpha", "chat-beta", "chat-gamma", "chat-delta"],
    })

    const grid = await panelGrid(4)
    expect(grid).toHaveAttribute("data-layout", "quad")
    expect(panelItems(grid)).toHaveLength(4)
    expect(grid.querySelectorAll('[data-slot="resizable-handle"]')).toHaveLength(3)
  })

  it("collapses to one column below 1100px", async () => {
    renderWorkspace({
      chats: ["chat-alpha", "chat-beta", "chat-gamma"],
      width: 1000,
    })

    const grid = await panelGrid(3)
    await waitFor(() => expect(grid).toHaveAttribute("data-layout", "stacked"))
    expect(grid).toHaveAttribute("data-stacked", "true")
    expect(panelItems(grid)).toHaveLength(3)
    expect(grid.querySelectorAll('[data-slot="resizable-handle"]')).toHaveLength(2)
  })

  it("opens task records from the session panel header", async () => {
    const { user } = renderWorkspace({ chats: ["chat-alpha"] })
    await panelGrid(1)

    await user.click(
      screen.getByRole("button", { name: "Open task records" }),
    )

    const dialog = await screen.findByRole("dialog", { name: "Task records" })
    expect(dialog).toBeInTheDocument()
    expect(
      await screen.findByText(/No task records yet/i),
    ).toBeInTheDocument()
  })

  it("focuses a panel for the inspector without hiding the other panel", async () => {
    const { workspace, user } = renderWorkspace({
      chats: ["chat-alpha", "chat-beta"],
      activeChatId: "chat-beta",
    })
    const alpha = await panelFor(/Alpha task/)
    expect(await panelFor(/Beta task, focused/)).toBeInTheDocument()

    await user.click(within(alpha).getByRole("button", { name: "Focus panel" }))

    expect(workspace.getState().activeChatId).toBe("chat-alpha")
    expect(await panelFor(/Alpha task, focused/)).toBeInTheDocument()
    expect(screen.queryByRole("region", { name: /Beta task, focused/ }))
      .not.toBeInTheDocument()
    expect(screen.getByRole("list", { name: "Session panels" })).toHaveAttribute(
      "data-panel-count",
      "2",
    )
    const inspector = screen.getByRole("complementary", {
      name: "Focused session inspector",
    })
    expect(within(inspector).getByText("Alpha task")).toBeInTheDocument()
  })

  it("splits the focused session into a new panel", async () => {
    const { workspace, user } = renderWorkspace({ chats: ["chat-alpha"] })
    await panelGrid(1)

    await user.click(
      screen.getByRole("button", { name: "Split focused session" }),
    )

    await panelGrid(2)
    expect(workspace.getState().activeChatId).not.toBe("chat-alpha")
    expect(workspace.getState().backgroundChatIds).toEqual([])
  })

  it("solos a panel by backgrounding the others without removing them", async () => {
    const { workspace, user } = renderWorkspace({
      chats: ["chat-alpha", "chat-beta", "chat-gamma"],
    })
    const beta = await panelFor(/Beta task/)

    await user.click(within(beta).getByRole("button", { name: "Solo panel" }))

    await panelGrid(1)
    expect(workspace.getState().activeChatId).toBe("chat-beta")
    expect([...workspace.getState().backgroundChatIds].sort()).toEqual([
      "chat-alpha",
      "chat-gamma",
    ])
    expect(workspace.getState().groups[0]?.chatIds).toEqual([
      "chat-alpha",
      "chat-beta",
      "chat-gamma",
    ])
    expect(
      await screen.findByText(
        /Other panels moved to background\. Their Gateway tasks continue/,
      ),
    ).toBeInTheDocument()
  })

  it("shows a notice instead of a fifth panel", async () => {
    const { workspace, user } = renderWorkspace({
      chats: ["chat-alpha", "chat-beta", "chat-gamma", "chat-delta"],
    })
    await panelGrid(4)
    const focused = workspace.getState().activeChatId

    await user.click(
      screen.getByRole("button", { name: "Split focused session" }),
    )

    expect(
      await screen.findByText(
        "Four panels are already visible. Solo or background a session before splitting.",
      ),
    ).toBeInTheDocument()
    await panelGrid(4)
    expect(workspace.getState().activeChatId).toBe(focused)
    expect(workspace.getState().backgroundChatIds).toEqual([])
  })

  it("backgrounds one panel and restores the same local chat", async () => {
    const { workspace, user, history } = renderWorkspace({
      chats: ["chat-alpha", "chat-beta"],
      activeChatId: "chat-beta",
    })
    const alpha = await panelFor(/Alpha task/)

    await user.click(
      within(alpha).getByRole("button", { name: "Background session" }),
    )

    await panelGrid(1)
    expect(workspace.getState().backgroundChatIds).toEqual(["chat-alpha"])
    expect(workspace.getState().groups[0]?.chatIds).toContain("chat-alpha")

    await user.click(
      await screen.findByRole("button", { name: "Restore Alpha task" }),
    )

    await panelGrid(2)
    expect(workspace.getState().activeChatId).toBe("chat-alpha")
    expect(workspace.getState().backgroundChatIds).toEqual([])
    expect(history.loadChatCalls).toContain("chat-alpha")
  })

  it("exposes Split, Solo, and inspector actions in the command palette", async () => {
    const { user } = renderWorkspace({
      chats: ["chat-alpha", "chat-beta"],
    })
    await panelGrid(2)

    await user.keyboard("{Control>}k{/Control}")
    let dialog = await screen.findByRole("dialog", { name: "Command palette" })
    expect(
      within(dialog).getByRole("option", { name: /Split focused session/ }),
    ).toBeInTheDocument()
    expect(
      within(dialog).getByRole("option", {
        name: /Solo focused session: Beta task/,
      }),
    ).toBeInTheDocument()

    const closeInspector = within(dialog).getByRole("option", {
      name: /Close focused-session inspector/,
    })
    await user.click(closeInspector)
    await waitFor(() => {
      expect(
        screen.queryByRole("complementary", {
          name: "Focused session inspector",
        }),
      ).not.toBeInTheDocument()
    })

    await user.keyboard("{Control>}k{/Control}")
    dialog = await screen.findByRole("dialog", { name: "Command palette" })
    await user.click(
      within(dialog).getByRole("option", {
        name: /Open focused-session inspector/,
      }),
    )
    expect(
      await screen.findByRole("complementary", {
        name: "Focused session inspector",
      }),
    ).toBeInTheDocument()
  })

  it("closes the inspector by default on narrow screens and overlays on request", async () => {
    const { user } = renderWorkspace({
      chats: ["chat-alpha"],
      width: 700,
    })
    await panelGrid(1)
    await waitFor(() => {
      expect(
        screen.queryByRole("complementary", {
          name: "Focused session inspector",
        }),
      ).not.toBeInTheDocument()
    })

    await user.click(
      screen.getByRole("button", { name: "Open session inspector" }),
    )
    expect(
      await screen.findByRole("complementary", {
        name: "Focused session inspector",
      }),
    ).toBeInTheDocument()
    expect(
      screen.getByRole("button", { name: "Close inspector overlay" }),
    ).toBeInTheDocument()
  })
})


import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { render, screen, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, describe, expect, it } from "vitest"

import { App } from "@/App"
import {
  createConnectionStore,
  type ConnectionClient,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import { createTokenStore } from "@/features/connection/token-store"
import { createFakeProfileClient } from "@/features/runtime-profiles/test-fixtures"
import { createFakeTeamClient } from "@/features/teams/test-fixtures"
import { createClientSettingsStore } from "@/features/workspace/client-settings"
import { createGatewayProfilesStore } from "@/features/workspace/gateway-profiles"
import { createWorkspaceStore } from "@/features/workspace/workspace-store"
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

const workers: WorkerSummary[] = [
  {
    id: "general-a",
    instance: "default",
    package: "general",
    version: "0.1.0",
    status: "approved",
    reachable: true,
    locality: "local",
  },
  {
    id: "research-b",
    instance: "research",
    package: "general",
    version: "0.1.0",
    status: "approved",
    reachable: true,
    locality: "local",
  },
]

function connectedStore(): ConnectionStore {
  const store = createConnectionStore({
    tokenStore: createTokenStore({ sessionStorage: memoryStorage() }),
    defaultUrl: "http://gateway.test",
    createClient: (): ConnectionClient => ({
      checkConnection: async () => ({
        status: "connected",
        health: { status: "ok", workers: 2, reachable: 2 },
        workers,
      }),
    }),
  })
  store.setState({
    status: "connected",
    gatewayUrl: "http://gateway.test",
    token: "caller-token",
    health: { status: "ok", workers: 2, reachable: 2 },
    workers,
  })
  return store
}

function renderApp(route: string) {
  const storage = memoryStorage()
  let counter = 0
  const workspace = createWorkspaceStore({
    storage,
    createId: (prefix) => `${prefix}-${++counter}`,
  })
  const preferences = createClientSettingsStore({ storage: memoryStorage() })
  const profiles = createGatewayProfilesStore({
    storage: memoryStorage(),
    createId: (prefix) => `${prefix}-${++counter}`,
  })
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const teams = createFakeTeamClient()
  const profilesClient = createFakeProfileClient()
  window.location.hash = route
  const user = userEvent.setup()
  render(
    <QueryClientProvider client={queryClient}>
      <App
        store={connectedStore()}
        history={new MemoryChatHistory()}
        workspace={workspace}
        preferences={preferences}
        profiles={profiles}
        createTeamClient={() => teams}
        createProfileClient={() => profilesClient}
      />
    </QueryClientProvider>,
  )
  return { user, teams, profilesClient, queryClient }
}

afterEach(() => {
  window.location.hash = ""
  window.localStorage.clear()
})

describe("management routes and navigation", () => {
  it("creates a team from the rail route, opens it, and activates it", async () => {
    const { user, teams } = renderApp("#/teams")
    await screen.findByRole("heading", { name: "Teams" })

    const railLink = screen.getByRole("link", { name: "Teams" })
    expect(railLink).toHaveAttribute("href", "#/teams")
    expect(railLink).toHaveAttribute("aria-current", "page")

    await user.type(screen.getByLabelText("Team name"), "field")
    await user.type(screen.getByLabelText("Master Worker"), "general-a")
    const members = screen.getByRole("group", { name: "Members" })
    await user.type(
      within(members).getByLabelText("Member 1 worker"),
      "research-b",
    )
    await user.type(
      within(members).getByLabelText("Member 1 capabilities"),
      "research",
    )
    await user.click(screen.getByRole("button", { name: "Create team" }))

    const link = await screen.findByRole("link", { name: "field" })
    await user.click(link)

    expect(
      await screen.findByRole("heading", { name: "field" }),
    ).toBeInTheDocument()
    expect(window.location.hash).toBe("#/teams/field")

    await user.click(screen.getByRole("button", { name: "Activate team" }))

    expect(
      await screen.findByText("Team activated at version 1."),
    ).toBeInTheDocument()
    expect(teams.calls.activate).toEqual([{ name: "field", version: 1 }])
    expect(JSON.stringify(window.localStorage)).not.toContain("caller-token")
  })

  it("reaches teams, profiles, and a profile detail through the command palette", async () => {
    const { user, profilesClient } = renderApp("#/chat")

    await user.keyboard("{Control>}k{/Control}")
    let dialog = await screen.findByRole("dialog", { name: "Command palette" })
    const input = within(dialog).getByRole("combobox", { name: "Command" })
    await user.type(input, "Runtime Profiles")
    await user.click(
      await within(dialog).findByRole("option", {
        name: /Open Runtime Profiles/,
      }),
    )

    expect(
      await screen.findByRole("heading", { name: "Runtime Profiles" }),
    ).toBeInTheDocument()
    expect(window.location.hash).toBe("#/profiles")

    const research = await screen.findByRole("link", { name: "research" })
    expect(research).toHaveAttribute("href", "#/profiles/general-a/research")
    await user.click(research)

    expect(
      await screen.findByRole("heading", { name: "research" }),
    ).toBeInTheDocument()
    expect(profilesClient.calls.profile[0]).toEqual({
      worker: "general-a",
      name: "research",
    })

    await user.keyboard("{Control>}k{/Control}")
    dialog = await screen.findByRole("dialog", { name: "Command palette" })
    await user.type(
      within(dialog).getByRole("combobox", { name: "Command" }),
      "teams",
    )
    await user.click(
      await within(dialog).findByRole("option", { name: /Open teams/ }),
    )

    expect(
      await screen.findByRole("heading", { name: "Teams" }),
    ).toBeInTheDocument()
    expect(window.location.hash).toBe("#/teams")
    expect(JSON.stringify(window.localStorage)).not.toContain("caller-token")
  })
})

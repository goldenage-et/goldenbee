import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { describe, expect, it } from "vitest"

import { App } from "@/App"
import {
  createConnectionStore,
  type ConnectionClient,
} from "@/features/connection/connection-store"
import { createTokenStore } from "@/features/connection/token-store"
import { createFakeProviderClient } from "@/features/providers/test-fixtures"

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

const health = { status: "ok", workers: 1, reachable: 1 }
const workers = [
  {
    id: "general-a",
    instance: "default",
    package: "general",
    version: "0.1.0",
    status: "approved" as const,
    reachable: true,
    locality: "local" as const,
  },
]

async function connect(user: ReturnType<typeof userEvent.setup>) {
  await user.clear(screen.getByLabelText("Gateway URL"))
  await user.type(screen.getByLabelText("Gateway URL"), "http://127.0.0.1:8080")
  await user.clear(screen.getByLabelText("Caller token"))
  await user.type(screen.getByLabelText("Caller token"), "admin-token")
  await user.click(screen.getByRole("button", { name: "Connect" }))
  await screen.findByText("Gateway connected")
}

function renderApp() {
  const store = createConnectionStore({
    tokenStore: createTokenStore({ sessionStorage: memoryStorage() }),
    defaultUrl: "http://127.0.0.1:8080",
    createClient: (): ConnectionClient => ({
      checkConnection: async () => ({ status: "connected", health, workers }),
    }),
  })
  const providers = createFakeProviderClient()
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const user = userEvent.setup()
  render(
    <QueryClientProvider client={queryClient}>
      <App
        store={store}
        createProviderClient={() => providers}
      />
    </QueryClientProvider>,
  )
  return { providers, user }
}

describe("provider routes and navigation", () => {
  it("navigates from the connected shell to the catalog and a provider detail", async () => {
    const { user } = renderApp()
    await connect(user)

    const providersLink = screen.getByRole("link", { name: "Providers" })
    expect(providersLink).toHaveAttribute("href", "#/providers")

    await user.click(providersLink)

    expect(
      await screen.findByRole("heading", { name: "Model providers" }),
    ).toBeInTheDocument()
    expect(screen.getByRole("link", { name: "Providers" })).toHaveAttribute(
      "aria-current",
      "page",
    )

    await user.click(await screen.findByRole("link", { name: /openrouter/i }))

    expect(
      await screen.findByRole("heading", { name: "openrouter" }),
    ).toBeInTheDocument()
    expect(
      screen.getByRole("button", { name: "Refresh models" }),
    ).toBeInTheDocument()
  })
})

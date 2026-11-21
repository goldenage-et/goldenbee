import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { describe, expect, it } from "vitest"

import { ProvidersScreen } from "./providers-screen"
import {
  createFakeProviderClient,
  scopeDenied,
} from "./test-fixtures"

function renderProviders(client = createFakeProviderClient()) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const user = userEvent.setup()
  render(
    <QueryClientProvider client={queryClient}>
      <ProvidersScreen client={client} />
    </QueryClientProvider>,
  )
  return { client, user }
}

describe("provider creation", () => {
  it("adds a provider with the entered metadata and shows it in the catalog", async () => {
    const { client, user } = renderProviders()
    await screen.findByRole("link", { name: /openrouter/i })

    await user.click(screen.getByRole("button", { name: "Add provider" }))
    await user.type(screen.getByLabelText("Provider id"), "local-ollama")
    await user.selectOptions(screen.getByLabelText("Adapter"), "ollama")
    await user.clear(screen.getByLabelText("Residency"))
    await user.type(screen.getByLabelText("Residency"), "local")
    await user.type(screen.getByLabelText("Endpoint"), "http://127.0.0.1:11434")
    await user.type(screen.getByLabelText("Default model"), "llama3:latest")
    await user.click(screen.getByRole("button", { name: "Create provider" }))

    expect(await screen.findByRole("link", { name: /local-ollama/i })).toBeInTheDocument()
    expect(client.calls.create).toHaveLength(1)
    expect(client.calls.create[0]).toMatchObject({
      id: "local-ollama",
      adapter: "ollama",
      residency: "local",
      endpoint: "http://127.0.0.1:11434",
      model: "llama3:latest",
      modalities: ["text"],
    })
  })

  it("rejects a new provider id that already exists without adding a row", async () => {
    const client = createFakeProviderClient()
    client.createProvider = async () => {
      throw scopeDenied()
    }
    const { user } = renderProviders(client)
    await screen.findByRole("link", { name: /openrouter/i })

    await user.click(screen.getByRole("button", { name: "Add provider" }))
    await user.type(screen.getByLabelText("Provider id"), "openrouter")
    await user.click(screen.getByRole("button", { name: "Create provider" }))

    const alert = await screen.findByRole("alert")
    expect(alert).toHaveTextContent("Not permitted")
    expect(alert).toHaveTextContent("scope.provider_admin")
    await user.click(screen.getByRole("button", { name: "Cancel" }))
    expect(screen.getAllByRole("link", { name: /openrouter/i })).toHaveLength(1)
  })
})

import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { render, screen } from "@testing-library/react"
import { describe, expect, it } from "vitest"

import { ProvidersScreen } from "./providers-screen"
import { createFakeProviderClient, sdkFailure } from "./test-fixtures"

function renderProviders(client = createFakeProviderClient()) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  render(
    <QueryClientProvider client={queryClient}>
      <ProvidersScreen client={client} />
    </QueryClientProvider>,
  )
  return { client, queryClient }
}

describe("provider catalog list", () => {
  it("shows adapter, model, locality, residency, models, readiness, and active state", async () => {
    renderProviders()

    expect(
      await screen.findByRole("heading", { name: "Model providers" }),
    ).toBeInTheDocument()

    const row = await screen.findByRole("link", { name: /openrouter/i })
    expect(row).toHaveAttribute("href", "#/providers/openrouter")

    expect(screen.getAllByText("openrouter").length).toBeGreaterThan(0)
    expect(screen.getByText("openai/gpt-4.1")).toBeInTheDocument()
    expect(screen.getByText("Remote")).toBeInTheDocument()
    expect(screen.getByText("residency: remote")).toBeInTheDocument()
    expect(
      screen.getByText("openai/gpt-4.1, anthropic/claude-sonnet-4"),
    ).toBeInTheDocument()
    expect(screen.getByText("Ready")).toBeInTheDocument()
    expect(screen.getByText("Active")).toBeInTheDocument()
  })

  it("shows the vault-disabled scope error without inventing an empty catalog", async () => {
    const client = createFakeProviderClient()
    client.providers = async () => {
      throw sdkFailure({
        kind: "forbidden",
        status: 403,
        code: "scope.provider_admin",
        message: "caller token is not allowed to administer model providers",
        recovery: "use a caller token with the provider:admin scope",
      })
    }

    renderProviders(client)

    const alert = await screen.findByRole("alert")
    expect(alert).toHaveTextContent("Not permitted")
    expect(alert).toHaveTextContent("scope.provider_admin")
    expect(alert).toHaveTextContent("use a caller token with the provider:admin scope")
    expect(screen.queryByText(/No providers/)).not.toBeInTheDocument()
  })

  it("shows an empty catalog as empty rather than an error", async () => {
    renderProviders(createFakeProviderClient([]))

    expect(await screen.findByText("No providers configured.")).toBeInTheDocument()
  })
})

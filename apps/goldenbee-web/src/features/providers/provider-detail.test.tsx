import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { describe, expect, it } from "vitest"

import { ProviderDetailScreen } from "./provider-detail-screen"
import {
  createFakeProviderClient,
  providerFixture,
  providerNotFound,
  revisionConflict,
  sdkFailure,
} from "./test-fixtures"

function renderDetail(
  client = createFakeProviderClient(),
  id = "openrouter",
) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const user = userEvent.setup()
  render(
    <QueryClientProvider client={queryClient}>
      <ProviderDetailScreen client={client} id={id} />
    </QueryClientProvider>,
  )
  return { client, queryClient, user }
}

async function waitForDetail() {
  return screen.findByRole("heading", { name: "openrouter" })
}

describe("provider detail", () => {
  it("shows adapter, locality, residency, allowed models, workers, readiness, and credential metadata", async () => {
    renderDetail()

    await waitForDetail()
    expect(screen.getAllByText("openrouter").length).toBeGreaterThan(0)
    expect(screen.getAllByText("openai/gpt-4.1").length).toBeGreaterThan(0)
    expect(screen.getByText("Remote")).toBeInTheDocument()
    expect(screen.getByText("residency: remote")).toBeInTheDocument()
    expect(screen.getByText("general-a")).toBeInTheDocument()
    expect(screen.getAllByText("Active").length).toBeGreaterThan(0)
    expect(screen.getByText("Ready")).toBeInTheDocument()
    expect(screen.getByText("Revision 4")).toBeInTheDocument()
    expect(screen.getByText("Version 2")).toBeInTheDocument()
    expect(screen.getByText("sha256:8f14e45fceea167a")).toBeInTheDocument()
    expect(
      screen.getAllByText(/anthropic\/claude-sonnet-4/).length,
    ).toBeGreaterThan(0)
  })

  it("updates metadata with the observed provider revision", async () => {
    const { client, user } = renderDetail()
    await waitForDetail()

    const model = screen.getByLabelText("Default model")
    await user.clear(model)
    await user.type(model, "anthropic/claude-sonnet-4")
    await user.click(screen.getByRole("button", { name: "Save provider" }))

    expect(await screen.findByText("Revision 5")).toBeInTheDocument()
    expect(client.calls.update).toHaveLength(1)
    expect(client.calls.update[0]).toMatchObject({
      id: "openrouter",
      request: { revision: 4, model: "anthropic/claude-sonnet-4" },
    })
  })

  it("surfaces a stale revision without claiming the edit was saved", async () => {
    const client = createFakeProviderClient()
    client.updateProvider = async (id, request) => {
      client.calls.update.push({ id, request })
      throw revisionConflict()
    }
    const { user } = renderDetail(client)
    await waitForDetail()

    const residency = screen.getByLabelText("Residency")
    await user.clear(residency)
    await user.type(residency, "eu")
    await user.click(screen.getByRole("button", { name: "Save provider" }))

    const alert = await screen.findByRole("alert")
    expect(alert).toHaveTextContent("Provider changed elsewhere")
    expect(alert).toHaveTextContent("provider.revision_conflict")
    expect(screen.getByText("Revision 4")).toBeInTheDocument()
    expect(screen.queryByText("Revision 5")).not.toBeInTheDocument()
  })

  it("disables a provider only after the Gateway confirms the revision", async () => {
    const { client, user } = renderDetail()
    await waitForDetail()

    await user.click(screen.getByRole("button", { name: "Disable provider" }))

    expect(await screen.findByText("Disabled")).toBeInTheDocument()
    expect(screen.getByText("Revision 5")).toBeInTheDocument()
    expect(client.calls.update[0]).toMatchObject({
      id: "openrouter",
      request: { revision: 4, active: false },
    })
  })

  it("explains an invalid provider without rendering a stale projection", async () => {
    const client = createFakeProviderClient()
    client.provider = async (id) => {
      throw providerNotFound(id)
    }
    renderDetail(client, "missing")

    const alert = await screen.findByRole("alert")
    expect(alert).toHaveTextContent("Provider not found")
    expect(alert).toHaveTextContent("provider.not_found")
    expect(screen.queryByText("Revision 4")).not.toBeInTheDocument()
  })

  it("refreshes discovered models and shows the new revision", async () => {
    const { client, user } = renderDetail()
    await waitForDetail()

    expect(
      screen.getAllByText("openai/gpt-4.1, anthropic/claude-sonnet-4").length,
    ).toBeGreaterThan(0)
    await user.click(screen.getByRole("button", { name: "Refresh models" }))

    expect(
      await screen.findByText("openai/gpt-4.1, google/gemini-2.5-pro"),
    ).toBeInTheDocument()
    expect(screen.getByText("Revision 5")).toBeInTheDocument()
    expect(client.calls.discover).toEqual(["openrouter"])
  })

  it("shows a typed discovery failure instead of an empty ready provider", async () => {
    const client = createFakeProviderClient()
    client.discoverProvider = async (id) => {
      client.calls.discover.push(id)
      return providerFixture({
        ready: false,
        models: [],
        discovery_error:
          "provider rejected the credential during model discovery",
        discovery_error_code: "provider.discovery_auth",
        revision: 5,
      })
    }
    const { user } = renderDetail(client)
    await waitForDetail()

    await user.click(screen.getByRole("button", { name: "Refresh models" }))

    const alert = await screen.findByRole("alert")
    expect(alert).toHaveTextContent("provider.discovery_auth")
    expect(alert).toHaveTextContent(
      "provider rejected the credential during model discovery",
    )
    expect(screen.getByText("Not ready")).toBeInTheDocument()
  })

  it("reports a discovery action failure with the typed Gateway error", async () => {
    const client = createFakeProviderClient()
    client.discoverProvider = async () => {
      throw sdkFailure({
        kind: "server",
        status: 503,
        code: "provider.vault_disabled",
        message: "Gateway provider vault is not enabled in this deployment",
        recovery: "enable the provider vault and retry",
        retryable: true,
      })
    }
    const { user } = renderDetail(client)
    await waitForDetail()

    await user.click(screen.getByRole("button", { name: "Refresh models" }))

    const alert = await screen.findByRole("alert")
    expect(alert).toHaveTextContent("provider.vault_disabled")
    expect(alert).toHaveTextContent("enable the provider vault and retry")
  })
})

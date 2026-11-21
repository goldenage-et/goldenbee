import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { describe, expect, it } from "vitest"

import { ProviderDetailScreen } from "./provider-detail-screen"
import {
  createFakeProviderClient,
  scopeDenied,
  sdkFailure,
} from "./test-fixtures"

function renderDetail(client = createFakeProviderClient()) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const user = userEvent.setup()
  render(
    <QueryClientProvider client={queryClient}>
      <ProviderDetailScreen client={client} id="openrouter" />
    </QueryClientProvider>,
  )
  return { client, queryClient, user }
}

const secret = "sk-live-SUPER-SECRET-provider-key"

describe("provider credential write-only flow", () => {
  it("clears the submitted key immediately and never renders or caches it", async () => {
    const { client, queryClient, user } = renderDetail()
    await screen.findByRole("heading", { name: "openrouter" })

    await user.click(screen.getByRole("button", { name: "Rotate credential" }))
    const input = screen.getByLabelText("Provider API key")
    expect(input).toHaveAttribute("type", "password")
    expect(input).toHaveAttribute("autocomplete", "off")

    await user.type(input, secret)
    await user.click(screen.getByRole("button", { name: "Write credential" }))

    expect(await screen.findByText("Credential written")).toBeInTheDocument()
    expect(input).toHaveValue("")
    expect(document.body.textContent).not.toContain(secret)
    expect(screen.getAllByText("Version 3").length).toBeGreaterThan(0)
    expect(screen.getByText("Fingerprint sha256:deadbeefcafebabe")).toBeInTheDocument()

    expect(client.calls.credential).toEqual([
      { id: "openrouter", key: secret },
    ])

    const cached = JSON.stringify(
      queryClient.getQueryCache().getAll().map((query) => query.state.data),
    )
    expect(cached).not.toContain(secret)
  })

  it("reports a denied credential write without changing the projection", async () => {
    const client = createFakeProviderClient()
    client.setProviderCredential = async (id, key) => {
      client.calls.credential.push({ id, key })
      throw scopeDenied()
    }
    const { user } = renderDetail(client)
    await screen.findByRole("heading", { name: "openrouter" })

    await user.click(screen.getByRole("button", { name: "Rotate credential" }))
    await user.type(screen.getByLabelText("Provider API key"), secret)
    await user.click(screen.getByRole("button", { name: "Write credential" }))

    const alert = await screen.findByRole("alert")
    expect(alert).toHaveTextContent("Not permitted")
    expect(alert).toHaveTextContent("scope.provider_admin")
    expect(screen.getByText("Version 2")).toBeInTheDocument()
    expect(screen.queryByText("Version 3")).not.toBeInTheDocument()
    expect(document.body.textContent).not.toContain(secret)
  })

  it("shows a revoked credential only after the Gateway confirms revocation", async () => {
    const { client, user } = renderDetail()
    await screen.findByRole("heading", { name: "openrouter" })

    await user.click(screen.getByRole("button", { name: "Revoke credential" }))

    expect(await screen.findByText("Revoked")).toBeInTheDocument()
    expect(client.calls.revoke).toEqual(["openrouter"])
    expect(
      screen.queryByRole("button", { name: "Revoke credential" }),
    ).not.toBeInTheDocument()
  })

  it("keeps the credential active when revocation fails", async () => {
    const client = createFakeProviderClient()
    client.revokeProviderCredential = async (id) => {
      client.calls.revoke.push(id)
      throw sdkFailure({
        kind: "forbidden",
        status: 403,
        code: "scope.provider_admin",
        message: "caller token is not allowed to administer model providers",
        recovery: "use a caller token with the provider:admin scope",
      })
    }
    const { user } = renderDetail(client)
    await screen.findByRole("heading", { name: "openrouter" })

    await user.click(screen.getByRole("button", { name: "Revoke credential" }))

    const alert = await screen.findByRole("alert")
    expect(alert).toHaveTextContent("scope.provider_admin")
    expect(screen.queryByText("Revoked")).not.toBeInTheDocument()
    expect(screen.getByText("Version 2")).toBeInTheDocument()
  })
})

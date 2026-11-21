import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import type { ReactNode } from "react"
import { describe, expect, it, vi } from "vitest"

import { AuditPage, type AuditGateway } from "@/features/audit/audit-page"
import {
  createConnectionStore,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import { createTokenStore } from "@/features/connection/token-store"
import {
  GatewayError,
  type ControlAuditEntry,
  type RegistryAuditEntry,
  type WorkerSummary,
} from "@goldenbee/sdk"

const worker: WorkerSummary = {
  id: "worker-1",
  instance: "worker-a",
  package: "general",
  version: "0.1.0",
  status: "approved",
  reachable: true,
  locality: "local",
}

const registryEntries: RegistryAuditEntry[] = [
  {
    at: 1_700_000_000_000,
    action: "worker.register",
    worker: "worker-1",
    principal: "operator-a",
    outcome: "accepted",
    detail: "lease renewed",
    source: "gateway",
    policy_revision: "7",
    correlation: "corr-1",
  },
  {
    at: 1_700_000_001_000,
    action: "worker.decommission",
    worker: "worker-2",
    principal: "operator-b",
    outcome: "denied",
    detail: "denied by policy at /home/operator/goldenbee/registry.toml",
    source: "gateway",
    policy_revision: "8",
  },
]

const controlEntries: ControlAuditEntry[] = [
  {
    at: 1_700_000_002_000,
    action: "control.pause",
    session_id: "session-1",
    principal: "operator-a",
    generation: 3,
    outcome: "accepted",
    detail: "control delegation accepted",
  },
  {
    at: 1_700_000_003_000,
    action: "control.handback",
    session_id: "session-1",
    principal: "operator-b",
    generation: 4,
    outcome: "rejected",
    detail: "generation is stale",
  },
]

function connectedStore(): ConnectionStore {
  const store = createConnectionStore({
    tokenStore: createTokenStore({ sessionStorage: null }),
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

function renderPage(client: AuditGateway | null) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  function wrapper({ children }: { children: ReactNode }) {
    return (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    )
  }
  return {
    user: userEvent.setup(),
    ...render(<AuditPage store={connectedStore()} client={client} />, {
      wrapper,
    }),
  }
}

function gateway(
  overrides: Partial<AuditGateway> = {},
): AuditGateway {
  return {
    workerAudit: vi.fn(async () => registryEntries),
    controlAudit: vi.fn(async () => controlEntries),
    ...overrides,
  }
}

describe("audit page", () => {
  it("renders registry principal, action, decision, reason, resource, and time", async () => {
    const client = gateway()
    renderPage(client)

    expect(await screen.findByText("worker.register")).toBeInTheDocument()
    expect(screen.getByText("operator-a")).toBeInTheDocument()
    expect(screen.getByText("accepted")).toBeInTheDocument()
    expect(screen.getByText("lease renewed")).toBeInTheDocument()
    expect(screen.getByText("policy 7")).toBeInTheDocument()
    expect(screen.getByText("corr-1")).toBeInTheDocument()
    expect(screen.getByText("worker-2")).toBeInTheDocument()
    expect(screen.getByText("denied")).toBeInTheDocument()
    expect(screen.getByText("2023-11-14T22:13:20.000Z")).toBeInTheDocument()

    // A host path in a retained decision never reaches the DOM.
    expect(document.body.textContent ?? "").not.toContain("/home/operator")
    expect(screen.getByText(/denied by policy at \[redacted path\]/)).toBeInTheDocument()
    expect(document.body.textContent ?? "").toContain("Redaction active")
    expect(client.workerAudit).toHaveBeenCalledWith("worker-1")
  })

  it("reads control decisions for an explicit Worker and session", async () => {
    const client = gateway()
    const { user } = renderPage(client)
    await screen.findByText("worker.register")

    expect(
      screen.getByText("Enter a session id to read Worker control decisions."),
    ).toBeInTheDocument()

    await user.type(screen.getByLabelText("Session id"), "session-1")
    await user.click(screen.getByRole("button", { name: "Load control audit" }))

    await waitFor(() =>
      expect(client.controlAudit).toHaveBeenCalledWith("worker-1", "session-1"),
    )
    expect(await screen.findByText("control.pause")).toBeInTheDocument()
    expect(screen.getByText("generation 3")).toBeInTheDocument()
    expect(screen.getByText("generation is stale")).toBeInTheDocument()
    expect(screen.getByText("rejected")).toBeInTheDocument()
  })

  it("shows a typed scope-denied state instead of an empty registry list", async () => {
    const client = gateway({
      workerAudit: async () => {
        throw new GatewayError({
          kind: "forbidden",
          status: 403,
          code: "scope.admin_required",
          message: "caller is not authorized for this registry read",
          recovery: "request the admin scope",
          retryable: false,
        })
      },
    })
    renderPage(client)

    expect(
      await screen.findByText("Not authorized to read registry audit"),
    ).toBeInTheDocument()
    expect(screen.getByText(/scope\.admin_required/)).toBeInTheDocument()
    expect(screen.getByText(/request the admin scope/)).toBeInTheDocument()
    expect(
      screen.queryByText("No registry decisions have been retained."),
    ).not.toBeInTheDocument()
  })

  it("shows a typed scope-denied state for control audit", async () => {
    const client = gateway({
      controlAudit: async () => {
        throw new GatewayError({
          kind: "unauthorized",
          status: 401,
          code: "auth.control_denied",
          message: "control audit is not available to this caller",
          retryable: false,
        })
      },
    })
    const { user } = renderPage(client)
    await screen.findByText("worker.register")

    await user.type(screen.getByLabelText("Session id"), "session-1")
    await user.click(screen.getByRole("button", { name: "Load control audit" }))

    expect(
      await screen.findByText("Not authorized to read control audit"),
    ).toBeInTheDocument()
    expect(
      screen.queryByText("No control decisions were retained for this session."),
    ).not.toBeInTheDocument()
  })

  it("shows honest empty and loading states", async () => {
    const empty = gateway({
      workerAudit: async () => [],
    })
    const { unmount } = renderPage(empty)
    expect(await screen.findByText("Loading registry audit…")).toBeInTheDocument()
    expect(
      await screen.findByText("No registry decisions have been retained."),
    ).toBeInTheDocument()
    unmount()

    renderPage(
      gateway({ workerAudit: () => new Promise(() => undefined) }),
    )
    expect(await screen.findByText("Loading registry audit…")).toBeInTheDocument()
  })

  it("shows a typed error when the registry read fails", async () => {
    const client = gateway({
      workerAudit: async () => {
        throw new GatewayError({
          kind: "network",
          message: "No Gateway answered at this address.",
          retryable: true,
        })
      },
    })
    renderPage(client)

    expect(
      await screen.findByText("Registry audit unavailable"),
    ).toBeInTheDocument()
    expect(
      screen.getByText("No Gateway answered at this address."),
    ).toBeInTheDocument()
    expect(screen.getByText(/network/)).toBeInTheDocument()
  })

  it("bounds a large retained audit tail", async () => {
    const entries = Array.from({ length: 150 }, (_, index) => ({
      ...registryEntries[0],
      action: `worker.action.${index}`,
      at: 1_700_000_000_000 + index,
    }))
    renderPage(gateway({ workerAudit: async () => entries }))

    expect(await screen.findByText("Showing 100 of 150 entries")).toBeInTheDocument()
    expect(screen.getByText("worker.action.0")).toBeInTheDocument()
    expect(screen.queryByText("worker.action.120")).not.toBeInTheDocument()
  })

  it("filters registry decisions by the selected Worker", async () => {
    const workerAudit = vi.fn(async () => registryEntries)
    const { user } = renderPage(gateway({ workerAudit }))
    await screen.findByText("worker.register")

    await user.selectOptions(
      screen.getByLabelText("Registry Worker filter"),
      "",
    )

    await waitFor(() => expect(workerAudit).toHaveBeenCalledWith(undefined))
  })

  it("names the empty Worker state honestly", async () => {
    const store = connectedStore()
    store.setState({ workers: [] })
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    })
    render(
      <QueryClientProvider client={queryClient}>
        <AuditPage store={store} client={gateway()} />
      </QueryClientProvider>,
    )

    expect(
      await screen.findByText("No Worker is visible to this caller"),
    ).toBeInTheDocument()
    expect(screen.getByText(/audit cannot be read/i)).toBeInTheDocument()
  })
})

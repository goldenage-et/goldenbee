import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { render, screen, waitFor, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import type { ReactNode } from "react"
import { describe, expect, it, vi } from "vitest"

import {
  DiagnosticsPage,
  type DiagnosticsGateway,
} from "@/features/diagnostics/diagnostics-page"
import {
  createConnectionStore,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import { createTokenStore } from "@/features/connection/token-store"
import {
  GatewayError,
  type DiagnosticBundle,
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
  capacity: { active: 2, limit: 8, queued: 1, queue_limit: 4 },
  lease_expires_at: 1_800_000,
  incarnations: [
    { incarnation: "inc-a", first_seen_at: 1_000, last_seen_at: 1_500 },
  ],
}

const bundle: DiagnosticBundle = {
  generated_at: 1_700_000_000_000,
  health: { status: "ok", workers: 3, reachable: 2, version: "1.0.0" },
  metrics: [
    {
      name: "agent_tasks_active",
      value: 2,
      quality: "exact",
      labels: { instance: "worker-a" },
    },
    {
      name: "agent_tasks_queued",
      value: null,
      quality: "unknown",
      labels: {},
    },
  ],
  storage: {
    metadata: "ok",
    artifacts: "degraded",
    degraded: ["writes slowed under load"],
  },
  findings: [
    {
      kind: "degraded_storage",
      source: "storage",
      code: "storage.artifact_degraded",
      message:
        "Artifact metadata at /home/operator/.local/share/goldenbee/artifacts is degraded",
      action: "Inspect the artifact volume",
      trace_id: "trace-1",
    },
  ],
  correlation: { trace_id: "trace-1", task_id: "task-1", attempt: 1 },
  audit: [
    {
      action: "worker.register",
      principal: "operator-a",
      detail: "Bearer abcdefghijklmnop accepted from /var/lib/goldenbee/run.sock",
    },
  ],
  traces: [
    {
      trace_id: "trace-1",
      task_id: "task-1",
      span: "gateway.diagnostics",
      duration_ms: 12,
      at: 1_700_000_000_100,
      attributes: {
        instance: "worker-a",
        socket: "/var/lib/goldenbee/run.sock",
      },
    },
  ],
  subsystems: {
    storage: "degraded",
    providers: "ok",
    runtime: "ok",
  },
}

function connectedStore(workers: WorkerSummary[] = [worker]): ConnectionStore {
  const store = createConnectionStore({
    tokenStore: createTokenStore({ sessionStorage: null }),
    defaultUrl: "http://gateway.test",
    createClient: () => ({
      checkConnection: async () => ({
        status: "connected",
        health: { status: "ok", workers: workers.length, reachable: workers.length },
        workers,
      }),
    }),
  })
  store.setState({
    status: "connected",
    gatewayUrl: "http://gateway.test",
    token: "caller-token",
    health: { status: "ok", workers: workers.length, reachable: workers.length },
    workers,
  })
  return store
}

function renderPage(client: DiagnosticsGateway | null, workers?: WorkerSummary[]) {
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
    ...render(<DiagnosticsPage store={connectedStore(workers)} client={client} />, {
      wrapper,
    }),
  }
}

describe("diagnostics page", () => {
  it("renders redacted health, capacity, findings, storage, and traces", async () => {
    const client: DiagnosticsGateway = {
      diagnostics: vi.fn(async () => bundle),
    }
    renderPage(client)

    expect(await screen.findByText("agent_tasks_active")).toBeInTheDocument()
    expect(screen.getByText("2 / 8 active")).toBeInTheDocument()
    expect(screen.getByText("1 / 4 queued")).toBeInTheDocument()
    expect(
      screen.getByText(/Artifact metadata at \[redacted path\] is degraded/),
    ).toBeInTheDocument()
    expect(screen.getByText("gateway.diagnostics")).toBeInTheDocument()
    expect(screen.getByText("12 ms")).toBeInTheDocument()
    expect(screen.getByText("writes slowed under load")).toBeInTheDocument()
    expect(screen.getAllByText("degraded").length).toBeGreaterThan(0)

    // No raw host path, token-shaped value, or credential text reaches the DOM.
    expect(document.body.textContent ?? "").not.toContain("/home/operator")
    expect(document.body.textContent ?? "").not.toContain("/var/lib/goldenbee")
    expect(document.body.textContent ?? "").not.toContain("abcdefghijklmnop")
    expect(document.body.textContent ?? "").toContain("Redaction active")
    expect(screen.getByText(/Some reported values were redacted/)).toBeInTheDocument()
  })

  it("bounds long findings, traces, and metric lists", async () => {
    const findings = Array.from({ length: 25 }, (_, index) => ({
      kind: "hook_failure",
      source: "hooks",
      code: `hook.${index}`,
      message: `Finding ${index}`,
      action: "Review the hook",
    }))
    const traces = Array.from({ length: 14 }, (_, index) => ({
      trace_id: `trace-${index}`,
      task_id: `task-${index}`,
      span: `span.${index}`,
      duration_ms: index,
      at: 1_000 + index,
      attributes: {},
    }))
    const metrics = Array.from({ length: 30 }, (_, index) => ({
      name: `metric_${index}`,
      value: index,
      quality: "exact",
      labels: {},
    }))
    const client: DiagnosticsGateway = {
      diagnostics: async () => ({ ...bundle, findings, traces, metrics }),
    }
    renderPage(client)

    expect(await screen.findByText("metric_0")).toBeInTheDocument()
    expect(screen.queryByText("metric_20")).not.toBeInTheDocument()
    expect(screen.getByText("Showing 20 of 30 metrics")).toBeInTheDocument()
    expect(screen.getByText("Showing 20 of 25 findings")).toBeInTheDocument()
    expect(screen.getByText("Showing 10 of 14 traces")).toBeInTheDocument()
    expect(screen.queryByText("span.10")).not.toBeInTheDocument()
  })

  it("passes the optional task filter to the Gateway", async () => {
    const diagnostics = vi.fn(async () => ({
      ...bundle,
      task: {
        task_id: "task-1",
        state: "running" as const,
        attempt: 2,
        profile: "default",
        profile_revision: 3,
        asset_revision: 4,
        traffic: "product",
        trace_id: "trace-1",
        approvals: [],
        delegations: [],
      },
    }))
    const { user } = renderPage({ diagnostics })

    const taskInput = await screen.findByLabelText("Task id filter")
    await user.type(taskInput, "task-1")
    await user.click(screen.getByRole("button", { name: "Apply filter" }))

    await waitFor(() =>
      expect(diagnostics).toHaveBeenCalledWith("worker-1", "task-1"),
    )
    expect(await screen.findByText("Task diagnosis")).toBeInTheDocument()
    expect(screen.getByText("running")).toBeInTheDocument()
    expect(screen.getByText("default · revision 3")).toBeInTheDocument()
  })

  it("shows a loading state while the bundle is pending", async () => {
    const client: DiagnosticsGateway = {
      diagnostics: () => new Promise(() => undefined),
    }
    renderPage(client)

    expect(await screen.findByText("Loading diagnostics…")).toBeInTheDocument()
  })

  it("shows an explicit authorization state instead of an empty bundle", async () => {
    const client: DiagnosticsGateway = {
      diagnostics: async () => {
        throw new GatewayError({
          kind: "forbidden",
          status: 403,
          code: "scope.worker_denied",
          message: "caller is not authorized for Worker 'worker-1'",
          recovery: "request task:read for this Worker",
          retryable: false,
        })
      },
    }
    renderPage(client)

    expect(
      await screen.findByText("Not authorized to read diagnostics"),
    ).toBeInTheDocument()
    expect(screen.getByText(/scope\.worker_denied/)).toBeInTheDocument()
    expect(
      screen.getByText(/request task:read for this Worker/),
    ).toBeInTheDocument()
    expect(screen.queryByText(/No findings/)).not.toBeInTheDocument()
  })

  it("shows a typed failure when the Gateway cannot answer", async () => {
    const client: DiagnosticsGateway = {
      diagnostics: async () => {
        throw new GatewayError({
          kind: "network",
          message: "No Gateway answered at this address.",
          retryable: true,
        })
      },
    }
    renderPage(client)

    expect(
      await screen.findByText("Diagnostics unavailable"),
    ).toBeInTheDocument()
    expect(
      screen.getByText("No Gateway answered at this address."),
    ).toBeInTheDocument()
    expect(screen.getByText(/network/)).toBeInTheDocument()
  })

  it("names the empty Worker state honestly", async () => {
    renderPage({ diagnostics: vi.fn() }, [])

    expect(
      await screen.findByText("No Worker is visible to this caller"),
    ).toBeInTheDocument()
    expect(
      screen.getByText(/diagnostics cannot be requested/i),
    ).toBeInTheDocument()
  })

  it("lets the operator choose another visible Worker", async () => {
    const second: WorkerSummary = {
      ...worker,
      id: "worker-2",
      instance: "worker-b",
      package: "research",
    }
    const diagnostics = vi.fn(async () => bundle)
    const { user } = renderPage({ diagnostics }, [worker, second])
    await screen.findByText("agent_tasks_active")

    await user.selectOptions(
      screen.getByLabelText("Worker"),
      within(screen.getByLabelText("Worker")).getByRole("option", {
        name: /research/,
      }),
    )

    await waitFor(() =>
      expect(diagnostics).toHaveBeenCalledWith("worker-2", undefined),
    )
  })
})

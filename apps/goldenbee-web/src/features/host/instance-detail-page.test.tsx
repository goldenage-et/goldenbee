import { render, screen, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { MemoryRouter, Route, Routes } from "react-router-dom"
import { describe, expect, it, vi } from "vitest"

import { GatewayClientProvider } from "@/features/connection/gateway-client-provider"
import { InstanceDetailPage } from "@/features/host/instance-detail-page"
import { fakeGatewayClient } from "@/test/fake-gateway-client"
import {
  GatewayError,
  type GatewayClient,
  type HostInstanceView,
  type HostOperationOutcome,
  type HostRollbackRequest,
  type HostUpgradeRequest,
} from "@goldenbee/sdk"

const instance: HostInstanceView = {
  host: "local",
  instance: "general-a",
  package: "general",
  version: "0.4.1",
  artifact_digest: "sha256:1111aaaa",
  binary_digest: "sha256:9999ffff",
  signer: "goldenbee-release",
  target: "x86_64-unknown-linux-gnu",
  protocol: "v1",
  lifecycle: "running",
  process_health: "healthy",
  task_activity: "idle",
  active_profile: "default:1",
  endpoint: "http://127.0.0.1:9101",
  grants: ["task:submit"],
  limits: { max_active: 4, max_queue: 8, per_principal: 2 },
  workspace_write: false,
  operator_config: true,
  trust: { decision: "continue", reason: "signature verified" },
  compatibility: {
    target: "x86_64-unknown-linux-gnu",
    protocol: "v1",
    runtime: "0.4.1",
    adk: "2.3.0",
    compatible: true,
    issues: [],
  },
}

function renderDetail(client: GatewayClient) {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
      mutations: { retry: false },
    },
  })

  return render(
    <QueryClientProvider client={queryClient}>
      <GatewayClientProvider client={client}>
        <MemoryRouter initialEntries={["/instances/general-a"]}>
          <Routes>
            <Route path="/instances/:instance" element={<InstanceDetailPage />} />
          </Routes>
        </MemoryRouter>
      </GatewayClientProvider>
    </QueryClientProvider>,
  )
}

async function openControl(
  user: ReturnType<typeof userEvent.setup>,
  control: "Start" | "Stop" | "Restart" | "Upgrade" | "Rollback",
) {
  await user.click(screen.getByRole("button", { name: control }))
  return screen.findByRole("dialog")
}

describe("instance detail page", () => {
  it("shows lifecycle, health, endpoint, digest, compatibility, and logs", async () => {
    const client = fakeGatewayClient({
      localInstance: vi.fn(async () => ({
        host: "local",
        view: instance,
      })),
      instanceLogs: vi.fn(async () => ({
        instance: "general-a",
        lines: ["worker started", "lease renewed"],
        truncated: false,
      })),
    })

    renderDetail(client)

    expect(await screen.findByRole("heading", { name: "general-a" })).toBeInTheDocument()
    expect(screen.getAllByText("running").length).toBeGreaterThan(0)
    expect(screen.getByText("healthy")).toBeInTheDocument()
    expect(screen.getByText("http://127.0.0.1:9101")).toBeInTheDocument()
    expect(screen.getByText("sha256:1111aaaa")).toBeInTheDocument()
    expect(screen.getByText("Compatible")).toBeInTheDocument()
    expect(screen.getByText(/signature verified/)).toBeInTheDocument()
    expect(await screen.findByText(/worker started/)).toBeInTheDocument()
    expect(screen.getByText(/lease renewed/)).toBeInTheDocument()
  })

  it("starts, stops, and restarts only after a visible confirmation", async () => {
    const startInstance = vi.fn(async () => ({
      host: "local",
      view: instance,
    }))
    const stopInstance = vi.fn(async () => ({
      host: "local",
      view: { ...instance, lifecycle: "stopped" },
    }))
    const restartInstance = vi.fn(async () => ({
      host: "local",
      view: instance,
    }))
    const client = fakeGatewayClient({
      localInstance: vi.fn(async () => ({ host: "local", view: instance })),
      instanceLogs: vi.fn(async () => ({
        instance: "general-a",
        lines: [],
      })),
      startInstance,
      stopInstance,
      restartInstance,
    })

    renderDetail(client)
    const user = userEvent.setup()

    await screen.findByRole("heading", { name: "general-a" })

    let dialog = await openControl(user, "Start")
    expect(
      within(dialog).getByText(/privileged action/i),
    ).toBeInTheDocument()
    await user.click(within(dialog).getByRole("button", { name: "Start" }))
    expect(await screen.findByText("Instance started")).toBeInTheDocument()
    expect(startInstance).toHaveBeenCalledWith("general-a")

    dialog = await openControl(user, "Stop")
    await user.click(within(dialog).getByRole("button", { name: "Stop" }))
    expect(await screen.findByText("Instance stopped")).toBeInTheDocument()
    expect(stopInstance).toHaveBeenCalledWith("general-a")

    dialog = await openControl(user, "Restart")
    await user.click(within(dialog).getByRole("button", { name: "Restart" }))
    expect(await screen.findByText("Instance restarted")).toBeInTheDocument()
    expect(restartInstance).toHaveBeenCalledWith("general-a")
  })

  it("upgrades with one stable operation id across an uncertain retry", async () => {
    const upgradeLocalInstance = vi
      .fn<
        (name: string, request: HostUpgradeRequest) => Promise<HostOperationOutcome>
      >()
      .mockRejectedValueOnce(
        new GatewayError({
          kind: "timeout",
          message: "the upgrade response timed out",
          retryable: true,
        }),
      )
      .mockResolvedValueOnce({
        operation_id: "upgrade-test",
        kind: "upgrade",
        host: "local",
        instance: "general-a",
        status: "succeeded",
        view: instance,
      })
    const client = fakeGatewayClient({
      localInstance: vi.fn(async () => ({ host: "local", view: instance })),
      instanceLogs: vi.fn(async () => ({ instance: "general-a", lines: [] })),
      upgradeLocalInstance,
    })

    renderDetail(client)
    const user = userEvent.setup()

    await screen.findByRole("heading", { name: "general-a" })
    const dialog = await openControl(user, "Upgrade")
    await user.type(
      within(dialog).getByLabelText("Release digest"),
      "sha256:2222bbbb",
    )
    await user.click(within(dialog).getByRole("button", { name: "Upgrade" }))

    expect(await screen.findByText("Outcome uncertain")).toBeInTheDocument()
    await user.click(
      screen.getByRole("button", { name: "Retry upgrade" }),
    )

    expect(upgradeLocalInstance).toHaveBeenCalledTimes(2)
    const first = upgradeLocalInstance.mock.calls[0]!
    const second = upgradeLocalInstance.mock.calls[1]!
    expect(first[0]).toBe("general-a")
    expect(second[0]).toBe("general-a")
    expect(second[1]).toEqual(first[1])
    expect(first[1]).toMatchObject({
      release_digest: "sha256:2222bbbb",
      work_policy: "drain",
    })
    expect(first[1].operation_id).toMatch(/^upgrade-/)
    expect(await screen.findByText("Instance upgraded")).toBeInTheDocument()
  })

  it("rolls back only after confirmation with a rollback operation id", async () => {
    const rollbackLocalInstance = vi.fn<
      (name: string, request: HostRollbackRequest) => Promise<HostOperationOutcome>
    >()
    rollbackLocalInstance.mockResolvedValue({
      operation_id: "rollback-test",
      kind: "rollback",
      host: "local",
      instance: "general-a",
      status: "succeeded",
      view: instance,
    })
    const client = fakeGatewayClient({
      localInstance: vi.fn(async () => ({ host: "local", view: instance })),
      instanceLogs: vi.fn(async () => ({ instance: "general-a", lines: [] })),
      rollbackLocalInstance,
    })

    renderDetail(client)
    const user = userEvent.setup()

    await screen.findByRole("heading", { name: "general-a" })
    const dialog = await openControl(user, "Rollback")
    expect(
      within(dialog).getByText(/restore the previous release/i),
    ).toBeInTheDocument()
    await user.click(within(dialog).getByRole("button", { name: "Roll back" }))

    expect(rollbackLocalInstance).toHaveBeenCalledTimes(1)
    const [name, request] = rollbackLocalInstance.mock.calls[0]!
    expect(name).toBe("general-a")
    expect(request.operation_id).toMatch(/^rollback-/)
    expect(await screen.findByText("Instance rolled back")).toBeInTheDocument()
  })

  it("surfaces a scope denial for a privileged control", async () => {
    const client = fakeGatewayClient({
      localInstance: vi.fn(async () => ({ host: "local", view: instance })),
      instanceLogs: vi.fn(async () => ({ instance: "general-a", lines: [] })),
      stopInstance: vi.fn(async () => {
        throw new GatewayError({
          kind: "forbidden",
          status: 403,
          code: "scope.denied",
          message: "caller is missing instance:admin",
          retryable: false,
        })
      }),
    })

    renderDetail(client)
    const user = userEvent.setup()

    await screen.findByRole("heading", { name: "general-a" })
    const dialog = await openControl(user, "Stop")
    await user.click(within(dialog).getByRole("button", { name: "Stop" }))

    expect(await screen.findByText("Access denied")).toBeInTheDocument()
    expect(screen.getByText(/missing instance:admin/i)).toBeInTheDocument()
  })
})

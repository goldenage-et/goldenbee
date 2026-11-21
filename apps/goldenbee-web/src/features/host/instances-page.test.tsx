import { render, screen, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { MemoryRouter } from "react-router-dom"
import { describe, expect, it, vi } from "vitest"

import { GatewayClientProvider } from "@/features/connection/gateway-client-provider"
import { InstancesPage } from "@/features/host/instances-page"
import { fakeGatewayClient } from "@/test/fake-gateway-client"
import {
  GatewayError,
  type GatewayClient,
  type HostInstanceView,
  type HostOperationOutcome,
  type HostProvisionRequest,
  type PackageSummary,
} from "@goldenbee/sdk"

const packages: PackageSummary[] = [
  {
    package: "general",
    version: "0.4.1",
    artifact_digest: "sha256:1111aaaa",
    signer: "goldenbee-release",
    target: "x86_64-unknown-linux-gnu",
  },
]

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
  compatibility: {
    target: "x86_64-unknown-linux-gnu",
    protocol: "v1",
    runtime: "0.4.1",
    adk: "2.3.0",
    compatible: true,
    issues: [],
  },
}

function renderInstances(client: GatewayClient) {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
      mutations: { retry: false },
    },
  })

  return render(
    <QueryClientProvider client={queryClient}>
      <GatewayClientProvider client={client}>
        <MemoryRouter>
          <InstancesPage />
        </MemoryRouter>
      </GatewayClientProvider>
    </QueryClientProvider>,
  )
}

async function openDeployDialog(
  user: ReturnType<typeof userEvent.setup>,
  instanceName = "general-b",
) {
  await user.click(screen.getByRole("button", { name: "Deploy instance" }))
  const dialog = await screen.findByRole("dialog")
  await user.type(within(dialog).getByLabelText("Instance name"), instanceName)
  await user.type(within(dialog).getByLabelText("Worker id"), instanceName)
  await user.selectOptions(
    within(dialog).getByLabelText("Release digest"),
    "sha256:1111aaaa",
  )
  return dialog
}

describe("instances page", () => {
  it("lists instances with lifecycle, health, endpoint, digest, and compatibility", async () => {
    const client = fakeGatewayClient({
      packages: vi.fn(async () => ({ packages })),
      instances: vi.fn(async () => [instance]),
    })

    renderInstances(client)

    expect(await screen.findByText("general-a")).toBeInTheDocument()
    expect(screen.getByText("running")).toBeInTheDocument()
    expect(screen.getByText("healthy")).toBeInTheDocument()
    expect(screen.getByText("http://127.0.0.1:9101")).toBeInTheDocument()
    expect(screen.getByText("sha256:1111aaaa")).toBeInTheDocument()
    expect(screen.getByText("Compatible")).toBeInTheDocument()
  })

  it("shows compatibility issues for an incompatible instance", async () => {
    const client = fakeGatewayClient({
      packages: vi.fn(async () => ({ packages })),
      instances: vi.fn(async () => [
        {
          ...instance,
          compatibility: {
            ...instance.compatibility,
            compatible: false,
            issues: ["runtime protocol v2 is not supported"],
          },
        },
      ]),
    })

    renderInstances(client)

    expect(await screen.findByText("Incompatible")).toBeInTheDocument()
    expect(
      screen.getByText("runtime protocol v2 is not supported"),
    ).toBeInTheDocument()
  })

  it("explains an unconfigured local host instead of an empty instance list", async () => {
    const client = fakeGatewayClient({
      packages: vi.fn(async () => ({ packages })),
      instances: vi.fn(async () => {
        throw new GatewayError({
          kind: "http",
          status: 400,
          code: "host.not_configured",
          message: "this Gateway does not manage a local host",
          retryable: false,
          recovery: "enable instances_root and packages_root",
        })
      }),
    })

    renderInstances(client)

    expect(
      await screen.findByText("Local host is not configured"),
    ).toBeInTheDocument()
    expect(screen.queryByText("No instances")).not.toBeInTheDocument()
  })

  it("deploys an instance with digest, identity, profile, and grants", async () => {
    const provisionLocalInstance = vi.fn<
      (request: HostProvisionRequest) => Promise<HostOperationOutcome>
    >()
    provisionLocalInstance.mockResolvedValue({
      operation_id: "provision-test",
      kind: "provision",
      host: "local",
      instance: "general-b",
      status: "succeeded",
      view: { ...instance, instance: "general-b" },
    })
    const client = fakeGatewayClient({
      packages: vi.fn(async () => ({ packages })),
      instances: vi.fn(async () => [instance]),
      provisionLocalInstance,
    })

    renderInstances(client)
    const user = userEvent.setup()

    await screen.findByText("general-a")
    const dialog = await openDeployDialog(user)
    await user.clear(within(dialog).getByLabelText("Profile name"))
    await user.type(within(dialog).getByLabelText("Profile name"), "operator")
    await user.type(
      within(dialog).getByLabelText("Grants"),
      "task:submit, task:read",
    )
    await user.click(within(dialog).getByRole("button", { name: "Deploy" }))

    expect(provisionLocalInstance).toHaveBeenCalledTimes(1)
    const request = provisionLocalInstance.mock.calls[0]![0]
    expect(request).toMatchObject({
      instance: "general-b",
      worker: "general-b",
      release_digest: "sha256:1111aaaa",
      grants: ["task:submit", "task:read"],
      profile: { name: "operator" },
    })
    expect(request.operation_id).toMatch(/^provision-/)
  })

  it("keeps one operation id when an uncertain deployment is retried", async () => {
    const provisionLocalInstance = vi
      .fn<
        (request: HostProvisionRequest) => Promise<HostOperationOutcome>
      >()
      .mockRejectedValueOnce(
        new GatewayError({
          kind: "network",
          message: "fetch failed after the request was sent",
          retryable: true,
        }),
      )
      .mockResolvedValueOnce({
        operation_id: "provision-test",
        kind: "provision",
        host: "local",
        instance: "general-b",
        status: "succeeded",
      })
    const client = fakeGatewayClient({
      packages: vi.fn(async () => ({ packages })),
      instances: vi.fn(async () => [instance]),
      provisionLocalInstance,
    })

    renderInstances(client)
    const user = userEvent.setup()

    await screen.findByText("general-a")
    const dialog = await openDeployDialog(user)
    await user.click(within(dialog).getByRole("button", { name: "Deploy" }))

    expect(await screen.findByText("Outcome uncertain")).toBeInTheDocument()
    expect(
      screen.getByRole("button", { name: "Retry deployment" }),
    ).toBeInTheDocument()

    await user.click(screen.getByRole("button", { name: "Retry deployment" }))

    expect(provisionLocalInstance).toHaveBeenCalledTimes(2)
    const first = provisionLocalInstance.mock.calls[0]![0]
    const second = provisionLocalInstance.mock.calls[1]![0]
    expect(second.operation_id).toBe(first.operation_id)
    expect(second).toEqual(first)
  })

  it("reports an operation conflict without retrying with a new identity", async () => {
    const client = fakeGatewayClient({
      packages: vi.fn(async () => ({ packages })),
      instances: vi.fn(async () => [instance]),
      provisionLocalInstance: vi.fn(async () => {
        throw new GatewayError({
          kind: "conflict",
          status: 409,
          code: "host.operation_conflict",
          message: "operation id belongs to a different request",
          retryable: false,
        })
      }),
    })

    renderInstances(client)
    const user = userEvent.setup()

    await screen.findByText("general-a")
    const dialog = await openDeployDialog(user)
    await user.click(within(dialog).getByRole("button", { name: "Deploy" }))

    expect(await screen.findByText("Operation conflict")).toBeInTheDocument()
    expect(
      screen.getByText(/operation id belongs to a different request/i),
    ).toBeInTheDocument()
    expect(
      screen.queryByRole("button", { name: "Retry deployment" }),
    ).not.toBeInTheDocument()
  })

  it("surfaces a scope denial for deployment", async () => {
    const client = fakeGatewayClient({
      packages: vi.fn(async () => ({ packages })),
      instances: vi.fn(async () => [instance]),
      provisionLocalInstance: vi.fn(async () => {
        throw new GatewayError({
          kind: "forbidden",
          status: 403,
          code: "scope.denied",
          message: "caller is missing instance:admin",
          retryable: false,
          recovery: "ask the operator for instance:admin",
        })
      }),
    })

    renderInstances(client)
    const user = userEvent.setup()

    await screen.findByText("general-a")
    const dialog = await openDeployDialog(user)
    await user.click(within(dialog).getByRole("button", { name: "Deploy" }))

    expect(await screen.findByText("Access denied")).toBeInTheDocument()
    expect(screen.getByText(/missing instance:admin/i)).toBeInTheDocument()
  })
})

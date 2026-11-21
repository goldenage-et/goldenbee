import { render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { MemoryRouter } from "react-router-dom"
import { describe, expect, it, vi } from "vitest"

import { GatewayClientProvider } from "@/features/connection/gateway-client-provider"
import { PackagesPage } from "@/features/host/packages-page"
import { fakeGatewayClient } from "@/test/fake-gateway-client"
import {
  GatewayError,
  type GatewayClient,
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
  {
    package: "effect-fixture",
    version: "0.2.0",
    artifact_digest: "sha256:2222bbbb",
    signer: "goldenbee-release",
    target: "aarch64-apple-darwin",
  },
]

function renderPackages(client: GatewayClient) {
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
          <PackagesPage />
        </MemoryRouter>
      </GatewayClientProvider>
    </QueryClientProvider>,
  )
}

describe("packages page", () => {
  it("lists installed packages with version, digest, signer, and target", async () => {
    const client = fakeGatewayClient({
      packages: vi.fn(async () => ({ packages })),
    })

    renderPackages(client)

    expect(await screen.findByText("general")).toBeInTheDocument()
    expect(screen.getByText("0.4.1")).toBeInTheDocument()
    expect(screen.getByText("sha256:1111aaaa")).toBeInTheDocument()
    expect(
      screen.getAllByText("goldenbee-release").length,
    ).toBeGreaterThan(0)
    expect(screen.getByText("x86_64-unknown-linux-gnu")).toBeInTheDocument()
    expect(screen.getByText("effect-fixture")).toBeInTheDocument()
    expect(screen.getByText("aarch64-apple-darwin")).toBeInTheDocument()
  })

  it("explains an unconfigured local host instead of showing an empty list", async () => {
    const client = fakeGatewayClient({
      packages: vi.fn(async () => {
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

    renderPackages(client)

    expect(
      await screen.findByText("Local host is not configured"),
    ).toBeInTheDocument()
    expect(
      screen.getByText(/does not manage a local host/i),
    ).toBeInTheDocument()
    expect(screen.queryByText("No packages installed")).not.toBeInTheDocument()
  })

  it("shows a scope-denied state without claiming the list is empty", async () => {
    const client = fakeGatewayClient({
      packages: vi.fn(async () => {
        throw new GatewayError({
          kind: "forbidden",
          status: 403,
          code: "scope.denied",
          message: "caller is missing instance:read",
          retryable: false,
          recovery: "ask the operator for instance:read",
        })
      }),
    })

    renderPackages(client)

    expect(await screen.findByText("Access denied")).toBeInTheDocument()
    expect(
      screen.getByText(/missing instance:read/i),
    ).toBeInTheDocument()
    expect(screen.queryByText("No packages installed")).not.toBeInTheDocument()
  })

  it("installs from a URL by default and reports package facts", async () => {
    const installPackage = vi.fn(async () => ({
      package: packages[0]!,
      installed_now: true,
    }))
    const client = fakeGatewayClient({
      packages: vi.fn(async () => ({ packages: [] })),
      installPackage,
    })

    renderPackages(client)
    const user = userEvent.setup()

    await screen.findByText("No packages installed")
    await user.type(
      screen.getByLabelText("Package URL"),
      "https://releases.example.com/general.tar.gz",
    )
    await user.click(screen.getByRole("button", { name: "Install package" }))
    await user.click(screen.getByRole("button", { name: "Install" }))

    expect(installPackage).toHaveBeenCalledWith({
      source: {
        kind: "url",
        url: "https://releases.example.com/general.tar.gz",
      },
    })
    expect(await screen.findByText("Package installed")).toBeInTheDocument()
    expect(screen.getByText(/sha256:1111aaaa/)).toBeInTheDocument()
    expect(screen.getByText(/signer: goldenbee-release/)).toBeInTheDocument()
  })

  it("does not report install success when the outcome is uncertain", async () => {
    const installPackage = vi
      .fn()
      .mockRejectedValueOnce(
        new GatewayError({
          kind: "network",
          message: "connection closed after the install request",
          retryable: true,
        }),
      )
      .mockResolvedValueOnce({ package: packages[0]!, installed_now: true })
    const client = fakeGatewayClient({
      packages: vi.fn(async () => ({ packages: [] })),
      installPackage,
    })

    renderPackages(client)
    const user = userEvent.setup()

    await screen.findByText("No packages installed")
    await user.type(
      screen.getByLabelText("Package URL"),
      "https://releases.example.com/general.tar.gz",
    )
    await user.click(screen.getByRole("button", { name: "Install package" }))
    await user.click(screen.getByRole("button", { name: "Install" }))

    expect(await screen.findByText("Outcome uncertain")).toBeInTheDocument()
    expect(screen.queryByText("Package installed")).not.toBeInTheDocument()

    await user.click(screen.getByRole("button", { name: "Retry install" }))

    expect(installPackage).toHaveBeenCalledTimes(2)
    expect(installPackage.mock.calls[1]![0]).toEqual(
      installPackage.mock.calls[0]![0],
    )
    expect(await screen.findByText("Package installed")).toBeInTheDocument()
  })

  it("uses a Gateway host folder only through the explicit operator field", async () => {
    const installPackage = vi.fn(async () => ({
      package: packages[0]!,
      installed_now: false,
    }))
    const client = fakeGatewayClient({
      packages: vi.fn(async () => ({ packages })),
      installPackage,
    })

    renderPackages(client)
    const user = userEvent.setup()

    await screen.findByText("general")
    expect(screen.queryByLabelText("Gateway host path")).not.toBeInTheDocument()

    await user.click(screen.getByRole("radio", { name: "Gateway host folder" }))

    expect(screen.getByLabelText("Gateway host path")).toBeInTheDocument()
    expect(
      screen.getByText(/resolved on the Gateway host/i),
    ).toBeInTheDocument()

    await user.type(
      screen.getByLabelText("Gateway host path"),
      "/srv/goldenbee/releases/general",
    )
    await user.click(screen.getByRole("button", { name: "Install package" }))
    await user.click(screen.getByRole("button", { name: "Install" }))

    expect(installPackage).toHaveBeenCalledWith({
      source: {
        kind: "folder",
        path: "/srv/goldenbee/releases/general",
      },
    })
    expect(
      await screen.findByText("Already installed"),
    ).toBeInTheDocument()
  })
})

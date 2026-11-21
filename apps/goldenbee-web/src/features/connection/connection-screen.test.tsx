import { render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { describe, expect, it } from "vitest"

import { App } from "@/App"
import {
  createConnectionStore,
  type ConnectionClient,
} from "@/features/connection/connection-store"
import { createTokenStore } from "@/features/connection/token-store"
import { GatewayError, type ConnectionResult } from "@goldenbee/sdk"

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

const health = { status: "ok", workers: 2, reachable: 1 }
const workers = [
  {
    id: "worker-1",
    instance: "default",
    package: "general",
    version: "0.1.0",
    status: "approved" as const,
    reachable: true,
    locality: "local" as const,
  },
]

function renderApp(result: ConnectionResult) {
  const calls: Array<{ baseUrl: string; token: string }> = []
  const store = createConnectionStore({
    tokenStore: createTokenStore({ sessionStorage: memoryStorage() }),
    defaultUrl: "http://127.0.0.1:8080",
    createClient: (options): ConnectionClient => {
      calls.push(options)
      return { checkConnection: async () => result }
    },
  })
  render(<App store={store} />)
  return { calls, store, user: userEvent.setup() }
}

async function submitCredentials(
  user: ReturnType<typeof userEvent.setup>,
  url = "http://127.0.0.1:8080",
  token = "caller-token",
) {
  await user.clear(screen.getByLabelText("Gateway URL"))
  await user.type(screen.getByLabelText("Gateway URL"), url)
  await user.clear(screen.getByLabelText("Caller token"))
  await user.type(screen.getByLabelText("Caller token"), token)
  await user.click(screen.getByRole("button", { name: "Connect" }))
}

describe("connection screen", () => {
  it("connects with entered credentials and shows Gateway health", async () => {
    const { user, calls } = renderApp({ status: "connected", health, workers })

    await submitCredentials(user)

    expect(await screen.findByText("Gateway connected")).toBeInTheDocument()
    expect(calls).toEqual([
      { baseUrl: "http://127.0.0.1:8080", token: "caller-token" },
    ])
    expect(screen.getByText("2")).toBeInTheDocument()
    expect(screen.getByText("1")).toBeInTheDocument()
    expect(screen.getByText("worker-1")).toBeInTheDocument()
    expect(screen.getByText("In memory for this tab")).toBeInTheDocument()
    expect(screen.getByText("Gateway health verified")).toBeInTheDocument()
    expect(
      screen.getByText("Caller token accepted · 1 Worker visible"),
    ).toBeInTheDocument()
  })

  it("keeps the caller token out of plain view", () => {
    renderApp({ status: "connected", health, workers })

    expect(screen.getByLabelText("Caller token")).toHaveAttribute(
      "type",
      "password",
    )
  })

  it("shows a loading state while the connection check runs", async () => {
    let finish: (result: ConnectionResult) => void = () => undefined
    const pending = new Promise<ConnectionResult>((resolve) => {
      finish = resolve
    })
    const store = createConnectionStore({
      tokenStore: createTokenStore({ sessionStorage: memoryStorage() }),
      defaultUrl: "http://127.0.0.1:8080",
      createClient: () => ({ checkConnection: () => pending }),
    })
    const user = userEvent.setup()
    render(<App store={store} />)

    await submitCredentials(user)

    expect(screen.getByRole("button", { name: /Connecting/ })).toBeDisabled()
    expect(screen.getByText("Checking Gateway health and caller token.")).toBeInTheDocument()

    finish({ status: "connected", health, workers })
    expect(await screen.findByText("Gateway connected")).toBeInTheDocument()
  })

  it("explains a rejected caller token", async () => {
    const { user } = renderApp({
      status: "unauthorized",
      health,
      error: new GatewayError({
        kind: "unauthorized",
        status: 401,
        message: "token is not recognized by this Gateway",
        code: "auth.unauthenticated",
        recovery: "check the caller token",
      }),
    })

    await submitCredentials(user)

    expect(await screen.findByText("Caller token rejected")).toBeInTheDocument()
    expect(
      screen.getByText("token is not recognized by this Gateway"),
    ).toBeInTheDocument()
    expect(screen.getByText("check the caller token")).toBeInTheDocument()
    expect(screen.getByText("Unauthorized")).toBeInTheDocument()
  })

  it("explains an unreachable Gateway", async () => {
    const { user } = renderApp({
      status: "unreachable",
      error: new GatewayError({
        kind: "network",
        message: "The Gateway could not be reached: fetch failed",
        retryable: true,
      }),
    })

    await submitCredentials(user)

    expect(await screen.findByText("Gateway unreachable")).toBeInTheDocument()
    expect(
      screen.getByText("The Gateway could not be reached: fetch failed"),
    ).toBeInTheDocument()
    expect(screen.getByText("Unreachable")).toBeInTheDocument()
  })

  it("rejects an invalid Gateway URL without calling Gateway", async () => {
    const { user, calls } = renderApp({
      status: "connected",
      health,
      workers,
    })

    await submitCredentials(user, "gateway.local")

    expect(
      await screen.findByText("Gateway URL must start with http:// or https://."),
    ).toBeInTheDocument()
    expect(calls).toEqual([])
  })

  it("marks a remembered token as saved for this tab", async () => {
    const { user } = renderApp({ status: "connected", health, workers })

    await user.click(screen.getByLabelText("Remember token for this tab"))
    await submitCredentials(user)

    expect(await screen.findByText("Saved for this tab")).toBeInTheDocument()
  })

  it("adds package and instance navigation after connecting", async () => {
    const { user } = renderApp({ status: "connected", health, workers })

    await submitCredentials(user)

    expect(await screen.findByRole("link", { name: "Packages" })).toHaveAttribute(
      "href",
      "#/packages",
    )
    expect(screen.getByRole("link", { name: "Instances" })).toHaveAttribute(
      "href",
      "#/instances",
    )
  })
})

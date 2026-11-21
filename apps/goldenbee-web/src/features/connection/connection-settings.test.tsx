import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { describe, expect, it } from "vitest"

import { ConnectionSettingsPanel } from "@/features/connection/connection-settings"
import {
  createConnectionStore,
  type ConnectionStore,
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

const connected: ConnectionResult = {
  status: "connected",
  health: { status: "ok", workers: 1, reachable: 1 },
  workers: [],
}

function setup(result: ConnectionResult = connected) {
  const sessionStorage = memoryStorage()
  const calls: Array<{ baseUrl: string; token: string }> = []
  const store: ConnectionStore = createConnectionStore({
    tokenStore: createTokenStore({ sessionStorage }),
    defaultUrl: "http://gateway.test",
    createClient: (options) => {
      calls.push(options)
      return { checkConnection: async () => result }
    },
  })
  store.setState({
    status: "connected",
    gatewayUrl: "http://gateway.test",
    token: "old-token",
    rememberToken: false,
    health: { status: "ok", workers: 1, reachable: 1 },
    workers: [],
    error: null,
    tokenPersisted: false,
  })
  const user = userEvent.setup()
  render(<ConnectionSettingsPanel store={store} />)
  return { store, calls, sessionStorage, user }
}

describe("connection settings", () => {
  it("edits the Gateway URL and caller token without displaying the token", () => {
    setup()

    expect(screen.getByLabelText("Gateway URL")).toHaveValue("http://gateway.test")
    const token = screen.getByLabelText("Caller token")
    expect(token).toHaveAttribute("type", "password")
    expect(token).toHaveValue("old-token")
    expect(document.body.textContent ?? "").not.toContain("old-token")
    expect(screen.getByText("Memory for this tab")).toBeInTheDocument()
  })

  it("reconnects with a new URL and token and keeps the token in memory", async () => {
    const { store, calls, sessionStorage, user } = setup()

    await user.clear(screen.getByLabelText("Gateway URL"))
    await user.type(screen.getByLabelText("Gateway URL"), "http://next-gateway.test")
    await user.clear(screen.getByLabelText("Caller token"))
    await user.type(screen.getByLabelText("Caller token"), "new-token")
    await user.click(screen.getByRole("button", { name: "Reconnect" }))

    await waitFor(() =>
      expect(calls.at(-1)).toEqual({
        baseUrl: "http://next-gateway.test",
        token: "new-token",
      }),
    )
    expect(store.getState().status).toBe("connected")
    expect(store.getState().token).toBe("new-token")
    expect(store.getState().tokenPersisted).toBe(false)
    expect(sessionStorage.getItem("goldenbee.caller-token")).toBeNull()
    expect(screen.getByText("Memory for this tab")).toBeInTheDocument()
    expect(document.body.textContent ?? "").not.toContain("new-token")
  })

  it("persists the token only with the explicit current-tab choice", async () => {
    const { store, sessionStorage, user } = setup()

    await user.click(
      screen.getByRole("checkbox", { name: "Remember token for this tab" }),
    )
    await user.click(screen.getByRole("button", { name: "Reconnect" }))

    await waitFor(() => expect(store.getState().tokenPersisted).toBe(true))
    expect(sessionStorage.getItem("goldenbee.caller-token")).toBe("old-token")
    expect(screen.getByText("Saved for this tab")).toBeInTheDocument()
    expect(document.body.textContent ?? "").not.toContain("old-token")
  })

  it("rejects an invalid Gateway URL with a typed field error", async () => {
    const { calls, user } = setup()

    await user.clear(screen.getByLabelText("Gateway URL"))
    await user.type(screen.getByLabelText("Gateway URL"), "ftp://gateway.test")
    await user.click(screen.getByRole("button", { name: "Reconnect" }))

    expect(
      screen.getByText("Gateway URL must use http:// or https://."),
    ).toBeInTheDocument()
    expect(screen.getByLabelText("Gateway URL")).toHaveAttribute(
      "aria-invalid",
      "true",
    )
    expect(calls).toHaveLength(0)
  })

  it("requires a caller token before reconnecting", async () => {
    const { calls, user } = setup()

    await user.clear(screen.getByLabelText("Caller token"))
    await user.click(screen.getByRole("button", { name: "Reconnect" }))

    expect(screen.getByText("Enter the caller token.")).toBeInTheDocument()
    expect(screen.getByLabelText("Caller token")).toHaveAttribute(
      "aria-invalid",
      "true",
    )
    expect(calls).toHaveLength(0)
  })

  it("shows a typed rejection from the Gateway", async () => {
    const { user } = setup({
      status: "unauthorized",
      health: { status: "ok", workers: 1, reachable: 1 },
      error: new GatewayError({
        kind: "unauthorized",
        status: 401,
        code: "auth.unauthenticated",
        message: "token is not recognized by this Gateway",
        recovery: "check the caller token",
        retryable: false,
      }),
    })

    await user.click(screen.getByRole("button", { name: "Reconnect" }))

    expect(
      await screen.findByText("Caller token rejected"),
    ).toBeInTheDocument()
    expect(
      screen.getByText("token is not recognized by this Gateway"),
    ).toBeInTheDocument()
    expect(screen.getByText(/auth\.unauthenticated/)).toBeInTheDocument()
    expect(screen.getByText(/check the caller token/)).toBeInTheDocument()
  })

  it("disconnects and clears the in-memory token", async () => {
    const { store, sessionStorage, user } = setup()

    await user.click(screen.getByRole("button", { name: "Disconnect" }))

    expect(store.getState().status).toBe("idle")
    expect(store.getState().token).toBe("")
    expect(sessionStorage.getItem("goldenbee.caller-token")).toBeNull()
  })
})

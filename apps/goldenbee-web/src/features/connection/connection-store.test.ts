import { describe, expect, it } from "vitest"

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

function harness(result: ConnectionResult) {
  const session = memoryStorage()
  const tokenStore = createTokenStore({ sessionStorage: session })
  const calls: Array<{ baseUrl: string; token: string }> = []
  const store = createConnectionStore({
    tokenStore,
    createClient: (options): ConnectionClient => {
      calls.push(options)
      return { checkConnection: async () => result }
    },
    defaultUrl: "http://127.0.0.1:8080",
  })
  return { store, session, tokenStore, calls }
}

describe("connection store", () => {
  it("connects, records health, and keeps the token in memory", async () => {
    const { store, tokenStore, calls, session } = harness({
      status: "connected",
      health,
      workers,
    })

    await store
      .getState()
      .connect({ gatewayUrl: "http://gateway.test/", token: "token-a" })

    expect(calls).toEqual([
      { baseUrl: "http://gateway.test", token: "token-a" },
    ])
    expect(store.getState()).toMatchObject({
      status: "connected",
      gatewayUrl: "http://gateway.test",
      health,
      workers,
      tokenPersisted: false,
      error: null,
    })
    expect(tokenStore.read()).toBe("token-a")
    expect(tokenStore.readPersistence()).toBe("memory")
    expect(session.length).toBe(0)
  })

  it("uses current-tab storage only when the operator opts in", async () => {
    const { store, tokenStore, session } = harness({
      status: "connected",
      health,
      workers,
    })

    await store.getState().connect({
      gatewayUrl: "http://gateway.test",
      token: "token-a",
      rememberToken: true,
    })

    expect(tokenStore.readPersistence()).toBe("session")
    expect(session.getItem("goldenbee.caller-token")).toBe("token-a")
  })

  it("reports an unauthorized caller and does not persist the token", async () => {
    const error = new GatewayError({
      kind: "unauthorized",
      status: 401,
      message: "token is not recognized by this Gateway",
      code: "auth.unauthenticated",
      recovery: "check the caller token",
    })
    const { store, tokenStore, session } = harness({
      status: "unauthorized",
      health,
      error,
    })

    await store.getState().connect({
      gatewayUrl: "http://gateway.test",
      token: "wrong",
      rememberToken: true,
    })

    expect(store.getState()).toMatchObject({
      status: "unauthorized",
      health,
      error: {
        kind: "unauthorized",
        message: "token is not recognized by this Gateway",
        recovery: "check the caller token",
      },
    })
    expect(tokenStore.read()).toBeNull()
    expect(session.length).toBe(0)
  })

  it("reports an unreachable Gateway without keeping a session token", async () => {
    const error = new GatewayError({
      kind: "network",
      message: "The Gateway could not be reached: fetch failed",
      retryable: true,
    })
    const { store, tokenStore } = harness({ status: "unreachable", error })

    await store.getState().connect({
      gatewayUrl: "http://gateway.test",
      token: "token-a",
      rememberToken: true,
    })

    expect(store.getState()).toMatchObject({
      status: "unreachable",
      error: { kind: "network" },
    })
    expect(tokenStore.read()).toBeNull()
  })

  it("rejects an invalid URL before creating a client", async () => {
    const { store, calls } = harness({ status: "connected", health, workers })

    await store.getState().connect({
      gatewayUrl: "not a url",
      token: "token-a",
    })

    expect(store.getState()).toMatchObject({
      status: "invalid",
      error: { kind: "invalid-url", field: "gatewayUrl" },
    })
    expect(calls).toEqual([])
  })

  it("requires a caller token", async () => {
    const { store, calls } = harness({ status: "connected", health, workers })

    await store.getState().connect({
      gatewayUrl: "http://gateway.test",
      token: "   ",
    })

    expect(store.getState()).toMatchObject({
      status: "invalid",
      error: { message: "Enter the caller token.", field: "token" },
    })
    expect(calls).toEqual([])
  })

  it("clears the token and returns to idle on disconnect", async () => {
    const { store, tokenStore } = harness({
      status: "connected",
      health,
      workers,
    })
    await store.getState().connect({
      gatewayUrl: "http://gateway.test",
      token: "token-a",
      rememberToken: true,
    })

    store.getState().disconnect()

    expect(store.getState()).toMatchObject({
      status: "idle",
      token: "",
      health: null,
      workers: [],
      error: null,
    })
    expect(store.getState().gatewayUrl).toBe("http://gateway.test")
    expect(tokenStore.read()).toBeNull()
  })
})

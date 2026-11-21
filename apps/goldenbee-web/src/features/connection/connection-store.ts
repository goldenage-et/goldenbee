import { createStore, type StoreApi } from "zustand/vanilla"
import { useStore } from "zustand"

import {
  createGatewayClient,
  isGatewayError,
  normalizeGatewayUrl,
  type ConnectionResult,
  type GatewayErrorKind,
  type GatewayHealth,
  type GatewayWorker,
} from "@goldenbee/sdk"

import { createTokenStore, type TokenStore } from "./token-store"

export type ConnectionStatus =
  | "idle"
  | "connecting"
  | "connected"
  | "unauthorized"
  | "forbidden"
  | "unreachable"
  | "error"
  | "invalid"

export interface ConnectionErrorView {
  kind: GatewayErrorKind | "validation"
  message: string
  code?: string
  recovery?: string
  retryable?: boolean
  field?: "gatewayUrl" | "token"
}

export interface ConnectionInput {
  gatewayUrl: string
  token: string
  rememberToken?: boolean
}

export interface ConnectionState {
  status: ConnectionStatus
  gatewayUrl: string
  token: string
  rememberToken: boolean
  health: GatewayHealth | null
  workers: GatewayWorker[]
  error: ConnectionErrorView | null
  tokenPersisted: boolean
  connect(input: ConnectionInput): Promise<void>
  /** Reconnect once from a remembered current-tab token. */
  autoConnect(): Promise<void>
  disconnect(): void
}

/** The slice of the SDK the connection screen needs. */
export interface ConnectionClient {
  checkConnection(): Promise<ConnectionResult>
}

export interface ConnectionDeps {
  tokenStore: TokenStore
  createClient(options: { baseUrl: string; token: string }): ConnectionClient
  defaultUrl?: string
  initialUrl?: string
}

export type ConnectionStore = StoreApi<ConnectionState>

export function defaultGatewayUrl(): string {
  const configured = import.meta.env.VITE_GATEWAY_URL
  if (configured) {
    return configured
  }
  if (typeof window !== "undefined") {
    return window.location.origin
  }
  return "http://127.0.0.1:8080"
}

function errorView(cause: unknown): ConnectionErrorView {
  if (isGatewayError(cause)) {
    return {
      kind: cause.kind,
      message: cause.message,
      code: cause.code,
      recovery: cause.recovery,
      retryable: cause.retryable,
    }
  }
  return {
    kind: "network",
    message:
      cause instanceof Error && cause.message
        ? cause.message
        : "The Gateway connection failed.",
  }
}

export function createConnectionStore(deps: ConnectionDeps): ConnectionStore {
  const initialToken = deps.tokenStore.read() ?? ""
  let generation = 0

  return createStore<ConnectionState>((set, get) => ({
    status: "idle",
    gatewayUrl: deps.initialUrl ?? deps.defaultUrl ?? defaultGatewayUrl(),
    token: initialToken,
    rememberToken: deps.tokenStore.readPersistence() === "session",
    health: null,
    workers: [],
    error: null,
    tokenPersisted: deps.tokenStore.readPersistence() === "session",

    async connect(input) {
      const token = input.token.trim()
      const rememberToken = input.rememberToken ?? false
      const attempt = ++generation

      if (!token) {
        set({
          status: "invalid",
          gatewayUrl: input.gatewayUrl,
          token,
          rememberToken,
          health: null,
          workers: [],
          error: {
            kind: "validation",
            message: "Enter the caller token.",
            field: "token",
          },
          tokenPersisted: false,
        })
        return
      }

      let baseUrl: string
      try {
        baseUrl = normalizeGatewayUrl(input.gatewayUrl)
      } catch (cause) {
        set({
          status: "invalid",
          gatewayUrl: input.gatewayUrl,
          token,
          rememberToken,
          health: null,
          workers: [],
          error: { ...errorView(cause), field: "gatewayUrl" },
          tokenPersisted: false,
        })
        return
      }

      // Memory first: an attempt replaces the previous tab token, and only a
      // verified caller token is written back to sessionStorage.
      deps.tokenStore.write(token, "memory")
      set({
        status: "connecting",
        gatewayUrl: baseUrl,
        token,
        rememberToken,
        health: null,
        workers: [],
        error: null,
        tokenPersisted: false,
      })

      let result: ConnectionResult
      try {
        result = await deps
          .createClient({ baseUrl, token })
          .checkConnection()
      } catch (cause) {
        if (attempt !== generation) {
          return
        }
        deps.tokenStore.clear()
        set({
          status: "error",
          error: errorView(cause),
          tokenPersisted: false,
        })
        return
      }

      if (attempt !== generation) {
        return
      }

      switch (result.status) {
        case "connected": {
          const write = deps.tokenStore.write(
            token,
            rememberToken ? "session" : "memory",
          )
          set({
            status: "connected",
            health: result.health,
            workers: result.workers,
            error: null,
            tokenPersisted: write.persisted,
          })
          return
        }
        case "unauthorized":
        case "forbidden":
        case "error":
          deps.tokenStore.clear()
          set({
            status: result.status,
            health: result.health,
            workers: [],
            error: errorView(result.error),
            tokenPersisted: false,
          })
          return
        case "unreachable":
          deps.tokenStore.clear()
          set({
            status: "unreachable",
            health: null,
            workers: [],
            error: errorView(result.error),
            tokenPersisted: false,
          })
          return
      }
    },

    async autoConnect() {
      const state = get()
      if (
        state.status !== "idle" ||
        !state.token ||
        !state.rememberToken
      ) {
        return
      }
      await state.connect({
        gatewayUrl: state.gatewayUrl,
        token: state.token,
        rememberToken: true,
      })
    },

    disconnect() {
      generation += 1
      deps.tokenStore.clear()
      set({
        status: "idle",
        token: "",
        rememberToken: false,
        health: null,
        workers: [],
        error: null,
        tokenPersisted: false,
      })
    },
  }))
}

function browserSessionStorage(): Storage | null {
  try {
    return typeof window === "undefined" ? null : window.sessionStorage
  } catch {
    return null
  }
}

function defaultCreateClient(options: {
  baseUrl: string
  token: string
}): ConnectionClient {
  return createGatewayClient({
    ...options,
    // The SDK is platform-neutral; the browser supplies its own fetch and
    // WebSocket implementations.
    fetch: (input, init) => fetch(input, init),
    webSocket: (url) => new WebSocket(url),
  })
}

export const connectionStore = createConnectionStore({
  tokenStore: createTokenStore({ sessionStorage: browserSessionStorage() }),
  createClient: defaultCreateClient,
})

export function useConnection<T>(
  selector: (state: ConnectionState) => T,
  store: ConnectionStore = connectionStore,
): T {
  return useStore(store, selector)
}

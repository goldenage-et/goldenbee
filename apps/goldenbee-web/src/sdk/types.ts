import type { GatewayError } from "./errors"

/** Wire types shared by the Gateway client. gateway-v1.json is authoritative. */

export interface GatewayHealth {
  status: string
  workers: number
  reachable: number
}

export interface GatewayCapability {
  id: string
  kind: "agent" | "tool"
  description: string
  permissions?: string[]
}

export interface GatewayWorker {
  id: string
  instance: string
  package: string
  version: string
  status:
    | "approved"
    | "registered"
    | "unreachable"
    | "disabled"
    | "decommissioned"
  reachable: boolean
  locality: "local" | "remote"
  capabilities?: GatewayCapability[]
}

export interface GatewayRequestOptions {
  signal?: AbortSignal
}

export type ConnectionResult =
  | {
      status: "connected"
      health: GatewayHealth
      workers: GatewayWorker[]
    }
  | {
      status: "unauthorized"
      health: GatewayHealth
      error: GatewayError
    }
  | {
      status: "forbidden"
      health: GatewayHealth
      error: GatewayError
    }
  | {
      status: "error"
      health: GatewayHealth
      error: GatewayError
    }
  | {
      status: "unreachable"
      error: GatewayError
    }

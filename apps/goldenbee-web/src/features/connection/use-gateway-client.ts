import { useMemo } from "react"

import { createGatewayClient, type GatewayClient } from "@goldenbee/sdk"

import { useConnection, type ConnectionStore } from "./connection-store"

/** Build one memoized Gateway client for the current authenticated connection. */
export function useGatewayClient(store: ConnectionStore): GatewayClient | null {
  const connected = useConnection((state) => state.status === "connected", store)
  const gatewayUrl = useConnection((state) => state.gatewayUrl, store)
  const token = useConnection((state) => state.token, store)

  return useMemo(() => {
    if (!connected) {
      return null
    }
    return createGatewayClient({
      baseUrl: gatewayUrl,
      token,
      fetch: (input, init) => fetch(input, init),
    })
  }, [connected, gatewayUrl, token])
}

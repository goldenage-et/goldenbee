import { useContext } from "react"

import { GatewayClientContext } from "./gateway-client-context"
import type { GatewayClient } from "@goldenbee/sdk"

/** The connected Gateway client. Management pages never build their own. */
export function useGatewayClient(): GatewayClient {
  const client = useContext(GatewayClientContext)
  if (!client) {
    throw new Error(
      "useGatewayClient must be called inside a GatewayClientProvider",
    )
  }
  return client
}

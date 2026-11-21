import type { ReactNode } from "react"

import { GatewayClientContext } from "./gateway-client-context"
import type { GatewayClient } from "@goldenbee/sdk"

export function GatewayClientProvider({
  client,
  children,
}: {
  client: GatewayClient
  children: ReactNode
}) {
  return (
    <GatewayClientContext.Provider value={client}>
      {children}
    </GatewayClientContext.Provider>
  )
}

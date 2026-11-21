import { createContext } from "react"

import type { GatewayClient } from "@goldenbee/sdk"

export const GatewayClientContext = createContext<GatewayClient | null>(null)

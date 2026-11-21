import { useMemo } from "react"
import { Link, Route, Routes } from "react-router-dom"

import { Button } from "@/components/ui/button"
import { ConnectedView } from "@/features/connection/connected-view"
import {
  useConnection,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import { GatewayClientProvider } from "@/features/connection/gateway-client-provider"
import { InstanceDetailPage } from "@/features/host/instance-detail-page"
import { InstancesPage } from "@/features/host/instances-page"
import { PackagesPage } from "@/features/host/packages-page"
import { createGatewayClient } from "@goldenbee/sdk"

function NotFoundPage() {
  return (
    <div className="flex flex-col items-start gap-3">
      <h1 className="font-heading text-2xl font-semibold">Page not found</h1>
      <p className="text-sm text-muted-foreground">
        This browser Client has no route at that address.
      </p>
      <Button variant="outline" render={<Link to="/" />}>
        Back to overview
      </Button>
    </div>
  )
}

/**
 * Authenticated routes. The SDK client is created once per connection and
 * shared through context so pages never construct transport requests.
 */
export function ConnectedRoutes({ store }: { store: ConnectionStore }) {
  const gatewayUrl = useConnection((state) => state.gatewayUrl, store)
  const token = useConnection((state) => state.token, store)

  const client = useMemo(
    () =>
      createGatewayClient({
        baseUrl: gatewayUrl,
        token,
        fetch: (input, init) => fetch(input, init),
        webSocket: (url) => new WebSocket(url),
      }),
    [gatewayUrl, token],
  )

  return (
    <GatewayClientProvider client={client}>
      <Routes>
        <Route path="/" element={<ConnectedView store={store} />} />
        <Route path="/packages" element={<PackagesPage />} />
        <Route path="/instances" element={<InstancesPage />} />
        <Route path="/instances/:instance" element={<InstanceDetailPage />} />
        <Route path="*" element={<NotFoundPage />} />
      </Routes>
    </GatewayClientProvider>
  )
}

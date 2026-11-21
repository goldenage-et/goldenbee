import { useMemo, useState } from "react"

import { useAppRoute } from "@/app/router"
import { AppShell } from "@/components/shell/app-shell"
import { AppNav } from "@/components/shell/app-nav"
import { ConnectedView } from "@/features/connection/connected-view"
import {
  connectionStore,
  useConnection,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import { ConnectionScreen } from "@/features/connection/connection-screen"
import { ProviderDetailScreen } from "@/features/providers/provider-detail-screen"
import {
  createProviderGatewayClient,
  type ProviderClientFactory,
} from "@/features/providers/provider-gateway"
import { ProvidersScreen } from "@/features/providers/providers-screen"

export interface AppProps {
  store?: ConnectionStore
  /** Test seam; production uses the browser Gateway client. */
  createProviderClient?: ProviderClientFactory
}

export function App({ store = connectionStore, createProviderClient }: AppProps) {
  const connected = useConnection((state) => state.status === "connected", store)
  const gatewayUrl = useConnection((state) => state.gatewayUrl, store)
  const token = useConnection((state) => state.token, store)
  const route = useAppRoute()
  const [factory] = useState<ProviderClientFactory>(
    () => createProviderClient ?? createProviderGatewayClient,
  )

  const providerClient = useMemo(
    () => (connected ? factory({ baseUrl: gatewayUrl, token }) : null),
    [connected, factory, gatewayUrl, token],
  )

  function connectedContent() {
    if (!providerClient) {
      return null
    }
    switch (route.name) {
      case "providers":
        return <ProvidersScreen client={providerClient} />
      case "provider":
        return <ProviderDetailScreen client={providerClient} id={route.id} />
      case "overview":
        return <ConnectedView store={store} />
    }
  }

  return (
    <AppShell store={store} nav={connected ? <AppNav /> : undefined}>
      {connected ? connectedContent() : <ConnectionScreen store={store} />}
    </AppShell>
  )
}

export default App

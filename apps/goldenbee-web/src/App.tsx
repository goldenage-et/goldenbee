import { useMemo, useState } from "react"

import { useAppRoute } from "@/app/router"
import { AppShell } from "@/components/shell/app-shell"
import { AppNav } from "@/components/shell/app-nav"
import { ChatView } from "@/features/chat/chat-view"
import {
  browserChatHistory,
  type ChatHistoryStore,
} from "@/features/chat/local-chat-history"
import { ConnectedView } from "@/features/connection/connected-view"
import {
  connectionStore,
  useConnection,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import { ConnectionScreen } from "@/features/connection/connection-screen"
import { InstanceDetailPage } from "@/features/host/instance-detail-page"
import { InstancesPage } from "@/features/host/instances-page"
import { PackagesPage } from "@/features/host/packages-page"
import { ProviderDetailScreen } from "@/features/providers/provider-detail-screen"
import {
  createProviderGatewayClient,
  type ProviderClientFactory,
} from "@/features/providers/provider-gateway"
import { ProvidersScreen } from "@/features/providers/providers-screen"
import { createGatewayClient } from "@goldenbee/sdk"

export interface AppProps {
  store?: ConnectionStore
  /** Browser-local chat history; null in environments without IndexedDB. */
  history?: ChatHistoryStore | null
  /** Test seam; production uses the browser Gateway client. */
  createProviderClient?: ProviderClientFactory
}

const defaultChatHistory = browserChatHistory()

export function App({
  store = connectionStore,
  history = defaultChatHistory,
  createProviderClient,
}: AppProps) {
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

  const hostClient = useMemo(
    () =>
      connected
        ? createGatewayClient({
            baseUrl: gatewayUrl,
            token,
            fetch: (input, init) => fetch(input, init),
          })
        : null,
    [connected, gatewayUrl, token],
  )

  function connectedContent() {
    switch (route.name) {
      case "chat":
        return <ChatView store={store} history={history} />
      case "packages":
        return hostClient ? <PackagesPage client={hostClient} /> : null
      case "instances":
        return hostClient ? <InstancesPage client={hostClient} /> : null
      case "instance":
        return hostClient ? (
          <InstanceDetailPage client={hostClient} instance={route.instance} />
        ) : null
      case "providers":
        return providerClient ? <ProvidersScreen client={providerClient} /> : null
      case "provider":
        return providerClient ? (
          <ProviderDetailScreen client={providerClient} id={route.id} />
        ) : null
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

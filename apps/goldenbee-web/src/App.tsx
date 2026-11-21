import { AppShell } from "@/components/shell/app-shell"
import { ConnectedView } from "@/features/connection/connected-view"
import {
  connectionStore,
  useConnection,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import { ConnectionScreen } from "@/features/connection/connection-screen"

export interface AppProps {
  store?: ConnectionStore
}

export function App({ store = connectionStore }: AppProps) {
  const connected = useConnection((state) => state.status === "connected", store)

  return (
    <AppShell store={store}>
      {connected ? (
        <ConnectedView store={store} />
      ) : (
        <ConnectionScreen store={store} />
      )}
    </AppShell>
  )
}

export default App

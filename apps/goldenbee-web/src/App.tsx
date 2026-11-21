import { BrowserRouter } from "react-router-dom"

import { AppShell } from "@/components/shell/app-shell"
import { ConnectedRoutes } from "@/components/shell/connected-routes"
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
    <BrowserRouter>
      <AppShell store={store}>
        {connected ? (
          <ConnectedRoutes store={store} />
        ) : (
          <ConnectionScreen store={store} />
        )}
      </AppShell>
    </BrowserRouter>
  )
}

export default App

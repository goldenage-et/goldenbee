import { useRoute } from "@/app/router"
import { AppShell } from "@/components/shell/app-shell"
import { ChatView } from "@/features/chat/chat-view"
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
  const route = useRoute()

  return (
    <AppShell store={store}>
      {connected ? (
        route === "chat" ? (
          <ChatView store={store} />
        ) : (
          <ConnectedView store={store} />
        )
      ) : (
        <ConnectionScreen store={store} />
      )}
    </AppShell>
  )
}

export default App

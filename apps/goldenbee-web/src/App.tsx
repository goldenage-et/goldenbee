import { useMemo, useState } from "react"

import { useAppRoute } from "@/app/router"
import { CommandCenterShell } from "@/components/shell/command-center-shell"
import { AttentionView } from "@/features/attention/attention-view"
import { AuditPage } from "@/features/audit/audit-page"
import { DiagnosticsPage } from "@/features/diagnostics/diagnostics-page"
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
import { TaskDetailPage } from "@/features/tasks/task-detail-page"
import {
  createProfileGatewayClient,
  type ProfileClientFactory,
} from "@/features/runtime-profiles/profile-gateway"
import { ProfileDetailPage } from "@/features/runtime-profiles/profile-detail-page"
import { ProfilesPage } from "@/features/runtime-profiles/profiles-page"
import { TeamDetailPage } from "@/features/teams/team-detail-page"
import {
  createTeamGatewayClient,
  type TeamClientFactory,
} from "@/features/teams/team-gateway"
import { TeamsPage } from "@/features/teams/teams-page"
import {
  clientSettingsStore,
  type ClientSettingsStore,
} from "@/features/workspace/client-settings"
import {
  gatewayProfilesStore,
  type GatewayProfilesStore,
} from "@/features/workspace/gateway-profiles"
import { useWorkspaceController } from "@/features/workspace/use-workspace-controller"
import {
  workspaceStore,
  type WorkspaceStore,
} from "@/features/workspace/workspace-store"
import { createGatewayClient } from "@goldenbee/sdk"

export interface AppProps {
  store?: ConnectionStore
  /** Browser-local chat history; null in environments without IndexedDB. */
  history?: ChatHistoryStore | null
  /** Test seam; production uses the browser Gateway client. */
  createProviderClient?: ProviderClientFactory
  /** Test seam for team administration. */
  createTeamClient?: TeamClientFactory
  /** Test seam for Worker-owned Runtime Profile administration. */
  createProfileClient?: ProfileClientFactory
  /** Local session layout; one record set per browser profile. */
  workspace?: WorkspaceStore
  /** Local presentation preferences. */
  preferences?: ClientSettingsStore
  /** Local Gateway address profiles (URL/name only). */
  profiles?: GatewayProfilesStore
}

const defaultChatHistory = browserChatHistory()

export function App({
  store = connectionStore,
  history = defaultChatHistory,
  createProviderClient,
  createTeamClient,
  createProfileClient,
  workspace = workspaceStore,
  preferences = clientSettingsStore,
  profiles = gatewayProfilesStore,
}: AppProps) {
  const connected = useConnection((state) => state.status === "connected", store)
  const gatewayUrl = useConnection((state) => state.gatewayUrl, store)
  const token = useConnection((state) => state.token, store)
  const workers = useConnection((state) => state.workers, store)
  const route = useAppRoute()
  const [factory] = useState<ProviderClientFactory>(
    () => createProviderClient ?? createProviderGatewayClient,
  )
  const [teamFactory] = useState<TeamClientFactory>(
    () => createTeamClient ?? createTeamGatewayClient,
  )
  const [profileFactory] = useState<ProfileClientFactory>(
    () => createProfileClient ?? createProfileGatewayClient,
  )

  const controller = useWorkspaceController({
    workspace,
    history,
    connected,
    route,
  })

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

  const teamClient = useMemo(
    () => (connected ? teamFactory({ baseUrl: gatewayUrl, token }) : null),
    [connected, teamFactory, gatewayUrl, token],
  )

  const profileClient = useMemo(
    () => (connected ? profileFactory({ baseUrl: gatewayUrl, token }) : null),
    [connected, profileFactory, gatewayUrl, token],
  )

  function connectedContent() {
    switch (route.name) {
      case "chat":
        return null
      case "attention":
        return <AttentionView />
      case "diagnostics":
        return <DiagnosticsPage store={store} client={hostClient} />
      case "audit":
        return <AuditPage store={store} client={hostClient} />
      case "packages":
        return hostClient ? <PackagesPage client={hostClient} /> : null
      case "instances":
        return hostClient ? <InstancesPage client={hostClient} /> : null
      case "instance":
        return hostClient ? (
          <InstanceDetailPage client={hostClient} instance={route.instance} />
        ) : null
      case "teams":
        return teamClient ? <TeamsPage client={teamClient} /> : null
      case "team":
        return teamClient ? (
          <TeamDetailPage client={teamClient} name={route.team} />
        ) : null
      case "profiles":
        return profileClient ? (
          <ProfilesPage
            key={route.worker ?? "default"}
            client={profileClient}
            workers={workers}
            worker={route.worker}
          />
        ) : null
      case "profile":
        return profileClient ? (
          <ProfileDetailPage
            client={profileClient}
            worker={route.worker}
            name={route.profile}
          />
        ) : null
      case "providers":
        return providerClient ? <ProvidersScreen client={providerClient} /> : null
      case "provider":
        return providerClient ? (
          <ProviderDetailScreen client={providerClient} id={route.id} />
        ) : null
      case "task":
        return <TaskDetailPage client={hostClient} taskId={route.taskId} />
      case "overview":
        return <ConnectedView store={store} />
    }
  }

  return (
    <CommandCenterShell
      store={store}
      route={route}
      connected={connected}
      history={history}
      controller={controller}
      profiles={profiles}
      settings={preferences}
    >
      {connected ? connectedContent() : <ConnectionScreen store={store} />}
    </CommandCenterShell>
  )
}

export default App

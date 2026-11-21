import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react"

import { navigate, type AppRoute } from "@/app/router"
import { StatusDot } from "@/components/shell/status-dot"
import { StatusBadge } from "@/components/shell/status-badge"
import { IconRail } from "@/components/shell/icon-rail"
import { ContextSidebar } from "@/components/shell/context-sidebar"
import { SessionTabs } from "@/components/shell/session-tabs"
import {
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
} from "@/components/ui/resizable"
import {
  CommandPalette,
  type PaletteAction,
} from "@/components/shell/command-palette"
import {
  SettingsDialog,
  type SettingsTab,
} from "@/components/shell/settings-dialog"
import {
  CloseGroupDialog,
  SessionGroupDialog,
  type GroupDialogState,
} from "@/components/shell/session-group-dialogs"
import { Button } from "@/components/ui/button"
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "@/components/ui/empty"
import { useViewportBelow } from "@/components/shell/use-viewport"
import { ChatView } from "@/features/chat/chat-view"
import {
  createChatInspectionStore,
  useChatInspection,
} from "@/features/chat/chat-inspection-store"
import type { ChatHistoryStore } from "@/features/chat/local-chat-history"
import { SessionInspector } from "@/features/inspector/session-inspector"
import { WorkspaceFilePreview } from "@/features/inspector/workspace-file-preview"
import { useWorkspaceLive } from "@/features/inspector/use-workspace-live"
import { useGatewayClient } from "@/features/connection/use-gateway-client"
import {
  useConnection,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import {
  useClientSettings,
  type ClientSettingsStore,
} from "@/features/workspace/client-settings"
import type { GatewayProfilesStore } from "@/features/workspace/gateway-profiles"
import type { WorkspaceController } from "@/features/workspace/use-workspace-controller"
import type { SessionGroup } from "@/features/workspace/workspace-store"

const panelLimitNotice =
  "Four panels are already visible. Solo or background a session before splitting."

/** Invisible resize target; a quiet line appears only on hover, focus, or drag. */
function PanelHandle() {
  return (
    <ResizableHandle
      className="w-2 bg-transparent transition-colors after:bg-transparent hover:bg-line/40 focus-visible:bg-line/60 data-[separator=active]:bg-line/60 aria-[orientation=horizontal]:h-2"
    />
  )
}

export interface CommandCenterShellProps {
  store: ConnectionStore
  route: AppRoute
  connected: boolean
  history: ChatHistoryStore | null
  controller: WorkspaceController
  profiles: GatewayProfilesStore
  settings: ClientSettingsStore
  children: React.ReactNode
}

export function CommandCenterShell({
  store,
  route,
  connected,
  history,
  controller,
  profiles,
  settings,
  children,
}: CommandCenterShellProps) {
  const workers = useConnection((state) => state.workers, store)
  const density = useClientSettings((state) => state.density, settings)
  const defaultProfileId = useClientSettings(
    (state) => state.defaultGatewayProfileId,
    settings,
  )
  const setDefaultProfile = useClientSettings(
    (state) => state.setDefaultGatewayProfile,
    settings,
  )
  const inspectorDefault = useClientSettings(
    (state) => state.inspectorDefault,
    settings,
  )
  const setInspectorDefault = useClientSettings(
    (state) => state.setInspectorDefault,
    settings,
  )
  const singleColumn = useViewportBelow(1100)
  const narrow = useViewportBelow(780)
  const gatewayClient = useGatewayClient(store)
  const [inspections] = useState(createChatInspectionStore)
  const [inspectorOpen, setInspectorOpen] = useState(
    () => inspectorDefault === "open",
  )
  const [drawerOpen, setDrawerOpen] = useState(false)
  const [sidebarOpen, setSidebarOpen] = useState(true)
  const [paletteOpen, setPaletteOpen] = useState(false)
  const [settingsState, setSettingsState] = useState<{
    open: boolean
    tab: SettingsTab
  }>({ open: false, tab: "client" })
  const [groupDialog, setGroupDialog] = useState<GroupDialogState | null>(null)
  const [closing, setClosing] = useState<SessionGroup | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [previewPath, setPreviewPath] = useState<string | null>(null)
  const previousInspectorDefault = useRef(inspectorDefault)
  const activeInspection = useChatInspection(
    (state) =>
      controller.activeChatId
        ? state.inspections[controller.activeChatId]
        : undefined,
    inspections,
  )
  const activeWorker = activeInspection?.selection?.worker ?? null
  const workspaceLive = useWorkspaceLive(activeWorker, gatewayClient)

  useEffect(() => {
    // The inspector becomes an on-demand overlay below the narrow breakpoint.
    if (narrow) {
      // oxlint-disable-next-line react/set-state-in-effect
      setInspectorOpen(false)
    }
  }, [narrow])

  useEffect(() => {
    // A focused session change closes the transient file preview.
    // oxlint-disable-next-line react/set-state-in-effect
    setPreviewPath(null)
  }, [controller.activeChatId])

  useEffect(() => {
    if (previousInspectorDefault.current === inspectorDefault) {
      return
    }
    previousInspectorDefault.current = inspectorDefault
    if (!narrow) {
      // The Client settings dialog is the other owner of this preference.
      // oxlint-disable-next-line react/set-state-in-effect
      setInspectorOpen(inspectorDefault === "open")
    }
  }, [inspectorDefault, narrow])

  const toggleInspector = useCallback(
    (open?: boolean) => {
      setInspectorOpen((current) => {
        const next = open ?? !current
        setInspectorDefault(next ? "open" : "closed")
        return next
      })
    },
    [setInspectorDefault],
  )

  const splitPanel = useCallback(() => {
    if (!controller.split()) {
      setNotice(panelLimitNotice)
    }
  }, [controller])

  const soloPanel = useCallback(
    (chatId: string) => {
      if (!controller.solo(chatId)) {
        return
      }
      setInspectorOpen(true)
      setInspectorDefault("open")
      setNotice(
        "Other panels moved to background. Their Gateway tasks continue; nothing was cancelled.",
      )
    },
    [controller, setInspectorDefault],
  )

  useEffect(() => {
    // The drawer is a temporary navigation surface; any route change closes it.
    // oxlint-disable-next-line react/set-state-in-effect
    setDrawerOpen(false)
  }, [route.name])

  const openSettings = useCallback((tab: SettingsTab) => {
    setSettingsState({ open: true, tab })
  }, [])

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent): void {
      if (
        (event.metaKey || event.ctrlKey) &&
        event.key.toLowerCase() === "k"
      ) {
        event.preventDefault()
        setPaletteOpen(true)
        return
      }
      if (event.key === "Escape" && paletteOpen) {
        setPaletteOpen(false)
        return
      }
      if (event.key === "Escape" && narrow && inspectorOpen) {
        setInspectorOpen(false)
        return
      }
      if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
        event.preventDefault()
        splitPanel()
      }
    }
    window.addEventListener("keydown", onKeyDown)
    return () => {
      window.removeEventListener("keydown", onKeyDown)
    }
  }, [controller, inspectorOpen, narrow, paletteOpen, splitPanel])

  const backgroundSet = useMemo(
    () => new Set(controller.backgroundChatIds),
    [controller.backgroundChatIds],
  )
  const visibleChatIds = controller.visibleChatIds
  const stackedPanels = singleColumn || visibleChatIds.length === 1
  const previewName = previewPath?.split("/").at(-1) ?? previewPath ?? ""

  const paletteActions = useMemo(() => {
    const actions: PaletteAction[] = [
      {
        id: "new-session",
        label: "New session",
        group: "Actions",
        hint: "Ctrl+Enter",
        run: () => splitPanel(),
      },
      {
        id: "split-session",
        label: "Split focused session",
        group: "Actions",
        run: () => splitPanel(),
      },
      {
        id: "open-inspector",
        label: inspectorOpen
          ? "Close focused-session inspector"
          : "Open focused-session inspector",
        group: "Actions",
        run: () => toggleInspector(),
      },
      {
        id: "new-group",
        label: "New session group",
        group: "Actions",
        run: () =>
          setGroupDialog({
            mode: "create",
            name: `Group ${controller.groups.length + 1}`,
          }),
      },
    ]
    if (controller.activeGroup) {
      actions.push({
        id: "rename-group",
        label: `Rename group: ${controller.activeGroup.name}`,
        group: "Actions",
        run: () =>
          setGroupDialog({
            mode: "rename",
            id: controller.activeGroup!.id,
            name: controller.activeGroup!.name,
          }),
      })
      actions.push({
        id: "close-group",
        label: `Close group: ${controller.activeGroup.name}`,
        group: "Actions",
        run: () => setClosing(controller.activeGroup ?? null),
      })
    }
    if (controller.activeChatId) {
      actions.push({
        id: "solo-session",
        label: `Solo focused session: ${controller.activeChatTitle ?? "Local chat"}`,
        group: "Actions",
        run: () => soloPanel(controller.activeChatId!),
      })
      actions.push({
        id: "background-session",
        label: "Background current session",
        group: "Actions",
        run: () => {
          controller.backgroundActive()
          setNotice(
            "Session moved to background. Its Gateway task continues; nothing was cancelled.",
          )
        },
      })
    }
    for (const group of controller.groups) {
      actions.push({
        id: `group-${group.id}`,
        label: `Open group: ${group.name}`,
        group: "Groups",
        run: () => controller.selectGroup(group.id),
      })
    }

    const chats = new Map<string, string>()
    for (const chat of controller.chats) {
      chats.set(chat.id, chat.title)
    }
    for (const group of controller.groups) {
      for (const chatId of group.chatIds) {
        if (!chats.has(chatId)) {
          chats.set(chatId, "Local chat")
        }
      }
    }
    for (const session of controller.backgroundSessions) {
      if (!chats.has(session.id)) {
        chats.set(session.id, session.title)
      }
    }
    for (const [chatId, title] of chats) {
      const background = backgroundSet.has(chatId)
      actions.push({
        id: `chat-${chatId}`,
        label: `Open session: ${title}`,
        group: "Sessions",
        hint: background ? "Background" : undefined,
        run: () => {
          if (background) {
            if (!controller.restoreChat(chatId)) {
              setNotice(panelLimitNotice)
            }
          } else {
            controller.selectChat(chatId)
          }
        },
      })
    }

    const navigation: Array<[string, string, AppRoute]> = [
      ["overview", "Open Gateway overview", { name: "overview" }],
      ["attention", "Open attention queue", { name: "attention" }],
      ["diagnostics", "Open diagnostics", { name: "diagnostics" }],
      ["audit", "Open audit", { name: "audit" }],
      ["teams", "Open teams", { name: "teams" }],
      ["profiles", "Open Runtime Profiles", { name: "profiles" }],
      ["packages", "Open packages", { name: "packages" }],
      ["instances", "Open instances", { name: "instances" }],
      ["providers", "Open providers", { name: "providers" }],
    ]
    for (const [id, label, target] of navigation) {
      actions.push({
        id: `nav-${id}`,
        label,
        group: "Navigation",
        run: () => navigate(target),
      })
    }

    actions.push(
      {
        id: "settings-client",
        label: "Open Client settings",
        group: "Settings",
        run: () => openSettings("client"),
      },
      {
        id: "settings-connection",
        label: "Open Connection settings",
        group: "Settings",
        run: () => openSettings("connection"),
      },
      {
        id: "settings-gateway",
        label: "Open Gateway settings",
        group: "Settings",
        run: () => openSettings("gateway"),
      },
      {
        id: "settings-worker",
        label: "Open Worker settings",
        group: "Settings",
        run: () => openSettings("worker"),
      },
    )

    return actions
  }, [
    backgroundSet,
    controller,
    inspectorOpen,
    openSettings,
    soloPanel,
    splitPanel,
    toggleInspector,
  ])

  function addProfile(url: string): void {
    const profile = profiles.getState().addProfile({ url })
    if (!profile) {
      setNotice("The current Gateway URL cannot be saved as a profile.")
      return
    }
    if (!defaultProfileId) {
      setDefaultProfile(profile.id)
    }
    setNotice("Gateway profile saved locally. No caller token was stored.")
  }

  function useProfile(id: string): void {
    const profile = profiles.getState().profiles.find((entry) => entry.id === id)
    if (!profile) {
      return
    }
    setDefaultProfile(profile.id)
    store.setState({ gatewayUrl: profile.url })
    store.getState().disconnect()
    setDrawerOpen(false)
    setNotice("Gateway profile selected. Connect with a caller token.")
  }

  function saveGroup(): void {
    const name = groupDialog?.name.trim()
    if (!groupDialog || !name) {
      return
    }
    if (groupDialog.mode === "create") {
      controller.createGroup(name)
      setNotice(`Session group “${name}” created.`)
    } else if (groupDialog.id) {
      controller.renameGroup(groupDialog.id, name)
      setNotice(`Session group renamed to “${name}”.`)
    }
    setGroupDialog(null)
  }

  function closeGroup(): void {
    if (!closing) {
      return
    }
    const visible = closing.chatIds.filter(
      (chatId) => !backgroundSet.has(chatId),
    ).length
    controller.closeGroup(closing.id)
    setClosing(null)
    setNotice(
      visible > 0
        ? `${visible} session${visible === 1 ? "" : "s"} moved to background. Gateway tasks were not cancelled.`
        : "Session group closed.",
    )
  }

  function renderPanel(chatId: string) {
    return (
      <div
        role="listitem"
        data-panel-id={chatId}
        className="flex min-h-0 min-w-0 flex-1"
      >
        <ChatView
          store={store}
          history={history}
          chatId={chatId}
          title={controller.titleFor(chatId)}
          focused={chatId === controller.activeChatId}
          panelLabel={`Session ${controller.titleFor(chatId)}`}
          inspectionStore={inspections}
          inspectorOpen={inspectorOpen}
          onToggleInspector={() => toggleInspector()}
          onFocus={() => controller.focusChat(chatId)}
          onSolo={() => soloPanel(chatId)}
          onNewChat={splitPanel}
          onBackground={() => {
            controller.backgroundChat(chatId)
            setNotice(
              "Session moved to background. Its Gateway task continues; nothing was cancelled.",
            )
          }}
        />
      </div>
    )
  }

  function horizontalPair(first: string, second: string) {
    return (
      <ResizablePanelGroup
        orientation="horizontal"
        className="h-full min-h-0"
      >
        <ResizablePanel
          defaultSize="50%"
          minSize="20%"
          className="flex min-h-0"
        >
          {renderPanel(first)}
        </ResizablePanel>
        <PanelHandle />
        <ResizablePanel
          defaultSize="50%"
          minSize="20%"
          className="flex min-h-0"
        >
          {renderPanel(second)}
        </ResizablePanel>
      </ResizablePanelGroup>
    )
  }

  function renderWorkspace() {
    const ids = visibleChatIds

    if (ids.length === 0) {
      return (
        <div className="flex flex-1 items-center justify-center">
          <Empty className="max-w-md border border-dashed border-line bg-surface/40">
            <EmptyHeader>
              <EmptyTitle>No visible session in this group</EmptyTitle>
              <EmptyDescription>
                Background sessions keep running at the Gateway. Restore one
                from the sidebar, or start a new session.
              </EmptyDescription>
            </EmptyHeader>
            <Button type="button" size="sm" onClick={splitPanel}>
              New session
            </Button>
          </Empty>
        </div>
      )
    }

    if (ids.length === 1 || stackedPanels) {
      if (ids.length === 1) {
        return (
          <div
            role="list"
            aria-label="Session panels"
            data-panel-count="1"
            data-layout="single"
            className="flex min-h-0 flex-1"
          >
            {renderPanel(ids[0]!)}
          </div>
        )
      }

      return (
        <ResizablePanelGroup
          orientation="vertical"
          role="list"
          aria-label="Session panels"
          data-panel-count={ids.length}
          data-layout="stacked"
          data-stacked="true"
          className="min-h-0 flex-1"
        >
          {ids.map((chatId, index) => (
            <Fragment key={chatId}>
              <ResizablePanel
                defaultSize={`${100 / ids.length}%`}
                minSize="15%"
                className="flex min-h-0"
              >
                {renderPanel(chatId)}
              </ResizablePanel>
              {index < ids.length - 1 ? (
                <PanelHandle />
              ) : null}
            </Fragment>
          ))}
        </ResizablePanelGroup>
      )
    }

    if (ids.length === 2) {
      return (
        <ResizablePanelGroup
          orientation="horizontal"
          role="list"
          aria-label="Session panels"
          data-panel-count="2"
          data-layout="double"
          className="min-h-0 flex-1"
        >
          <ResizablePanel
            defaultSize="50%"
            minSize="25%"
            className="flex min-h-0"
          >
            {renderPanel(ids[0]!)}
          </ResizablePanel>
          <PanelHandle />
          <ResizablePanel
            defaultSize="50%"
            minSize="25%"
            className="flex min-h-0"
          >
            {renderPanel(ids[1]!)}
          </ResizablePanel>
        </ResizablePanelGroup>
      )
    }

    if (ids.length === 3) {
      return (
        <ResizablePanelGroup
          orientation="vertical"
          role="list"
          aria-label="Session panels"
          data-panel-count="3"
          data-layout="triple"
          className="min-h-0 flex-1"
        >
          <ResizablePanel
            defaultSize="55%"
            minSize="25%"
            className="flex min-h-0"
          >
            {renderPanel(ids[0]!)}
          </ResizablePanel>
          <PanelHandle />
          <ResizablePanel
            defaultSize="45%"
            minSize="25%"
            className="flex min-h-0"
          >
            {horizontalPair(ids[1]!, ids[2]!)}
          </ResizablePanel>
        </ResizablePanelGroup>
      )
    }

    return (
      <ResizablePanelGroup
        orientation="vertical"
        role="list"
        aria-label="Session panels"
        data-panel-count="4"
        data-layout="quad"
        className="min-h-0 flex-1"
      >
        <ResizablePanel
          defaultSize="50%"
          minSize="25%"
          className="flex min-h-0"
        >
          {horizontalPair(ids[0]!, ids[1]!)}
        </ResizablePanel>
        <PanelHandle />
        <ResizablePanel
          defaultSize="50%"
          minSize="25%"
          className="flex min-h-0"
        >
          {horizontalPair(ids[2]!, ids[3]!)}
        </ResizablePanel>
      </ResizablePanelGroup>
    )
  }

  if (!connected) {
    return (
      <div className="flex min-h-dvh flex-col bg-background">
        <header className="flex items-center gap-2 border-b border-line px-4 py-2 sm:px-6">
          <span className="text-meta font-semibold tracking-tight">Goldenbee</span>
          <span className="text-micro text-dim">Gateway Client</span>
          <div className="ml-auto">
            <StatusBadge store={store} />
          </div>
        </header>
        <main className="mx-auto flex w-full max-w-6xl flex-1 flex-col px-4 py-8 sm:px-6 sm:py-10">
          {children}
        </main>
        <footer className="border-t border-line px-4 py-2 text-micro text-dim sm:px-6">
          The caller token is never written to localStorage.
        </footer>
      </div>
    )
  }

  return (
    <div
      data-density={density}
      className="flex h-dvh overflow-hidden bg-background text-foreground"
    >
      <IconRail
        route={route}
        sidebarOpen={drawerOpen}
        onToggleSidebar={() => setDrawerOpen((open) => !open)}
        onOpenPalette={() => setPaletteOpen(true)}
        onOpenSettings={() => openSettings("client")}
      />

      <ContextSidebar
        store={store}
        profiles={profiles}
        defaultProfileId={defaultProfileId}
        chats={controller.chats}
        groups={controller.groups}
        activeChatId={controller.activeChatId}
        backgroundChatIds={controller.backgroundChatIds}
        backgroundSessions={controller.backgroundSessions}
        inspections={inspections}
        drawerOpen={drawerOpen}
        sidebarOpen={sidebarOpen}
        narrow={narrow}
        onCloseDrawer={() => setDrawerOpen(false)}
        onAddProfile={addProfile}
        onRemoveProfile={(id) => profiles.getState().removeProfile(id)}
        onUseProfile={useProfile}
        onSelectChat={(chatId) => {
          if (!controller.selectChat(chatId)) {
            setNotice(panelLimitNotice)
          }
          setDrawerOpen(false)
        }}
        onRestoreChat={(chatId) => {
          if (!controller.restoreChat(chatId)) {
            setNotice(panelLimitNotice)
          }
          setDrawerOpen(false)
        }}
        onNewSession={() => {
          splitPanel()
          setDrawerOpen(false)
        }}
      />

      {narrow && drawerOpen ? (
        <button
          type="button"
          aria-label="Close navigation overlay"
          onClick={() => setDrawerOpen(false)}
          className="fixed inset-0 z-30 bg-black/55"
        />
      ) : null}

      <div className="flex min-w-0 flex-1 flex-col">
        {route.name === "chat" ? (
          <>
            <SessionTabs
              groups={controller.groups}
              activeGroupId={controller.activeGroupId}
              backgroundChatIds={controller.backgroundChatIds}
              sidebarOpen={sidebarOpen}
              onToggleSidebar={() => setSidebarOpen((open) => !open)}
              onSelectGroup={(id) => {
                setPreviewPath(null)
                controller.selectGroup(id)
              }}
              onNewSession={() => {
                setPreviewPath(null)
                splitPanel()
              }}
              onNewGroup={() =>
                setGroupDialog({
                  mode: "create",
                  name: `Group ${controller.groups.length + 1}`,
                })
              }
              onRequestRename={(group) =>
                setGroupDialog({
                  mode: "rename",
                  id: group.id,
                  name: group.name,
                })
              }
              onRequestClose={(group) => setClosing(group)}
              preview={
                previewPath
                  ? { path: previewPath, name: previewName }
                  : null
              }
              previewActive={previewPath !== null}
              onSelectPreview={() => {
                if (previewPath) {
                  setPreviewPath(previewPath)
                }
              }}
              onClosePreview={() => setPreviewPath(null)}
            />

            <main
              aria-label="Session workspace"
              className="flex min-h-0 flex-1 overflow-hidden p-2"
            >
              <div
                id="session-workspace"
                role="tabpanel"
                aria-label={`Session group ${controller.activeGroup?.name ?? "Sessions"}`}
                className="flex min-h-0 flex-1 gap-2"
              >
                <div className={previewPath ? "hidden" : "contents"}>
                  {renderWorkspace()}
                </div>
                {previewPath ? (
                  <WorkspaceFilePreview
                    client={gatewayClient}
                    worker={activeWorker}
                    path={previewPath}
                    liveVersion={workspaceLive.version}
                    liveStatus={workspaceLive.status}
                    onClose={() => setPreviewPath(null)}
                  />
                ) : null}

                {!narrow && inspectorOpen ? (
                  <SessionInspector
                    history={history}
                    inspections={inspections}
                    artifactsClient={gatewayClient}
                    workspaceClient={gatewayClient}
                    eventsClient={gatewayClient}
                    chatId={controller.activeChatId}
                    title={controller.activeChatTitle}
                    open={inspectorOpen}
                    liveVersion={workspaceLive.version}
                    liveStatus={workspaceLive.status}
                    onOpenFile={setPreviewPath}
                    onClose={() => toggleInspector(false)}
                  />
                ) : null}
              </div>

              {narrow && inspectorOpen ? (
                <>
                  <button
                    type="button"
                    aria-label="Close inspector overlay"
                    onClick={() => toggleInspector(false)}
                    className="fixed inset-0 z-30 bg-black/55"
                  />
                  <div className="fixed inset-y-0 right-0 z-40 flex shadow-2xl">
                    <SessionInspector
                      history={history}
                      inspections={inspections}
                      artifactsClient={gatewayClient}
                      workspaceClient={gatewayClient}
                      eventsClient={gatewayClient}
                      chatId={controller.activeChatId}
                      title={controller.activeChatTitle}
                      open={inspectorOpen}
                      liveVersion={workspaceLive.version}
                      liveStatus={workspaceLive.status}
                      onOpenFile={setPreviewPath}
                      onClose={() => toggleInspector(false)}
                    />
                  </div>
                </>
              ) : null}
            </main>
          </>
        ) : (
          <main className="min-h-0 flex-1 overflow-y-auto">
            <div className="mx-auto w-full max-w-7xl px-3 py-3 sm:px-4">
              {children}
            </div>
          </main>
        )}

        <footer className="flex h-8 shrink-0 items-center gap-3 border-t border-line bg-surface px-3 text-micro text-dim">
          <StatusDot tone="positive" className="shrink-0">
            {workers.length === 1
              ? "Connected · 1 Worker visible"
              : `Connected · ${workers.length} Workers visible`}
          </StatusDot>
          {notice ? (
            <span role="status" aria-live="polite" className="truncate text-positive">
              {notice}
            </span>
          ) : (
            <span className="hidden truncate sm:block">
              Agent Gateway → Agent Worker · Sessions persist independently of
              the client
            </span>
          )}
          <div className="ml-auto flex shrink-0 items-center gap-3">
            <span className="hidden lg:block">
              Enter sends · Shift+Enter adds a line · Attachments are not
              supported by this Gateway release.
            </span>
            <span className="hidden sm:block">Goldenbee Web Client</span>
          </div>
        </footer>
      </div>

      <CommandPalette
        open={paletteOpen}
        onClose={() => setPaletteOpen(false)}
        actions={paletteActions}
      />

      <SettingsDialog
        open={settingsState.open}
        tab={settingsState.tab}
        onOpenChange={(open) =>
          setSettingsState((state) => ({ ...state, open }))
        }
        onTabChange={(tab) =>
          setSettingsState((state) => ({ ...state, tab }))
        }
        store={store}
        profiles={profiles}
        settings={settings}
      />

      <SessionGroupDialog
        state={groupDialog}
        onNameChange={(name) =>
          setGroupDialog((state) => (state ? { ...state, name } : state))
        }
        onCancel={() => setGroupDialog(null)}
        onSave={saveGroup}
      />

      <CloseGroupDialog
        group={closing}
        onCancel={() => setClosing(null)}
        onConfirm={closeGroup}
      />
    </div>
  )
}

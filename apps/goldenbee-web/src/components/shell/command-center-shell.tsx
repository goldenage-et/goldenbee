import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { cn } from "cn"

import { navigate, type AppRoute } from "@/app/router"
import { StatusDot } from "@/components/shell/status-dot"
import { StatusBadge } from "@/components/shell/status-badge"
import { IconRail } from "@/components/shell/icon-rail"
import { ContextSidebar } from "@/components/shell/context-sidebar"
import { Topbar } from "@/components/shell/topbar"
import { SessionTabs } from "@/components/shell/session-tabs"
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
import { createChatInspectionStore } from "@/features/chat/chat-inspection-store"
import type { ChatHistoryStore } from "@/features/chat/local-chat-history"
import { SessionInspector } from "@/features/inspector/session-inspector"
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
  const [paletteOpen, setPaletteOpen] = useState(false)
  const [settingsState, setSettingsState] = useState<{
    open: boolean
    tab: SettingsTab
  }>({ open: false, tab: "client" })
  const [groupDialog, setGroupDialog] = useState<GroupDialogState | null>(null)
  const [closing, setClosing] = useState<SessionGroup | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const previousInspectorDefault = useRef(inspectorDefault)

  useEffect(() => {
    // The inspector becomes an on-demand overlay below the narrow breakpoint.
    if (narrow) {
      // oxlint-disable-next-line react/set-state-in-effect
      setInspectorOpen(false)
    }
  }, [narrow])

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
        onOpenPalette={() => setPaletteOpen(true)}
        onOpenSettings={() => openSettings("client")}
      />

      <ContextSidebar
        store={store}
        route={route}
        profiles={profiles}
        defaultProfileId={defaultProfileId}
        backgroundSessions={controller.backgroundSessions}
        drawerOpen={drawerOpen}
        narrow={narrow}
        onCloseDrawer={() => setDrawerOpen(false)}
        onAddProfile={addProfile}
        onRemoveProfile={(id) => profiles.getState().removeProfile(id)}
        onUseProfile={useProfile}
        onRestoreChat={(chatId) => {
          if (!controller.restoreChat(chatId)) {
            setNotice(panelLimitNotice)
          }
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
        <Topbar
          route={route}
          store={store}
          groupName={controller.activeGroup?.name}
          sidebarOpen={drawerOpen}
          onToggleSidebar={() => setDrawerOpen((open) => !open)}
          onOpenPalette={() => setPaletteOpen(true)}
          onOpenGatewaySettings={() => openSettings("gateway")}
          onNewSession={() => controller.newSession()}
        />

        {route.name === "chat" ? (
          <>
            <SessionTabs
              groups={controller.groups}
              activeGroupId={controller.activeGroupId}
              backgroundChatIds={controller.backgroundChatIds}
              inspectorOpen={inspectorOpen}
              onSelectGroup={controller.selectGroup}
              onNewSession={splitPanel}
              onSplit={splitPanel}
              onToggleInspector={() => toggleInspector()}
              onRequestNewGroup={() =>
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
            />

            <main
              aria-label="Session workspace"
              className="flex min-h-0 flex-1 overflow-hidden p-2.5"
            >
              <div
                id="session-workspace"
                role="tabpanel"
                aria-label={`Session group ${controller.activeGroup?.name ?? "Sessions"}`}
                className="flex min-h-0 flex-1 gap-2.5"
              >
                {visibleChatIds.length > 0 ? (
                  <div
                    role="list"
                    aria-label="Session panels"
                    data-panel-count={visibleChatIds.length}
                    data-layout={
                      visibleChatIds.length === 1
                        ? "single"
                        : visibleChatIds.length === 3
                          ? "triple"
                          : visibleChatIds.length === 4
                            ? "quad"
                            : "double"
                    }
                    className={cn(
                      "grid min-h-0 flex-1 auto-rows-[minmax(0,1fr)] gap-2.5",
                      stackedPanels ? "grid-cols-1" : "grid-cols-2",
                    )}
                  >
                    {visibleChatIds.map((chatId, index) => (
                      <div
                        key={chatId}
                        role="listitem"
                        data-panel-id={chatId}
                        className={cn(
                          "flex min-h-0 min-w-0",
                          visibleChatIds.length === 3 &&
                            index === 0 &&
                            !stackedPanels &&
                            "col-span-2",
                        )}
                      >
                        <ChatView
                          store={store}
                          history={history}
                          chatId={chatId}
                          title={controller.titleFor(chatId)}
                          focused={chatId === controller.activeChatId}
                          panelLabel={`Session ${controller.titleFor(chatId)}`}
                          inspectionStore={inspections}
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
                    ))}
                  </div>
                ) : (
                  <div className="flex flex-1 items-center justify-center">
                    <Empty className="max-w-md border border-dashed border-line bg-surface/40">
                      <EmptyHeader>
                        <EmptyTitle>No visible session in this group</EmptyTitle>
                        <EmptyDescription>
                          Background sessions keep running at the Gateway. Restore
                          one from the sidebar, or start a new session.
                        </EmptyDescription>
                      </EmptyHeader>
                      <Button
                        type="button"
                        size="sm"
                        onClick={splitPanel}
                      >
                        New session
                      </Button>
                    </Empty>
                  </div>
                )}

                {!narrow && inspectorOpen ? (
                  <SessionInspector
                    history={history}
                    inspections={inspections}
                    artifactsClient={gatewayClient}
                    chatId={controller.activeChatId}
                    title={controller.activeChatTitle}
                    open={inspectorOpen}
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
                      chatId={controller.activeChatId}
                      title={controller.activeChatTitle}
                      open={inspectorOpen}
                      onClose={() => toggleInspector(false)}
                    />
                  </div>
                </>
              ) : null}
            </main>
          </>
        ) : (
          <main className="min-h-0 flex-1 overflow-y-auto">
            <div className="mx-auto w-full max-w-6xl px-4 py-5 sm:px-6">
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
          <span className="ml-auto hidden shrink-0 sm:block">
            Goldenbee Web Client
          </span>
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

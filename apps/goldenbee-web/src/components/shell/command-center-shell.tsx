import { useCallback, useEffect, useMemo, useState } from "react"

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
import type { ChatHistoryStore } from "@/features/chat/local-chat-history"
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
  const narrow = useViewportBelow(780)
  const [drawerOpen, setDrawerOpen] = useState(false)
  const [paletteOpen, setPaletteOpen] = useState(false)
  const [settingsState, setSettingsState] = useState<{
    open: boolean
    tab: SettingsTab
  }>({ open: false, tab: "client" })
  const [groupDialog, setGroupDialog] = useState<GroupDialogState | null>(null)
  const [closing, setClosing] = useState<SessionGroup | null>(null)
  const [notice, setNotice] = useState<string | null>(null)

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
      if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
        event.preventDefault()
        controller.newSession()
      }
    }
    window.addEventListener("keydown", onKeyDown)
    return () => {
      window.removeEventListener("keydown", onKeyDown)
    }
  }, [controller, paletteOpen])

  const backgroundSet = useMemo(
    () => new Set(controller.backgroundChatIds),
    [controller.backgroundChatIds],
  )

  const paletteActions = useMemo(() => {
    const actions: PaletteAction[] = [
      {
        id: "new-session",
        label: "New session",
        group: "Actions",
        hint: "Ctrl+Enter",
        run: () => controller.newSession(),
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
            controller.restoreChat(chatId)
          } else {
            controller.selectChat(chatId)
          }
        },
      })
    }

    const navigation: Array<[string, string, AppRoute]> = [
      ["overview", "Open Gateway overview", { name: "overview" }],
      ["attention", "Open attention queue", { name: "attention" }],
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
    openSettings,
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
          controller.restoreChat(chatId)
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
              onSelectGroup={controller.selectGroup}
              onNewSession={() => controller.newSession()}
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
              className="flex min-h-0 flex-1 flex-col overflow-hidden p-2.5"
            >
              <div
                id="session-workspace"
                role="tabpanel"
                aria-label={`Session group ${controller.activeGroup?.name ?? "Sessions"}`}
                className="flex min-h-0 flex-1 flex-col"
              >
                {controller.activeChatId ? (
                  <ChatView
                    key={controller.activeChatId}
                    store={store}
                    history={history}
                    chatId={controller.activeChatId}
                    title={controller.activeChatTitle}
                    onNewChat={() => controller.newSession()}
                    onBackground={() => {
                      controller.backgroundActive()
                      setNotice(
                        "Session moved to background. Its Gateway task continues; nothing was cancelled.",
                      )
                    }}
                  />
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
                        onClick={() => controller.newSession()}
                      >
                        New session
                      </Button>
                    </Empty>
                  </div>
                )}
              </div>
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

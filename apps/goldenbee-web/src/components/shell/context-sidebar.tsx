import {
  MessageSquareIcon,
  PlusIcon,
  RotateCcwIcon,
  ServerIcon,
  Trash2Icon,
  XIcon,
} from "lucide-react"
import type { CSSProperties } from "react"
import { cn } from "cn"

import { StatusDot } from "@/components/shell/status-dot"
import {
  Sidebar,
  SidebarContent,
  SidebarGroup,
  SidebarGroupAction,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarMenu,
  SidebarMenuAction,
  SidebarMenuBadge,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
} from "@/components/ui/sidebar"
import {
  useConnection,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import {
  profileNameForUrl,
  useGatewayProfiles,
  type GatewayProfilesStore,
} from "@/features/workspace/gateway-profiles"
import {
  useChatInspection,
  type ChatInspectionStore,
} from "@/features/chat/chat-inspection-store"
import type { StoredChat } from "@/features/chat/local-chat-history"
import type { SessionGroup } from "@/features/workspace/workspace-store"
import type { GatewayWorker } from "@goldenbee/sdk"

const sectionLabel =
  "text-micro font-bold tracking-[0.14em] text-dim uppercase"

const sidebarStyle = {
  "--sidebar-width": "238px",
} as CSSProperties

interface GatewayRow {
  id: string
  name: string
  url: string
  saved: boolean
  current: boolean
  isDefault: boolean
}

interface SessionRow {
  id: string
  title: string
  worker: string
  agent: string
  profile: string
  groupName: string
  background: boolean
  active: boolean
  updatedAt: number
}

export interface ContextSidebarProps {
  store: ConnectionStore
  profiles: GatewayProfilesStore
  defaultProfileId: string | null
  chats: StoredChat[]
  groups: SessionGroup[]
  activeChatId: string | null
  backgroundChatIds: string[]
  backgroundSessions: Array<{ id: string; title: string }>
  inspections: ChatInspectionStore
  drawerOpen: boolean
  sidebarOpen: boolean
  narrow: boolean
  onCloseDrawer(): void
  onAddProfile(url: string): void
  onRemoveProfile(id: string): void
  onUseProfile(id: string): void
  onSelectChat(id: string): void
  onRestoreChat(id: string): void
  onNewSession(): void
}

export function ContextSidebar({
  store,
  profiles,
  defaultProfileId,
  chats,
  groups,
  activeChatId,
  backgroundChatIds,
  backgroundSessions,
  inspections,
  drawerOpen,
  sidebarOpen,
  narrow,
  onCloseDrawer,
  onAddProfile,
  onRemoveProfile,
  onUseProfile,
  onSelectChat,
  onRestoreChat,
  onNewSession,
}: ContextSidebarProps) {
  const gatewayUrl = useConnection((state) => state.gatewayUrl, store)
  const health = useConnection((state) => state.health, store)
  const workers = useConnection((state) => state.workers, store)
  const savedProfiles = useGatewayProfiles((state) => state.profiles, profiles)
  const liveInspections = useChatInspection(
    (state) => state.inspections,
    inspections,
  )
  const currentName = profileNameForUrl(gatewayUrl)
  const currentUrl = gatewayUrl.replace(/\/$/, "")
  const currentSaved = savedProfiles.some(
    (profile) => profile.url === currentUrl,
  )
  const sidebarVisible = narrow ? drawerOpen : sidebarOpen

  const gatewayRows: GatewayRow[] = savedProfiles
    .map((profile) => ({
      id: profile.id,
      name: profile.name,
      url: profile.url,
      saved: true,
      current: profile.url === currentUrl,
      isDefault: profile.id === defaultProfileId,
    }))
    .sort(
      (a, b) =>
        Number(b.current) - Number(a.current) ||
        Number(b.isDefault) - Number(a.isDefault),
    )
  if (!currentSaved) {
    gatewayRows.unshift({
      id: "current-gateway",
      name: currentName,
      url: currentUrl,
      saved: false,
      current: true,
      isDefault: false,
    })
  }

  const groupByChat = new Map<string, string>()
  for (const group of groups) {
    for (const chatId of group.chatIds) {
      groupByChat.set(chatId, group.name)
    }
  }

  const sessions = new Map<string, SessionRow>()
  function addSession(input: {
    id: string
    title: string
    selection?: StoredChat["selection"]
    updatedAt?: number
  }): void {
    if (sessions.has(input.id)) {
      return
    }
    sessions.set(input.id, {
      id: input.id,
      title: input.title,
      worker: input.selection?.worker ?? "",
      agent: input.selection?.agent ?? "",
      profile: input.selection?.profile ?? "",
      groupName: groupByChat.get(input.id) ?? "",
      background: backgroundChatIds.includes(input.id),
      active: input.id === activeChatId,
      updatedAt: input.updatedAt ?? 0,
    })
  }

  for (const chat of chats) {
    addSession({
      id: chat.id,
      title: chat.title,
      selection: chat.selection,
      updatedAt: chat.updatedAt,
    })
  }
  for (const group of groups) {
    for (const chatId of group.chatIds) {
      addSession({ id: chatId, title: "Local chat" })
    }
  }
  for (const session of backgroundSessions) {
    addSession({ id: session.id, title: session.title })
  }
  if (activeChatId) {
    addSession({ id: activeChatId, title: "Local chat" })
  }
  for (const [chatId, inspection] of Object.entries(liveInspections)) {
    const selection = inspection.selection ?? undefined
    const existing = sessions.get(chatId)
    if (existing) {
      if (selection?.worker) {
        existing.worker = selection.worker
      }
      if (selection?.agent) {
        existing.agent = selection.agent
      }
      if (selection?.profile !== undefined) {
        existing.profile = selection.profile
      }
      if (inspection.title) {
        existing.title = inspection.title
      }
      continue
    }
    addSession({
      id: chatId,
      title: inspection.title ?? "Local chat",
      selection,
    })
  }

  const workerById = new Map<string, GatewayWorker>(
    workers.map((worker) => [worker.id, worker]),
  )
  const clusters = new Map<string, SessionRow[]>()
  for (const session of sessions.values()) {
    const key = session.worker || "__unassigned__"
    const bucket = clusters.get(key)
    if (bucket) {
      bucket.push(session)
    } else {
      clusters.set(key, [session])
    }
  }
  const orderedClusters = [...clusters.entries()]
    .map(([key, rows]) => {
      const worker = key === "__unassigned__" ? undefined : workerById.get(key)
      return {
        key,
        worker,
        label:
          worker !== undefined
            ? `${worker.package} · ${worker.instance}`
            : key === "__unassigned__"
              ? "Unassigned"
              : key,
        sessions: [...rows].sort(
          (a, b) =>
            Number(a.background) - Number(b.background) ||
            b.updatedAt - a.updatedAt,
        ),
      }
    })
    .sort((a, b) => a.label.localeCompare(b.label))

  return (
    <SidebarProvider className="contents" style={sidebarStyle}>
      <Sidebar
        id="context-sidebar"
        role="complementary"
        aria-label="Gateway context"
        collapsible="none"
        data-state={sidebarVisible ? "open" : "closed"}
        aria-hidden={!sidebarVisible ? true : undefined}
        inert={!sidebarVisible}
        style={{ "--sidebar-width": sidebarOpen ? "238px" : "0px" } as CSSProperties}
        className={cn(
          "relative h-full shrink-0 overflow-hidden transition-[width] duration-200 ease-linear",
          sidebarOpen ? "border-r border-line" : "border-r-0",
          "max-[1100px]:w-[210px]",
          "max-[780px]:fixed max-[780px]:inset-y-0 max-[780px]:left-14 max-[780px]:z-40 max-[780px]:w-[264px] max-[780px]:border-r max-[780px]:border-line max-[780px]:shadow-2xl max-[780px]:transition-transform",
          drawerOpen
            ? "max-[780px]:translate-x-0"
            : "max-[780px]:-translate-x-[calc(100%+3.5rem)]",
        )}
      >
        {narrow ? (
          <div className="flex justify-end p-2 pb-0">
            <button
              type="button"
              aria-label="Close navigation"
              onClick={onCloseDrawer}
              className="grid size-7 place-items-center rounded-[7px] text-muted-foreground hover:bg-sidebar-accent hover:text-foreground"
            >
              <XIcon className="size-4" aria-hidden="true" />
            </button>
          </div>
        ) : null}

        <SidebarContent className="gap-0">
          <SidebarGroup>
            <SidebarGroupLabel className={sectionLabel}>
              Gateways
            </SidebarGroupLabel>
            <SidebarGroupAction
              type="button"
              aria-label="Save current Gateway as profile"
              title="Save current Gateway"
              disabled={currentSaved}
              onClick={() => onAddProfile(gatewayUrl)}
            >
              <PlusIcon aria-hidden="true" />
            </SidebarGroupAction>
            <SidebarGroupContent>
              <SidebarMenu>
                {gatewayRows.map((gateway) => (
                  <SidebarMenuItem key={gateway.id}>
                    <SidebarMenuButton
                      size="lg"
                      isActive={gateway.current}
                      aria-label={
                        gateway.saved
                          ? `Use profile ${gateway.name}`
                          : `Current Gateway ${gateway.name}`
                      }
                      title={
                        gateway.current && health
                          ? `${gateway.url} · ${health.reachable} of ${health.workers} Workers reachable`
                          : gateway.url
                      }
                      onClick={() => {
                        if (gateway.saved && !gateway.current) {
                          onUseProfile(gateway.id)
                        }
                      }}
                      className="h-auto min-h-11 items-start py-1.5"
                    >
                      <ServerIcon className="mt-0.5" aria-hidden="true" />
                      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                        <span className="flex min-w-0 items-center gap-1.5">
                          <span className="truncate text-meta font-medium">
                            {gateway.name}
                          </span>
                          {gateway.current ? (
                            <StatusDot
                              tone="positive"
                              className="shrink-0 text-micro text-positive"
                            >
                              live
                            </StatusDot>
                          ) : null}
                          {gateway.isDefault ? (
                            <span className="shrink-0 text-[9px] font-semibold tracking-wide text-gold uppercase">
                              default
                            </span>
                          ) : null}
                        </span>
                        <span
                          className="truncate text-micro font-normal text-sidebar-foreground/55"
                          title={gateway.url}
                        >
                          {gateway.url}
                        </span>
                      </span>
                    </SidebarMenuButton>
                    {gateway.saved ? (
                      <SidebarMenuAction
                        showOnHover
                        aria-label={`Remove profile ${gateway.name}`}
                        title={`Remove ${gateway.name}`}
                        onClick={() => onRemoveProfile(gateway.id)}
                      >
                        <Trash2Icon aria-hidden="true" />
                      </SidebarMenuAction>
                    ) : null}
                  </SidebarMenuItem>
                ))}
              </SidebarMenu>
            </SidebarGroupContent>
          </SidebarGroup>

          <SidebarGroup className="min-h-0 flex-1">
            <SidebarGroupLabel className={sectionLabel}>
              Sessions
            </SidebarGroupLabel>
            <SidebarGroupAction
              type="button"
              aria-label="New session"
              title="New session"
              onClick={onNewSession}
            >
              <PlusIcon aria-hidden="true" />
            </SidebarGroupAction>
            <SidebarGroupContent>
              {orderedClusters.length === 0 ? (
                <p className="px-2 py-1 text-meta text-muted-foreground">
                  No sessions yet.
                </p>
              ) : (
                orderedClusters.map((cluster) => (
                  <div key={cluster.key} className="mt-1 first:mt-0">
                    <div className="flex items-center gap-2 px-2 pt-1 pb-1">
                      <StatusDot
                        tone={
                          cluster.worker
                            ? cluster.worker.reachable
                              ? "positive"
                              : "dim"
                            : "dim"
                        }
                        className="min-w-0 flex-1 text-micro font-semibold text-sidebar-foreground/65"
                      >
                        {cluster.label}
                      </StatusDot>
                      <span className="text-micro tabular-nums text-dim">
                        {cluster.sessions.length}
                      </span>
                    </div>
                    <SidebarMenu>
                      {cluster.sessions.map((session) => (
                        <SidebarMenuItem key={session.id}>
                          <SidebarMenuButton
                            size="sm"
                            isActive={session.active}
                            title={
                              session.groupName
                                ? `${session.title} · ${session.groupName}`
                                : session.title
                            }
                            aria-label={
                              session.background
                                ? `Restore ${session.title}`
                                : `Focus ${session.title}`
                            }
                            onClick={() => {
                              if (session.background) {
                                onRestoreChat(session.id)
                              } else {
                                onSelectChat(session.id)
                              }
                            }}
                            className={cn(
                              "h-auto items-start py-1.5",
                              session.background && "pr-8",
                            )}
                          >
                            {session.background ? (
                              <RotateCcwIcon
                                className="mt-0.5 text-gold"
                                aria-hidden="true"
                              />
                            ) : (
                              <MessageSquareIcon
                                className="mt-0.5"
                                aria-hidden="true"
                              />
                            )}
                            <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                              <span className="truncate text-meta font-medium">
                                {session.title}
                              </span>
                              {session.agent || session.profile ? (
                                <span className="truncate text-micro font-normal text-sidebar-foreground/55">
                                  {[session.agent, session.profile]
                                    .filter(Boolean)
                                    .join(" · ")}
                                </span>
                              ) : null}
                            </span>
                          </SidebarMenuButton>
                          {session.background ? (
                            <SidebarMenuBadge className="top-2 right-2 text-[9px] text-dim">
                              BG
                            </SidebarMenuBadge>
                          ) : null}
                        </SidebarMenuItem>
                      ))}
                    </SidebarMenu>
                  </div>
                ))
              )}
            </SidebarGroupContent>
          </SidebarGroup>
        </SidebarContent>
      </Sidebar>
    </SidebarProvider>
  )
}

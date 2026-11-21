import {
  PlusIcon,
  RotateCcwIcon,
  ScrollTextIcon,
  ServerIcon,
  StethoscopeIcon,
  Trash2Icon,
  XIcon,
} from "lucide-react"
import { cn } from "cn"

import { routeHref, type AppRoute } from "@/app/router"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { StatusDot } from "@/components/shell/status-dot"
import {
  useConnection,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import {
  profileNameForUrl,
  useGatewayProfiles,
  type GatewayProfilesStore,
} from "@/features/workspace/gateway-profiles"
import type { BackgroundSession } from "@/features/workspace/use-workspace-controller"

const sectionLabel =
  "px-3 text-micro font-bold tracking-[0.14em] text-dim uppercase"

export interface ContextSidebarProps {
  store: ConnectionStore
  route: AppRoute
  profiles: GatewayProfilesStore
  defaultProfileId: string | null
  backgroundSessions: BackgroundSession[]
  drawerOpen: boolean
  narrow: boolean
  onCloseDrawer(): void
  onAddProfile(url: string): void
  onRemoveProfile(id: string): void
  onUseProfile(id: string): void
  onRestoreChat(id: string): void
}

export function ContextSidebar({
  store,
  route,
  profiles,
  defaultProfileId,
  backgroundSessions,
  drawerOpen,
  narrow,
  onCloseDrawer,
  onAddProfile,
  onRemoveProfile,
  onUseProfile,
  onRestoreChat,
}: ContextSidebarProps) {
  const gatewayUrl = useConnection((state) => state.gatewayUrl, store)
  const health = useConnection((state) => state.health, store)
  const workers = useConnection((state) => state.workers, store)
  const tokenPersisted = useConnection((state) => state.tokenPersisted, store)
  const savedProfiles = useGatewayProfiles((state) => state.profiles, profiles)
  const currentName = profileNameForUrl(gatewayUrl)
  const currentSaved = savedProfiles.some(
    (profile) => profile.url === gatewayUrl.replace(/\/$/, ""),
  )

  return (
    <aside
      id="context-sidebar"
      aria-label="Gateway context"
      data-state={drawerOpen ? "open" : "closed"}
      aria-hidden={narrow && !drawerOpen ? true : undefined}
      inert={narrow && !drawerOpen}
      className={cn(
        "flex w-[238px] shrink-0 flex-col border-r border-line bg-sidebar",
        "max-[1100px]:w-[210px]",
        "max-[780px]:fixed max-[780px]:inset-y-0 max-[780px]:left-14 max-[780px]:z-40 max-[780px]:w-[264px] max-[780px]:shadow-2xl max-[780px]:transition-transform",
        drawerOpen
          ? "max-[780px]:translate-x-0"
          : "max-[780px]:-translate-x-[calc(100%+3.5rem)]",
      )}
    >
      <div className="flex items-center justify-between px-3 pt-4 pb-2">
        <span className={sectionLabel}>Gateways</span>
        <div className="flex items-center gap-1">
          <Button
            type="button"
            size="icon-xs"
            variant="ghost"
            aria-label="Save current Gateway as profile"
            title="Save current Gateway"
            disabled={currentSaved}
            onClick={() => onAddProfile(gatewayUrl)}
          >
            <PlusIcon aria-hidden="true" />
          </Button>
          {narrow ? (
            <Button
              type="button"
              size="icon-xs"
              variant="ghost"
              aria-label="Close navigation"
              onClick={onCloseDrawer}
            >
              <XIcon aria-hidden="true" />
            </Button>
          ) : null}
        </div>
      </div>

      <div className="px-2.5">
        <div className="rounded-[9px] border border-gold/20 bg-gold/5 px-2.5 py-2">
          <div className="flex items-center gap-2">
            <ServerIcon className="size-3.5 shrink-0 text-gold" aria-hidden="true" />
            <span className="min-w-0 flex-1 truncate text-body font-medium">
              {currentName}
            </span>
            <StatusDot tone="positive" className="text-micro text-positive">
              Connected
            </StatusDot>
          </div>
          <p className="mt-1 truncate text-micro text-dim" title={gatewayUrl}>
            {gatewayUrl}
          </p>
          <p className="mt-1 text-micro text-muted-foreground">
            {health
              ? `${health.workers} Workers approved · ${health.reachable} reachable`
              : "Gateway health is unavailable."}
          </p>
        </div>
      </div>

      <div className="mt-3 flex flex-col gap-0.5 px-2.5">
        <span className={sectionLabel}>Operator evidence</span>
        <a
          href={routeHref({ name: "diagnostics" })}
          aria-current={route.name === "diagnostics" ? "page" : undefined}
          className="density-row flex items-center gap-2 rounded-[7px] px-1 py-1.5 text-meta text-muted-foreground hover:bg-surface-3 hover:text-foreground aria-[current=page]:bg-gold/10 aria-[current=page]:text-gold"
        >
          <StethoscopeIcon className="size-3.5 shrink-0" aria-hidden="true" />
          Diagnostics
        </a>
        <a
          href={routeHref({ name: "audit" })}
          aria-current={route.name === "audit" ? "page" : undefined}
          className="density-row flex items-center gap-2 rounded-[7px] px-1 py-1.5 text-meta text-muted-foreground hover:bg-surface-3 hover:text-foreground aria-[current=page]:bg-gold/10 aria-[current=page]:text-gold"
        >
          <ScrollTextIcon className="size-3.5 shrink-0" aria-hidden="true" />
          Audit
        </a>
      </div>

      <div className="mt-3 flex flex-col gap-1 px-2.5">
        <span className="px-1 text-micro font-semibold tracking-wide text-dim uppercase">
          Saved profiles
        </span>
        {savedProfiles.length === 0 ? (
          <p className="px-1 py-1 text-meta text-muted-foreground">
            No saved Gateway profiles. Profiles store a name and URL only.
          </p>
        ) : (
          <ul className="flex flex-col">
            {savedProfiles.map((profile) => (
              <li
                key={profile.id}
                className="group/profile flex items-center gap-1 rounded-[7px] px-1 py-1 hover:bg-surface-3"
              >
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-1.5">
                    <span className="truncate text-meta font-medium">
                      {profile.name}
                    </span>
                    {profile.id === defaultProfileId ? (
                      <Badge
                        variant="outline"
                        className="h-4 rounded px-1 text-micro text-gold"
                      >
                        Default
                      </Badge>
                    ) : null}
                  </div>
                  <p className="truncate text-micro text-dim" title={profile.url}>
                    {profile.url}
                  </p>
                </div>
                <Button
                  type="button"
                  size="icon-xs"
                  variant="ghost"
                  aria-label={`Use profile ${profile.name}`}
                  title="Disconnect and open this Gateway address"
                  onClick={() => onUseProfile(profile.id)}
                >
                  <RotateCcwIcon aria-hidden="true" />
                </Button>
                <Button
                  type="button"
                  size="icon-xs"
                  variant="ghost"
                  aria-label={`Remove profile ${profile.name}`}
                  onClick={() => onRemoveProfile(profile.id)}
                >
                  <Trash2Icon aria-hidden="true" />
                </Button>
              </li>
            ))}
          </ul>
        )}
      </div>

      <div className="mt-4 flex min-h-0 flex-1 flex-col overflow-hidden">
        <div className="flex items-center justify-between px-3 pb-1">
          <span className={sectionLabel}>Workers</span>
          <span className="text-micro text-dim">{workers.length} visible</span>
        </div>
        <div className="min-h-0 overflow-y-auto px-2.5">
          {workers.length === 0 ? (
            <p className="px-1 py-1 text-meta text-muted-foreground">
              No Worker is visible to this caller.
            </p>
          ) : (
            <ul className="flex flex-col gap-0.5">
              {workers.map((worker) => (
                <li
                  key={worker.id}
                  className="density-row flex items-center gap-2 rounded-[7px] px-1 py-1.5"
                >
                  <span
                    aria-hidden="true"
                    className="grid size-6 shrink-0 place-items-center rounded-[7px] bg-surface-3 text-micro font-semibold text-profile"
                  >
                    {worker.package.slice(0, 2).toUpperCase()}
                  </span>
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-meta font-medium">
                      {worker.package} · {worker.instance}
                    </p>
                    <StatusDot
                      tone={worker.reachable ? "positive" : "dim"}
                      className="text-micro text-muted-foreground"
                    >
                      {worker.reachable ? "Reachable" : worker.status}
                    </StatusDot>
                  </div>
                </li>
              ))}
            </ul>
          )}
        </div>

        <div className="mt-3 border-t border-line px-3 pt-3 pb-1">
          <div className="flex items-center justify-between">
            <span className={sectionLabel}>Background sessions</span>
            <Badge
              variant="outline"
              className="h-4 rounded px-1 text-micro text-muted-foreground"
            >
              {backgroundSessions.length}
            </Badge>
          </div>
        </div>
        <div className="min-h-0 overflow-y-auto px-2.5 pb-2">
          {backgroundSessions.length === 0 ? (
            <p className="px-1 py-1 text-meta text-muted-foreground">
              No background sessions.
            </p>
          ) : (
            <ul className="flex flex-col gap-0.5">
              {backgroundSessions.map((session) => (
                <li key={session.id}>
                  <button
                    type="button"
                    aria-label={`Restore ${session.title}`}
                    onClick={() => onRestoreChat(session.id)}
                    className="density-row flex w-full items-center gap-2 rounded-[7px] px-1 py-1.5 text-left hover:bg-surface-3"
                  >
                    <RotateCcwIcon
                      className="size-3.5 shrink-0 text-gold"
                      aria-hidden="true"
                    />
                    <span className="min-w-0 flex-1">
                      <span className="block truncate text-meta font-medium">
                        {session.title}
                      </span>
                      <span className="block text-micro text-muted-foreground">
                        Background · Restore
                      </span>
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>

      <div className="border-t border-line px-3 py-2.5">
        <StatusDot tone="positive" className="text-meta">
          <span className="font-medium text-positive">Gateway connection</span>
        </StatusDot>
        <p className="mt-1 pl-3 text-micro text-dim">
          {tokenPersisted
            ? "Caller token saved for this tab."
            : "Caller token in memory for this tab."}
        </p>
      </div>
    </aside>
  )
}

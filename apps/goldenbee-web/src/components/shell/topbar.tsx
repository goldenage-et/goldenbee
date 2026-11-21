import {
  MenuIcon,
  PlusIcon,
  SearchIcon,
  SettingsIcon,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import type { AppRoute } from "@/app/router"
import {
  useConnection,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import { profileNameForUrl } from "@/features/workspace/gateway-profiles"

const routeLabels: Partial<Record<AppRoute["name"], string>> = {
  overview: "Gateway overview",
  attention: "Attention",
  packages: "Packages",
  instances: "Instances",
  instance: "Instance detail",
  providers: "Providers",
  provider: "Provider detail",
}

export interface TopbarProps {
  route: AppRoute
  store: ConnectionStore
  groupName: string | undefined
  sidebarOpen: boolean
  onToggleSidebar(): void
  onOpenPalette(): void
  onOpenGatewaySettings(): void
  onNewSession(): void
}

export function Topbar({
  route,
  store,
  groupName,
  sidebarOpen,
  onToggleSidebar,
  onOpenPalette,
  onOpenGatewaySettings,
  onNewSession,
}: TopbarProps) {
  const gatewayUrl = useConnection((state) => state.gatewayUrl, store)
  const current =
    route.name === "chat"
      ? (groupName ?? "Chat")
      : (routeLabels[route.name] ?? "Gateway")

  return (
    <header className="flex h-12 shrink-0 items-center gap-2 border-b border-line bg-surface px-3">
      <Button
        type="button"
        size="icon-sm"
        variant="ghost"
        aria-label="Open navigation"
        aria-controls="context-sidebar"
        aria-expanded={sidebarOpen}
        onClick={onToggleSidebar}
        className="hidden max-[780px]:inline-flex"
      >
        <MenuIcon aria-hidden="true" />
      </Button>

      <nav aria-label="Breadcrumb" className="min-w-0">
        <ol className="flex min-w-0 items-center gap-1.5 text-meta">
          <li className="hidden shrink-0 text-dim min-[600px]:block">Goldenbee</li>
          <li
            aria-hidden="true"
            className="hidden shrink-0 text-dim min-[600px]:block"
          >
            /
          </li>
          <li className="hidden min-w-0 shrink truncate text-muted-foreground sm:block">
            {profileNameForUrl(gatewayUrl)}
          </li>
          <li aria-hidden="true" className="hidden text-dim sm:block">
            /
          </li>
          <li
            aria-current="page"
            className="truncate font-medium text-foreground"
          >
            {current}
          </li>
        </ol>
      </nav>

      <div className="ml-auto flex items-center gap-1.5">
        <Button
          type="button"
          size="sm"
          variant="ghost"
          aria-label="Search commands"
          onClick={onOpenPalette}
          className="rounded-full border border-line px-2.5 text-meta text-muted-foreground"
        >
          <SearchIcon aria-hidden="true" />
          <span className="max-[780px]:hidden">Search</span>
          <kbd className="hidden rounded border border-line bg-surface-2 px-1 font-sans text-micro text-dim min-[900px]:inline">
            ⌘K
          </kbd>
        </Button>
        <Button
          type="button"
          size="sm"
          variant="outline"
          aria-label="Gateway settings"
          onClick={onOpenGatewaySettings}
        >
          <SettingsIcon aria-hidden="true" />
          <span className="max-[780px]:hidden">Gateway</span>
        </Button>
        <Button
          type="button"
          size="sm"
          onClick={onNewSession}
          aria-label="New session"
          className="bg-gold text-primary-foreground hover:bg-gold/85"
        >
          <PlusIcon aria-hidden="true" />
          <span className="max-[780px]:hidden">New session</span>
        </Button>
      </div>
    </header>
  )
}

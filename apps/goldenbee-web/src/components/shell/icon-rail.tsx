import {
  BellIcon,
  BoxesIcon,
  HexagonIcon,
  MenuIcon,
  MessageSquareIcon,
  PackageIcon,
  PlugIcon,
  ScrollTextIcon,
  SearchIcon,
  SettingsIcon,
  StethoscopeIcon,
  UserCogIcon,
  UsersIcon,
  type LucideIcon,
} from "lucide-react"
import { cn } from "cn"

import { routeHref, type AppRoute } from "@/app/router"

interface RailItem {
  label: string
  title?: string
  route: AppRoute
  icon: LucideIcon
  active(route: AppRoute): boolean
}

const items: RailItem[] = [
  {
    label: "Chat",
    title: "Command Center",
    route: { name: "chat" },
    icon: MessageSquareIcon,
    active: (route) => route.name === "chat",
  },
  {
    label: "Gateway overview",
    route: { name: "overview" },
    icon: HexagonIcon,
    active: (route) => route.name === "overview",
  },
  {
    label: "Attention",
    route: { name: "attention" },
    icon: BellIcon,
    active: (route) => route.name === "attention",
  },
  {
    label: "Diagnostics",
    route: { name: "diagnostics" },
    icon: StethoscopeIcon,
    active: (route) => route.name === "diagnostics",
  },
  {
    label: "Audit",
    route: { name: "audit" },
    icon: ScrollTextIcon,
    active: (route) => route.name === "audit",
  },
  {
    label: "Teams",
    route: { name: "teams" },
    icon: UsersIcon,
    active: (route) => route.name === "teams" || route.name === "team",
  },
  {
    label: "Runtime Profiles",
    route: { name: "profiles" },
    icon: UserCogIcon,
    active: (route) => route.name === "profiles" || route.name === "profile",
  },
  {
    label: "Packages",
    route: { name: "packages" },
    icon: PackageIcon,
    active: (route) => route.name === "packages",
  },
  {
    label: "Instances",
    route: { name: "instances" },
    icon: BoxesIcon,
    active: (route) =>
      route.name === "instances" || route.name === "instance",
  },
  {
    label: "Providers",
    route: { name: "providers" },
    icon: PlugIcon,
    active: (route) =>
      route.name === "providers" || route.name === "provider",
  },
]

const railItemClass =
  "grid size-10 place-items-center rounded-[10px] border border-transparent text-muted-foreground transition-colors hover:bg-surface-3 hover:text-foreground focus-visible:outline-2 focus-visible:outline-gold aria-[current=page]:border-gold/25 aria-[current=page]:bg-gold/10 aria-[current=page]:text-gold"

export function IconRail({
  route,
  sidebarOpen,
  onToggleSidebar,
  onOpenPalette,
  onOpenSettings,
}: {
  route: AppRoute
  sidebarOpen: boolean
  onToggleSidebar(): void
  onOpenPalette(): void
  onOpenSettings(): void
}) {
  return (
    <nav
      aria-label="Primary navigation"
      className="flex w-[62px] shrink-0 flex-col items-center gap-2 border-r border-line bg-[#0d1219] px-2 py-3 max-[780px]:w-14"
    >
      <button
        type="button"
        aria-label="Open navigation"
        aria-controls="context-sidebar"
        aria-expanded={sidebarOpen}
        onClick={onToggleSidebar}
        className={cn(railItemClass, "hidden max-[780px]:grid")}
      >
        <MenuIcon className="size-[18px]" aria-hidden="true" />
      </button>

      <a
        href="#/"
        aria-label="Goldenbee overview"
        title="Goldenbee"
        className="mb-1 grid size-10 place-items-center rounded-[11px] bg-gradient-to-br from-[#f3ce7a] to-[#a97828] text-lg font-black text-[#20190d]"
      >
        <span aria-hidden="true">✦</span>
      </a>

      <ul className="flex w-full flex-col items-center gap-1">
        {items.map((item) => {
          const Icon = item.icon
          const active = item.active(route)
          return (
            <li key={item.label}>
              <a
                href={routeHref(item.route)}
                aria-label={item.label}
                aria-current={active ? "page" : undefined}
                title={item.title ?? item.label}
                className={cn(railItemClass)}
              >
                <Icon className="size-[18px]" aria-hidden="true" />
              </a>
            </li>
          )
        })}
      </ul>

      <div className="mt-auto flex w-full flex-col items-center gap-1">
        <button
          type="button"
          aria-label="Open command palette"
          title="Search / command palette (Ctrl+K)"
          onClick={onOpenPalette}
          className={railItemClass}
        >
          <SearchIcon className="size-[18px]" aria-hidden="true" />
        </button>
        <button
          type="button"
          aria-label="Client settings"
          title="Client settings"
          onClick={onOpenSettings}
          className={railItemClass}
        >
          <SettingsIcon className="size-[18px]" aria-hidden="true" />
        </button>
      </div>
    </nav>
  )
}

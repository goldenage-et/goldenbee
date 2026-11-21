import { cn } from "cn"

import { useAppRoute, routeHref, type AppRoute } from "@/app/router"

const items: Array<{ route: AppRoute; label: string; match: AppRoute["name"] }> = [
  { route: { name: "overview" }, label: "Overview", match: "overview" },
  { route: { name: "chat" }, label: "Chat", match: "chat" },
  { route: { name: "providers" }, label: "Providers", match: "providers" },
]

export function AppNav() {
  const route = useAppRoute()
  return (
    <nav aria-label="Primary">
      <ul className="flex items-center gap-1">
        {items.map((item) => {
          const active =
            item.match === "providers"
              ? route.name === "providers" || route.name === "provider"
              : route.name === item.match
          return (
            <li key={item.label}>
              <a
                href={routeHref(item.route)}
                aria-current={active ? "page" : undefined}
                className={cn(
                  "inline-flex h-8 items-center rounded-2xl px-3 text-sm font-medium text-muted-foreground transition-colors hover:bg-muted hover:text-foreground",
                  active && "bg-muted text-foreground",
                )}
              >
                {item.label}
              </a>
            </li>
          )
        })}
      </ul>
    </nav>
  )
}

import { cn } from "cn"

import { useAppRoute, routeHref, type AppRoute } from "@/app/router"

const items: Array<{
  route: AppRoute
  label: string
  active: (route: AppRoute) => boolean
}> = [
  {
    route: { name: "overview" },
    label: "Overview",
    active: (route) => route.name === "overview",
  },
  {
    route: { name: "packages" },
    label: "Packages",
    active: (route) => route.name === "packages",
  },
  {
    route: { name: "instances" },
    label: "Instances",
    active: (route) => route.name === "instances" || route.name === "instance",
  },
  {
    route: { name: "providers" },
    label: "Providers",
    active: (route) => route.name === "providers" || route.name === "provider",
  },
]

export function AppNav() {
  const route = useAppRoute()
  return (
    <nav aria-label="Primary">
      <ul className="flex items-center gap-1">
        {items.map((item) => {
          const active = item.active(route)
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

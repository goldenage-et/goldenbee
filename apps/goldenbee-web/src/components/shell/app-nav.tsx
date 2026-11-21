import { NavLink } from "react-router-dom"
import { cn } from "cn"

const links = [
  { to: "/", label: "Overview", end: true },
  { to: "/packages", label: "Packages", end: false },
  { to: "/instances", label: "Instances", end: false },
]

export function AppNav() {
  return (
    <nav aria-label="Primary" className="flex items-center gap-1">
      {links.map((link) => (
        <NavLink
          key={link.to}
          to={link.to}
          end={link.end}
          className={({ isActive }) =>
            cn(
              "rounded-xl px-2.5 py-1.5 text-sm transition-colors",
              isActive
                ? "bg-muted font-medium text-foreground"
                : "text-muted-foreground hover:bg-muted/60 hover:text-foreground",
            )
          }
        >
          {link.label}
        </NavLink>
      ))}
    </nav>
  )
}

import { useSyncExternalStore } from "react"

export type AppRoute =
  | { name: "overview" }
  | { name: "chat" }
  | { name: "attention" }
  | { name: "diagnostics" }
  | { name: "audit" }
  | { name: "packages" }
  | { name: "instances" }
  | { name: "instance"; instance: string }
  | { name: "providers" }
  | { name: "provider"; id: string }
  | { name: "task"; taskId: string }
  | { name: "teams" }
  | { name: "team"; team: string }
  | { name: "profiles"; worker?: string }
  | { name: "profile"; worker: string; profile: string }

/**
 * Hash routing keeps the static build free of a server route handler.
 *
 * The Gateway serves no SPA fallback, so a `#/...` fragment is the only route
 * that survives a reload without a reverse-proxy rewrite.
 */
export function parseRoute(hash: string): AppRoute {
  const raw = hash.replace(/^#/, "").replace(/^\/+/, "")
  if (raw.length === 0) {
    return { name: "overview" }
  }
  const [section, id, sub] = raw.split("/")
  if (section === "chat") {
    return { name: "chat" }
  }
  if (section === "attention") {
    return { name: "attention" }
  }
  if (section === "diagnostics") {
    return { name: "diagnostics" }
  }
  if (section === "audit") {
    return { name: "audit" }
  }
  if (section === "packages") {
    return { name: "packages" }
  }
  if (section === "instances") {
    return id
      ? { name: "instance", instance: decodeURIComponent(id) }
      : { name: "instances" }
  }
  if (section === "providers") {
    return id ? { name: "provider", id: decodeURIComponent(id) } : { name: "providers" }
  }
  if (section === "tasks" && id) {
    return { name: "task", taskId: decodeURIComponent(id) }
  }
  if (section === "teams") {
    return id ? { name: "team", team: decodeURIComponent(id) } : { name: "teams" }
  }
  if (section === "profiles") {
    if (!id) {
      return { name: "profiles" }
    }
    const worker = decodeURIComponent(id)
    return sub
      ? { name: "profile", worker, profile: decodeURIComponent(sub) }
      : { name: "profiles", worker }
  }
  return { name: "overview" }
}

export function routeHref(route: AppRoute): string {
  switch (route.name) {
    case "overview":
      return "#/"
    case "chat":
      return "#/chat"
    case "attention":
      return "#/attention"
    case "diagnostics":
      return "#/diagnostics"
    case "audit":
      return "#/audit"
    case "packages":
      return "#/packages"
    case "instances":
      return "#/instances"
    case "instance":
      return `#/instances/${encodeURIComponent(route.instance)}`
    case "providers":
      return "#/providers"
    case "provider":
      return `#/providers/${encodeURIComponent(route.id)}`
    case "task":
      return `#/tasks/${encodeURIComponent(route.taskId)}`
    case "teams":
      return "#/teams"
    case "team":
      return `#/teams/${encodeURIComponent(route.team)}`
    case "profiles":
      return route.worker
        ? `#/profiles/${encodeURIComponent(route.worker)}`
        : "#/profiles"
    case "profile":
      return `#/profiles/${encodeURIComponent(route.worker)}/${encodeURIComponent(route.profile)}`
  }
}

export function navigate(route: AppRoute): void {
  window.location.hash = routeHref(route)
}

function subscribe(onChange: () => void): () => void {
  window.addEventListener("hashchange", onChange)
  window.addEventListener("popstate", onChange)
  return () => {
    window.removeEventListener("hashchange", onChange)
    window.removeEventListener("popstate", onChange)
  }
}

function snapshot(): string {
  return window.location.hash
}

export function useAppRoute(): AppRoute {
  const hash = useSyncExternalStore(subscribe, snapshot, () => "")
  return parseRoute(hash)
}

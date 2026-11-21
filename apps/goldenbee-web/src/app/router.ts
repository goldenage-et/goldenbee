import { useSyncExternalStore } from "react"

export type AppRoute =
  | { name: "overview" }
  | { name: "chat" }
  | { name: "attention" }
  | { name: "packages" }
  | { name: "instances" }
  | { name: "instance"; instance: string }
  | { name: "providers" }
  | { name: "provider"; id: string }

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
  const [section, id] = raw.split("/")
  if (section === "chat") {
    return { name: "chat" }
  }
  if (section === "attention") {
    return { name: "attention" }
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

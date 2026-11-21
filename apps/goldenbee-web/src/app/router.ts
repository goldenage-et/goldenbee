import { useSyncExternalStore } from "react"

export type AppRoute = "overview" | "chat"

const routeHashes: Record<AppRoute, string> = {
  overview: "#/overview",
  chat: "#/chat",
}

export function routeFromHash(hash: string): AppRoute {
  return hash.startsWith("#/chat") ? "chat" : "overview"
}

export function routeHref(route: AppRoute): string {
  return routeHashes[route]
}

function subscribe(listener: () => void): () => void {
  window.addEventListener("hashchange", listener)
  return () => window.removeEventListener("hashchange", listener)
}

function snapshot(): AppRoute {
  return routeFromHash(window.location.hash)
}

function serverSnapshot(): AppRoute {
  return "overview"
}

/**
 * Minimal same-document routing.
 *
 * The first release is a single static bundle behind a same-origin reverse
 * proxy, so hash routes need no server rewrite and no routing dependency.
 */
export function useRoute(): AppRoute {
  return useSyncExternalStore(subscribe, snapshot, serverSnapshot)
}

import type { ReactNode } from "react"

import { AppNav } from "./app-nav"
import { StatusBadge } from "./status-badge"
import {
  useConnection,
  type ConnectionStore,
} from "@/features/connection/connection-store"

export function AppShell({
  children,
  store,
}: {
  children: ReactNode
  store: ConnectionStore
}) {
  const connected = useConnection((state) => state.status === "connected", store)

  return (
    <div className="flex min-h-dvh flex-col bg-background">
      <header className="sticky top-0 z-10 border-b bg-background/85 backdrop-blur supports-[backdrop-filter]:bg-background/70">
        <div className="mx-auto flex w-full max-w-6xl flex-wrap items-center gap-x-4 gap-y-2 px-4 py-3 sm:px-6">
          <div className="flex items-center gap-2.5">
            <div
              aria-hidden="true"
              className="grid size-8 place-items-center rounded-xl bg-primary text-primary-foreground"
            >
              <svg
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.8"
                strokeLinecap="round"
                strokeLinejoin="round"
                className="size-4.5"
              >
                <path d="M12 2.5 20 7v10l-8 4.5L4 17V7l8-4.5Z" />
                <path d="M8.5 12h3l1.5-3 1.5 6 1.5-3h1.5" />
              </svg>
            </div>
            <div className="leading-tight">
              <p className="text-sm font-semibold tracking-tight">Goldenbee</p>
              <p className="text-xs text-muted-foreground">Gateway Client</p>
            </div>
          </div>
          <div className="ml-auto flex items-center gap-2">
            {connected ? <AppNav /> : null}
            <StatusBadge store={store} />
          </div>
        </div>
      </header>

      <main className="mx-auto flex w-full max-w-6xl flex-1 flex-col px-4 py-8 sm:px-6 sm:py-10 lg:py-12">
        {children}
      </main>

      <footer className="border-t">
        <div className="mx-auto flex w-full max-w-6xl flex-wrap items-center justify-between gap-2 px-4 py-3 text-xs text-muted-foreground sm:px-6">
          <p>Browser Client for the Goldenbee Gateway protocol.</p>
          <p>The caller token is never written to localStorage.</p>
        </div>
      </footer>
    </div>
  )
}

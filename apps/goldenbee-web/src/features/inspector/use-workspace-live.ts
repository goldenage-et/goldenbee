import { useEffect, useRef, useState } from "react"

import type { GatewayTaskStream } from "@goldenbee/sdk"

export type WorkspaceLiveStatus = "disabled" | "connecting" | "live" | "reconnecting"

export interface WorkspaceLiveGateway {
  workspaceEvents(
    worker: string,
    options?: { signal?: AbortSignal },
  ): Promise<GatewayTaskStream>
}

export interface WorkspaceLive {
  status: WorkspaceLiveStatus
  /** Increments after a debounced batch of workspace changes. */
  version: number
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

/**
 * One shared workspace SSE subscription for the active Worker.
 *
 * The Worker stream is best-effort and not replayable. A reconnect increments
 * the version so every visible directory and the preview refetch from the
 * authoritative tree/file endpoints.
 */
export function useWorkspaceLive(
  worker: string | null,
  client: WorkspaceLiveGateway | null,
): WorkspaceLive {
  const [status, setStatus] = useState<WorkspaceLiveStatus>("disabled")
  const [version, setVersion] = useState(0)
  const versionRef = useRef(0)
  const pendingRef = useRef<Set<string>>(new Set())
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const reconnectRef = useRef<ReturnType<typeof setTimeout> | null>(null)

  useEffect(() => {
    if (!worker || !client) {
      // oxlint-disable-next-line react/set-state-in-effect
      setStatus("disabled")
      return
    }
    let closed = false
    let stream: GatewayTaskStream | null = null
    const gateway = client
    const controller = new AbortController()

    function bump() {
      timerRef.current = null
      pendingRef.current.clear()
      versionRef.current += 1
      setVersion(versionRef.current)
    }

    function push(paths: string[] | undefined) {
      if (paths) {
        for (const path of paths) {
          pendingRef.current.add(path)
        }
      }
      if (timerRef.current) {
        clearTimeout(timerRef.current)
      }
      timerRef.current = setTimeout(bump, 180)
    }

    async function connect() {
      if (closed) {
        return
      }
      // oxlint-disable-next-line react/set-state-in-effect
      setStatus("connecting")
      try {
        stream = await gateway.workspaceEvents(worker!, {
          signal: controller.signal,
        })
        if (closed) {
          await stream.close()
          return
        }
        // oxlint-disable-next-line react/set-state-in-effect
        setStatus("live")
        bump()
        for await (const frame of stream) {
          if (frame.type !== "event") {
            continue
          }
          if (frame.event.kind !== "workspace.changed") {
            continue
          }
          const payload = isRecord(frame.event.payload)
            ? frame.event.payload
            : {}
          const paths = Array.isArray(payload.paths)
            ? payload.paths.filter(
                (path): path is string => typeof path === "string",
              )
            : undefined
          push(paths)
        }
      } catch {
        if (!closed) {
          // oxlint-disable-next-line react/set-state-in-effect
          setStatus("reconnecting")
        }
      }
      if (closed) {
        return
      }
      setStatus("reconnecting")
      reconnectRef.current = setTimeout(() => {
        void connect()
      }, 1_500)
    }

    void connect()
    return () => {
      closed = true
      controller.abort()
      if (timerRef.current) {
        clearTimeout(timerRef.current)
      }
      if (reconnectRef.current) {
        clearTimeout(reconnectRef.current)
      }
      void stream?.close()
    }
  }, [worker, client])

  return { status, version }
}

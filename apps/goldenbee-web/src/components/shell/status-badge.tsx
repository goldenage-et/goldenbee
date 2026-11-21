import { CircleDashedIcon, LoaderCircleIcon } from "lucide-react"

import { Badge } from "@/components/ui/badge"
import {
  useConnection,
  type ConnectionStatus,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import { cn } from "@/lib/utils"

const labels: Record<ConnectionStatus, string> = {
  idle: "Not connected",
  connecting: "Connecting",
  connected: "Connected",
  unauthorized: "Unauthorized",
  forbidden: "Forbidden",
  unreachable: "Unreachable",
  error: "Gateway error",
  invalid: "Check details",
}

const tones: Partial<Record<ConnectionStatus, string>> = {
  connected: "border-emerald-600/20 bg-emerald-500/10 text-emerald-700",
  unauthorized: "border-destructive/20 bg-destructive/10 text-destructive",
  forbidden: "border-destructive/20 bg-destructive/10 text-destructive",
  unreachable: "border-destructive/20 bg-destructive/10 text-destructive",
  error: "border-destructive/20 bg-destructive/10 text-destructive",
}

export function StatusBadge({ store }: { store: ConnectionStore }) {
  const status = useConnection((state) => state.status, store)
  const busy = status === "connecting"

  return (
    <Badge
      variant="outline"
      className={cn("h-6 gap-1.5 rounded-full px-2.5", tones[status])}
      aria-live="polite"
    >
      {busy ? (
        <LoaderCircleIcon className="animate-spin" aria-hidden="true" />
      ) : (
        <CircleDashedIcon
          className={cn(
            status === "connected" && "fill-emerald-600 text-emerald-600",
          )}
          aria-hidden="true"
        />
      )}
      {labels[status]}
    </Badge>
  )
}

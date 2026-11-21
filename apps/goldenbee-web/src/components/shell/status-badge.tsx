import { Badge } from "@/components/ui/badge"
import { StatusDot, type StatusTone } from "@/components/shell/status-dot"
import {
  useConnection,
  type ConnectionStatus,
  type ConnectionStore,
} from "@/features/connection/connection-store"

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

const tones: Record<ConnectionStatus, StatusTone> = {
  idle: "dim",
  connecting: "gold",
  connected: "positive",
  unauthorized: "danger",
  forbidden: "danger",
  unreachable: "danger",
  error: "danger",
  invalid: "danger",
}

/** Connection status in the compact dot-plus-text form. */
export function StatusBadge({ store }: { store: ConnectionStore }) {
  const status = useConnection((state) => state.status, store)
  return (
    <Badge
      variant="outline"
      className="h-5 gap-1 rounded-[7px] border-line px-1.5 text-micro text-muted-foreground"
      aria-live="polite"
    >
      <StatusDot tone={tones[status]}>{labels[status]}</StatusDot>
    </Badge>
  )
}

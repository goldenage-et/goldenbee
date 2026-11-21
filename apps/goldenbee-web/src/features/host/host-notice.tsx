import { AlertTriangleIcon, RefreshCwIcon } from "lucide-react"

import {
  Alert,
  AlertDescription,
  AlertTitle,
} from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { hostErrorView } from "./host-errors"

export function HostLoading({ label }: { label: string }) {
  return (
    <div className="flex items-center gap-2 py-8 text-sm text-muted-foreground">
      <Spinner />
      <span>{label}</span>
    </div>
  )
}

export function HostErrorNotice({
  error,
  action = "read",
  onRetry,
  retryLabel = "Try again",
}: {
  error: unknown
  action?: "read" | "write"
  onRetry?: () => void
  retryLabel?: string
}) {
  const view = hostErrorView(error, action)

  return (
    <Alert variant="destructive">
      <AlertTriangleIcon aria-hidden="true" />
      <AlertTitle>{view.title}</AlertTitle>
      <AlertDescription>
        <p>{view.message}</p>
        {view.recovery ? <p>{view.recovery}</p> : null}
        {view.code ? (
          <p className="font-mono text-xs text-muted-foreground">
            code: {view.code}
          </p>
        ) : null}
      </AlertDescription>
      {onRetry && view.retryable ? (
        <div className="mt-3">
          <Button variant="outline" size="sm" onClick={onRetry}>
            <RefreshCwIcon data-icon="inline-start" aria-hidden="true" />
            {retryLabel}
          </Button>
        </div>
      ) : null}
    </Alert>
  )
}

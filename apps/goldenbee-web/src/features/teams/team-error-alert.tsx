import { AlertTriangleIcon } from "lucide-react"

import {
  Alert,
  AlertDescription,
  AlertTitle,
} from "@/components/ui/alert"
import { teamErrorView } from "./team-errors"

export function TeamErrorAlert({ error }: { error: unknown }) {
  const view = teamErrorView(error)
  return (
    <Alert variant="destructive">
      <AlertTriangleIcon aria-hidden="true" />
      <AlertTitle>{view.title}</AlertTitle>
      <AlertDescription>
        <p>{view.message}</p>
        <p className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs">
          {view.code ? (
            <code className="rounded bg-muted px-1.5 py-0.5 text-[11px]">
              {view.code}
            </code>
          ) : null}
          {view.recovery ? <span>{view.recovery}</span> : null}
        </p>
      </AlertDescription>
    </Alert>
  )
}

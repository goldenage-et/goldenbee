import { ShieldAlertIcon, ShieldCheckIcon } from "lucide-react"
import { useEffect, useState } from "react"

import { routeHref } from "@/app/router"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import type { ChatApprovalPart } from "@/features/chat/chat-types"
import {
  isGatewayError,
  type ApprovalDecision,
  type GatewayApproval,
  type GatewayRequestOptions,
} from "@goldenbee/sdk"

/** The minimal approval surface the inline card needs. */
export interface ApprovalGateway {
  decideApproval(
    taskId: string,
    approvalId: string,
    decision: ApprovalDecision,
    options?: GatewayRequestOptions,
  ): Promise<GatewayApproval>
  revokeApproval(
    taskId: string,
    approvalId: string,
    options?: GatewayRequestOptions,
  ): Promise<GatewayApproval>
}

export interface ApprovalCardProps {
  part: ChatApprovalPart
  /** Gateway task that owns this approval, when known locally. */
  taskId?: string
  client: ApprovalGateway | null
  /** Receives the authoritative Gateway response for an in-place update. */
  onUpdated(approval: GatewayApproval): void
  /** Test seam for deterministic expiry rendering. */
  now?: () => number
}

interface ApprovalErrorView {
  title: string
  message: string
  code?: string
  recovery?: string
}

function approvalError(cause: unknown): ApprovalErrorView {
  if (isGatewayError(cause)) {
    const denied =
      cause.kind === "forbidden" || cause.kind === "unauthorized"
    return {
      title: denied
        ? "Not authorized to decide this approval"
        : "Approval action failed",
      message: cause.message,
      code: cause.code,
      recovery: cause.recovery,
    }
  }
  return {
    title: "Approval action failed",
    message:
      cause instanceof Error && cause.message
        ? cause.message
        : "The Gateway approval action failed.",
  }
}

function statusTone(
  status: ChatApprovalPart["status"],
): "outline" | "secondary" | "destructive" {
  if (status === "approved") {
    return "secondary"
  }
  if (status === "rejected" || status === "revoked" || status === "expired") {
    return "destructive"
  }
  return "outline"
}

const defaultNow = () => Date.now()

function formatExpiry(expiresAt: number | undefined): string {
  if (expiresAt === undefined) {
    return "Not reported"
  }
  return new Date(expiresAt).toISOString()
}

function Fact({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[7.5rem_minmax(0,1fr)] gap-2">
      <dt className="text-micro text-dim">{label}</dt>
      <dd className="min-w-0 break-words text-micro text-foreground">
        {children}
      </dd>
    </div>
  )
}

/**
 * Inline approval card.
 *
 * It shows only the values the Gateway reported. The decision always carries
 * the digest currently on the card, and no local approval state changes until
 * the Gateway returns an authoritative record.
 */
export function ApprovalCard({
  part,
  taskId,
  client,
  onUpdated,
  now = defaultNow,
}: ApprovalCardProps) {
  const [busy, setBusy] = useState<"approved" | "rejected" | "revoked" | null>(
    null,
  )
  const [error, setError] = useState<ApprovalErrorView | null>(null)
  const [clock, setClock] = useState(now)

  useEffect(() => {
    if (
      part.status !== "pending" ||
      part.expiresAt === undefined ||
      part.expiresAt <= clock
    ) {
      return
    }
    const remaining = Math.max(0, part.expiresAt - clock)
    const timer = setTimeout(() => setClock(now()), remaining)
    return () => {
      clearTimeout(timer)
    }
  }, [clock, now, part.expiresAt, part.status])

  const expired = part.expiresAt !== undefined && part.expiresAt <= clock
  const canDecide =
    client !== null &&
    taskId !== undefined &&
    part.status === "pending" &&
    part.digest.length > 0 &&
    !part.digestChanged &&
    !expired &&
    busy === null
  const canRevoke = canDecide
  const taskLink = taskId ? routeHref({ name: "task", taskId }) : null

  async function decide(decision: ApprovalDecision["decision"]): Promise<void> {
    if (!client || !taskId || !canDecide) {
      return
    }
    // Read the digest at click time. A changed card never reuses the digest of
    // an earlier render or an earlier decision attempt.
    const digest = part.digest
    if (!digest) {
      setError({
        title: "Approval action failed",
        message: "The Gateway did not report a digest, so this approval cannot be decided.",
      })
      return
    }
    setBusy(decision)
    setError(null)
    try {
      const approval = await client.decideApproval(
        taskId,
        part.approvalId,
        { decision, digest },
      )
      onUpdated(approval)
    } catch (cause) {
      setError(approvalError(cause))
    } finally {
      setBusy(null)
    }
  }

  async function revoke(): Promise<void> {
    if (!client || !taskId || !canRevoke) {
      return
    }
    setBusy("revoked")
    setError(null)
    try {
      const approval = await client.revokeApproval(taskId, part.approvalId)
      onUpdated(approval)
    } catch (cause) {
      setError(approvalError(cause))
    } finally {
      setBusy(null)
    }
  }

  return (
    <section
      aria-label={`Approval ${part.operation ?? part.approvalId}`}
      className="grid gap-2 rounded-[9px] border border-gold/40 bg-gold/5 p-3"
    >
      <div className="flex flex-wrap items-center gap-2">
        <ShieldAlertIcon className="size-4 text-gold" aria-hidden="true" />
        <h3 className="text-meta font-semibold text-foreground">Approval</h3>
        <Badge variant={statusTone(part.status)} className="h-5 rounded-md px-1.5 text-micro">
          {part.status}
        </Badge>
        {expired && part.status === "pending" ? (
          <span className="text-micro text-destructive">expiry passed</span>
        ) : null}
        {part.digestChanged ? (
          <span className="text-micro text-destructive">digest changed</span>
        ) : null}
      </div>

      <dl className="grid gap-1">
        <Fact label="Operation">
          <span className="font-mono">{part.operation ?? "Not reported"}</span>
        </Fact>
        <Fact label="Target">
          <span className="font-mono">{part.target ?? "Not reported"}</span>
        </Fact>
        <Fact label="Digest">
          <span className="break-all font-mono">{part.digest || "Not reported"}</span>
        </Fact>
        <Fact label="Risk">{part.risk ?? "Not reported"}</Fact>
        <Fact label="Requested by">{part.requestedBy ?? "Not reported"}</Fact>
        <Fact label="Expires">{formatExpiry(part.expiresAt)}</Fact>
        {part.decidedBy ? <Fact label="Decided by">{part.decidedBy}</Fact> : null}
        {part.receipt ? (
          <Fact label="Receipt">
            <span className="break-all font-mono">{part.receipt}</span>
          </Fact>
        ) : null}
      </dl>

      {part.digestChanged ? (
        <Alert variant="destructive">
          <ShieldAlertIcon />
          <AlertTitle>Approval target changed</AlertTitle>
          <AlertDescription>
            The Gateway reported a different digest for this approval. Decisions
            are disabled so an earlier approval cannot be reused for the changed
            target.
          </AlertDescription>
        </Alert>
      ) : null}

      {expired && part.status === "pending" ? (
        <p className="text-micro text-muted-foreground">
          The expiry time has passed. The Gateway records the final outcome;
          decisions are disabled until it does.
        </p>
      ) : null}

      {error ? (
        <Alert variant="destructive" role="alert">
          <ShieldAlertIcon />
          <AlertTitle>{error.title}</AlertTitle>
          <AlertDescription>
            <p>{error.message}</p>
            {error.recovery ? <p>{error.recovery}</p> : null}
            {error.code ? (
              <p className="font-mono text-micro">{error.code}</p>
            ) : null}
          </AlertDescription>
        </Alert>
      ) : null}

      <div className="flex flex-wrap items-center gap-1.5">
        <Button
          type="button"
          size="sm"
          disabled={!canDecide}
          onClick={() => {
            void decide("approved")
          }}
        >
          <ShieldCheckIcon data-icon="inline-start" aria-hidden="true" />
          {busy === "approved" ? "Approving…" : "Approve"}
        </Button>
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={!canDecide}
          onClick={() => {
            void decide("rejected")
          }}
        >
          {busy === "rejected" ? "Rejecting…" : "Reject"}
        </Button>
        <Button
          type="button"
          size="sm"
          variant="destructive"
          disabled={!canRevoke}
          onClick={() => {
            void revoke()
          }}
        >
          {busy === "revoked" ? "Revoking…" : "Revoke"}
        </Button>
        {taskLink ? (
          <a
            href={taskLink}
            className="ml-auto text-micro text-gold underline-offset-2 hover:underline"
          >
            Open task
          </a>
        ) : null}
      </div>

      {!taskId ? (
        <p className="text-micro text-muted-foreground">
          The owning task id is not available yet, so this approval cannot be
          decided from this card.
        </p>
      ) : null}
    </section>
  )
}

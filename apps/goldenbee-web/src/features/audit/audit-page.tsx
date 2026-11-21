import { useQuery } from "@tanstack/react-query"
import { RefreshCwIcon, ShieldAlertIcon, TriangleAlertIcon } from "lucide-react"
import { useState, type FormEvent } from "react"
import { isGatewayError } from "@goldenbee/sdk"
import type {
  ControlAuditEntry,
  GatewayRequestOptions,
  RegistryAuditEntry,
} from "@goldenbee/sdk"

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "@/components/ui/empty"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import {
  useConnection,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import { redactText } from "@/features/evidence/redaction"

/** The read-only Gateway audit surface this page needs. */
export interface AuditGateway {
  workerAudit(
    worker?: string,
    options?: GatewayRequestOptions,
  ): Promise<RegistryAuditEntry[]>
  controlAudit(
    worker: string,
    session: string,
    options?: GatewayRequestOptions,
  ): Promise<ControlAuditEntry[]>
}

export interface AuditPageProps {
  store: ConnectionStore
  client: AuditGateway | null
}

const rowLimit = 100

function formatTime(value: number): string {
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? "Not reported" : date.toISOString()
}

function isAuthorizationError(cause: unknown): boolean {
  return (
    isGatewayError(cause) &&
    (cause.kind === "forbidden" || cause.kind === "unauthorized")
  )
}

function errorMessage(cause: unknown): string {
  if (isGatewayError(cause)) {
    return redactText(cause.message).text
  }
  return cause instanceof Error && cause.message
    ? redactText(cause.message).text
    : "The Gateway request failed."
}

function errorCode(cause: unknown): string {
  if (!isGatewayError(cause)) {
    return "unknown"
  }
  const code = cause.code ? `${cause.kind} · ${cause.code}` : cause.kind
  return redactText(code).text
}

function Cell({ value }: { value: unknown }) {
  const result = redactText(value)
  return (
    <span
      className="break-words"
      title={result.redacted ? "Redacted before display" : undefined}
    >
      {result.text.length > 0 ? result.text : "Not reported"}
    </span>
  )
}

function Outcome({ value }: { value: "accepted" | "denied" | "rejected" }) {
  return (
    <Badge
      variant={value === "accepted" ? "outline" : "destructive"}
      className="h-4 rounded px-1 text-micro"
    >
      {value}
    </Badge>
  )
}

function AuditFailure({ label, error }: { label: string; error: unknown }) {
  if (isAuthorizationError(error)) {
    return (
      <Alert variant="destructive" data-state="authorization-denied">
        <ShieldAlertIcon aria-hidden="true" />
        <AlertTitle>Not authorized to read {label} audit</AlertTitle>
        <AlertDescription>
          <p>
            The Gateway denied this audit read. The caller token does not hold the
            scope this evidence requires.
          </p>
          <p>{errorMessage(error)}</p>
          <p className="font-mono text-micro">{errorCode(error)}</p>
          {isGatewayError(error) && error.recovery ? (
            <p className="mt-1">
              <span className="font-medium text-foreground">Next: </span>
              {error.recovery}
            </p>
          ) : null}
        </AlertDescription>
      </Alert>
    )
  }
  return (
    <Alert variant="destructive">
      <TriangleAlertIcon aria-hidden="true" />
      <AlertTitle>
        {label === "registry" ? "Registry audit unavailable" : "Control audit unavailable"}
      </AlertTitle>
      <AlertDescription>
        <p>{errorMessage(error)}</p>
        <p className="font-mono text-micro">{errorCode(error)}</p>
      </AlertDescription>
    </Alert>
  )
}

function Section({
  title,
  description,
  children,
}: {
  title: string
  description: string
  children: React.ReactNode
}) {
  return (
    <section
      aria-label={title}
      className="grid gap-2 rounded-[9px] border border-line bg-surface/60 p-3"
    >
      <div>
        <h2 className="text-meta font-semibold">{title}</h2>
        <p className="mt-0.5 text-micro text-muted-foreground">{description}</p>
      </div>
      {children}
    </section>
  )
}

function rowCountNotice(shown: number, total: number): string | undefined {
  return total > shown ? `Showing ${shown} of ${total} entries` : undefined
}

/**
 * Read-only audit evidence.
 *
 * Registry decisions are filtered by Worker; control decisions need an explicit
 * Worker and session. A scope denial is a first-class typed state, never an
 * empty list. Every cell is redacted and escaped, and rows are bounded.
 */
export function AuditPage({ store, client }: AuditPageProps) {
  const workers = useConnection((state) => state.workers, store)
  const [workerId, setWorkerId] = useState(() => workers[0]?.id ?? "")
  const [sessionInput, setSessionInput] = useState("")
  const [appliedSession, setAppliedSession] = useState("")
  // An empty id is the explicit "all visible Workers" registry filter. Any
  // stale id falls back to the first visible Worker.
  const selectedId =
    workerId === "" || workers.some((worker) => worker.id === workerId)
      ? workerId
      : (workers[0]?.id ?? "")
  const selectedWorker = workers.find((worker) => worker.id === selectedId)

  const registry = useQuery({
    queryKey: ["audit", "registry", selectedId],
    queryFn: () => client!.workerAudit(selectedId || undefined),
    enabled: client !== null && workers.length > 0,
    retry: false,
  })

  const control = useQuery({
    queryKey: ["audit", "control", selectedId, appliedSession],
    queryFn: () => client!.controlAudit(selectedId, appliedSession),
    enabled:
      client !== null && selectedId !== "" && appliedSession !== "",
    retry: false,
  })

  function onControlSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    setAppliedSession(sessionInput.trim())
  }

  if (workers.length === 0) {
    return (
      <div className="grid gap-4">
        <header>
          <h1 className="font-heading text-panel font-semibold tracking-tight">
            Audit
          </h1>
        </header>
        <Empty className="border border-dashed border-line bg-surface/40">
          <EmptyHeader>
            <EmptyTitle>No Worker is visible to this caller</EmptyTitle>
            <EmptyDescription>
              Audit cannot be read without a visible Worker. Worker discovery is
              filtered by caller visibility at the Gateway.
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
      </div>
    )
  }

  const registryRows = (registry.data ?? []).slice(0, rowLimit)
  const controlRows = (control.data ?? []).slice(0, rowLimit)
  const registryNotice = rowCountNotice(
    registryRows.length,
    registry.data?.length ?? 0,
  )
  const controlNotice = rowCountNotice(
    controlRows.length,
    control.data?.length ?? 0,
  )

  return (
    <article className="grid gap-4" aria-label="Audit">
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <h1 className="font-heading text-panel font-semibold tracking-tight">
              Audit
            </h1>
            <Badge variant="outline" className="h-5 rounded-md px-1.5 text-micro text-gold">
              Read-only
            </Badge>
          </div>
          <p className="mt-1 max-w-2xl text-meta text-muted-foreground">
            Retained Gateway registry and Worker control decisions with principal,
            action, outcome, reason, resource, and time. This view never changes
            Gateway policy.
          </p>
        </div>
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={registry.isFetching}
          onClick={() => void registry.refetch()}
        >
          <RefreshCwIcon data-icon="inline-start" aria-hidden="true" />
          Refresh registry audit
        </Button>
      </header>

      <p className="flex items-start gap-1.5 rounded-[7px] border border-gold/20 bg-gold/5 px-2.5 py-2 text-micro text-muted-foreground">
        <ShieldAlertIcon className="mt-0.5 size-3 shrink-0 text-gold" aria-hidden="true" />
        <span>
          <span className="font-medium text-foreground">Redaction active.</span>{" "}
          Host paths, credentials, and token-shaped values are removed before
          rendering.
        </span>
      </p>

      <Section
        title="Registry audit"
        description="Gateway registry admission and lifecycle decisions, newest last."
      >
        <div className="flex flex-wrap items-end justify-between gap-2">
          <div className="grid min-w-[14rem] flex-1 gap-1.5">
            <label
              htmlFor="audit-registry-worker"
              className="text-micro font-semibold tracking-[0.12em] text-dim uppercase"
            >
              Worker filter
            </label>
            <select
              id="audit-registry-worker"
              aria-label="Registry Worker filter"
              className="h-8 w-full rounded-[7px] border border-line bg-surface-2 px-2 text-body text-foreground outline-none focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/30"
              value={selectedId}
              onChange={(event) => setWorkerId(event.target.value)}
            >
              <option value="">All visible Workers</option>
              {workers.map((worker) => (
                <option key={worker.id} value={worker.id}>
                  {worker.package} · {worker.instance}
                </option>
              ))}
            </select>
          </div>
          {registryNotice ? (
            <span className="text-micro text-dim">{registryNotice}</span>
          ) : null}
        </div>

        {registry.isLoading ? (
          <p role="status" className="flex items-center gap-2 text-meta text-muted-foreground">
            <Spinner className="size-3" /> Loading registry audit…
          </p>
        ) : registry.isError ? (
          <AuditFailure label="registry" error={registry.error} />
        ) : registryRows.length === 0 ? (
          <p className="text-meta text-muted-foreground">
            No registry decisions have been retained.
          </p>
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Time</TableHead>
                <TableHead>Principal</TableHead>
                <TableHead>Action</TableHead>
                <TableHead>Outcome</TableHead>
                <TableHead>Worker</TableHead>
                <TableHead>Reason and policy</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {registryRows.map((entry, index) => (
                <TableRow key={`${entry.at}-${index}`}>
                  <TableCell className="text-micro whitespace-nowrap text-dim">
                    {formatTime(entry.at)}
                  </TableCell>
                  <TableCell className="text-micro">
                    <Cell value={entry.principal} />
                  </TableCell>
                  <TableCell className="font-mono text-micro">
                    <Cell value={entry.action} />
                  </TableCell>
                  <TableCell>
                    <Outcome value={entry.outcome} />
                  </TableCell>
                  <TableCell className="font-mono text-micro">
                    <Cell value={entry.worker} />
                  </TableCell>
                  <TableCell className="text-micro text-muted-foreground">
                    <Cell value={entry.detail} />
                    <span className="mt-0.5 flex flex-wrap gap-1.5 text-dim">
                      {entry.policy_revision ? (
                        <span>policy {redactText(entry.policy_revision).text}</span>
                      ) : null}
                      {entry.correlation ? (
                        <span>{redactText(entry.correlation).text}</span>
                      ) : null}
                      {entry.source ? (
                        <span>{redactText(entry.source).text}</span>
                      ) : null}
                    </span>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </Section>

      <Section
        title="Control audit"
        description="Interactive control decisions retained by one Worker for one session."
      >
        <form onSubmit={onControlSubmit} className="flex flex-wrap items-end gap-2">
          <div className="grid min-w-[14rem] flex-1 gap-1.5">
            <label
              htmlFor="audit-session"
              className="text-micro font-semibold tracking-[0.12em] text-dim uppercase"
            >
              Session id
            </label>
            <Input
              id="audit-session"
              aria-label="Session id"
              placeholder="Session id visible to this caller"
              autoComplete="off"
              spellCheck={false}
              value={sessionInput}
              onChange={(event) => setSessionInput(event.target.value)}
              className="h-8"
            />
          </div>
          <Button
            type="submit"
            size="sm"
            variant="outline"
            disabled={selectedWorker === undefined || sessionInput.trim() === ""}
          >
            Load control audit
          </Button>
          {controlNotice ? (
            <span className="text-micro text-dim">{controlNotice}</span>
          ) : null}
        </form>

        {selectedWorker ? (
          <p className="text-micro text-dim">
            Worker {redactText(selectedWorker.id).text} ·{" "}
            {redactText(selectedWorker.package).text}
          </p>
        ) : null}

        {selectedId === "" ? (
          <p className="text-meta text-muted-foreground">
            Select one Worker to read control decisions.
          </p>
        ) : appliedSession === "" ? (
          <p className="text-meta text-muted-foreground">
            Enter a session id to read Worker control decisions.
          </p>
        ) : control.isLoading ? (
          <p role="status" className="flex items-center gap-2 text-meta text-muted-foreground">
            <Spinner className="size-3" /> Loading control audit…
          </p>
        ) : control.isError ? (
          <AuditFailure label="control" error={control.error} />
        ) : controlRows.length === 0 ? (
          <p className="text-meta text-muted-foreground">
            No control decisions were retained for this session.
          </p>
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Time</TableHead>
                <TableHead>Principal</TableHead>
                <TableHead>Action</TableHead>
                <TableHead>Outcome</TableHead>
                <TableHead>Session</TableHead>
                <TableHead>Generation</TableHead>
                <TableHead>Reason</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {controlRows.map((entry, index) => (
                <TableRow key={`${entry.at}-${index}`}>
                  <TableCell className="text-micro whitespace-nowrap text-dim">
                    {formatTime(entry.at)}
                  </TableCell>
                  <TableCell className="text-micro">
                    <Cell value={entry.principal} />
                  </TableCell>
                  <TableCell className="font-mono text-micro">
                    <Cell value={entry.action} />
                  </TableCell>
                  <TableCell>
                    <Outcome value={entry.outcome} />
                  </TableCell>
                  <TableCell className="font-mono text-micro">
                    <Cell value={entry.session_id} />
                  </TableCell>
                  <TableCell className="text-micro tabular-nums text-muted-foreground">
                    generation {entry.generation}
                  </TableCell>
                  <TableCell className="text-micro text-muted-foreground">
                    <Cell value={entry.detail} />
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </Section>
    </article>
  )
}

import { useQuery } from "@tanstack/react-query"
import {
  DatabaseIcon,
  RefreshCwIcon,
  ShieldAlertIcon,
  TriangleAlertIcon,
} from "lucide-react"
import { useMemo, useState, type FormEvent } from "react"
import { isGatewayError } from "@goldenbee/sdk"
import type {
  DiagnosticBundle,
  GatewayRequestOptions,
  WorkerSummary,
} from "@goldenbee/sdk"

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "@/components/ui/empty"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { StatusDot } from "@/components/shell/status-dot"
import {
  useConnection,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import {
  bounded,
  redactText,
  safeRows,
  scanRedaction,
} from "@/features/evidence/redaction"

/** The read-only Gateway surface the diagnostics page needs. */
export interface DiagnosticsGateway {
  diagnostics(
    worker: string,
    task?: string,
    options?: GatewayRequestOptions,
  ): Promise<DiagnosticBundle>
}

export interface DiagnosticsPageProps {
  store: ConnectionStore
  client: DiagnosticsGateway | null
}

const metricLimit = 20
const findingLimit = 20
const traceLimit = 10
const auditLimit = 20
const nestedLimit = 10
const selectClass =
  "h-8 w-full rounded-[7px] border border-line bg-surface-2 px-2 text-body text-foreground outline-none focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/30"

function formatTime(value: unknown): string {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return "Not reported"
  }
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
  return cause.code ? `${cause.kind} · ${cause.code}` : cause.kind
}

function Text({ value, limit }: { value: unknown; limit?: number }) {
  const result = redactText(value, limit)
  if (result.text.length === 0) {
    return <>Not reported</>
  }
  return <span title={result.redacted ? "Redacted before display" : undefined}>{result.text}</span>
}

function Fact({
  label,
  children,
}: {
  label: string
  children: React.ReactNode
}) {
  return (
    <div className="grid grid-cols-[9rem_minmax(0,1fr)] gap-2 border-b border-line/70 py-1.5 last:border-b-0">
      <dt className="text-micro text-dim">{label}</dt>
      <dd className="min-w-0 break-words text-micro text-foreground">
        {children}
      </dd>
    </div>
  )
}

function Section({
  title,
  description,
  count,
  children,
}: {
  title: string
  description?: string
  count?: string
  children: React.ReactNode
}) {
  return (
    <section
      aria-label={title}
      className="grid gap-2 rounded-[9px] border border-line bg-surface/60 p-3"
    >
      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <h2 className="text-meta font-semibold">{title}</h2>
        {count ? <span className="text-micro text-dim">{count}</span> : null}
      </div>
      {description ? (
        <p className="text-micro text-muted-foreground">{description}</p>
      ) : null}
      {children}
    </section>
  )
}

function Rows({ value, limit }: { value: unknown; limit?: number }) {
  const result = safeRows(value, limit)
  if (result.rows.length === 0) {
    return (
      <p className="text-meta text-muted-foreground">
        No flat facts were reported.
      </p>
    )
  }
  return (
    <dl>
      {result.rows.map((row) => (
        <Fact key={row.label} label={row.label}>
          <span
            className="whitespace-pre-wrap"
            title={row.redacted ? "Redacted before display" : undefined}
          >
            {row.value}
          </span>
        </Fact>
      ))}
    </dl>
  )
}

function Capacity({ worker }: { worker: WorkerSummary }) {
  const capacity = worker.capacity
  if (!capacity) {
    return (
      <p className="text-meta text-muted-foreground">
        The Worker summary reports no queue or capacity counters. Missing values
        are not treated as zero.
      </p>
    )
  }
  return (
    <div className="flex flex-wrap gap-2">
      <Badge variant="outline" className="h-5 rounded-md px-1.5 text-micro tabular-nums">
        {capacity.active} / {capacity.limit} active
      </Badge>
      <Badge variant="outline" className="h-5 rounded-md px-1.5 text-micro tabular-nums">
        {capacity.queued} / {capacity.queue_limit} queued
      </Badge>
    </div>
  )
}

function findingMessage(value: unknown): string {
  return redactText(value).text
}

/**
 * Operator diagnostics surface.
 *
 * It reads one Worker's redacted bundle through the typed SDK. Rendering is
 * bounded and every scalar is passed through redaction; nested payloads are
 * never serialized as JSON. The page is read-only and cannot change Gateway
 * policy.
 */
export function DiagnosticsPage({ store, client }: DiagnosticsPageProps) {
  const workers = useConnection((state) => state.workers, store)
  const [workerId, setWorkerId] = useState(() => workers[0]?.id ?? "")
  const [taskInput, setTaskInput] = useState("")
  const [appliedTask, setAppliedTask] = useState("")
  const selectedId = workers.some((worker) => worker.id === workerId)
    ? workerId
    : (workers[0]?.id ?? "")
  const selectedWorker = workers.find((worker) => worker.id === selectedId)

  const query = useQuery({
    queryKey: ["diagnostics", selectedId, appliedTask],
    queryFn: () =>
      client!.diagnostics(selectedId, appliedTask || undefined),
    enabled: client !== null && selectedId !== "",
    retry: false,
  })

  const bundle = query.data
  const scan = useMemo(
    () =>
      bundle
        ? scanRedaction(bundle)
        : { redacted: false, bounded: false },
    [bundle],
  )

  function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    setAppliedTask(taskInput.trim())
  }

  if (workers.length === 0) {
    return (
      <div className="grid gap-4">
        <header>
          <h1 className="font-heading text-panel font-semibold tracking-tight">
            Diagnostics
          </h1>
        </header>
        <Empty className="border border-dashed border-line bg-surface/40">
          <EmptyHeader>
            <EmptyTitle>No Worker is visible to this caller</EmptyTitle>
            <EmptyDescription>
              Diagnostics cannot be requested without a visible Worker. Worker
              discovery is filtered by caller visibility at the Gateway.
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
      </div>
    )
  }

  const metrics = bundle ? bounded(bundle.metrics, metricLimit) : []
  const findings = bundle ? bounded(bundle.findings, findingLimit) : []
  const traces = bundle ? bounded(bundle.traces, traceLimit) : []
  const auditTail = bundle ? bounded(bundle.audit, auditLimit) : []

  return (
    <article className="grid gap-4" aria-label="Diagnostics">
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <h1 className="font-heading text-panel font-semibold tracking-tight">
              Diagnostics
            </h1>
            <Badge variant="outline" className="h-5 rounded-md px-1.5 text-micro text-gold">
              Read-only
            </Badge>
          </div>
          <p className="mt-1 max-w-2xl text-meta text-muted-foreground">
            One Worker&apos;s redacted diagnostic bundle: health, capacity,
            findings, storage, bounded traces, and an optional task diagnosis.
            This view never changes Gateway policy.
          </p>
        </div>
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={query.isFetching}
          onClick={() => void query.refetch()}
        >
          <RefreshCwIcon data-icon="inline-start" aria-hidden="true" />
          Refresh diagnostics
        </Button>
      </header>

      <div className="grid gap-3 rounded-[9px] border border-line bg-surface/60 p-3">
        <div className="grid gap-1.5">
          <label
            htmlFor="diagnostics-worker"
            className="text-micro font-semibold tracking-[0.12em] text-dim uppercase"
          >
            Worker
          </label>
          <select
            id="diagnostics-worker"
            aria-label="Worker"
            className={selectClass}
            value={selectedId}
            onChange={(event) => setWorkerId(event.target.value)}
          >
            {workers.map((worker) => (
              <option key={worker.id} value={worker.id}>
                {worker.package} · {worker.instance}
              </option>
            ))}
          </select>
        </div>

        <form onSubmit={onSubmit} className="flex flex-wrap items-end gap-2">
          <div className="grid min-w-[14rem] flex-1 gap-1.5">
            <label
              htmlFor="diagnostics-task"
              className="text-micro font-semibold tracking-[0.12em] text-dim uppercase"
            >
              Task id filter (optional)
            </label>
            <Input
              id="diagnostics-task"
              aria-label="Task id filter"
              placeholder="Task id visible to this caller"
              autoComplete="off"
              spellCheck={false}
              value={taskInput}
              onChange={(event) => setTaskInput(event.target.value)}
              className="h-8"
            />
          </div>
          <Button type="submit" size="sm" variant="outline">
            Apply filter
          </Button>
          {appliedTask ? (
            <Button
              type="button"
              size="sm"
              variant="ghost"
              onClick={() => {
                setTaskInput("")
                setAppliedTask("")
              }}
            >
              Clear filter
            </Button>
          ) : null}
        </form>

        <div className="grid gap-1 text-micro text-muted-foreground">
          <p className="flex items-start gap-1.5">
            <ShieldAlertIcon
              className="mt-0.5 size-3 shrink-0 text-gold"
              aria-hidden="true"
            />
            <span>
              <span className="font-medium text-foreground">Redaction active.</span>{" "}
              Host paths, credentials, prompt text, and token values are not
              rendered.
            </span>
          </p>
          {scan.redacted ? (
            <p className="text-gold">Some reported values were redacted before display.</p>
          ) : null}
          {scan.bounded ? (
            <p className="text-dim">
              The payload is larger than the display bound; only a sample is shown.
            </p>
          ) : null}
        </div>
      </div>

      {query.isLoading ? (
        <p role="status" className="flex items-center gap-2 text-meta text-muted-foreground">
          <Spinner className="size-3" /> Loading diagnostics…
        </p>
      ) : query.isError ? (
        isAuthorizationError(query.error) ? (
          <Alert variant="destructive" data-state="authorization-denied">
            <ShieldAlertIcon aria-hidden="true" />
            <AlertTitle>Not authorized to read diagnostics</AlertTitle>
            <AlertDescription>
              <p>The Gateway denied this diagnostic read for the selected Worker.</p>
              <p>{errorMessage(query.error)}</p>
              <p className="font-mono text-micro">{errorCode(query.error)}</p>
              {isGatewayError(query.error) && query.error.recovery ? (
                <p className="mt-1">
                  <span className="font-medium text-foreground">Next: </span>
                  {redactText(query.error.recovery).text}
                </p>
              ) : null}
            </AlertDescription>
          </Alert>
        ) : (
          <Alert variant="destructive">
            <TriangleAlertIcon aria-hidden="true" />
            <AlertTitle>Diagnostics unavailable</AlertTitle>
            <AlertDescription>
              <p>{errorMessage(query.error)}</p>
              <p className="font-mono text-micro">{errorCode(query.error)}</p>
            </AlertDescription>
          </Alert>
        )
      ) : null}

      {bundle ? (
        <>
          <Section
            title="Gateway health"
            count={`Generated ${formatTime(bundle.generated_at)}`}
            description="Health facts reported by the owning Worker runtime. Values are shown as reported."
          >
            <Rows value={bundle.health} />
          </Section>

          <Section title="Worker reachability">
            {selectedWorker ? (
              <dl>
                <Fact label="Worker id">{selectedWorker.id}</Fact>
                <Fact label="Instance">{selectedWorker.instance}</Fact>
                <Fact label="Package">{selectedWorker.package}</Fact>
                <Fact label="Status">
                  <StatusDot
                    tone={selectedWorker.reachable ? "positive" : "danger"}
                  >
                    {selectedWorker.reachable ? "Reachable" : "Unreachable"} ·{" "}
                    {selectedWorker.status}
                  </StatusDot>
                </Fact>
                <Fact label="Locality">{selectedWorker.locality}</Fact>
                <Fact label="Lease expires">
                  {formatTime(selectedWorker.lease_expires_at)}
                </Fact>
              </dl>
            ) : (
              <p className="text-meta text-muted-foreground">
                The selected Worker is no longer in the discovery result.
              </p>
            )}
          </Section>

          <Section
            title="Queue and capacity"
            description="Counters come from the Worker summary; missing counters stay unknown."
          >
            {selectedWorker ? (
              <Capacity worker={selectedWorker} />
            ) : (
              <p className="text-meta text-muted-foreground">
                Capacity is unavailable for this Worker.
              </p>
            )}
          </Section>

          <Section
            title="Metrics"
            count={
              bundle.metrics.length > 0
                ? `Showing ${metrics.length} of ${bundle.metrics.length} metrics`
                : undefined
            }
          >
            {metrics.length === 0 ? (
              <p className="text-meta text-muted-foreground">
                No metric samples were reported.
              </p>
            ) : (
              <ul className="grid gap-1">
                {metrics.map((metric, index) => {
                  const name = redactText(metric.name)
                  const quality = redactText(metric.quality)
                  const value =
                    metric.value === null || metric.value === undefined
                      ? "unknown"
                      : redactText(metric.value).text
                  const labels = safeRows(metric.labels, 8)
                  return (
                    <li
                      key={`${name.text}-${index}`}
                      className="grid grid-cols-[minmax(0,1fr)_auto] gap-2 border-b border-line/60 py-1 text-micro last:border-b-0"
                    >
                      <span className="break-words font-mono text-foreground">
                        {name.text}
                      </span>
                      <span className="text-right text-muted-foreground tabular-nums">
                        {value}
                        <span className="text-dim"> · {quality.text}</span>
                      </span>
                      {labels.rows.length > 0 ? (
                        <span className="col-span-2 text-dim">
                          {labels.rows
                            .map((row) => `${row.label}=${row.value}`)
                            .join(" · ")}
                        </span>
                      ) : null}
                    </li>
                  )
                })}
              </ul>
            )}
          </Section>

          <Section
            title="Findings"
            count={
              bundle.findings.length > 0
                ? `Showing ${findings.length} of ${bundle.findings.length} findings`
                : undefined
            }
            description="Actionable conditions reported by the Worker runtime."
          >
            {findings.length === 0 ? (
              <p className="text-meta text-muted-foreground">
                No findings were reported.
              </p>
            ) : (
              <ol className="grid gap-1.5">
                {findings.map((finding, index) => (
                  <li
                    key={`${finding.code}-${index}`}
                    className="rounded-[7px] border border-line bg-surface-2 px-2.5 py-2"
                  >
                    <div className="flex flex-wrap items-center gap-2">
                      <Badge variant="outline" className="h-4 rounded px-1 text-micro">
                        {redactText(finding.kind).text}
                      </Badge>
                      <span className="font-mono text-micro text-foreground">
                        {redactText(finding.code).text}
                      </span>
                      <span className="text-micro text-dim">
                        {redactText(finding.source).text}
                      </span>
                      {finding.trace_id ? (
                        <span className="ml-auto font-mono text-micro text-dim">
                          {redactText(finding.trace_id).text}
                        </span>
                      ) : null}
                    </div>
                    <p className="mt-1 text-micro break-words text-muted-foreground">
                      {findingMessage(finding.message)}
                    </p>
                    <p className="mt-0.5 text-micro text-foreground">
                      <span className="font-medium">Next: </span>
                      {redactText(finding.action).text}
                    </p>
                  </li>
                ))}
              </ol>
            )}
          </Section>

          <Section title="Storage">
            <dl>
              <Fact label="Metadata">
                <Text value={bundle.storage.metadata} />
              </Fact>
              <Fact label="Artifacts">
                <Text value={bundle.storage.artifacts} />
              </Fact>
            </dl>
            {bundle.storage.degraded.length === 0 ? (
              <p className="text-meta text-muted-foreground">
                No degraded storage observations were reported.
              </p>
            ) : (
              <ul className="grid gap-1">
                {bounded(bundle.storage.degraded, nestedLimit).map((reason, index) => (
                  <li key={index} className="text-micro text-muted-foreground">
                    {redactText(reason).text}
                  </li>
                ))}
              </ul>
            )}
          </Section>

          <Section
            title="Subsystems"
            description="Subsystem summaries contain no instance paths."
          >
            <Rows value={bundle.subsystems} limit={20} />
          </Section>

          <Section
            title="Traces"
            count={
              bundle.traces.length > 0
                ? `Showing ${traces.length} of ${bundle.traces.length} traces`
                : undefined
            }
            description="Redacted trace samples; attributes are flat and bounded."
          >
            {traces.length === 0 ? (
              <p className="text-meta text-muted-foreground">
                No trace samples were reported.
              </p>
            ) : (
              <ol className="grid gap-1.5">
                {traces.map((trace, index) => {
                  const span = redactText(trace.span)
                  const duration = redactText(trace.duration_ms)
                  const attributes = safeRows(trace.attributes, 8)
                  return (
                    <li
                      key={`${span.text}-${index}`}
                      className="rounded-[7px] border border-line bg-surface-2 px-2.5 py-2"
                    >
                      <div className="flex flex-wrap items-center gap-2">
                        <span className="font-mono text-micro text-foreground">
                          {span.text}
                        </span>
                        <span className="text-micro tabular-nums text-muted-foreground">
                          {duration.text} ms
                        </span>
                        <span className="ml-auto font-mono text-micro text-dim">
                          {redactText(trace.trace_id).text}
                        </span>
                      </div>
                      <p className="mt-0.5 text-micro text-dim">
                        {redactText(trace.task_id).text} ·{" "}
                        {formatTime(trace.at)}
                      </p>
                      {attributes.rows.length > 0 ? (
                        <p className="mt-1 text-micro text-muted-foreground">
                          {attributes.rows
                            .map((row) => `${row.label}=${row.value}`)
                            .join(" · ")}
                        </p>
                      ) : null}
                    </li>
                  )
                })}
              </ol>
            )}
          </Section>

          {bundle.task ? (
            <Section
              title="Task diagnosis"
              description="The task projection is included without prompt text or approval targets."
            >
              <dl>
                <Fact label="Task id">
                  <Text value={bundle.task.task_id} />
                </Fact>
                <Fact label="State">{bundle.task.state}</Fact>
                <Fact label="Attempt">{bundle.task.attempt}</Fact>
                <Fact label="Profile">
                  {`${redactText(bundle.task.profile).text} · revision ${bundle.task.profile_revision}`}
                </Fact>
                <Fact label="Asset revision">{bundle.task.asset_revision}</Fact>
                <Fact label="Traffic">{bundle.task.traffic}</Fact>
                <Fact label="Trace">
                  <Text value={bundle.task.trace_id} />
                </Fact>
                <Fact label="Native invocation">
                  <Text value={bundle.task.native_invocation_id} />
                </Fact>
              </dl>
              {bundle.task.error ? (
                <Alert variant="destructive">
                  <TriangleAlertIcon aria-hidden="true" />
                  <AlertTitle>Task error</AlertTitle>
                  <AlertDescription>
                    <p>{redactText(bundle.task.error.message).text}</p>
                    <p className="font-mono text-micro">
                      {redactText(bundle.task.error.code).text}
                    </p>
                  </AlertDescription>
                </Alert>
              ) : null}
              <p className="text-micro text-dim">
                {(bundle.task.delegations ?? []).length} delegations ·{" "}
                {(bundle.task.approvals ?? []).length} approvals
              </p>
            </Section>
          ) : null}

          <Section
            title="Audit tail"
            count={
              bundle.audit.length > 0
                ? `Showing ${auditTail.length} of ${bundle.audit.length} entries`
                : undefined
            }
            description="Redacted decisions retained by the Worker runtime."
          >
            {auditTail.length === 0 ? (
              <p className="text-meta text-muted-foreground">
                No audit entries were included in this bundle.
              </p>
            ) : (
              <ol className="grid gap-1.5">
                {auditTail.map((entry, index) => (
                  <li
                    key={index}
                    className="rounded-[7px] border border-line bg-surface-2 px-2.5 py-2"
                  >
                    <Rows value={entry} limit={8} />
                  </li>
                ))}
              </ol>
            )}
          </Section>

          <p className="flex items-center gap-1.5 text-micro text-dim">
            <DatabaseIcon className="size-3" aria-hidden="true" />
            Diagnostics is a read-only projection. Disconnecting the browser does
            not change Worker state.
          </p>
        </>
      ) : null}
    </article>
  )
}

import { useQuery } from "@tanstack/react-query"
import {
  ArrowLeftIcon,
  RefreshCwIcon,
  ShieldCheckIcon,
  TriangleAlertIcon,
} from "lucide-react"
import { isGatewayError } from "@goldenbee/sdk"
import type {
  GatewayApproval,
  GatewayRequestOptions,
  GatewayTask,
  StreamEvent,
  StreamPage,
  TaskUsage,
} from "@goldenbee/sdk"

import { routeHref } from "@/app/router"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "@/components/ui/empty"
import { Spinner } from "@/components/ui/spinner"

/** The read-only Gateway surface a task detail page needs. */
export interface TaskDetailGateway {
  task(taskId: string, options?: GatewayRequestOptions): Promise<GatewayTask>
  events(
    taskId: string,
    after?: number,
    limit?: number,
    options?: GatewayRequestOptions,
  ): Promise<StreamPage>
  approvals(
    taskId: string,
    pendingOnly?: boolean,
    options?: GatewayRequestOptions,
  ): Promise<GatewayApproval[]>
}

export interface TaskDetailPageProps {
  client: TaskDetailGateway | null
  taskId: string
}

const eventLimit = 500

const sensitiveKey =
  /secret|token|passw|credential|authorization|cookie|api[-_]?key|private[-_]?key|bearer/i

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

/**
 * Extract only flat, non-sensitive primitive facts.
 *
 * Nested structures are not dumped as JSON, and keys that name credentials are
 * skipped. React escapes every value, so payload HTML is text, never markup.
 */
function safeFacts(value: unknown): Array<[string, string]> {
  if (!isRecord(value)) {
    return []
  }
  const facts: Array<[string, string]> = []
  for (const [key, entry] of Object.entries(value)) {
    if (sensitiveKey.test(key)) {
      continue
    }
    if (
      typeof entry !== "string" &&
      typeof entry !== "number" &&
      typeof entry !== "boolean"
    ) {
      continue
    }
    const text = String(entry)
    facts.push([key, text.length > 512 ? `${text.slice(0, 512)}…` : text])
  }
  return facts
}

function formatTime(value: number | undefined): string {
  return value === undefined ? "Not reported" : new Date(value).toISOString()
}

function errorMessage(cause: unknown): string {
  if (isGatewayError(cause)) {
    return cause.message
  }
  return cause instanceof Error && cause.message
    ? cause.message
    : "The Gateway request failed."
}

function isDenied(cause: unknown): boolean {
  return (
    isGatewayError(cause) &&
    (cause.kind === "forbidden" || cause.kind === "unauthorized")
  )
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

function LoadFailure({
  label,
  error,
}: {
  label: string
  error: unknown
}) {
  return (
    <Alert variant="destructive">
      <TriangleAlertIcon />
      <AlertTitle>
        {isDenied(error)
          ? `Not authorized to read ${label}`
          : `${label} could not be read`}
      </AlertTitle>
      <AlertDescription>{errorMessage(error)}</AlertDescription>
    </Alert>
  )
}

function usageNumber(usage: TaskUsage | undefined, key: string): number | undefined {
  const value = usage?.[key]
  return typeof value === "number" && Number.isFinite(value) ? value : undefined
}

function UsageFacts({ usage }: { usage: TaskUsage | undefined }) {
  const rows: Array<[string, number | undefined]> = [
    ["Prompt tokens", usageNumber(usage, "prompt_tokens")],
    ["Candidate tokens", usageNumber(usage, "candidate_tokens")],
    ["Estimated tokens", usageNumber(usage, "estimated_tokens")],
    ["Estimated cost (micro-units)", usageNumber(usage, "estimated_cost_micros")],
    ["Model calls", usageNumber(usage, "model_calls")],
    ["Tool calls", usageNumber(usage, "tool_calls")],
  ]
  if (rows.every(([, value]) => value === undefined)) {
    return (
      <p className="text-meta text-muted-foreground">
        The task projection reports no usage counters. Missing values are not
        treated as zero.
      </p>
    )
  }
  return (
    <dl>
      {rows
        .filter(([, value]) => value !== undefined)
        .map(([label, value]) => (
          <Fact key={label} label={label}>
            <span className="tabular-nums">{value}</span>
          </Fact>
        ))}
    </dl>
  )
}

function EventRow({ event }: { event: StreamEvent }) {
  const facts = safeFacts(event.payload)
  return (
    <li className="rounded-[7px] border border-line bg-surface-2 px-2.5 py-2">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-mono text-micro text-foreground">
          #{event.seq} {event.kind}
        </span>
        <Badge variant="outline" className="h-4 rounded px-1 text-micro">
          {event.durable ? "durable" : "ephemeral"}
        </Badge>
        <span className="text-micro text-dim">{formatTime(event.at)}</span>
      </div>
      {facts.length > 0 ? (
        <dl className="mt-1 grid gap-0.5">
          {facts.map(([key, value]) => (
            <div
              key={key}
              className="grid grid-cols-[8rem_minmax(0,1fr)] gap-2 text-micro"
            >
              <dt className="text-dim">{key}</dt>
              <dd className="break-words whitespace-pre-wrap text-muted-foreground">
                {value}
              </dd>
            </div>
          ))}
        </dl>
      ) : null}
    </li>
  )
}

function ApprovalRow({ approval }: { approval: GatewayApproval }) {
  const record = approval.approval
  return (
    <li className="rounded-[7px] border border-line bg-surface-2 px-2.5 py-2">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-mono text-micro text-foreground">{record.id}</span>
        <Badge variant="outline" className="h-4 rounded px-1 text-micro">
          {record.status}
        </Badge>
      </div>
      <dl className="mt-1">
        <Fact label="Operation">
          <span className="font-mono">{record.operation}</span>
        </Fact>
        <Fact label="Target">
          <span className="font-mono">{record.target}</span>
        </Fact>
        <Fact label="Digest">
          <span className="break-all font-mono">{record.digest}</span>
        </Fact>
        <Fact label="Risk">{record.risk}</Fact>
        <Fact label="Requested by">{record.requested_by}</Fact>
        <Fact label="Expires">{formatTime(record.expires_at)}</Fact>
        {record.decided_by ? (
          <Fact label="Decided by">{record.decided_by}</Fact>
        ) : null}
        {record.decided_at !== undefined ? (
          <Fact label="Decided at">{formatTime(record.decided_at)}</Fact>
        ) : null}
        {record.receipt ? (
          <Fact label="Receipt">
            <span className="break-all font-mono">{record.receipt}</span>
          </Fact>
        ) : null}
      </dl>
    </li>
  )
}

/**
 * Durable task evidence page.
 *
 * It reads the canonical task projection, a retained event page, and the task
 * approval list directly from the Gateway. It does not depend on a live chat
 * stream, so history and approvals stay readable after that stream closes.
 */
export function TaskDetailPage({ client, taskId }: TaskDetailPageProps) {
  const taskQuery = useQuery({
    queryKey: ["task-detail", taskId, "task"],
    queryFn: () => client!.task(taskId),
    enabled: client !== null,
    retry: false,
  })
  const eventsQuery = useQuery({
    queryKey: ["task-detail", taskId, "events"],
    queryFn: () => client!.events(taskId, 0, eventLimit),
    enabled: client !== null,
    retry: false,
  })
  const approvalsQuery = useQuery({
    queryKey: ["task-detail", taskId, "approvals"],
    queryFn: () => client!.approvals(taskId),
    enabled: client !== null,
    retry: false,
  })

  if (client === null) {
    return (
      <Empty className="border border-dashed border-line bg-surface/40">
        <EmptyHeader>
          <EmptyTitle>Task evidence is unavailable</EmptyTitle>
          <EmptyDescription>
            Connect to a Gateway to read this task projection and its retained
            events.
          </EmptyDescription>
        </EmptyHeader>
      </Empty>
    )
  }

  const task = taskQuery.data?.task
  const events = eventsQuery.data?.events ?? []
  const approvals = approvalsQuery.data ?? []
  const evidenceFacts = safeFacts(task?.evidence)
  const refreshing =
    taskQuery.isFetching || eventsQuery.isFetching || approvalsQuery.isFetching

  return (
    <article className="grid gap-4" aria-label={`Task ${taskId}`}>
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div className="min-w-0">
          <a
            href={routeHref({ name: "chat" })}
            className="inline-flex items-center gap-1 text-micro text-gold underline-offset-2 hover:underline"
          >
            <ArrowLeftIcon className="size-3" aria-hidden="true" />
            Back to sessions
          </a>
          <h1 className="mt-1 truncate font-mono text-panel font-semibold tracking-tight">
            {taskId}
          </h1>
          <p className="mt-1 text-meta text-muted-foreground">
            Durable task projection, retained events, and recorded approvals.
            This page does not depend on the live chat stream.
          </p>
        </div>
        <div className="flex items-center gap-2">
          {task ? (
            <Badge variant="outline" className="h-5 rounded-md px-1.5 text-micro">
              {task.state}
            </Badge>
          ) : null}
          <Button
            type="button"
            size="sm"
            variant="outline"
            disabled={refreshing}
            onClick={() => {
              void Promise.all([
                taskQuery.refetch(),
                eventsQuery.refetch(),
                approvalsQuery.refetch(),
              ])
            }}
          >
            <RefreshCwIcon data-icon="inline-start" aria-hidden="true" />
            Refresh
          </Button>
        </div>
      </div>

      {taskQuery.isLoading ? (
        <p role="status" className="text-meta text-muted-foreground">
          <Spinner className="mr-1 inline size-3" /> Loading task projection…
        </p>
      ) : taskQuery.isError ? (
        <LoadFailure label="the task projection" error={taskQuery.error} />
      ) : null}

      {task ? (
        <section aria-label="Task state" className="grid gap-2 rounded-[9px] border border-line bg-surface/60 p-3">
          <h2 className="text-meta font-semibold">Task state</h2>
          <dl>
            <Fact label="State">{task.state}</Fact>
            <Fact label="Task id">{task.id}</Fact>
            <Fact label="Worker">{taskQuery.data?.worker ?? "Not reported"}</Fact>
            <Fact label="Worker instance">
              {taskQuery.data?.worker_instance ?? "Not reported"}
            </Fact>
            <Fact label="Connectivity">
              {taskQuery.data?.connectivity ?? "Not reported"}
            </Fact>
            <Fact label="Agent">{task.agent || "Not reported"}</Fact>
            <Fact label="Session id">{task.session_id || "Not reported"}</Fact>
            <Fact label="Profile">{task.profile ?? "Worker default / none"}</Fact>
            <Fact label="Attempt">{task.attempt ?? "Not reported"}</Fact>
            <Fact label="Created">{formatTime(task.created_at)}</Fact>
            <Fact label="Updated">{formatTime(task.updated_at)}</Fact>
            {task.uncertainty ? (
              <Fact label="Uncertainty">{task.uncertainty}</Fact>
            ) : null}
          </dl>
          {task.output ? (
            <div>
              <h3 className="text-micro font-semibold tracking-wide text-dim uppercase">
                Output
              </h3>
              <p className="mt-1 text-sm whitespace-pre-wrap">{task.output}</p>
            </div>
          ) : null}
          {task.error ? (
            <Alert variant="destructive">
              <TriangleAlertIcon />
              <AlertTitle>Task error</AlertTitle>
              <AlertDescription>
                <p>{task.error.message}</p>
                {task.error.recovery ? <p>{task.error.recovery}</p> : null}
                <p className="font-mono text-micro">{task.error.code}</p>
              </AlertDescription>
            </Alert>
          ) : null}
        </section>
      ) : null}

      {task ? (
        <section aria-label="Task usage" className="grid gap-2 rounded-[9px] border border-line bg-surface/60 p-3">
          <h2 className="text-meta font-semibold">Usage</h2>
          <UsageFacts usage={task.usage} />
        </section>
      ) : null}

      {task ? (
        <section aria-label="Effect evidence" className="grid gap-2 rounded-[9px] border border-line bg-surface/60 p-3">
          <h2 className="text-meta font-semibold">Effect evidence</h2>
          {evidenceFacts.length === 0 ? (
            <p className="text-meta text-muted-foreground">
              No safe flat evidence facts were reported for this task.
            </p>
          ) : (
            <dl>
              {evidenceFacts.map(([key, value]) => (
                <Fact key={key} label={key}>
                  <span className="whitespace-pre-wrap">{value}</span>
                </Fact>
              ))}
            </dl>
          )}
        </section>
      ) : null}

      <section aria-label="Retained events" className="grid gap-2 rounded-[9px] border border-line bg-surface/60 p-3">
        <div className="flex items-center justify-between gap-2">
          <h2 className="text-meta font-semibold">Retained events</h2>
          <span className="text-micro text-dim">
            {eventsQuery.data ? `${events.length} events` : "Not loaded"}
          </span>
        </div>
        {eventsQuery.data?.cursor_expired ? (
          <Alert>
            <TriangleAlertIcon />
            <AlertTitle>Event retention gap</AlertTitle>
            <AlertDescription>
              The requested cursor is older than the retained window. Earlier
              events are not available on this page.
            </AlertDescription>
          </Alert>
        ) : null}
        {eventsQuery.isLoading ? (
          <p role="status" className="text-meta text-muted-foreground">
            Loading retained events…
          </p>
        ) : eventsQuery.isError ? (
          <LoadFailure label="retained events" error={eventsQuery.error} />
        ) : events.length === 0 ? (
          <p className="text-meta text-muted-foreground">
            No retained events were reported for this task.
          </p>
        ) : (
          <ol className="grid gap-1.5">
            {events.map((event) => (
              <EventRow key={event.event_id} event={event} />
            ))}
          </ol>
        )}
      </section>

      <section aria-label="Task approvals" className="grid gap-2 rounded-[9px] border border-line bg-surface/60 p-3">
        <div className="flex items-center gap-2">
          <ShieldCheckIcon className="size-4 text-gold" aria-hidden="true" />
          <h2 className="text-meta font-semibold">Approvals</h2>
        </div>
        {approvalsQuery.isLoading ? (
          <p role="status" className="text-meta text-muted-foreground">
            Loading approvals…
          </p>
        ) : approvalsQuery.isError ? (
          <LoadFailure label="task approvals" error={approvalsQuery.error} />
        ) : approvals.length === 0 ? (
          <p className="text-meta text-muted-foreground">
            No approvals were recorded for this task.
          </p>
        ) : (
          <ol className="grid gap-1.5">
            {approvals.map((approval) => (
              <ApprovalRow key={approval.approval.id} approval={approval} />
            ))}
          </ol>
        )}
      </section>
    </article>
  )
}

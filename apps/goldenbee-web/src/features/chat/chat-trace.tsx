import {
  BrainIcon,
  CircleAlertIcon,
  CircleCheckIcon,
  CircleHelpIcon,
  LoaderCircleIcon,
  RotateCcwIcon,
  WrenchIcon,
} from "lucide-react"
import { useState } from "react"

import { Button } from "@/components/ui/button"
import {
  Trace,
  TraceCollapsible,
  TraceCollapsibleContent,
  TraceCollapsibleTrigger,
  TraceDescription,
  TraceIcon,
  TraceLine,
  TraceTitle,
} from "@/components/ui/trace"
import { taskStateView } from "@/features/chat/chat-status"
import type {
  ChatPlanPart,
  ChatPlanStepStatus,
  ChatReasoningPart,
  ChatTaskPart,
  ChatToolPart,
} from "@/features/chat/chat-types"

function traceStatus(
  status: "running" | "ok" | "error" | "pending" | "unknown" | "done",
): "running" | "error" | "pending" | undefined {
  switch (status) {
    case "running":
      return "running"
    case "error":
      return "error"
    case "pending":
      return "pending"
    default:
      return undefined
  }
}

export function ReasoningView({ part }: { part: ChatReasoningPart }) {
  const running = part.streaming && !part.complete
  const [open, setOpen] = useState<boolean | undefined>(undefined)
  // A user click wins; an unopened streaming run shows its text.
  const expanded = open ?? running

  return (
    <div className="flex flex-col gap-1">
      <TraceCollapsible open={expanded} onOpenChange={setOpen}>
        <TraceCollapsibleTrigger>
          <TraceLine status={running ? "running" : undefined}>
            <TraceIcon>
              {running ? (
                <LoaderCircleIcon className="animate-spin" />
              ) : (
                <BrainIcon />
              )}
            </TraceIcon>
            <TraceTitle>{running ? "Thinking" : "Thought"}</TraceTitle>
          </TraceLine>
        </TraceCollapsibleTrigger>
        <TraceCollapsibleContent>
          <p className="whitespace-pre-wrap">{part.text}</p>
        </TraceCollapsibleContent>
      </TraceCollapsible>
      <p className="pl-6 text-xs text-muted-foreground">
        Reasoning is live-only; reconnecting will not replay it.
      </p>
    </div>
  )
}

function toolTitle(part: ChatToolPart): string {
  switch (part.status) {
    case "running":
      return `Running ${part.name}`
    case "ok":
      return `Ran ${part.name}`
    case "error":
      return `Failed to run ${part.name}`
    default:
      return `${part.name} (outcome unknown)`
  }
}

export function ToolView({ part }: { part: ChatToolPart }) {
  const context = [part.agent, part.server].filter(
    (value): value is string => Boolean(value),
  )

  return (
    <Trace>
      <TraceLine status={traceStatus(part.status)}>
        <TraceIcon>
          {part.status === "running" ? (
            <LoaderCircleIcon className="animate-spin" />
          ) : part.status === "error" ? (
            <CircleAlertIcon />
          ) : part.status === "ok" ? (
            <CircleCheckIcon />
          ) : (
            <WrenchIcon />
          )}
        </TraceIcon>
        <TraceTitle>{toolTitle(part)}</TraceTitle>
        {context.length > 0 ? (
          <TraceDescription>{context.join(" · ")}</TraceDescription>
        ) : null}
      </TraceLine>
      {part.errorCode ? (
        <p className="pl-6 text-xs text-destructive">
          Error code: {part.errorCode}
        </p>
      ) : null}
    </Trace>
  )
}

function planStepIcon(status: ChatPlanStepStatus) {
  switch (status) {
    case "running":
      return <LoaderCircleIcon className="animate-spin" />
    case "done":
      return <CircleCheckIcon />
    case "error":
      return <CircleAlertIcon />
    case "unknown":
      return <CircleHelpIcon />
    default:
      return null
  }
}

export function PlanView({ part }: { part: ChatPlanPart }) {
  const [open, setOpen] = useState(true)
  const done = part.steps.filter((step) => step.status === "done").length
  const total = part.steps.length

  return (
    <TraceCollapsible open={open} onOpenChange={setOpen}>
      <TraceCollapsibleTrigger>
        <TraceLine>
          <TraceTitle>Plan</TraceTitle>
          <TraceDescription>
            {done} of {total} done
          </TraceDescription>
        </TraceLine>
      </TraceCollapsibleTrigger>
      <TraceCollapsibleContent>
        <Trace>
          {part.steps.map((step) => (
            <TraceLine key={step.id} status={traceStatus(step.status)}>
              <TraceIcon>{planStepIcon(step.status)}</TraceIcon>
              <TraceTitle>{step.target}</TraceTitle>
              {step.capability ? (
                <TraceDescription>{step.capability}</TraceDescription>
              ) : null}
            </TraceLine>
          ))}
        </Trace>
      </TraceCollapsibleContent>
    </TraceCollapsible>
  )
}



export function TaskView({
  task,
  busy,
  onRetry,
}: {
  task: ChatTaskPart
  busy: boolean
  onRetry?: () => void
}) {
  const view = taskStateView(task.state)
  const status =
    view.tone === "working"
      ? "running"
      : view.tone === "warning"
        ? "pending"
        : view.tone === "error"
          ? "error"
          : undefined

  return (
    <Trace>
      <TraceLine status={status}>
        <TraceIcon>
          {view.tone === "working" ? (
            <LoaderCircleIcon className="animate-spin" />
          ) : view.tone === "error" || view.tone === "warning" ? (
            <CircleAlertIcon />
          ) : null}
        </TraceIcon>
        <TraceTitle>{view.label}</TraceTitle>
        {task.attempt > 1 ? (
          <TraceDescription>attempt {task.attempt}</TraceDescription>
        ) : null}
      </TraceLine>
      {view.guidance ? (
        <p className="pl-6 text-xs text-muted-foreground">{view.guidance}</p>
      ) : null}
      {task.error ? (
        <p className="pl-6 text-xs text-destructive">
          {task.error.message}
          {task.error.recovery ? ` ${task.error.recovery}` : ""}
        </p>
      ) : null}
      {view.retryable && onRetry ? (
        <div className="pl-6">
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={busy}
            onClick={onRetry}
          >
            <RotateCcwIcon data-icon="inline-start" aria-hidden="true" />
            Retry
          </Button>
        </div>
      ) : null}
    </Trace>
  )
}

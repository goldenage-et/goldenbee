import { CircleAlertIcon, CircleHelpIcon, LoaderCircleIcon, RotateCcwIcon } from "lucide-react"

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import { Bubble, BubbleContent } from "@/components/ui/bubble"
import { Message, MessageContent } from "@/components/ui/message"
import {
  Trace,
  TraceDescription,
  TraceIcon,
  TraceLine,
  TraceTitle,
} from "@/components/ui/trace"
import { taskStateView } from "@/features/chat/chat-status"
import type {
  ChatMessage,
  ChatPart,
  ChatTaskPart,
  ChatTextPart,
} from "@/features/chat/chat-types"

function textParts(message: ChatMessage): ChatTextPart[] {
  return message.parts.filter(
    (part): part is ChatTextPart => part.type === "text",
  )
}

function taskPart(message: ChatMessage): ChatTaskPart | undefined {
  return message.parts.find(
    (part): part is ChatTaskPart => part.type === "task",
  )
}

function TaskView({
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

function ErrorView({ part }: { part: Extract<ChatPart, { type: "error" }> }) {
  return (
    <Alert variant="destructive">
      <CircleAlertIcon />
      <AlertTitle>{part.message}</AlertTitle>
      <AlertDescription>
        {part.recovery ? <p>{part.recovery}</p> : null}
        <p className="text-xs opacity-70">{part.code}</p>
      </AlertDescription>
    </Alert>
  )
}

function UncertaintyView({
  part,
}: {
  part: Extract<ChatPart, { type: "uncertainty" }>
}) {
  const details = Object.entries(part.details ?? {})
  return (
    <Alert>
      <CircleHelpIcon />
      <AlertTitle>Submission uncertain</AlertTitle>
      <AlertDescription>
        <p>{part.message}</p>
        {part.recovery ? <p>{part.recovery}</p> : null}
        {details.length > 0 ? (
          <dl className="mt-2 grid grid-cols-[auto_1fr] gap-x-2 text-xs">
            {details.map(([key, value]) => (
              <div key={key} className="contents">
                <dt className="font-medium">{key}</dt>
                <dd className="font-mono break-all">{value}</dd>
              </div>
            ))}
          </dl>
        ) : null}
      </AlertDescription>
    </Alert>
  )
}

export interface ChatMessageViewProps {
  message: ChatMessage
  busy: boolean
  onRetry?(userMessageId: string): void
}

export function ChatMessageView({
  message,
  busy,
  onRetry,
}: ChatMessageViewProps) {
  if (message.role === "user") {
    const text = textParts(message)
      .map((part) => part.text)
      .join("")
    return (
      <Message align="end">
        <MessageContent>
          <Bubble align="end" variant="default">
            <BubbleContent>{text}</BubbleContent>
          </Bubble>
        </MessageContent>
      </Message>
    )
  }

  const task = taskPart(message)
  const texts = textParts(message)
  const errors = message.parts.filter(
    (part): part is Extract<ChatPart, { type: "error" }> => part.type === "error",
  )
  const uncertainties = message.parts.filter(
    (part): part is Extract<ChatPart, { type: "uncertainty" }> =>
      part.type === "uncertainty",
  )
  const diagnostics = message.parts.filter(
    (part): part is Extract<ChatPart, { type: "diagnostic" }> =>
      part.type === "diagnostic",
  )
  const streaming = texts.some((part) => part.streaming)
  const waiting = !task && errors.length === 0 && uncertainties.length === 0

  return (
    <Message>
      <MessageContent>
        {texts.map((part, index) => (
          <div
            key={`${message.id}-text-${index}`}
            className="text-sm leading-relaxed whitespace-pre-wrap"
          >
            {part.text}
            {part.streaming ? (
              <span className="ml-0.5 inline-block animate-pulse" aria-hidden="true">
                ▍
              </span>
            ) : null}
          </div>
        ))}

        {waiting && streaming ? (
          <p className="text-sm text-muted-foreground">Waiting for the Worker…</p>
        ) : null}
        {message.parts.length === 0 ? (
          <p className="text-sm text-muted-foreground">Sending task…</p>
        ) : null}

        {errors.map((part, index) => (
          <ErrorView key={`${message.id}-error-${index}`} part={part} />
        ))}
        {uncertainties.map((part, index) => (
          <UncertaintyView key={`${message.id}-uncertainty-${index}`} part={part} />
        ))}

        {diagnostics.length > 0 ? (
          <div className="flex flex-col gap-1">
            {diagnostics.map((part, index) => (
              <p
                key={`${message.id}-diagnostic-${index}`}
                className="text-xs text-muted-foreground"
              >
                {part.detail}
              </p>
            ))}
          </div>
        ) : null}

        {task ? (
          <TaskView
            task={task}
            busy={busy}
            onRetry={
              onRetry && message.replyTo
                ? () => onRetry(message.replyTo!)
                : undefined
            }
          />
        ) : null}
      </MessageContent>
    </Message>
  )
}

import { CircleAlertIcon, CircleHelpIcon } from "lucide-react"

import type { GatewayApproval } from "@goldenbee/sdk"

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Bubble, BubbleContent } from "@/components/ui/bubble"
import { Message, MessageContent } from "@/components/ui/message"
import {
  ApprovalCard,
  type ApprovalGateway,
} from "@/features/approvals/approval-card"
import {
  PlanView,
  ReasoningView,
  TaskView,
  ToolView,
} from "@/features/chat/chat-trace"
import type {
  ChatMessage,
  ChatPart,
  ChatTextPart,
} from "@/features/chat/chat-types"

function textParts(message: ChatMessage): ChatTextPart[] {
  return message.parts.filter(
    (part): part is ChatTextPart => part.type === "text",
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
  /** Approval transport; null disables decisions without hiding the card. */
  approvalClient?: ApprovalGateway | null
  onApprovalUpdated?(approval: GatewayApproval): void
}

export function ChatMessageView({
  message,
  busy,
  onRetry,
  approvalClient = null,
  onApprovalUpdated,
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

  const task = message.parts.find(
    (part): part is Extract<ChatPart, { type: "task" }> => part.type === "task",
  )
  const texts = textParts(message)
  const reasoning = message.parts.filter(
    (part): part is Extract<ChatPart, { type: "reasoning" }> =>
      part.type === "reasoning",
  )
  const tools = message.parts.filter(
    (part): part is Extract<ChatPart, { type: "tool" }> => part.type === "tool",
  )
  const plans = message.parts.filter(
    (part): part is Extract<ChatPart, { type: "plan" }> => part.type === "plan",
  )
  const approvals = message.parts.filter(
    (part): part is Extract<ChatPart, { type: "approval" }> =>
      part.type === "approval",
  )
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
        {reasoning.length > 0 || tools.length > 0 || plans.length > 0 ? (
          <div className="flex flex-col gap-2">
            {reasoning.map((part, index) => (
              <ReasoningView key={`${message.id}-reasoning-${index}`} part={part} />
            ))}
            {tools.map((part) => (
              <ToolView key={`${message.id}-tool-${part.id}`} part={part} />
            ))}
            {plans.map((part) => (
              <PlanView key={`${message.id}-plan-${part.planId}`} part={part} />
            ))}
          </div>
        ) : null}

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

        {approvals.map((part) => (
          <ApprovalCard
            key={`${message.id}-approval-${part.approvalId}`}
            part={part}
            taskId={message.taskId}
            client={approvalClient}
            onUpdated={(approval) => onApprovalUpdated?.(approval)}
          />
        ))}

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

        {task && task.state !== "succeeded" ? (
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

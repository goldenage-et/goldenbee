import type { StreamEvent, TaskError, TaskState } from "@goldenbee/sdk"

import type {
  ChatDiagnosticPart,
  ChatErrorInfo,
  ChatMessage,
  ChatPart,
  ChatRunStatus,
  ChatState,
  ChatStreamGap,
  ChatTaskPart,
  ChatTextPart,
} from "./chat-types"

const taskStates: readonly TaskState[] = [
  "accepted",
  "queued",
  "running",
  "waiting_approval",
  "cancelling",
  "recovering",
  "unknown",
  "succeeded",
  "failed",
  "cancelled",
]

/**
 * Event kinds owned by later tickets. They are ignored rather than rendered as
 * invented content; ticket 04 adds reasoning/tool/plan parts, ticket 05 adds
 * approvals.
 */
const laterTicketKinds = new Set([
  "model.thinking",
  "model.thinking.complete",
  "tool.call",
  "tool.result",
  "tool.outcome_unknown",
  "approval.requested",
  "approval.approved",
  "approval.rejected",
  "approval.revoked",
  "approval.expired",
  "approval.receipt",
  "approval.resume_unavailable",
  "master.plan_created",
  "master.child_observed",
  "artifact.finalized",
  "artifact.recording_failed",
  "model.fallback",
  "context.compacted",
  "graph.interrupted",
  "graph.thread_bound",
  "graph.output",
  "team.receipt",
  "operation.reconciled",
  "budget.exhausted",
  "budget.usage_unavailable",
  "hook.after_task_failed",
])

const textKinds = new Set(["model.delta", "model.delta.complete"])

export const terminalKinds = new Set([
  "task.succeeded",
  "task.failed",
  "task.cancelled",
  "task.unknown",
])

export function isTerminalStatus(status: ChatRunStatus): boolean {
  return (
    status === "succeeded" ||
    status === "failed" ||
    status === "cancelled" ||
    status === "unknown"
  )
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function textField(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined
}

function numberField(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined
}

function taskStateField(value: unknown): TaskState | undefined {
  return typeof value === "string" && taskStates.includes(value as TaskState)
    ? (value as TaskState)
    : undefined
}

function taskErrorField(value: unknown): TaskError | undefined {
  if (!isRecord(value)) {
    return undefined
  }
  const code = textField(value.code)
  const message = textField(value.message)
  if (!code || !message) {
    return undefined
  }
  return {
    code,
    message,
    retryable: value.retryable === true,
    recovery: typeof value.recovery === "string" ? value.recovery : "",
    details: isRecord(value.details)
      ? Object.fromEntries(
          Object.entries(value.details).map(([key, detail]) => [
            key,
            String(detail),
          ]),
        )
      : undefined,
  }
}

function usageField(value: unknown): ChatTaskPart["usage"] | undefined {
  return isRecord(value) ? value : undefined
}

export function messageWithParts(
  state: ChatState,
  messageId: string,
  update: (message: ChatMessage) => ChatMessage,
): ChatState {
  let changed = false
  const messages = state.messages.map((message) => {
    if (message.id !== messageId) {
      return message
    }
    changed = true
    return update(message)
  })
  return changed ? { ...state, messages } : state
}

function replacePart(message: ChatMessage, part: ChatPart): ChatMessage {
  const index = message.parts.findIndex((candidate) => candidate.type === part.type)
  if (index === -1) {
    return { ...message, parts: [...message.parts, part] }
  }
  const parts = message.parts.map((candidate, position) =>
    position === index ? part : candidate,
  )
  return { ...message, parts }
}

export function appendPart(
  message: ChatMessage,
  part: ChatPart,
): ChatMessage {
  return { ...message, parts: [...message.parts, part] }
}

function appendDiagnostic(
  message: ChatMessage,
  diagnostic: ChatDiagnosticPart,
): ChatMessage {
  const duplicate = message.parts.some(
    (part) =>
      part.type === "diagnostic" &&
      part.eventKind === diagnostic.eventKind &&
      part.detail === diagnostic.detail,
  )
  return duplicate ? message : appendPart(message, diagnostic)
}

export function taskPart(message: ChatMessage): ChatTaskPart | undefined {
  return message.parts.find(
    (part): part is ChatTaskPart => part.type === "task",
  )
}

export function ensureTaskPart(
  message: ChatMessage,
  state: TaskState,
  attempt = 1,
): ChatMessage {
  const current = taskPart(message)
  const next: ChatTaskPart = {
    type: "task",
    state,
    attempt,
    output: current?.output,
    error: current?.error,
    uncertainty: current?.uncertainty,
    usage: current?.usage,
  }
  return replacePart(message, next)
}

export function updateTaskPart(
  message: ChatMessage,
  update: (current: ChatTaskPart) => ChatTaskPart,
): ChatMessage {
  const current = taskPart(message)
  if (!current) {
    return message
  }
  return replacePart(message, update(current))
}

function lastTextPart(message: ChatMessage): ChatTextPart | undefined {
  for (let index = message.parts.length - 1; index >= 0; index -= 1) {
    const part = message.parts[index]
    if (part?.type === "text") {
      return part
    }
  }
  return undefined
}

function updateTextPart(
  message: ChatMessage,
  target: ChatTextPart,
  next: ChatTextPart,
): ChatMessage {
  const index = message.parts.lastIndexOf(target)
  if (index === -1) {
    return message
  }
  const parts = message.parts.map((part, position) =>
    position === index ? next : part,
  )
  return { ...message, parts }
}

function appendDelta(
  message: ChatMessage,
  text: string,
  author?: string,
): ChatMessage {
  const current = lastTextPart(message)
  if (current?.streaming && (author === undefined || current.author === author)) {
    return updateTextPart(message, current, {
      ...current,
      text: current.text + text,
    })
  }
  return appendPart(message, {
    type: "text",
    text,
    author,
    streaming: true,
  })
}

/** Close the current delta run with the completed text for that run. */
function completeDelta(
  message: ChatMessage,
  text: string,
  author?: string,
): ChatMessage {
  const current = lastTextPart(message)
  if (!current) {
    return appendPart(message, { type: "text", text, author, streaming: false })
  }
  if (current.text === text) {
    return updateTextPart(message, current, { ...current, streaming: false })
  }
  if (text.startsWith(current.text)) {
    return updateTextPart(message, current, {
      ...current,
      text,
      streaming: false,
    })
  }
  // The completed run disagrees with the deltas; replace just that run.
  return updateTextPart(message, current, { ...current, text, streaming: false })
}

/**
 * Apply the authoritative final output without duplicating streamed text.
 *
 * If the observed text is a prefix of the output, the single final part carries
 * the output. If it differs, the streamed draft is replaced.
 */
export function reconcileOutput(
  message: ChatMessage,
  output: string,
): ChatMessage {
  const textParts = message.parts.filter(
    (part): part is ChatTextPart => part.type === "text",
  )
  const withoutText: ChatMessage = {
    ...message,
    parts: message.parts.filter((part) => part.type !== "text"),
  }
  return appendPart(withoutText, {
    type: "text",
    text: output,
    author: textParts.at(-1)?.author,
    streaming: false,
  })
}

export function statusForState(state: TaskState): ChatRunStatus {
  switch (state) {
    case "accepted":
    case "queued":
    case "running":
    case "waiting_approval":
      return "streaming"
    case "cancelling":
      return "stopping"
    case "recovering":
      return "recovering"
    case "succeeded":
      return "succeeded"
    case "failed":
      return "failed"
    case "cancelled":
      return "cancelled"
    case "unknown":
      return "unknown"
  }
}

function applyTaskEvent(
  message: ChatMessage,
  event: StreamEvent,
): ChatMessage {
  const payload = isRecord(event.payload) ? event.payload : {}
  const payloadState = taskStateField(payload.state)
  const attempt = numberField(payload.attempt)
  const output = textField(payload.output)
  const error = taskErrorField(payload.error)
  const uncertainty = textField(payload.uncertainty)
  const usage = usageField(payload.usage)

  switch (event.kind) {
    case "task.accepted":
      return ensureTaskPart(message, payloadState ?? "accepted", attempt ?? 1)
    case "task.queued":
    case "task.running": {
      const fallback: TaskState =
        event.kind === "task.queued" ? "queued" : "running"
      return updateTaskPart(message, (current) => ({
        ...current,
        state: payloadState ?? fallback,
        attempt: attempt ?? current.attempt,
      }))
    }
    case "task.cancellation_requested": {
      const nextState: TaskState =
        payloadState ?? (payload.confirmed === true ? "cancelled" : "cancelling")
      return updateTaskPart(message, (current) => ({
        ...current,
        state: nextState,
      }))
    }
    case "task.recovery_required": {
      const reason = textField(payload.reason) ?? "The Worker is reconciling the task."
      return appendDiagnostic(
        updateTaskPart(message, (current) => ({
          ...current,
          state: "recovering",
        })),
        {
          type: "diagnostic",
          eventKind: event.kind,
          detail: `Recovery required: ${reason}`,
        },
      )
    }
    case "task.recovered": {
      const reason = textField(payload.reason) ?? "The Gateway reconciled the task."
      return appendDiagnostic(
        updateTaskPart(message, (current) => ({
          ...current,
          state: payloadState ?? "accepted",
          uncertainty: undefined,
        })),
        {
          type: "diagnostic",
          eventKind: event.kind,
          detail: `Recovered: ${reason}`,
        },
      )
    }
    case "task.updated": {
      if (!payloadState) {
        return message
      }
      const next = updateTaskPart(message, (current) => ({
        ...current,
        state: payloadState,
        attempt: attempt ?? current.attempt,
        output: output ?? current.output,
        error: error ?? current.error,
        uncertainty: uncertainty ?? current.uncertainty,
        usage: usage ?? current.usage,
      }))
      return output ? reconcileOutput(next, output) : next
    }
    case "task.succeeded":
    case "task.failed":
    case "task.cancelled":
    case "task.unknown": {
      const fallback: TaskState =
        event.kind === "task.succeeded"
          ? "succeeded"
          : event.kind === "task.failed"
            ? "failed"
            : event.kind === "task.cancelled"
              ? "cancelled"
              : "unknown"
      const nextState = payloadState ?? fallback
      let next = ensureTaskPart(
        message,
        nextState,
        attempt ?? taskPart(message)?.attempt ?? 1,
      )
      next = updateTaskPart(next, (current) => ({
        ...current,
        state: nextState,
        output: output ?? current.output,
        error: error ?? current.error,
        uncertainty: uncertainty ?? current.uncertainty,
        usage: usage ?? current.usage,
      }))
      if (output) {
        next = reconcileOutput(next, output)
      }
      if (event.kind === "task.unknown") {
        const code = error?.code ?? "task.unknown"
        const duplicate = next.parts.some(
          (part) => part.type === "uncertainty" && part.code === code,
        )
        if (!duplicate) {
          next = appendPart(next, {
            type: "uncertainty",
            code,
            message:
              uncertainty ??
              "The Gateway could not confirm the task outcome. Inspect the task before retrying.",
            recovery:
              error?.recovery ??
              "Check the task on the Gateway, then start a new task if needed.",
            details: error?.details,
          })
        }
      }
      return next
    }
    case "task.resume_unavailable": {
      const info: ChatErrorInfo = {
        code: textField(payload.code) ?? "task.resume_unavailable",
        message: textField(payload.detail) ?? "The task could not resume.",
        recovery: textField(payload.guidance),
        retryable: false,
      }
      return appendPart(message, {
        type: "error",
        ...info,
        kind: "task",
      })
    }
    case "task.transfer": {
      const target = textField(payload.target) ?? "another agent"
      return appendPart(message, {
        type: "diagnostic",
        eventKind: event.kind,
        detail: `Transferred to ${target}.`,
      })
    }
    case "task.resume_accepted":
    case "task.resumed_from_checkpoint": {
      const step = numberField(payload.step)
      return appendPart(message, {
        type: "diagnostic",
        eventKind: event.kind,
        detail:
          step === undefined
            ? "Resumed from a checkpoint."
            : `Resumed from checkpoint step ${step}.`,
      })
    }
    default:
      return message
  }
}

function applyTextEvent(
  message: ChatMessage,
  event: StreamEvent,
): ChatMessage {
  const payload = isRecord(event.payload) ? event.payload : {}
  const text = textField(payload.text)
  if (!text) {
    return message
  }
  const author = textField(payload.author)
  switch (event.kind) {
    case "model.delta":
      return appendDelta(message, text, author)
    case "model.delta.complete":
      return completeDelta(message, text, author)
    default:
      return message
  }
}

function gapFromEvent(event: StreamEvent): ChatStreamGap | null {
  if (event.kind !== "stream.lagged") {
    return null
  }
  const payload = isRecord(event.payload) ? event.payload : {}
  return {
    reason: textField(payload.reason) ?? "subscriber_lagged",
    fromSeq: numberField(payload.from_seq) ?? event.seq,
  }
}

export interface StreamEventResult {
  message: ChatMessage
  gap: ChatStreamGap | null
}

/**
 * Apply one Gateway stream event to the assistant message.
 *
 * The caller owns sequence de-duplication and cursor state; this function owns
 * the kind-to-part mapping and never throws on an unknown kind.
 */
export function applyStreamEvent(
  message: ChatMessage,
  event: StreamEvent,
  status: ChatRunStatus,
): StreamEventResult {
  if (laterTicketKinds.has(event.kind)) {
    return { message, gap: null }
  }

  const gap = gapFromEvent(event)
  if (gap) {
    return { message, gap }
  }

  if (textKinds.has(event.kind)) {
    if (isTerminalStatus(status)) {
      return { message, gap: null }
    }
    return { message: applyTextEvent(message, event), gap: null }
  }

  return { message: applyTaskEvent(message, event), gap: null }
}

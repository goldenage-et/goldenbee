import type {
  GatewayTask,
  StreamEvent,
  TaskError,
  TaskState,
  TaskUsage,
} from "@goldenbee/sdk"

/**
 * Chat state is the reduced projection of Gateway task events.
 *
 * Text, reasoning, tools, task state, plans, usage, errors, and diagnostics are
 * explicit parts. Approvals are added by ticket 05; their event kinds are
 * intentionally ignored here instead of being turned into invented parts.
 */
export interface ChatSelection {
  worker: string
  agent: string
  profile?: string
}

export type ChatTextPart = {
  type: "text"
  text: string
  author?: string
  streaming: boolean
}

/**
 * Live reasoning trace. `model.thinking`/`model.thinking.complete` are not
 * durable, so this part is only what the current connection observed.
 */
export type ChatReasoningPart = {
  type: "reasoning"
  text: string
  author?: string
  streaming: boolean
  complete: boolean
}

export type ChatToolStatus = "running" | "ok" | "error" | "unknown"

/**
 * One tool call, keyed by the Gateway tool `id`. The stream carries name,
 * agent, server, and result status only; arguments, output, and duration are
 * deliberately unavailable.
 */
export type ChatToolPart = {
  type: "tool"
  id: string
  name: string
  agent?: string
  server?: string
  status: ChatToolStatus
  errorCode?: string
}

export type ChatPlanStepStatus =
  | "pending"
  | "running"
  | "done"
  | "error"
  | "unknown"

export interface ChatPlanStep {
  id: string
  target: string
  capability?: string
  status: ChatPlanStepStatus
}

/** Latest observation for one planned child. */
export interface ChatChildObservation {
  child: string
  binding?: string
  remoteTaskId?: string
  state?: string
  artifactDigests: string[]
}

/** One Master plan, keyed by `plan_id`. */
export type ChatPlanPart = {
  type: "plan"
  planId: string
  steps: ChatPlanStep[]
  observed: ChatChildObservation[]
}

export type ChatTaskPart = {
  type: "task"
  state: TaskState
  attempt: number
  output?: string
  error?: TaskError
  uncertainty?: string
  usage?: TaskUsage
}

export type ChatUsagePart = {
  type: "usage"
  usage: TaskUsage
  source: "terminal" | "budget" | "projection"
}

export type ChatErrorKind = "task" | "stream" | "protocol" | "submission"

export type ChatErrorPart = {
  type: "error"
  code: string
  message: string
  recovery?: string
  retryable: boolean
  kind: ChatErrorKind
}

/**
 * HTTP 202 `task.unknown_submission` is neither success nor rejection. It stays
 * its own part so the UI can ask the operator to inspect instead of resending.
 */
export type ChatUncertaintyPart = {
  type: "uncertainty"
  code: string
  message: string
  recovery?: string
  details?: Record<string, string>
}

export type ChatDiagnosticPart = {
  type: "diagnostic"
  eventKind: string
  detail: string
}

export type ChatPart =
  | ChatTextPart
  | ChatReasoningPart
  | ChatToolPart
  | ChatPlanPart
  | ChatTaskPart
  | ChatUsagePart
  | ChatErrorPart
  | ChatUncertaintyPart
  | ChatDiagnosticPart

export interface ChatMessage {
  id: string
  role: "user" | "assistant"
  createdAt: number
  parts: ChatPart[]
  /** Assistant reply association; also the retry join key. */
  replyTo?: string
  taskId?: string
  selection?: ChatSelection
  idempotencyKey?: string
}

export type ChatRunStatus =
  | "idle"
  | "submitting"
  | "streaming"
  | "stopping"
  | "succeeded"
  | "failed"
  | "cancelled"
  | "unknown"
  | "uncertain"
  | "recovering"

/** A retained-history gap; the replay is not a complete history. */
export interface ChatStreamGap {
  reason: string
  fromSeq: number
}

export interface ChatState {
  sessionId: string
  messages: ChatMessage[]
  status: ChatRunStatus
  activeTaskId: string | null
  activeAssistantId: string | null
  /** Highest observed sequence for in-memory de-duplication. */
  lastSeq: number
  /** Highest durable sequence; the only cursor a reconnect may use. */
  lastDurableSeq: number
  gap: ChatStreamGap | null
}

/** Plain error data accepted by the reducer, independent of transport types. */
export interface ChatErrorInfo {
  code: string
  message: string
  recovery?: string
  retryable: boolean
  details?: Record<string, string>
}

export type ChatAction =
  | {
      type: "send"
      userMessageId: string
      assistantMessageId: string
      text: string
      selection: ChatSelection
      idempotencyKey: string
      createdAt: number
    }
  | {
      type: "submitted"
      assistantMessageId: string
      taskId: string
      state: TaskState
    }
  | {
      type: "submit-uncertain"
      assistantMessageId: string
      error: ChatErrorInfo
    }
  | {
      type: "submit-failed"
      assistantMessageId: string
      error: ChatErrorInfo
    }
  | {
      type: "event"
      assistantMessageId: string
      event: StreamEvent
    }
  | { type: "gap"; reason: string; fromSeq: number }
  | {
      type: "stream-error"
      assistantMessageId: string
      error: ChatErrorInfo
    }
  | {
      type: "stream-draining"
      assistantMessageId: string
      phase: string
    }
  | { type: "stop-requested" }
  | {
      type: "stop-applied"
      assistantMessageId: string
      task: GatewayTask
    }
  | {
      type: "stop-failed"
      assistantMessageId: string
      error: ChatErrorInfo
    }
  | {
      type: "retry-started"
      userMessageId: string
      assistantMessageId: string
      idempotencyKey: string
    }
  | { type: "reset"; sessionId: string }

export function createInitialChatState(sessionId: string): ChatState {
  return {
    sessionId,
    messages: [],
    status: "idle",
    activeTaskId: null,
    activeAssistantId: null,
    lastSeq: 0,
    lastDurableSeq: 0,
    gap: null,
  }
}

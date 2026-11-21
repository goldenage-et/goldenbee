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
 * Ticket 03 renders answer text, task state, errors, and submission
 * uncertainty. Reasoning, tools, approvals, plans, and richer diagnostics are
 * added by later tickets; their event kinds are intentionally ignored here
 * instead of being turned into invented parts.
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

export type ChatRecoveryKind =
  | "reconciling"
  | "reconnected"
  | "detached"
  | "failed"

/** Reload-time reconciliation of a task that outlived the browser session. */
export interface ChatRecoveryNotice {
  kind: ChatRecoveryKind
  taskId: string
  detail?: string
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
  recovery: ChatRecoveryNotice | null
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
  | { type: "recovery-started"; taskId: string }
  | {
      type: "task-projection"
      assistantMessageId: string
      task: GatewayTask
    }
  | {
      type: "task-detached"
      assistantMessageId: string | null
      taskId: string
      detail: string
    }
  | {
      type: "recovery-failed"
      assistantMessageId: string | null
      taskId: string
      error: ChatErrorInfo
    }
  | { type: "cursor-reset" }
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
    recovery: null,
  }
}

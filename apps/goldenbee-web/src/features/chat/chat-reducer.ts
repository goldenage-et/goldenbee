import {
  appendPart,
  applyStreamEvent,
  ensureTaskPart,
  isTerminalStatus,
  messageWithParts,
  reconcileOutput,
  statusForState,
  taskPart,
  updateTaskPart,
} from "./chat-events"
import {
  createInitialChatState,
  type ChatAction,
  type ChatMessage,
  type ChatPart,
  type ChatState,
  type ChatTextPart,
} from "./chat-types"

/** Pure chat reducer over Gateway stream events and local chat actions. */
export function chatReducer(state: ChatState, action: ChatAction): ChatState {
  switch (action.type) {
    case "send": {
      const textPart: ChatTextPart = {
        type: "text",
        text: action.text,
        streaming: false,
      }
      const user: ChatMessage = {
        id: action.userMessageId,
        role: "user",
        createdAt: action.createdAt,
        parts: [textPart],
        selection: action.selection,
        idempotencyKey: action.idempotencyKey,
      }
      const assistant: ChatMessage = {
        id: action.assistantMessageId,
        role: "assistant",
        createdAt: action.createdAt,
        parts: [],
        replyTo: action.userMessageId,
      }
      return {
        ...state,
        messages: [...state.messages, user, assistant],
        status: "submitting",
        activeTaskId: null,
        activeAssistantId: action.assistantMessageId,
        lastSeq: 0,
        lastDurableSeq: 0,
        gap: null,
        recovery: null,
      }
    }

    case "submitted": {
      const next = messageWithParts(state, action.assistantMessageId, (message) =>
        ensureTaskPart({ ...message, taskId: action.taskId }, action.state),
      )
      return {
        ...next,
        status: "streaming",
        activeTaskId: action.taskId,
        activeAssistantId: action.assistantMessageId,
        lastSeq: 0,
        lastDurableSeq: 0,
        recovery: null,
      }
    }

    case "submit-uncertain": {
      const next = messageWithParts(state, action.assistantMessageId, (message) =>
        appendPart(message, {
          type: "uncertainty",
          code: action.error.code,
          message: action.error.message,
          recovery: action.error.recovery,
          details: action.error.details,
        }),
      )
      return {
        ...next,
        status: "uncertain",
        activeTaskId: null,
        activeAssistantId: action.assistantMessageId,
      }
    }

    case "submit-failed":
    case "stream-error": {
      const part: ChatPart = {
        type: "error",
        code: action.error.code,
        message: action.error.message,
        recovery: action.error.recovery,
        retryable: action.error.retryable,
        kind: action.type === "submit-failed" ? "submission" : "stream",
      }
      const next = messageWithParts(state, action.assistantMessageId, (message) =>
        appendPart(message, part),
      )
      return {
        ...next,
        status: "failed",
        activeTaskId: action.type === "stream-error" ? null : state.activeTaskId,
        activeAssistantId: action.assistantMessageId,
      }
    }

    case "event": {
      const message = state.messages.find(
        (candidate) => candidate.id === action.assistantMessageId,
      )
      if (!message) {
        return state
      }
      const { event } = action
      const control = event.kind.startsWith("stream.")
      if (!control && event.seq <= state.lastSeq) {
        return state
      }

      const result = applyStreamEvent(message, event, state.status)
      if (result.message === message && !result.gap) {
        // Ignored kinds still advance the cursor so a reconnect neither
        // replays them nor accepts an earlier out-of-order event.
        return {
          ...state,
          lastSeq: Math.max(state.lastSeq, event.seq),
          lastDurableSeq: event.durable
            ? Math.max(state.lastDurableSeq, event.seq)
            : state.lastDurableSeq,
        }
      }
      if (result.gap && result.message === message) {
        return {
          ...state,
          gap: result.gap,
          lastSeq: Math.max(state.lastSeq, event.seq),
          lastDurableSeq: event.durable
            ? Math.max(state.lastDurableSeq, event.seq)
            : state.lastDurableSeq,
        }
      }

      const nextTask = taskPart(result.message)
      const status = nextTask ? statusForState(nextTask.state) : state.status
      return messageWithParts(
        {
          ...state,
          status,
          lastSeq: event.seq,
          lastDurableSeq: event.durable ? event.seq : state.lastDurableSeq,
          activeTaskId: isTerminalStatus(status) ? null : state.activeTaskId,
        },
        message.id,
        () => result.message,
      )
    }

    case "gap":
      return {
        ...state,
        gap: { reason: action.reason, fromSeq: action.fromSeq },
      }

    case "stream-draining": {
      return messageWithParts(state, action.assistantMessageId, (message) =>
        appendPart(message, {
          type: "diagnostic",
          eventKind: "stream.draining",
          detail: `The Worker is ${action.phase}; the stream will reconnect.`,
        }),
      )
    }

    case "stop-requested":
      return { ...state, status: "stopping" }

    case "stop-applied": {
      const projection = action.task.task
      const next = messageWithParts(state, action.assistantMessageId, (message) => {
        let updated = ensureTaskPart(
          message,
          projection.state,
          projection.attempt ?? taskPart(message)?.attempt ?? 1,
        )
        updated = updateTaskPart(updated, (current) => ({
          ...current,
          state: projection.state,
          attempt: projection.attempt ?? current.attempt,
          output: projection.output ?? current.output,
          error: projection.error ?? current.error,
          uncertainty: projection.uncertainty ?? current.uncertainty,
          usage: projection.usage ?? current.usage,
        }))
        return projection.output
          ? reconcileOutput(updated, projection.output)
          : updated
      })
      const status = statusForState(projection.state)
      return {
        ...next,
        status,
        activeTaskId: isTerminalStatus(status) ? null : state.activeTaskId,
      }
    }

    case "task-projection": {
      const projection = action.task.task
      const next = messageWithParts(state, action.assistantMessageId, (message) => {
        let updated = ensureTaskPart(
          message,
          projection.state,
          projection.attempt ?? taskPart(message)?.attempt ?? 1,
        )
        updated = updateTaskPart(updated, (current) => ({
          ...current,
          state: projection.state,
          attempt: projection.attempt ?? current.attempt,
          output: projection.output ?? current.output,
          error: projection.error ?? current.error,
          uncertainty: projection.uncertainty ?? current.uncertainty,
          usage: projection.usage ?? current.usage,
        }))
        return projection.output
          ? reconcileOutput(updated, projection.output)
          : updated
      })
      const status = statusForState(projection.state)
      return {
        ...next,
        status,
        activeTaskId: isTerminalStatus(status) ? null : action.task.task_id,
        recovery: {
          kind: "reconnected",
          taskId: action.task.task_id,
          detail: "The task was reconciled with the Gateway after reload.",
        },
      }
    }

    case "task-detached": {
      const message = action.assistantMessageId
        ? state.messages.find((candidate) => candidate.id === action.assistantMessageId)
        : undefined
      const next = message
        ? messageWithParts(state, message.id, (current) =>
            appendPart(current, {
              type: "diagnostic",
              eventKind: "history.detached",
              detail: action.detail,
            }),
          )
        : state
      return {
        ...next,
        status: "failed",
        activeTaskId: null,
        recovery: {
          kind: "detached",
          taskId: action.taskId,
          detail: action.detail,
        },
      }
    }

    case "recovery-failed": {
      const message = action.assistantMessageId
        ? state.messages.find((candidate) => candidate.id === action.assistantMessageId)
        : undefined
      const next = message
        ? messageWithParts(state, message.id, (current) =>
            appendPart(current, {
              type: "error",
              code: action.error.code,
              message: action.error.message,
              recovery: action.error.recovery,
              retryable: action.error.retryable,
              kind: "stream",
            }),
          )
        : state
      return {
        ...next,
        status: "failed",
        activeTaskId: null,
        recovery: {
          kind: "failed",
          taskId: action.taskId,
          detail: action.error.message,
        },
      }
    }

    case "recovery-started":
      return {
        ...state,
        status: "recovering",
        activeTaskId: action.taskId,
        recovery: {
          kind: "reconciling",
          taskId: action.taskId,
          detail: "Reconciling the last task with the Gateway.",
        },
      }

    case "cursor-reset":
      return {
        ...state,
        lastSeq: 0,
        lastDurableSeq: 0,
      }

    case "stop-failed": {
      const next = messageWithParts(state, action.assistantMessageId, (message) =>
        appendPart(message, {
          type: "error",
          code: action.error.code,
          message: action.error.message,
          recovery: action.error.recovery,
          retryable: action.error.retryable,
          kind: "stream",
        }),
      )
      return {
        ...next,
        status: state.activeTaskId ? "streaming" : "failed",
      }
    }

    case "retry-started": {
      const messages = state.messages.map((message) => {
        if (message.id === action.userMessageId) {
          return { ...message, idempotencyKey: action.idempotencyKey }
        }
        if (message.id === action.assistantMessageId) {
          return { ...message, parts: [], taskId: undefined }
        }
        return message
      })
      return {
        ...state,
        messages,
        status: "submitting",
        activeTaskId: null,
        activeAssistantId: action.assistantMessageId,
        lastSeq: 0,
        lastDurableSeq: 0,
        gap: null,
        recovery: null,
      }
    }

    case "reset":
      return createInitialChatState(action.sessionId)
  }
}

export { taskStateView, type TaskStateView } from "./chat-status"

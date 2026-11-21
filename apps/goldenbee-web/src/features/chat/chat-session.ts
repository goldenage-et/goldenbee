import {
  isGatewayError,
  newIdempotencyKey,
  type GatewayApproval,
} from "@goldenbee/sdk"

import {
  ChatRunner,
  type ChatGateway,
  type ChatRunnerOptions,
} from "./chat-runner"
import { chatReducer } from "./chat-reducer"
import {
  createInitialChatState,
  type ChatAction,
  type ChatErrorInfo,
  type ChatMessage,
  type ChatSelection,
  type ChatState,
} from "./chat-types"
import {
  createChatSnapshot,
  type ChatHistoryStore,
} from "./local-chat-history"

export interface ChatSessionOptions {
  client: ChatGateway | null
  sessionId?: string
  /** Browser-local chat identity; one local record per chat. */
  chatId?: string
  /** Restored state from IndexedDB; omitted for a fresh chat. */
  initialState?: ChatState
  /** Restored local title; kept until the next user turn names the chat. */
  initialTitle?: string
  /** Restored non-secret selection. */
  initialSelection?: ChatSelection | null
  history?: ChatHistoryStore
  now?: () => number
  createId?: (prefix: string) => string
  createIdempotencyKey?: () => string
  createSessionId?: () => string
  createChatId?: () => string
  onPersistError?: (cause: unknown) => void
  sleep?: ChatRunnerOptions["sleep"]
  random?: ChatRunnerOptions["random"]
  maxReconnectAttempts?: number
  initialBackoffMs?: number
  maxBackoffMs?: number
}

export interface ChatSession {
  getState(): ChatState
  getChatId(): string
  subscribe(listener: () => void): () => void
  send(text: string, selection: ChatSelection): void
  /** Remember the non-secret selection for the next reload. */
  updateSelection(selection: ChatSelection): void
  stop(): void
  retry(userMessageId: string): void
  /** Apply the Gateway response for an approval decision or revocation. */
  applyApproval(approval: GatewayApproval): void
  /** Reconcile the last active task with the Gateway after a reload. */
  recover(): Promise<void>
  newChat(): void
  /** Abort the active subscription but keep the session usable. */
  detach(): void
  dispose(): void
}

const busyStatuses = new Set<ChatState["status"]>([
  "submitting",
  "streaming",
  "stopping",
  "recovering",
])

const retryableStates = new Set(["failed", "cancelled"])

const terminalStatuses = new Set<ChatState["status"]>([
  "succeeded",
  "failed",
  "cancelled",
  "unknown",
  "uncertain",
])

function defaultSessionId(): string {
  const cryptoApi = globalThis.crypto
  if (cryptoApi && typeof cryptoApi.randomUUID === "function") {
    return `chat-${cryptoApi.randomUUID()}`
  }
  return `chat-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`
}

function defaultCreateId(prefix: string): string {
  return `${prefix}-${newIdempotencyKey("id")}`
}

function recoveryErrorInfo(cause: unknown): ChatErrorInfo {
  if (isGatewayError(cause)) {
    return {
      code: cause.code ?? `sdk.${cause.kind}`,
      message: cause.message,
      recovery: cause.recovery,
      retryable: cause.retryable,
      details: cause.details,
    }
  }
  return {
    code: "sdk.unknown",
    message:
      cause instanceof Error && cause.message
        ? cause.message
        : "The Gateway task could not be reconciled.",
    retryable: false,
  }
}

function userText(message: ChatMessage): string {
  return message.parts
    .filter((part) => part.type === "text")
    .map((part) => part.text)
    .join("")
}

/**
 * One browser-local chat session.
 *
 * It owns the user turn, the idempotency key, the reduced state, and exactly one
 * runner. The runner owns the transport lifecycle and the session owns retry
 * semantics, so a retry never appends a second user message or reuses a key.
 */
export function createChatSession(options: ChatSessionOptions): ChatSession {
  const now = options.now ?? (() => Date.now())
  const createId = options.createId ?? defaultCreateId
  const createKey =
    options.createIdempotencyKey ?? (() => newIdempotencyKey("chat"))
  const createSessionId = options.createSessionId ?? defaultSessionId
  const createChatId = options.createChatId ?? createSessionId
  const history = options.history
  const initialSelection = options.initialSelection ?? null

  let chatId = options.chatId ?? createChatId()
  let chatCreatedAt = now()
  let restoredTitle = options.initialTitle
  let selection = initialSelection
  let state =
    options.initialState ??
    createInitialChatState(options.sessionId ?? createSessionId())
  const listeners = new Set<() => void>()
  let disposed = false
  let writeQueue: Promise<void> = Promise.resolve()
  let historyUnavailable = false

  function persistSnapshot(): void {
    if (!history || disposed || historyUnavailable) {
      return
    }
    const snapshot = createChatSnapshot({
      chatId,
      state,
      selection,
      createdAt: chatCreatedAt,
      title: state.messages.some((message) => message.role === "user")
        ? undefined
        : restoredTitle,
      now,
    })
    writeQueue = writeQueue
      .then(() => history.saveChat(snapshot))
      .catch((cause) => {
        historyUnavailable = true
        options.onPersistError?.(cause)
      })
  }

  const dispatch = (action: ChatAction): void => {
    const next = chatReducer(state, action)
    if (next === state) {
      return
    }
    state = next
    for (const listener of listeners) {
      listener()
    }
    persistSnapshot()
  }

  const createRunner = (): ChatRunner | null =>
    options.client
      ? new ChatRunner({
          client: options.client,
          dispatch,
          sessionId: () => state.sessionId,
          sleep: options.sleep,
          random: options.random,
          maxReconnectAttempts: options.maxReconnectAttempts,
          initialBackoffMs: options.initialBackoffMs,
          maxBackoffMs: options.maxBackoffMs,
        })
      : null

  let runner: ChatRunner | null = createRunner()

  function start(text: string, selection: ChatSelection): void {
    const value = text.trim()
    if (
      disposed ||
      !runner ||
      !value ||
      !selection.worker ||
      !selection.agent ||
      busyStatuses.has(state.status)
    ) {
      return
    }

    const userMessageId = createId("user")
    const assistantMessageId = createId("assistant")
    const idempotencyKey = createKey()
    const turnSelection = { ...selection }
    selection = turnSelection
    dispatch({
      type: "send",
      userMessageId,
      assistantMessageId,
      text: value,
      selection: turnSelection,
      idempotencyKey,
      createdAt: now(),
    })
    void runner.runTask({
      text: value,
      selection: turnSelection,
      assistantMessageId,
      idempotencyKey,
    })
  }

  function retry(userMessageId: string): void {
    if (disposed || !runner || busyStatuses.has(state.status)) {
      return
    }
    const user = state.messages.find(
      (message) => message.id === userMessageId && message.role === "user",
    )
    const assistant = state.messages.find(
      (message) =>
        message.role === "assistant" && message.replyTo === userMessageId,
    )
    if (!user || !assistant || !user.selection) {
      return
    }
    const task = assistant.parts.find((part) => part.type === "task")
    if (!task || !retryableStates.has(task.state)) {
      return
    }
    const text = userText(user).trim()
    if (!text) {
      return
    }

    const idempotencyKey = createKey()
    dispatch({
      type: "retry-started",
      userMessageId,
      assistantMessageId: assistant.id,
      idempotencyKey,
    })
    void runner.runTask({
      text,
      selection: user.selection,
      assistantMessageId: assistant.id,
      idempotencyKey,
    })
  }

  async function recover(): Promise<void> {
    if (disposed || !runner) {
      return
    }
    const taskId = state.activeTaskId
    const assistantMessageId = state.activeAssistantId
    if (!taskId) {
      if (assistantMessageId && busyStatuses.has(state.status)) {
        dispatch({
          type: "submit-uncertain",
          assistantMessageId,
          error: {
            code: "task.reload_interrupted",
            message:
              "The browser reloaded before a task identity was saved, so the Gateway outcome is unknown.",
            recovery:
              "Inspect the Worker or task evidence on the Gateway before sending the prompt again.",
            retryable: false,
          },
        })
      } else if (!assistantMessageId && busyStatuses.has(state.status)) {
        dispatch({
          type: "task-detached",
          assistantMessageId: null,
          taskId: "unknown",
          detail:
            "The saved chat was interrupted without a task identity. Local history is kept, but the task cannot be resumed.",
        })
      }
      return
    }
    if (!assistantMessageId || terminalStatuses.has(state.status)) {
      return
    }

    dispatch({ type: "recovery-started", taskId })

    let task
    try {
      task = await options.client!.task(taskId)
    } catch (cause) {
      if (disposed) {
        return
      }
      if (isGatewayError(cause) && cause.kind === "not-found") {
        dispatch({
          type: "task-detached",
          assistantMessageId,
          taskId,
          detail:
            "The Gateway no longer exposes this task. Local history is kept, but the task cannot be resumed.",
        })
      } else {
        dispatch({
          type: "recovery-failed",
          assistantMessageId,
          taskId,
          error: recoveryErrorInfo(cause),
        })
      }
      return
    }

    if (disposed) {
      return
    }
    dispatch({ type: "task-projection", assistantMessageId, task })
    if (terminalStatuses.has(state.status)) {
      return
    }

    void runner.resumeTask({
      taskId,
      assistantMessageId,
      after: state.lastDurableSeq,
    })
  }

  persistSnapshot()

  return {
    getState: () => state,

    getChatId: () => chatId,

    subscribe(listener) {
      listeners.add(listener)
      return () => {
        listeners.delete(listener)
      }
    },

    send(text, selection) {
      start(text, selection)
    },

    updateSelection(next) {
      if (disposed) {
        return
      }
      selection = { ...next }
      persistSnapshot()
    },

    stop() {
      if (disposed) {
        return
      }
      void runner?.stop()
    },

    retry,

    applyApproval(approval) {
      if (disposed) {
        return
      }
      dispatch({
        type: "approval-snapshot",
        taskId: approval.task_id,
        approval,
      })
    },

    recover,

    newChat() {
      if (disposed) {
        return
      }
      runner?.dispose()
      chatId = createChatId()
      chatCreatedAt = now()
      restoredTitle = undefined
      state = createInitialChatState(createSessionId())
      runner = createRunner()
      for (const listener of listeners) {
        listener()
      }
      persistSnapshot()
    },

    dispose() {
      disposed = true
      runner?.dispose()
      runner = null
      listeners.clear()
    },

    detach() {
      runner?.interrupt()
    },
  }
}

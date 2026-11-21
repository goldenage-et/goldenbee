import { newIdempotencyKey } from "@goldenbee/sdk"

import {
  ChatRunner,
  type ChatGateway,
  type ChatRunnerOptions,
} from "./chat-runner"
import { chatReducer } from "./chat-reducer"
import {
  createInitialChatState,
  type ChatAction,
  type ChatMessage,
  type ChatSelection,
  type ChatState,
} from "./chat-types"

export interface ChatSessionOptions {
  client: ChatGateway | null
  sessionId?: string
  now?: () => number
  createId?: (prefix: string) => string
  createIdempotencyKey?: () => string
  createSessionId?: () => string
  sleep?: ChatRunnerOptions["sleep"]
  random?: ChatRunnerOptions["random"]
  maxReconnectAttempts?: number
  initialBackoffMs?: number
  maxBackoffMs?: number
}

export interface ChatSession {
  getState(): ChatState
  subscribe(listener: () => void): () => void
  send(text: string, selection: ChatSelection): void
  stop(): void
  retry(userMessageId: string): void
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

  let state = createInitialChatState(options.sessionId ?? createSessionId())
  const listeners = new Set<() => void>()
  let disposed = false

  const dispatch = (action: ChatAction): void => {
    const next = chatReducer(state, action)
    if (next === state) {
      return
    }
    state = next
    for (const listener of listeners) {
      listener()
    }
  }

  let runner: ChatRunner | null = options.client
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
    dispatch({
      type: "send",
      userMessageId,
      assistantMessageId,
      text: value,
      selection,
      idempotencyKey,
      createdAt: now(),
    })
    void runner.runTask({
      text: value,
      selection,
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

  return {
    getState: () => state,

    subscribe(listener) {
      listeners.add(listener)
      return () => {
        listeners.delete(listener)
      }
    },

    send(text, selection) {
      start(text, selection)
    },

    stop() {
      if (disposed) {
        return
      }
      void runner?.stop()
    },

    retry,

    newChat() {
      if (disposed) {
        return
      }
      runner?.dispose()
      state = createInitialChatState(createSessionId())
      runner = options.client
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
      for (const listener of listeners) {
        listener()
      }
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

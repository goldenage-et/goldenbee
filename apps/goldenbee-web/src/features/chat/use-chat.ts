import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react"

import {
  createChatSession,
  type ChatSession,
  type ChatSessionOptions,
} from "./chat-session"
import type { ChatSelection, ChatState } from "./chat-types"
import { LocalHistoryError } from "./local-chat-history"

export type ChatLoadPhase = "loading" | "ready" | "reset-required"

export type ChatSessionView = ChatSession & {
  state: ChatState
  phase: ChatLoadPhase
  historyError: LocalHistoryError | null
  restoredSelection: ChatSelection | null
  resetLocalHistory(): Promise<void>
}

/**
 * Bind one browser-local chat session to React.
 *
 * Without a history store the session is created synchronously. With a store it
 * restores the last chat, reconciles the active task with the Gateway, and only
 * then reports `ready`; input stays disabled while `loading` or `recovering`.
 */
export function useChat(options: ChatSessionOptions): ChatSessionView {
  const { client, sessionId, history } = options
  const optionsRef = useRef(options)
  const generation = useRef(0)

  useEffect(() => {
    optionsRef.current = options
  })

  const [session, setSession] = useState<ChatSession>(() =>
    createChatSession({ ...options, history: undefined }),
  )
  const [phase, setPhase] = useState<ChatLoadPhase>(
    history ? "loading" : "ready",
  )
  const [historyError, setHistoryError] = useState<LocalHistoryError | null>(
    null,
  )
  const [restoredSelection, setRestoredSelection] =
    useState<ChatSelection | null>(null)

  useEffect(() => {
    const attempt = ++generation.current
    const current = optionsRef.current

    if (!history) {
      const next = createChatSession({ ...current, history: undefined })
      // A transport-only session starts synchronously; this effect only swaps
      // identity when the Gateway client changes.
      // oxlint-disable-next-line react/set-state-in-effect
      setPhase("ready")
      setHistoryError(null)
      setSession(next)
      return () => {
        next.detach()
      }
    }

    setPhase("loading")
    setHistoryError(null)
    setRestoredSelection(null)
    let cancelled = false

    void history
      .loadLastChat()
      .then((restored) => {
        if (cancelled || generation.current !== attempt) {
          return
        }
        if (!restored) {
          setSession(createChatSession({ ...current, history }))
          setPhase("ready")
          return
        }
        const restoredSession = createChatSession({
          ...current,
          history,
          chatId: restored.chat.id,
          initialState: restored.state,
          initialSelection: restored.selection,
        })
        setRestoredSelection(restored.selection)
        setSession(restoredSession)
        setPhase("ready")
        void restoredSession.recover()
      })
      .catch((cause: unknown) => {
        if (cancelled || generation.current !== attempt) {
          return
        }
        setHistoryError(
          cause instanceof LocalHistoryError
            ? cause
            : new LocalHistoryError(
                "unavailable",
                "Local chat history could not be read.",
                cause,
              ),
        )
        setPhase("reset-required")
      })

    return () => {
      cancelled = true
    }
  }, [history, client, sessionId])

  useEffect(() => () => session.detach(), [session])

  const state = useSyncExternalStore(
    session.subscribe,
    session.getState,
    session.getState,
  )

  const resetLocalHistory = useMemo(
    () => async () => {
      if (!history) {
        return
      }
      await history.reset()
      const next = createChatSession({ ...optionsRef.current, history })
      setHistoryError(null)
      setRestoredSelection(null)
      setSession(next)
      setPhase("ready")
    },
    [history],
  )

  return {
    ...session,
    state,
    phase,
    historyError,
    restoredSelection,
    resetLocalHistory,
  }
}

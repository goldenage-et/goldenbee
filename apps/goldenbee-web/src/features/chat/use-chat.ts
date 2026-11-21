import { useEffect, useMemo, useSyncExternalStore } from "react"

import {
  createChatSession,
  type ChatSession,
  type ChatSessionOptions,
} from "./chat-session"
import type { ChatState } from "./chat-types"

export type ChatSessionView = ChatSession & { state: ChatState }

/**
 * Bind one browser-local chat session to React.
 *
 * The session is recreated only when the Gateway client or an explicit session
 * id changes. Unmount disposes the runner, which aborts the fetch stream.
 */
export function useChat(options: ChatSessionOptions): ChatSessionView {
  const { client, sessionId } = options
  const session = useMemo(
    () => createChatSession(options),
    // Session identity follows the transport client and explicit session id.
    // Callback seams stay fixed for the lifetime of one session.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [client, sessionId],
  )

  useEffect(() => () => session.detach(), [session])

  const state = useSyncExternalStore(
    session.subscribe,
    session.getState,
    session.getState,
  )

  return { ...session, state }
}

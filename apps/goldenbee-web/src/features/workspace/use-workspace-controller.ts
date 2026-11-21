import { useCallback, useEffect, useState } from "react"

import type { AppRoute } from "@/app/router"
import { navigate } from "@/app/router"
import {
  MAX_VISIBLE_CHATS,
  createChatId,
  useWorkspace,
  visibleChats,
  type SessionGroup,
  type WorkspaceStore,
} from "@/features/workspace/workspace-store"
import type {
  ChatHistoryStore,
  StoredChat,
} from "@/features/chat/local-chat-history"

export interface BackgroundSession {
  id: string
  title: string
}

export interface WorkspaceController {
  groups: SessionGroup[]
  activeGroupId: string
  activeChatId: string | null
  backgroundChatIds: string[]
  activeGroup: SessionGroup | undefined
  activeChatTitle: string | undefined
  backgroundSessions: BackgroundSession[]
  chats: StoredChat[]
  /** Visible panels in the active group, in display order. */
  visibleChatIds: string[]
  /** Maximum number of visible panels the workspace renders. */
  maxVisibleChats: number
  /** True when a split/new session would exceed the visible panel limit. */
  atPanelLimit: boolean
  newSession(): boolean
  split(): boolean
  solo(chatId: string): boolean
  backgroundChat(chatId: string): boolean
  backgroundActive(): boolean
  restoreChat(chatId: string): boolean
  selectGroup(groupId: string): void
  selectChat(chatId: string): boolean
  focusChat(chatId: string): boolean
  createGroup(name?: string): void
  renameGroup(groupId: string, name: string): void
  closeGroup(groupId: string): void
  refreshChats(): Promise<void>
  titleFor(chatId: string): string
}

export interface WorkspaceControllerOptions {
  workspace: WorkspaceStore
  history: ChatHistoryStore | null
  connected: boolean
  route: AppRoute
}

function visibleChat(group: SessionGroup | undefined, background: string[]): string | null {
  return (
    group?.chatIds.find((chatId) => !background.includes(chatId)) ?? null
  )
}

/**
 * Bind the local session layout to the active route and chat history.
 *
 * The active group can show up to {@link MAX_VISIBLE_CHATS} local chats at once.
 * Entering the chat route with an empty group creates a new local chat for that
 * group; an explicitly backgrounded chat is never silently recreated, so the
 * workspace can show its honest empty state.
 */
export function useWorkspaceController({
  workspace,
  history,
  connected,
  route,
}: WorkspaceControllerOptions): WorkspaceController {
  const state = useWorkspace((current) => current, workspace)
  const {
    groups,
    activeGroupId,
    activeChatId,
    backgroundChatIds,
  } = state
  const activeGroup = groups.find((group) => group.id === activeGroupId)
  const [chats, setChats] = useState<StoredChat[]>([])

  const refreshChats = useCallback(async () => {
    if (!history) {
      setChats([])
      return
    }
    try {
      setChats(await history.listChats())
    } catch {
      setChats([])
    }
  }, [history])

  useEffect(() => {
    if (!history) {
      return
    }
    // oxlint-disable-next-line react/set-state-in-effect
    void refreshChats()
  }, [history, refreshChats])

  useEffect(() => {
    if (!history) {
      return
    }
    // oxlint-disable-next-line react/set-state-in-effect
    void refreshChats()
  }, [activeChatId, groups, history, refreshChats])

  useEffect(() => {
    if (!connected || route.name !== "chat") {
      return
    }
    if (activeChatId) {
      return
    }
    const group = groups.find((candidate) => candidate.id === activeGroupId)
    const visible = visibleChat(group, backgroundChatIds)
    if (visible) {
      workspace.getState().selectChat(visible)
      return
    }
    // Every chat in this group is backgrounded. Do not immediately create a
    // replacement: the operator asked for the workspace to be empty.
    if (group && group.chatIds.length > 0) {
      return
    }

    let cancelled = false
    void (async () => {
      // Restore the last local chat only for a genuinely empty workspace. A
      // newly created group must get its own chat instead of focusing a chat
      // that belongs to another group.
      const hasAnyChat = groups.some((group) => group.chatIds.length > 0)
      if (history && !hasAnyChat && backgroundChatIds.length === 0) {
        try {
          const restored = await history.loadLastChat()
          if (!cancelled && restored) {
            workspace.getState().addChat(restored.chat.id)
            return
          }
        } catch {
          // A corrupt local record is reported by the chat view itself; entering
          // the workspace must not fail here.
        }
      }
      if (!cancelled) {
        workspace.getState().addChat(createChatId())
      }
    })()

    return () => {
      cancelled = true
    }
  }, [
    activeChatId,
    activeGroupId,
    backgroundChatIds,
    connected,
    groups,
    history,
    route.name,
    workspace,
  ])

  const titleFor = useCallback(
    (chatId: string) =>
      chats.find((chat) => chat.id === chatId)?.title ?? "Local chat",
    [chats],
  )

  const backgroundSessions = backgroundChatIds.map((id) => ({
    id,
    title: titleFor(id),
  }))
  const visibleChatIds = visibleChats(activeGroup, backgroundChatIds)
  const atPanelLimit = visibleChatIds.length >= MAX_VISIBLE_CHATS

  function ensureChatRoute(): void {
    if (route.name !== "chat") {
      navigate({ name: "chat" })
    }
  }

  return {
    groups,
    activeGroupId,
    activeChatId,
    backgroundChatIds,
    activeGroup,
    activeChatTitle:
      activeChatId === null ? undefined : titleFor(activeChatId),
    backgroundSessions,
    chats,
    visibleChatIds,
    maxVisibleChats: MAX_VISIBLE_CHATS,
    atPanelLimit,
    newSession() {
      const chatId = workspace.getState().split()
      if (!chatId) {
        return false
      }
      void refreshChats()
      ensureChatRoute()
      return true
    },
    split() {
      const chatId = workspace.getState().split()
      if (!chatId) {
        return false
      }
      void refreshChats()
      ensureChatRoute()
      return true
    },
    solo(chatId) {
      const changed = workspace.getState().solo(chatId)
      if (changed) {
        void refreshChats()
      }
      return changed
    },
    backgroundChat(chatId) {
      const changed = workspace.getState().backgroundChat(chatId)
      if (changed) {
        void refreshChats()
      }
      return changed
    },
    backgroundActive() {
      if (!activeChatId) {
        return false
      }
      const changed = workspace.getState().backgroundChat(activeChatId)
      if (changed) {
        void refreshChats()
      }
      return changed
    },
    restoreChat(chatId) {
      const restored = workspace.getState().restoreChat(chatId)
      if (!restored) {
        return false
      }
      void refreshChats()
      ensureChatRoute()
      return true
    },
    selectGroup(groupId) {
      workspace.getState().selectGroup(groupId)
    },
    selectChat(chatId) {
      const selected = workspace.getState().selectChat(chatId)
      if (selected) {
        ensureChatRoute()
      }
      return selected
    },
    focusChat(chatId) {
      const focused = workspace.getState().focusChat(chatId)
      if (focused) {
        ensureChatRoute()
      }
      return focused
    },
    createGroup(name) {
      const id = workspace.getState().createGroup(name)
      workspace.getState().selectGroup(id)
    },
    renameGroup(groupId, name) {
      workspace.getState().renameGroup(groupId, name)
    },
    closeGroup(groupId) {
      workspace.getState().closeGroup(groupId)
      void refreshChats()
    },
    refreshChats,
    titleFor,
  }
}

import { useStore } from "zustand"
import { createStore, type StoreApi } from "zustand/vanilla"
import type { TaskUsage } from "@goldenbee/sdk"

import type {
  ChatRecoveryNotice,
  ChatRunStatus,
  ChatSelection,
  ChatStreamGap,
} from "@/features/chat/chat-types"

/**
 * Live session facts published by each mounted chat panel.
 *
 * The inspector only needs durable identity and status, not streamed text, so a
 * panel publishes a small snapshot when one of those fields changes. This keeps
 * the inspector in sync without moving chat execution state out of the panel.
 */
export interface ChatInspection {
  chatId: string
  sessionId: string
  title: string | undefined
  status: ChatRunStatus
  taskId: string | null
  activeAssistantId: string | null
  lastDurableSeq: number
  selection: ChatSelection | null
  usage: TaskUsage | null
  gap: ChatStreamGap | null
  recovery: ChatRecoveryNotice | null
}

export interface ChatInspectionState {
  inspections: Record<string, ChatInspection>
  publish(inspection: ChatInspection): void
  remove(chatId: string): void
}

export type ChatInspectionStore = StoreApi<ChatInspectionState>

function sameInspection(
  left: ChatInspection | undefined,
  right: ChatInspection,
): boolean {
  if (!left) {
    return false
  }
  return (
    left.sessionId === right.sessionId &&
    left.title === right.title &&
    left.status === right.status &&
    left.taskId === right.taskId &&
    left.activeAssistantId === right.activeAssistantId &&
    left.lastDurableSeq === right.lastDurableSeq &&
    left.selection?.worker === right.selection?.worker &&
    left.selection?.agent === right.selection?.agent &&
    left.selection?.profile === right.selection?.profile &&
    JSON.stringify(left.usage) === JSON.stringify(right.usage) &&
    left.gap?.reason === right.gap?.reason &&
    left.gap?.fromSeq === right.gap?.fromSeq &&
    left.recovery?.kind === right.recovery?.kind &&
    left.recovery?.taskId === right.recovery?.taskId &&
    left.recovery?.detail === right.recovery?.detail
  )
}

export function createChatInspectionStore(): ChatInspectionStore {
  return createStore<ChatInspectionState>((set, get) => ({
    inspections: {},

    publish(inspection) {
      const current = get().inspections[inspection.chatId]
      if (sameInspection(current, inspection)) {
        return
      }
      set((state) => ({
        inspections: {
          ...state.inspections,
          [inspection.chatId]: inspection,
        },
      }))
    },

    remove(chatId) {
      if (!get().inspections[chatId]) {
        return
      }
      set((state) => {
        const next = { ...state.inspections }
        delete next[chatId]
        return { inspections: next }
      })
    },
  }))
}

export const chatInspectionStore = createChatInspectionStore()

export function useChatInspection<T>(
  selector: (state: ChatInspectionState) => T,
  store: ChatInspectionStore = chatInspectionStore,
): T {
  return useStore(store, selector)
}

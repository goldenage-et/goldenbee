import type { ChatSelection } from "./chat-types"

/**
 * Browser-local target preference.
 *
 * Only the Worker, agent, and Runtime Profile names are stored. The caller
 * token, approval digests, task input, and any other transport state never
 * enter this store.
 */

const MAX_NAME_LENGTH = 128

export const chatSelectionStorageKey = "goldenbee.chat.selection.v1"

export interface ChatSelectionStore {
  read(): ChatSelection | null
  write(selection: ChatSelection): void
  clear(): void
}

export function sanitizeSelection(value: unknown): ChatSelection | null {
  if (typeof value !== "object" || value === null) {
    return null
  }
  const record = value as Record<string, unknown>
  const worker = name(record.worker)
  const agent = name(record.agent)
  const profile = name(record.profile)
  if (!worker && !agent && !profile) {
    return null
  }
  return { worker, agent, profile }
}

function name(value: unknown): string {
  if (typeof value !== "string") {
    return ""
  }
  return value.trim().slice(0, MAX_NAME_LENGTH)
}

export function browserLocalStorage(): Storage | null {
  try {
    return typeof window === "undefined" ? null : window.localStorage
  } catch {
    return null
  }
}

export function createChatSelectionStore(
  storage: Storage | null,
  key = chatSelectionStorageKey,
): ChatSelectionStore {
  return {
    read() {
      if (!storage) {
        return null
      }
      try {
        const raw = storage.getItem(key)
        return raw === null ? null : sanitizeSelection(JSON.parse(raw))
      } catch {
        // Corrupt or unreadable preferences fall back to the connection's
        // defaults; they must never block the chat.
        return null
      }
    },

    write(selection) {
      if (!storage) {
        return
      }
      const payload: ChatSelection = {
        worker: name(selection.worker),
        agent: name(selection.agent),
        profile: name(selection.profile ?? ""),
      }
      try {
        storage.setItem(key, JSON.stringify(payload))
      } catch {
        // Storage can be full or blocked. The in-memory selection still works.
      }
    },

    clear() {
      try {
        storage?.removeItem(key)
      } catch {
        // Ignore an unavailable store.
      }
    },
  }
}

export const chatSelectionStore = createChatSelectionStore(browserLocalStorage())

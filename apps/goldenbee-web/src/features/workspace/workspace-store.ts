import { useStore } from "zustand"
import { createStore, type StoreApi } from "zustand/vanilla"

/**
 * Browser-local session layout.
 *
 * Groups, tabs, and background markers are local Client state. A group is not a
 * Gateway resource and a chat id is not a task id. Closing a group only hides
 * its local chats: no Gateway task is cancelled from this store.
 */

export const workspaceStorageKey = "goldenbee.workspace.v1"
const STORAGE_VERSION = 2
const LEGACY_STORAGE_VERSION = 1
const MAX_GROUP_NAME_LENGTH = 60

/** The workspace never renders more than four concurrent chat panels. */
export const MAX_VISIBLE_CHATS = 4

export interface SessionGroup {
  id: string
  name: string
  chatIds: string[]
}

export interface WorkspaceState {
  groups: SessionGroup[]
  activeGroupId: string
  /** Focused local chat; it drives the inspector only, never execution. */
  activeChatId: string | null
  backgroundChatIds: string[]
  createGroup(name?: string): string
  renameGroup(groupId: string, name: string): void
  closeGroup(groupId: string): void
  selectGroup(groupId: string): void
  /** Add a local chat to the active group, or focus it if it already exists. */
  addChat(chatId: string): boolean
  /** Focus a chat for the inspector; restores it first when it is backgrounded. */
  selectChat(chatId: string): boolean
  /** Explicit name for the inspector-focus action. */
  focusChat(chatId: string): boolean
  /** Add a new local chat panel to the active group and focus it. */
  split(): string | null
  /** Hide every other visible chat in this chat's group and focus this one. */
  solo(chatId: string): boolean
  backgroundChat(chatId: string): boolean
  restoreChat(chatId: string): boolean
}

/** Visible chats in display order. A chat is visible when it is not backgrounded. */
export function visibleChats(
  group: SessionGroup | undefined,
  backgroundChatIds: string[],
): string[] {
  if (!group) {
    return []
  }
  return group.chatIds.filter((chatId) => !backgroundChatIds.includes(chatId))
}

export interface WorkspaceDeps {
  storage: Storage | null
  key?: string
  createId?: (prefix: string) => string
  defaultGroupName?: string
}

export type WorkspaceStore = StoreApi<WorkspaceState>

interface PersistedWorkspace {
  version: number
  groups: SessionGroup[]
  activeGroupId: string
  activeChatId: string | null
  backgroundChatIds: string[]
}

export function createChatId(): string {
  const cryptoApi = globalThis.crypto
  if (cryptoApi && typeof cryptoApi.randomUUID === "function") {
    return `chat-${cryptoApi.randomUUID()}`
  }
  return `chat-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`
}

function defaultCreateId(prefix: string): string {
  const cryptoApi = globalThis.crypto
  if (cryptoApi && typeof cryptoApi.randomUUID === "function") {
    return `${prefix}-${cryptoApi.randomUUID()}`
  }
  return `${prefix}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`
}

function cleanName(value: unknown, fallback: string): string {
  if (typeof value !== "string") {
    return fallback
  }
  const trimmed = value.trim().slice(0, MAX_GROUP_NAME_LENGTH)
  return trimmed.length > 0 ? trimmed : fallback
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function parseGroup(value: unknown): SessionGroup | null {
  if (!isRecord(value) || typeof value.id !== "string" || value.id === "") {
    return null
  }
  if (typeof value.name !== "string") {
    return null
  }
  if (!Array.isArray(value.chatIds)) {
    return null
  }
  const chatIds: string[] = []
  for (const chatId of value.chatIds) {
    if (typeof chatId === "string" && chatId !== "" && !chatIds.includes(chatId)) {
      chatIds.push(chatId)
    }
  }
  return {
    id: value.id,
    name: cleanName(value.name, "Sessions"),
    chatIds,
  }
}

function firstVisibleChat(
  group: SessionGroup | undefined,
  backgroundChatIds: string[],
): string | null {
  if (!group) {
    return null
  }
  return (
    group.chatIds.find((chatId) => !backgroundChatIds.includes(chatId)) ?? null
  )
}

function sanitize(
  raw: unknown,
  createId: (prefix: string) => string,
  defaultGroupName: string,
): PersistedWorkspace {
  const createDefault = (): PersistedWorkspace => {
    const id = createId("group")
    return {
      version: STORAGE_VERSION,
      groups: [{ id, name: defaultGroupName, chatIds: [] }],
      activeGroupId: id,
      activeChatId: null,
      backgroundChatIds: [],
    }
  }

  if (
    !isRecord(raw) ||
    (raw.version !== STORAGE_VERSION && raw.version !== LEGACY_STORAGE_VERSION)
  ) {
    return createDefault()
  }
  if (!Array.isArray(raw.groups) || raw.groups.length === 0) {
    return createDefault()
  }

  const groups: SessionGroup[] = []
  const seenGroups = new Set<string>()
  const seenChats = new Set<string>()
  for (const candidate of raw.groups) {
    const group = parseGroup(candidate)
    if (!group || seenGroups.has(group.id)) {
      continue
    }
    seenGroups.add(group.id)
    group.chatIds = group.chatIds.filter((chatId) => {
      if (seenChats.has(chatId)) {
        return false
      }
      seenChats.add(chatId)
      return true
    })
    groups.push(group)
  }
  if (groups.length === 0) {
    return createDefault()
  }

  const backgroundChatIds: string[] = []
  if (Array.isArray(raw.backgroundChatIds)) {
    for (const chatId of raw.backgroundChatIds) {
      if (
        typeof chatId === "string" &&
        seenChats.has(chatId) &&
        !backgroundChatIds.includes(chatId)
      ) {
        backgroundChatIds.push(chatId)
      }
    }
  }

  const activeGroupId =
    typeof raw.activeGroupId === "string" &&
    groups.some((group) => group.id === raw.activeGroupId)
      ? raw.activeGroupId
      : groups[0]!.id
  const activeGroup = groups.find((group) => group.id === activeGroupId)
  let activeChatId =
    typeof raw.activeChatId === "string" &&
    activeGroup?.chatIds.includes(raw.activeChatId) &&
    !backgroundChatIds.includes(raw.activeChatId)
      ? raw.activeChatId
      : firstVisibleChat(activeGroup, backgroundChatIds)

  // Ticket 13 had one visible chat per group. A legacy record may carry more
  // chats without background markers; keep the focused chat and up to three
  // others visible, then mark the rest as background instead of dropping them.
  for (const group of groups) {
    const visible = group.chatIds.filter(
      (chatId) => !backgroundChatIds.includes(chatId),
    )
    if (visible.length <= MAX_VISIBLE_CHATS) {
      continue
    }
    const keep = new Set(visible.slice(0, MAX_VISIBLE_CHATS))
    if (
      group.id === activeGroupId &&
      activeChatId !== null &&
      visible.includes(activeChatId) &&
      !keep.has(activeChatId)
    ) {
      keep.delete(visible[MAX_VISIBLE_CHATS - 1]!)
      keep.add(activeChatId)
    }
    for (const chatId of visible) {
      if (!keep.has(chatId)) {
        backgroundChatIds.push(chatId)
      }
    }
  }
  if (activeChatId !== null && backgroundChatIds.includes(activeChatId)) {
    activeChatId = firstVisibleChat(activeGroup, backgroundChatIds)
  }

  return {
    version: STORAGE_VERSION,
    groups,
    activeGroupId,
    activeChatId,
    backgroundChatIds,
  }
}

function loadInitial(
  storage: Storage | null,
  key: string,
  createId: (prefix: string) => string,
  defaultGroupName: string,
): PersistedWorkspace {
  if (!storage) {
    return sanitize(null, createId, defaultGroupName)
  }
  try {
    const raw = storage.getItem(key)
    return sanitize(raw === null ? null : JSON.parse(raw), createId, defaultGroupName)
  } catch {
    return sanitize(null, createId, defaultGroupName)
  }
}

function persist(
  storage: Storage | null,
  key: string,
  state: WorkspaceState,
): void {
  if (!storage) {
    return
  }
  const payload: PersistedWorkspace = {
    version: STORAGE_VERSION,
    groups: state.groups,
    activeGroupId: state.activeGroupId,
    activeChatId: state.activeChatId,
    backgroundChatIds: state.backgroundChatIds,
  }
  try {
    storage.setItem(key, JSON.stringify(payload))
  } catch {
    // The in-memory layout still works when storage is unavailable or full.
  }
}

export function createWorkspaceStore(deps: WorkspaceDeps): WorkspaceStore {
  const storage = deps.storage
  const key = deps.key ?? workspaceStorageKey
  const createId = deps.createId ?? defaultCreateId
  const defaultGroupName = deps.defaultGroupName ?? "Sessions"
  const initial = loadInitial(storage, key, createId, defaultGroupName)

  const store = createStore<WorkspaceState>((set, get) => ({
    groups: initial.groups,
    activeGroupId: initial.activeGroupId,
    activeChatId: initial.activeChatId,
    backgroundChatIds: initial.backgroundChatIds,

    createGroup(name) {
      const state = get()
      const id = createId("group")
      const group: SessionGroup = {
        id,
        name: cleanName(name, `Group ${state.groups.length + 1}`),
        chatIds: [],
      }
      set({
        groups: [...state.groups, group],
        activeGroupId: id,
        activeChatId: null,
      })
      return id
    },

    renameGroup(groupId, name) {
      const state = get()
      const next = cleanName(name, "Sessions")
      set({
        groups: state.groups.map((group) =>
          group.id === groupId ? { ...group, name: next } : group,
        ),
      })
    },

    closeGroup(groupId) {
      const state = get()
      const closing = state.groups.find((group) => group.id === groupId)
      if (!closing) {
        return
      }
      const visible = closing.chatIds.filter(
        (chatId) => !state.backgroundChatIds.includes(chatId),
      )
      const backgroundChatIds = [...state.backgroundChatIds]
      for (const chatId of visible) {
        if (!backgroundChatIds.includes(chatId)) {
          backgroundChatIds.push(chatId)
        }
      }

      const index = state.groups.findIndex((group) => group.id === groupId)
      const remaining = state.groups.filter((group) => group.id !== groupId)
      if (remaining.length === 0) {
        const id = createId("group")
        set({
          groups: [{ id, name: defaultGroupName, chatIds: [] }],
          activeGroupId: id,
          activeChatId: null,
          backgroundChatIds,
        })
        return
      }
      const next =
        remaining[Math.min(index, remaining.length - 1)] ?? remaining[0]!
      set({
        groups: remaining,
        activeGroupId: next.id,
        activeChatId: firstVisibleChat(next, backgroundChatIds),
        backgroundChatIds,
      })
    },

    selectGroup(groupId) {
      const state = get()
      if (!state.groups.some((group) => group.id === groupId)) {
        return
      }
      const group = state.groups.find((candidate) => candidate.id === groupId)
      set({
        activeGroupId: groupId,
        activeChatId: firstVisibleChat(group, state.backgroundChatIds),
      })
    },

    addChat(chatId) {
      if (!chatId) {
        return false
      }
      const state = get()
      const owner = state.groups.find((group) => group.chatIds.includes(chatId))
      if (owner) {
        return get().focusChat(chatId)
      }
      const active =
        state.groups.find((group) => group.id === state.activeGroupId) ??
        state.groups[0]
      if (!active) {
        const id = createId("group")
        set({
          groups: [{ id, name: defaultGroupName, chatIds: [chatId] }],
          activeGroupId: id,
          activeChatId: chatId,
          backgroundChatIds: state.backgroundChatIds.filter(
            (candidate) => candidate !== chatId,
          ),
        })
        return true
      }
      if (
        visibleChats(active, state.backgroundChatIds).length >= MAX_VISIBLE_CHATS
      ) {
        return false
      }
      set({
        groups: state.groups.map((group) =>
          group.id === active.id
            ? { ...group, chatIds: [...group.chatIds, chatId] }
            : group,
        ),
        activeGroupId: active.id,
        activeChatId: chatId,
        backgroundChatIds: state.backgroundChatIds.filter(
          (candidate) => candidate !== chatId,
        ),
      })
      return true
    },

    focusChat(chatId) {
      const state = get()
      const owner = state.groups.find((group) => group.chatIds.includes(chatId))
      if (!owner) {
        return get().addChat(chatId)
      }
      if (state.backgroundChatIds.includes(chatId)) {
        return get().restoreChat(chatId)
      }
      set({
        activeGroupId: owner.id,
        activeChatId: chatId,
      })
      return true
    },

    selectChat(chatId) {
      return get().focusChat(chatId)
    },

    split() {
      const state = get()
      const active =
        state.groups.find((group) => group.id === state.activeGroupId) ??
        state.groups[0]
      if (!active) {
        return null
      }
      if (
        visibleChats(active, state.backgroundChatIds).length >= MAX_VISIBLE_CHATS
      ) {
        return null
      }
      const chatId = createId("chat")
      set({
        groups: state.groups.map((group) =>
          group.id === active.id
            ? { ...group, chatIds: [...group.chatIds, chatId] }
            : group,
        ),
        activeGroupId: active.id,
        activeChatId: chatId,
        backgroundChatIds: state.backgroundChatIds.filter(
          (candidate) => candidate !== chatId,
        ),
      })
      return chatId
    },

    solo(chatId) {
      const state = get()
      const owner = state.groups.find((group) => group.chatIds.includes(chatId))
      if (!owner) {
        return false
      }
      const backgroundChatIds = [...state.backgroundChatIds]
      for (const candidate of owner.chatIds) {
        if (
          candidate !== chatId &&
          !backgroundChatIds.includes(candidate)
        ) {
          backgroundChatIds.push(candidate)
        }
      }
      set({
        activeGroupId: owner.id,
        activeChatId: chatId,
        backgroundChatIds: state.backgroundChatIds.includes(chatId)
          ? backgroundChatIds.filter((candidate) => candidate !== chatId)
          : backgroundChatIds,
      })
      return true
    },

    backgroundChat(chatId) {
      const state = get()
      const owner = state.groups.find((group) => group.chatIds.includes(chatId))
      if (!owner) {
        return false
      }
      const backgroundChatIds = state.backgroundChatIds.includes(chatId)
        ? state.backgroundChatIds
        : [...state.backgroundChatIds, chatId]
      const activeChatId =
        state.activeChatId === chatId
          ? firstVisibleChat(owner, backgroundChatIds)
          : state.activeChatId
      set({ backgroundChatIds, activeChatId })
      return true
    },

    restoreChat(chatId) {
      const state = get()
      const owner = state.groups.find((group) => group.chatIds.includes(chatId))
      const backgroundChatIds = state.backgroundChatIds.filter(
        (candidate) => candidate !== chatId,
      )
      if (owner) {
        if (!state.backgroundChatIds.includes(chatId)) {
          set({ activeGroupId: owner.id, activeChatId: chatId })
          return true
        }
        if (
          visibleChats(owner, state.backgroundChatIds).length >=
          MAX_VISIBLE_CHATS
        ) {
          return false
        }
        set({
          activeGroupId: owner.id,
          activeChatId: chatId,
          backgroundChatIds,
        })
        return true
      }
      // A chat from a closed group has no group left; restore it into the
      // active group without creating a second local chat record.
      const active =
        state.groups.find((group) => group.id === state.activeGroupId) ??
        state.groups[0]
      if (!active) {
        return get().addChat(chatId)
      }
      if (
        visibleChats(active, state.backgroundChatIds).length >= MAX_VISIBLE_CHATS
      ) {
        return false
      }
      set({
        groups: state.groups.map((group) =>
          group.id === active.id
            ? { ...group, chatIds: [...group.chatIds, chatId] }
            : group,
        ),
        activeGroupId: active.id,
        activeChatId: chatId,
        backgroundChatIds,
      })
      return true
    },
  }))

  // Rewrite the normalized layout immediately so a migrated ticket 13 record
  // is stored in the current schema even before the first user action.
  persist(storage, key, store.getState())
  store.subscribe((state) => persist(storage, key, state))
  return store
}

export function browserStorage(): Storage | null {
  try {
    return typeof window === "undefined" ? null : window.localStorage
  } catch {
    return null
  }
}

export const workspaceStore = createWorkspaceStore({
  storage: browserStorage(),
})

export function useWorkspace<T>(
  selector: (state: WorkspaceState) => T,
  store: WorkspaceStore = workspaceStore,
): T {
  return useStore(store, selector)
}

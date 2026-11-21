import { describe, expect, it } from "vitest"

import {
  MAX_VISIBLE_CHATS,
  createWorkspaceStore,
  visibleChats,
  workspaceStorageKey,
  type WorkspaceStore,
} from "./workspace-store"

function memoryStorage(): Storage {
  const values = new Map<string, string>()
  return {
    get length() {
      return values.size
    },
    clear: () => values.clear(),
    getItem: (key) => values.get(key) ?? null,
    key: (index) => Array.from(values.keys())[index] ?? null,
    removeItem: (key) => {
      values.delete(key)
    },
    setItem: (key, value) => {
      values.set(key, value)
    },
  }
}

function storeWithIds(storage: Storage): WorkspaceStore {
  let counter = 0
  return createWorkspaceStore({
    storage,
    createId: (prefix) => {
      counter += 1
      return `${prefix}-${counter}`
    },
  })
}

describe("workspace session layout", () => {
  it("creates, renames, and switches groups without losing their chats", () => {
    const store = storeWithIds(memoryStorage())
    const first = store.getState().activeGroupId
    store.getState().addChat("chat-a")
    expect(store.getState().activeChatId).toBe("chat-a")

    const second = store.getState().createGroup("Research")
    store.getState().renameGroup(second, "Research notes")
    store.getState().addChat("chat-b")

    expect(store.getState().groups).toEqual([
      { id: first, name: "Sessions", chatIds: ["chat-a"] },
      { id: second, name: "Research notes", chatIds: ["chat-b"] },
    ])

    store.getState().selectGroup(first)
    expect(store.getState().activeChatId).toBe("chat-a")
    store.getState().selectGroup(second)
    expect(store.getState().activeChatId).toBe("chat-b")
  })

  it("backgrounds visible chats when a group closes and never removes the chats", () => {
    const store = storeWithIds(memoryStorage())
    store.getState().addChat("chat-a")
    const second = store.getState().createGroup("Temp")
    store.getState().addChat("chat-b")

    store.getState().closeGroup(second)

    expect(store.getState().groups.map((group) => group.id)).not.toContain(second)
    expect(store.getState().backgroundChatIds).toContain("chat-b")
    expect(store.getState().groups[0]?.chatIds).toContain("chat-a")
  })

  it("hides a background chat and restores the same local id", () => {
    const store = storeWithIds(memoryStorage())
    const group = store.getState().activeGroupId
    store.getState().addChat("chat-a")
    store.getState().addChat("chat-b")
    store.getState().selectChat("chat-b")

    store.getState().backgroundChat("chat-b")

    expect(store.getState().backgroundChatIds).toContain("chat-b")
    expect(store.getState().activeChatId).toBe("chat-a")

    store.getState().restoreChat("chat-b")

    expect(store.getState().backgroundChatIds).not.toContain("chat-b")
    expect(store.getState().activeChatId).toBe("chat-b")
    expect(store.getState().activeGroupId).toBe(group)
    expect(store.getState().groups[0]?.chatIds).toEqual(["chat-a", "chat-b"])
  })

  it("restores a background chat from a closed group into the active group", () => {
    const store = storeWithIds(memoryStorage())
    const first = store.getState().activeGroupId
    store.getState().addChat("chat-a")
    const second = store.getState().createGroup("Temp")
    store.getState().addChat("chat-b")
    store.getState().closeGroup(second)

    store.getState().selectGroup(first)
    store.getState().restoreChat("chat-b")

    expect(store.getState().activeGroupId).toBe(first)
    expect(store.getState().activeChatId).toBe("chat-b")
    expect(store.getState().groups[0]?.chatIds).toContain("chat-b")
  })

  it("persists only layout data and ignores malformed records", () => {
    const storage = memoryStorage()
    const store = storeWithIds(storage)
    store.getState().addChat("chat-a")
    const raw = storage.getItem(workspaceStorageKey) ?? ""

    expect(raw).not.toContain("token")
    expect(raw).not.toContain("Authorization")
    expect(JSON.parse(raw)).toMatchObject({
      version: 2,
      activeChatId: "chat-a",
    })

    const corrupt = createWorkspaceStore({
      storage: memoryStorage(),
      key: workspaceStorageKey,
      createId: () => "group-fallback",
    })
    expect(corrupt.getState().groups).toHaveLength(1)
    expect(corrupt.getState().activeChatId).toBeNull()

    const restored = createWorkspaceStore({
      storage: {
        ...memoryStorage(),
        getItem: () => "{not-json",
      },
      createId: () => "group-once",
    })
    expect(restored.getState().groups[0]?.id).toBe("group-once")
  })

  it("shows several chats at once and tracks one focused chat", () => {
    const store = storeWithIds(memoryStorage())
    store.getState().addChat("chat-a")
    store.getState().addChat("chat-b")
    store.getState().addChat("chat-c")

    expect(store.getState().backgroundChatIds).toEqual([])
    expect(
      visibleChats(
        store.getState().groups[0],
        store.getState().backgroundChatIds,
      ),
    ).toEqual(["chat-a", "chat-b", "chat-c"])

    expect(store.getState().focusChat("chat-b")).toBe(true)
    expect(store.getState().activeChatId).toBe("chat-b")
    expect(store.getState().backgroundChatIds).toEqual([])
  })

  it("splits into a new focused chat and refuses a fifth visible panel", () => {
    const store = storeWithIds(memoryStorage())
    const first = store.getState().split()
    expect(first).toMatch(/^chat-/)
    expect(store.getState().activeChatId).toBe(first)

    for (let index = 0; index < MAX_VISIBLE_CHATS - 1; index += 1) {
      expect(store.getState().split()).not.toBeNull()
    }
    expect(
      visibleChats(
        store.getState().groups[0],
        store.getState().backgroundChatIds,
      ),
    ).toHaveLength(MAX_VISIBLE_CHATS)

    expect(store.getState().split()).toBeNull()
    expect(store.getState().addChat("chat-overflow")).toBe(false)
    expect(
      visibleChats(
        store.getState().groups[0],
        store.getState().backgroundChatIds,
      ),
    ).toHaveLength(MAX_VISIBLE_CHATS)
  })

  it("solos one chat by backgrounding the other visible panels", () => {
    const store = storeWithIds(memoryStorage())
    store.getState().addChat("chat-a")
    store.getState().addChat("chat-b")
    store.getState().addChat("chat-c")

    expect(store.getState().solo("chat-b")).toBe(true)

    expect(store.getState().activeChatId).toBe("chat-b")
    expect(store.getState().backgroundChatIds).toEqual(["chat-a", "chat-c"])
    expect(
      visibleChats(
        store.getState().groups[0],
        store.getState().backgroundChatIds,
      ),
    ).toEqual(["chat-b"])
    // The chats stay in the group; only their panels are hidden.
    expect(store.getState().groups[0]?.chatIds).toEqual([
      "chat-a",
      "chat-b",
      "chat-c",
    ])
  })

  it("restores a background chat without exceeding the visible limit", () => {
    const store = storeWithIds(memoryStorage())
    for (const chatId of ["chat-a", "chat-b", "chat-c", "chat-d", "chat-e"]) {
      store.getState().addChat(chatId)
    }
    expect(store.getState().addChat("chat-e")).toBe(false)

    const storeWithFour = storeWithIds(memoryStorage())
    storeWithFour.getState().addChat("chat-a")
    storeWithFour.getState().addChat("chat-b")
    storeWithFour.getState().addChat("chat-c")
    storeWithFour.getState().addChat("chat-d")
    storeWithFour.getState().backgroundChat("chat-d")
    storeWithFour.getState().addChat("chat-e")

    expect(storeWithFour.getState().restoreChat("chat-d")).toBe(false)
    expect(storeWithFour.getState().backgroundChatIds).toEqual(["chat-d"])

    storeWithFour.getState().backgroundChat("chat-e")
    expect(storeWithFour.getState().restoreChat("chat-d")).toBe(true)
    expect(storeWithFour.getState().activeChatId).toBe("chat-d")
  })

  it("migrates a ticket 13 record and caps visible panels at four", () => {
    const storage = memoryStorage()
    storage.setItem(
      workspaceStorageKey,
      JSON.stringify({
        version: 1,
        groups: [
          {
            id: "g1",
            name: "One",
            chatIds: ["chat-a", "chat-b", "chat-c", "chat-d", "chat-e"],
          },
        ],
        activeGroupId: "g1",
        activeChatId: "chat-e",
        backgroundChatIds: [],
      }),
    )
    const store = createWorkspaceStore({
      storage,
      createId: () => "group-new",
    })

    expect(
      visibleChats(
        store.getState().groups[0],
        store.getState().backgroundChatIds,
      ),
    ).toHaveLength(MAX_VISIBLE_CHATS)
    // The chat that was visible before the upgrade keeps its focus.
    expect(store.getState().backgroundChatIds).toEqual(["chat-d"])
    expect(store.getState().activeChatId).toBe("chat-e")

    const persisted = JSON.parse(storage.getItem(workspaceStorageKey) ?? "{}")
    expect(persisted.version).toBe(2)
    expect(persisted.groups[0].chatIds).toHaveLength(5)
  })

  it("drops duplicate chats and a dangling active chat from stored state", () => {
    const storage = memoryStorage()
    storage.setItem(
      workspaceStorageKey,
      JSON.stringify({
        version: 1,
        groups: [
          { id: "g1", name: "One", chatIds: ["chat-a", "chat-a"] },
          { id: "g2", name: "Two", chatIds: ["chat-a", "chat-b"] },
        ],
        activeGroupId: "g1",
        activeChatId: "chat-missing",
        backgroundChatIds: ["chat-b", "chat-unknown"],
      }),
    )
    const store = createWorkspaceStore({
      storage,
      createId: () => "group-new",
    })

    expect(store.getState().groups).toEqual([
      { id: "g1", name: "One", chatIds: ["chat-a"] },
      { id: "g2", name: "Two", chatIds: ["chat-b"] },
    ])
    expect(store.getState().activeChatId).toBe("chat-a")
    expect(store.getState().backgroundChatIds).toEqual(["chat-b"])
  })
})

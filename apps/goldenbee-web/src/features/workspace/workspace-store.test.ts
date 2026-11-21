import { describe, expect, it } from "vitest"

import {
  createWorkspaceStore,
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
      version: 1,
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

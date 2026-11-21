import { describe, expect, it } from "vitest"

import {
  chatSelectionStorageKey,
  createChatSelectionStore,
  sanitizeSelection,
} from "./chat-selection-store"

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

describe("chat selection store", () => {
  it("round-trips the three non-secret selection names", () => {
    const storage = memoryStorage()
    const store = createChatSelectionStore(storage)

    store.write({ worker: "general-a", agent: "general", profile: "default" })

    expect(store.read()).toEqual({
      worker: "general-a",
      agent: "general",
      profile: "default",
    })
  })

  it("stores only the selection names", () => {
    const storage = memoryStorage()
    const store = createChatSelectionStore(storage)

    store.write({ worker: "general-a", agent: "general", profile: "default" })

    const raw = storage.getItem(chatSelectionStorageKey)
    expect(raw).not.toBeNull()
    expect(Object.keys(JSON.parse(raw ?? "{}")).sort()).toEqual([
      "agent",
      "profile",
      "worker",
    ])
    expect(raw).not.toContain("token")
    expect(raw).not.toContain("Bearer")
  })

  it("ignores extra fields and control characters when reading", () => {
    expect(
      sanitizeSelection({
        worker: "  general-a  ",
        agent: "general",
        profile: "default",
        token: "caller-token",
      }),
    ).toEqual({ worker: "general-a", agent: "general", profile: "default" })
    expect(sanitizeSelection({ worker: 42, agent: null })).toBeNull()
    expect(sanitizeSelection("general-a")).toBeNull()
  })

  it("treats corrupt preferences as absent", () => {
    const storage = memoryStorage()
    storage.setItem(chatSelectionStorageKey, "{not-json")
    const store = createChatSelectionStore(storage)

    expect(store.read()).toBeNull()
  })

  it("removes a stored selection on clear", () => {
    const storage = memoryStorage()
    const store = createChatSelectionStore(storage)
    store.write({ worker: "general-a", agent: "general" })

    store.clear()

    expect(store.read()).toBeNull()
    expect(storage.getItem(chatSelectionStorageKey)).toBeNull()
  })

  it("does nothing when browser storage is unavailable", () => {
    const store = createChatSelectionStore(null)

    expect(() =>
      store.write({ worker: "general-a", agent: "general" }),
    ).not.toThrow()
    expect(store.read()).toBeNull()
    expect(() => store.clear()).not.toThrow()
  })

  it("survives a storage write failure", () => {
    const storage = memoryStorage()
    storage.setItem = () => {
      throw new Error("quota exceeded")
    }
    const store = createChatSelectionStore(storage)

    expect(() =>
      store.write({ worker: "general-a", agent: "general" }),
    ).not.toThrow()
  })
})

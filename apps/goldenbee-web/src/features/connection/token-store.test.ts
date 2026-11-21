import { describe, expect, it, vi } from "vitest"

import { createTokenStore } from "@/features/connection/token-store"

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

describe("caller token storage", () => {
  it("keeps the token in memory by default", () => {
    const session = memoryStorage()
    const store = createTokenStore({ sessionStorage: session })

    store.write("secret-token", "memory")

    expect(store.read()).toBe("secret-token")
    expect(store.readPersistence()).toBe("memory")
    expect(session.length).toBe(0)
  })

  it("persists only to the injected current-tab storage when asked", () => {
    const session = memoryStorage()
    const local = memoryStorage()
    vi.stubGlobal("localStorage", local)
    const store = createTokenStore({ sessionStorage: session })

    const result = store.write("secret-token", "session")

    expect(result.persisted).toBe(true)
    expect(store.readPersistence()).toBe("session")
    expect(session.getItem("goldenbee.caller-token")).toBe("secret-token")
    expect(local.length).toBe(0)
    vi.unstubAllGlobals()
  })

  it("restores a current-tab token into a new store", () => {
    const session = memoryStorage()
    createTokenStore({ sessionStorage: session }).write("secret-token", "session")

    const reloaded = createTokenStore({ sessionStorage: session })

    expect(reloaded.read()).toBe("secret-token")
  })

  it("clears both memory and current-tab storage", () => {
    const session = memoryStorage()
    const store = createTokenStore({ sessionStorage: session })
    store.write("secret-token", "session")

    store.clear()

    expect(store.read()).toBeNull()
    expect(session.length).toBe(0)
    expect(store.readPersistence()).toBe("memory")
  })

  it("degrades to memory when current-tab storage rejects a write", () => {
    const session = memoryStorage()
    session.setItem = () => {
      throw new DOMException("quota exceeded", "QuotaExceededError")
    }
    const store = createTokenStore({ sessionStorage: session })

    const result = store.write("secret-token", "session")

    expect(result.persisted).toBe(false)
    expect(store.read()).toBe("secret-token")
    expect(store.readPersistence()).toBe("memory")
  })
})

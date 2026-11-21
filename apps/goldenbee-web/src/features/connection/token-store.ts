/**
 * Caller token persistence policy.
 *
 * Memory is the default. `session` writes to the current tab's
 * sessionStorage only. This module never reads or writes localStorage.
 */
export type TokenPersistence = "memory" | "session"

export interface TokenStoreOptions {
  sessionStorage?: Storage | null
  key?: string
}

export interface TokenWriteResult {
  persisted: boolean
}

export interface TokenStore {
  read(): string | null
  write(token: string, persistence?: TokenPersistence): TokenWriteResult
  clear(): void
  /** Where a reload would find the current token. */
  readPersistence(): TokenPersistence
}

const defaultKey = "goldenbee.caller-token"

function defaultSessionStorage(): Storage | null {
  try {
    return typeof window === "undefined" ? null : window.sessionStorage
  } catch {
    return null
  }
}

export function createTokenStore(options: TokenStoreOptions = {}): TokenStore {
  const key = options.key ?? defaultKey
  const sessionStorage =
    options.sessionStorage === undefined
      ? defaultSessionStorage()
      : options.sessionStorage
  let memoryToken: string | null = null

  function readSession(): string | null {
    try {
      return sessionStorage?.getItem(key) ?? null
    } catch {
      return null
    }
  }

  return {
    read() {
      return memoryToken ?? readSession()
    },

    write(token, persistence = "memory") {
      memoryToken = token
      if (persistence !== "session") {
        try {
          sessionStorage?.removeItem(key)
        } catch {
          // A tab that cannot clear storage still keeps the memory token.
        }
        return { persisted: false }
      }
      try {
        sessionStorage?.setItem(key, token)
        return { persisted: Boolean(sessionStorage) }
      } catch {
        return { persisted: false }
      }
    },

    clear() {
      memoryToken = null
      try {
        sessionStorage?.removeItem(key)
      } catch {
        // The in-memory copy is already gone.
      }
    },

    readPersistence() {
      return readSession() === null ? "memory" : "session"
    },
  }
}

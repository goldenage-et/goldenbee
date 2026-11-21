import { IDBFactory } from "fake-indexeddb"
import { describe, expect, it } from "vitest"

import {
  CHAT_HISTORY_SCHEMA_VERSION,
  createChatHistory,
  createChatSnapshot,
  LocalHistoryError,
  type ChatHistoryStore,
} from "@/features/chat/local-chat-history"
import {
  createInitialChatState,
  type ChatMessage,
  type ChatState,
} from "@/features/chat/chat-types"

const selection = {
  worker: "general-a",
  agent: "general",
  profile: "default",
}

function populatedState(): ChatState {
  const createdAt = 100
  const user: ChatMessage = {
    id: "user-1",
    role: "user",
    createdAt,
    parts: [{ type: "text", text: "hello", streaming: false }],
    selection,
    idempotencyKey: "key-1",
  }
  const assistant: ChatMessage = {
    id: "assistant-1",
    role: "assistant",
    createdAt: createdAt + 1,
    replyTo: "user-1",
    taskId: "exec-1",
    parts: [
      {
        type: "task",
        state: "running",
        attempt: 1,
      },
      { type: "text", text: "partial answer", streaming: true },
    ],
  }
  return {
    ...createInitialChatState("chat-session-1"),
    messages: [user, assistant],
    status: "streaming",
    activeTaskId: "exec-1",
    activeAssistantId: "assistant-1",
    // Ephemeral deltas advanced the observed sequence past the durable cursor.
    lastSeq: 7,
    lastDurableSeq: 5,
  }
}

function newHistory(name: string): ChatHistoryStore {
  return createChatHistory({
    factory: new IDBFactory(),
    databaseName: name,
  })
}

async function rawDatabase(
  factory: IDBFactory,
  name: string,
): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = factory.open(name)
    request.onupgradeneeded = () => {
      reject(new Error("database was not created by the history store"))
    }
    request.onerror = () => reject(request.error)
    request.onsuccess = () => resolve(request.result)
  })
}

async function rawRecords(
  factory: IDBFactory,
  name: string,
  storeName: string,
): Promise<unknown[]> {
  const database = await rawDatabase(factory, name)
  return new Promise((resolve, reject) => {
    const transaction = database.transaction(storeName, "readonly")
    const request = transaction.objectStore(storeName).getAll()
    request.onerror = () => reject(request.error)
    request.onsuccess = () => resolve(request.result)
  })
}

describe("local chat history", () => {
  it("restores the last chat, parts, non-secret selection, and durable cursor", async () => {
    const factory = new IDBFactory()
    const history = createChatHistory({
      factory,
      databaseName: "history-restore",
    })
    const state = populatedState()

    await history.saveChat(
      createChatSnapshot({
        chatId: "chat-1",
        state,
        selection,
        now: () => 500,
      }),
    )
    await history.close()

    const reloaded = createChatHistory({
      factory,
      databaseName: "history-restore",
    })
    const restored = await reloaded.loadLastChat()
    await reloaded.close()

    expect(restored).not.toBeNull()
    expect(restored?.chat.id).toBe("chat-1")
    expect(restored?.chat.sessionId).toBe("chat-session-1")
    expect(restored?.selection).toEqual(selection)
    expect(restored?.state.messages).toEqual(state.messages)
    // Only the durable cursor crosses a reload; ephemeral deltas never do.
    expect(restored?.state.lastDurableSeq).toBe(5)
    expect(restored?.state.lastSeq).toBe(5)
    expect(restored?.activeTask).toMatchObject({
      taskId: "exec-1",
      chatId: "chat-1",
      assistantMessageId: "assistant-1",
      lastDurableSeq: 5,
      terminal: false,
    })
  })

  it("keeps earlier chats when a new chat starts", async () => {
    const history = newHistory("history-new-chat")
    const first = createInitialChatState("chat-session-a")
    first.messages = [
      {
        id: "user-a",
        role: "user",
        createdAt: 1,
        parts: [{ type: "text", text: "first", streaming: false }],
      },
    ]

    await history.saveChat(
      createChatSnapshot({ chatId: "chat-a", state: first, now: () => 1 }),
    )
    const second = createInitialChatState("chat-session-b")
    await history.saveChat(
      createChatSnapshot({ chatId: "chat-b", state: second, now: () => 2 }),
    )

    const chats = await history.listChats()
    const restored = await history.loadLastChat()
    await history.close()

    expect(chats.map((chat) => chat.id).sort()).toEqual(["chat-a", "chat-b"])
    expect(restored?.chat.id).toBe("chat-b")
  })

  it("persists the last selection without needing a message", async () => {
    const history = newHistory("history-selection")
    await history.saveSelection({ ...selection, profile: "review" })

    const restored = await history.loadSelection()
    await history.close()

    expect(restored).toEqual({ ...selection, profile: "review" })
  })

  it("fails closed on an incompatible record and recovers through reset", async () => {
    const factory = new IDBFactory()
    const history = createChatHistory({
      factory,
      databaseName: "history-incompatible",
    })
    await history.saveChat(
      createChatSnapshot({
        chatId: "chat-1",
        state: populatedState(),
        selection,
      }),
    )
    await history.close()

    const database = await rawDatabase(factory, "history-incompatible")
    await new Promise<void>((resolve, reject) => {
      const transaction = database.transaction("chats", "readwrite")
      transaction.objectStore("chats").put({
        id: "chat-1",
        schemaVersion: CHAT_HISTORY_SCHEMA_VERSION + 1,
      })
      transaction.onerror = () => reject(transaction.error)
      transaction.oncomplete = () => resolve()
    })
    database.close()

    const reloaded = createChatHistory({
      factory,
      databaseName: "history-incompatible",
    })
    await expect(reloaded.loadLastChat()).rejects.toMatchObject({
      code: "incompatible",
      recoverable: true,
    })
    await expect(reloaded.loadLastChat()).rejects.toBeInstanceOf(
      LocalHistoryError,
    )

    await reloaded.reset()
    expect(await reloaded.loadLastChat()).toBeNull()
    await reloaded.close()
  })

  it("fails closed on an unreadable record", async () => {
    const factory = new IDBFactory()
    const history = createChatHistory({
      factory,
      databaseName: "history-corrupt",
    })
    await history.saveChat(
      createChatSnapshot({ chatId: "chat-1", state: populatedState() }),
    )
    await history.close()

    const database = await rawDatabase(factory, "history-corrupt")
    await new Promise<void>((resolve, reject) => {
      const transaction = database.transaction("messages", "readwrite")
      transaction.objectStore("messages").put({
        id: "assistant-1",
        chatId: "chat-1",
        schemaVersion: CHAT_HISTORY_SCHEMA_VERSION,
        role: "assistant",
        createdAt: 1,
        parts: "not-a-list",
      })
      transaction.onerror = () => reject(transaction.error)
      transaction.oncomplete = () => resolve()
    })
    database.close()

    const reloaded = createChatHistory({
      factory,
      databaseName: "history-corrupt",
    })
    await expect(reloaded.loadLastChat()).rejects.toMatchObject({
      code: "corrupt",
      recoverable: true,
    })
    await reloaded.close()
  })

  it("recovers from a database version mismatch through reset", async () => {
    const factory = new IDBFactory()
    await new Promise<void>((resolve, reject) => {
      const request = factory.open("history-db-version", 2)
      request.onupgradeneeded = () => undefined
      request.onerror = () => reject(request.error)
      request.onsuccess = () => {
        request.result.close()
        resolve()
      }
    })

    const history = createChatHistory({
      factory,
      databaseName: "history-db-version",
    })
    await expect(history.loadLastChat()).rejects.toMatchObject({
      code: "incompatible",
      recoverable: true,
    })

    await history.reset()
    expect(await history.loadLastChat()).toBeNull()
    await history.close()
  })

  it("never writes a caller token or provider credential", async () => {
    const factory = new IDBFactory()
    const secret = "caller-token-do-not-persist"
    const history = createChatHistory({
      factory,
      databaseName: "history-token-absence",
    })

    await history.saveChat(
      createChatSnapshot({
        chatId: "chat-1",
        state: populatedState(),
        selection,
      }),
    )
    await history.saveSelection(selection)

    const records = [
      ...(await rawRecords(factory, "history-token-absence", "chats")),
      ...(await rawRecords(factory, "history-token-absence", "messages")),
      ...(await rawRecords(factory, "history-token-absence", "tasks")),
      ...(await rawRecords(factory, "history-token-absence", "settings")),
    ]
    await history.close()

    const serialized = JSON.stringify(records)
    expect(serialized).not.toContain(secret)
    expect(serialized).not.toContain("Bearer")
    expect(serialized).not.toMatch(/"token"\s*:/)
    expect(serialized).not.toMatch(/"credential"\s*:/)
  })
})

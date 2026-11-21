import type { TaskState } from "@goldenbee/sdk"

import type {
  ChatMessage,
  ChatPart,
  ChatRecoveryNotice,
  ChatSelection,
  ChatState,
  ChatStreamGap,
} from "./chat-types"

/**
 * Browser-local chat history.
 *
 * The store owns one IndexedDB database with a forward-only schema. It persists
 * reduced chat state, task identities, durable event cursors, and non-secret
 * selections. The caller token and provider credentials are not part of any
 * record type, and the store refuses to accept them.
 */
export const CHAT_HISTORY_SCHEMA_VERSION = 1
export const CHAT_HISTORY_DB_NAME = "goldenbee-web-history"
const DATABASE_VERSION = 1
const SETTINGS_KEY = "app"

export type LocalHistoryErrorCode = "corrupt" | "incompatible" | "unavailable"

export class LocalHistoryError extends Error {
  readonly code: LocalHistoryErrorCode
  readonly recoverable = true

  constructor(code: LocalHistoryErrorCode, message: string, cause?: unknown) {
    super(message, { cause })
    this.name = "LocalHistoryError"
    this.code = code
  }
}

export interface StoredChat {
  schemaVersion: number
  id: string
  sessionId: string
  title: string
  createdAt: number
  updatedAt: number
  status: ChatState["status"]
  activeTaskId: string | null
  activeAssistantId: string | null
  gap: ChatStreamGap | null
  recovery: ChatRecoveryNotice | null
  selection: ChatSelection | null
  lastDurableSeq: number
}

export interface StoredMessage extends Omit<ChatMessage, "role"> {
  schemaVersion: number
  chatId: string
  role: ChatMessage["role"]
  /** Stable order inside one chat; IndexedDB returns key order otherwise. */
  position: number
}

export interface StoredTask {
  schemaVersion: number
  taskId: string
  chatId: string
  assistantMessageId: string
  state: TaskState
  lastDurableSeq: number
  terminal: boolean
  updatedAt: number
}

export interface ChatSettings {
  schemaVersion: number
  key: string
  lastChatId: string | null
  selection: ChatSelection | null
}

export interface ChatSnapshot {
  chat: StoredChat
  messages: StoredMessage[]
  tasks: StoredTask[]
  settings: ChatSettings
}

export interface RestoredChat {
  chat: StoredChat
  messages: StoredMessage[]
  tasks: StoredTask[]
  activeTask: StoredTask | null
  selection: ChatSelection | null
  state: ChatState
}

export interface ChatSnapshotInput {
  chatId: string
  state: ChatState
  selection?: ChatSelection | null
  title?: string
  createdAt?: number
  now?: () => number
}

export interface ChatHistoryOptions {
  factory?: IDBFactory
  databaseName?: string
}

export interface ChatHistoryStore {
  saveChat(snapshot: ChatSnapshot): Promise<void>
  saveSelection(selection: ChatSelection | null): Promise<void>
  loadLastChat(): Promise<RestoredChat | null>
  loadChat(chatId: string): Promise<RestoredChat | null>
  loadSelection(): Promise<ChatSelection | null>
  listChats(): Promise<StoredChat[]>
  reset(): Promise<void>
  close(): void
}

const terminalStates = new Set<TaskState>([
  "succeeded",
  "failed",
  "cancelled",
  "unknown",
])

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function isString(value: unknown): value is string {
  return typeof value === "string"
}

function isNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value)
}

function isNullableString(value: unknown): value is string | null {
  return value === null || isString(value)
}

function isSelection(value: unknown): value is ChatSelection {
  if (!isRecord(value)) {
    return false
  }
  return (
    isString(value.worker) &&
    isString(value.agent) &&
    (value.profile === undefined || isString(value.profile))
  )
}

function isGap(value: unknown): value is ChatStreamGap | null {
  if (value === null) {
    return true
  }
  return (
    isRecord(value) && isString(value.reason) && isNumber(value.fromSeq)
  )
}

function isRecovery(value: unknown): value is ChatRecoveryNotice | null {
  if (value === null) {
    return true
  }
  return (
    isRecord(value) &&
    isString(value.kind) &&
    isString(value.taskId) &&
    (value.detail === undefined || isString(value.detail))
  )
}

function isPart(value: unknown): value is ChatPart {
  return isRecord(value) && isString(value.type)
}

function titleFor(state: ChatState, fallback: string): string {
  const first = state.messages.find((message) => message.role === "user")
  const text = first?.parts
    .filter((part) => part.type === "text")
    .map((part) => part.text)
    .join(" ")
    .trim()
  if (!text) {
    return fallback
  }
  return text.length > 80 ? `${text.slice(0, 77)}...` : text
}

function taskParts(message: ChatMessage) {
  return message.parts.find((part) => part.type === "task")
}

/** Build the one consistent snapshot that a reload will read. */
export function createChatSnapshot(input: ChatSnapshotInput): ChatSnapshot {
  const now = input.now ?? (() => Date.now())
  const updatedAt = now()
  const createdAt = input.createdAt ?? updatedAt
  const selection = input.selection ?? null
  const chat: StoredChat = {
    schemaVersion: CHAT_HISTORY_SCHEMA_VERSION,
    id: input.chatId,
    sessionId: input.state.sessionId,
    title: input.title ?? titleFor(input.state, "New chat"),
    createdAt,
    updatedAt,
    status: input.state.status,
    activeTaskId: input.state.activeTaskId,
    activeAssistantId: input.state.activeAssistantId,
    gap: input.state.gap,
    recovery: input.state.recovery,
    selection,
    lastDurableSeq: input.state.lastDurableSeq,
  }

  const messages: StoredMessage[] = input.state.messages.map((message, position) => ({
    ...message,
    schemaVersion: CHAT_HISTORY_SCHEMA_VERSION,
    chatId: input.chatId,
    position,
  }))

  const tasks: StoredTask[] = []
  for (const message of input.state.messages) {
    if (!message.taskId) {
      continue
    }
    const task = taskParts(message)
    if (!task) {
      continue
    }
    const active = message.id === input.state.activeAssistantId
    tasks.push({
      schemaVersion: CHAT_HISTORY_SCHEMA_VERSION,
      taskId: message.taskId,
      chatId: input.chatId,
      assistantMessageId: message.id,
      state: task.state,
      lastDurableSeq: active ? input.state.lastDurableSeq : 0,
      terminal: terminalStates.has(task.state),
      updatedAt,
    })
  }

  return {
    chat,
    messages,
    tasks,
    settings: {
      schemaVersion: CHAT_HISTORY_SCHEMA_VERSION,
      key: SETTINGS_KEY,
      lastChatId: input.chatId,
      selection,
    },
  }
}

/** Rebuild chat state without ever restoring an ephemeral delta cursor. */
export function stateFromStored(
  chat: StoredChat,
  messages: StoredMessage[],
): ChatState {
  return {
    sessionId: chat.sessionId,
    messages: [...messages]
      .sort((left, right) => left.position - right.position)
      .map(
        ({
          schemaVersion: _version,
          chatId: _chat,
          position: _position,
          ...message
        }) => {
          void _version
          void _chat
          void _position
          return message
        },
      ),
    status: chat.status,
    activeTaskId: chat.activeTaskId,
    activeAssistantId: chat.activeAssistantId,
    lastSeq: chat.lastDurableSeq,
    lastDurableSeq: chat.lastDurableSeq,
    gap: chat.gap,
    recovery: chat.recovery,
  }
}

function assertChat(value: unknown): asserts value is StoredChat {
  if (!isRecord(value)) {
    throw new LocalHistoryError("corrupt", "A stored chat is not an object.")
  }
  if (value.schemaVersion !== CHAT_HISTORY_SCHEMA_VERSION) {
    throw new LocalHistoryError(
      "incompatible",
      "The stored chat schema is not supported by this Client version.",
    )
  }
  if (
    !isString(value.id) ||
    !isString(value.sessionId) ||
    !isString(value.title) ||
    !isNumber(value.createdAt) ||
    !isNumber(value.updatedAt) ||
    !isString(value.status) ||
    !isNullableString(value.activeTaskId) ||
    !isNullableString(value.activeAssistantId) ||
    !isGap(value.gap) ||
    !isRecovery(value.recovery) ||
    (value.selection !== null && !isSelection(value.selection)) ||
    !isNumber(value.lastDurableSeq)
  ) {
    throw new LocalHistoryError("corrupt", "A stored chat is missing required fields.")
  }
}

function assertMessage(
  value: unknown,
  chatId: string,
): asserts value is StoredMessage {
  if (!isRecord(value)) {
    throw new LocalHistoryError("corrupt", "A stored message is not an object.")
  }
  if (value.schemaVersion !== CHAT_HISTORY_SCHEMA_VERSION) {
    throw new LocalHistoryError(
      "incompatible",
      "The stored message schema is not supported by this Client version.",
    )
  }
  if (
    !isString(value.id) ||
    value.chatId !== chatId ||
    (value.role !== "user" && value.role !== "assistant") ||
    !isNumber(value.createdAt) ||
    !Array.isArray(value.parts) ||
    !value.parts.every(isPart) ||
    !isNumber(value.position)
  ) {
    throw new LocalHistoryError("corrupt", "A stored message is malformed.")
  }
}

function assertTask(value: unknown): asserts value is StoredTask {
  if (!isRecord(value)) {
    throw new LocalHistoryError("corrupt", "A stored task is not an object.")
  }
  if (value.schemaVersion !== CHAT_HISTORY_SCHEMA_VERSION) {
    throw new LocalHistoryError(
      "incompatible",
      "The stored task schema is not supported by this Client version.",
    )
  }
  if (
    !isString(value.taskId) ||
    !isString(value.chatId) ||
    !isString(value.assistantMessageId) ||
    !isString(value.state) ||
    !isNumber(value.lastDurableSeq) ||
    typeof value.terminal !== "boolean" ||
    !isNumber(value.updatedAt)
  ) {
    throw new LocalHistoryError("corrupt", "A stored task is malformed.")
  }
}

function assertSettings(value: unknown): asserts value is ChatSettings {
  if (!isRecord(value)) {
    throw new LocalHistoryError("corrupt", "Stored settings are not an object.")
  }
  if (value.schemaVersion !== CHAT_HISTORY_SCHEMA_VERSION) {
    throw new LocalHistoryError(
      "incompatible",
      "The stored settings schema is not supported by this Client version.",
    )
  }
  if (
    value.key !== SETTINGS_KEY ||
    !isNullableString(value.lastChatId) ||
    (value.selection !== null && !isSelection(value.selection))
  ) {
    throw new LocalHistoryError("corrupt", "Stored settings are malformed.")
  }
}

function requestResult<T>(request: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result)
    request.onerror = () => reject(request.error)
  })
}

function transactionDone(transaction: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    transaction.oncomplete = () => resolve()
    transaction.onerror = () => reject(transaction.error)
    transaction.onabort = () => reject(transaction.error)
  })
}

function upgrade(database: IDBDatabase): void {
  if (!database.objectStoreNames.contains("chats")) {
    database.createObjectStore("chats", { keyPath: "id" })
  }
  if (!database.objectStoreNames.contains("messages")) {
    const messages = database.createObjectStore("messages", { keyPath: "id" })
    messages.createIndex("chatId", "chatId", { unique: false })
  }
  if (!database.objectStoreNames.contains("tasks")) {
    const tasks = database.createObjectStore("tasks", { keyPath: "taskId" })
    tasks.createIndex("chatId", "chatId", { unique: false })
  }
  if (!database.objectStoreNames.contains("settings")) {
    database.createObjectStore("settings", { keyPath: "key" })
  }
}

function defaultFactory(): IDBFactory {
  const factory = globalThis.indexedDB
  if (!factory) {
    throw new LocalHistoryError(
      "unavailable",
      "IndexedDB is not available in this browser, so chat history cannot be restored.",
    )
  }
  return factory
}

export function createChatHistory(
  options: ChatHistoryOptions = {},
): ChatHistoryStore {
  const factory = options.factory ?? defaultFactory()
  const databaseName = options.databaseName ?? CHAT_HISTORY_DB_NAME
  let databasePromise: Promise<IDBDatabase> | null = null

  function open(): Promise<IDBDatabase> {
    if (databasePromise) {
      return databasePromise
    }
    databasePromise = new Promise<IDBDatabase>((resolve, reject) => {
      let request: IDBOpenDBRequest
      try {
        request = factory.open(databaseName, DATABASE_VERSION)
      } catch (cause) {
        reject(
          new LocalHistoryError(
            "incompatible",
            "The local chat database could not be opened.",
            cause,
          ),
        )
        return
      }
      request.onupgradeneeded = () => upgrade(request.result)
      request.onblocked = () => {
        reject(
          new LocalHistoryError(
            "unavailable",
            "Another tab is blocking the local chat database upgrade.",
          ),
        )
      }
      request.onerror = () => {
        reject(
          new LocalHistoryError(
            isVersionError(request.error) ? "incompatible" : "unavailable",
            "The local chat database could not be opened.",
            request.error,
          ),
        )
      }
      request.onsuccess = () => {
        const database = request.result
        database.onversionchange = () => database.close()
        resolve(database)
      }
    }).catch((cause) => {
      databasePromise = null
      throw cause
    })
    return databasePromise
  }

  function isVersionError(error: DOMException | null): boolean {
    return error?.name === "VersionError"
  }

  function deleteDatabase(): Promise<void> {
    return new Promise((resolve, reject) => {
      let request: IDBOpenDBRequest
      try {
        request = factory.deleteDatabase(databaseName)
      } catch (cause) {
        reject(
          new LocalHistoryError(
            "unavailable",
            "The local chat database could not be removed.",
            cause,
          ),
        )
        return
      }
      request.onerror = () =>
        reject(
          new LocalHistoryError(
            "unavailable",
            "The local chat database could not be removed.",
            request.error,
          ),
        )
      request.onblocked = () =>
        reject(
          new LocalHistoryError(
            "unavailable",
            "Another tab is blocking the local chat database reset.",
          ),
        )
      request.onsuccess = () => resolve()
    })
  }

  async function readSettings(): Promise<ChatSettings | null> {
    const database = await open()
    const transaction = database.transaction("settings", "readonly")
    const value = await requestResult(
      transaction.objectStore("settings").get(SETTINGS_KEY),
    )
    await transactionDone(transaction)
    if (value === undefined) {
      return null
    }
    assertSettings(value)
    return value
  }

  async function loadChat(chatId: string): Promise<RestoredChat | null> {
    const database = await open()
    const transaction = database.transaction(
      ["chats", "messages", "tasks"],
      "readonly",
    )
    const chatRequest = requestResult(
      transaction.objectStore("chats").get(chatId),
    )
    const messageRequest = requestResult(
      transaction.objectStore("messages").index("chatId").getAll(chatId),
    )
    const taskRequest = requestResult(
      transaction.objectStore("tasks").index("chatId").getAll(chatId),
    )
    const [chatValue, messageValues, taskValues] = await Promise.all([
      chatRequest,
      messageRequest,
      taskRequest,
    ])
    await transactionDone(transaction)

    if (chatValue === undefined) {
      return null
    }
    assertChat(chatValue)
    const messages: StoredMessage[] = []
    for (const value of messageValues as unknown[]) {
      assertMessage(value, chatId)
      messages.push(value)
    }
    const tasks: StoredTask[] = []
    for (const value of taskValues as unknown[]) {
      assertTask(value)
      if (value.chatId !== chatId) {
        throw new LocalHistoryError(
          "corrupt",
          "A stored task points at a different chat.",
        )
      }
      tasks.push(value)
    }

    const activeTask =
      tasks.find(
        (task) =>
          task.taskId === chatValue.activeTaskId &&
          task.assistantMessageId === chatValue.activeAssistantId,
      ) ?? null
    return {
      chat: chatValue,
      messages,
      tasks,
      activeTask,
      selection: chatValue.selection,
      state: stateFromStored(chatValue, messages),
    }
  }

  return {
    async saveChat(snapshot) {
      const database = await open()
      const transaction = database.transaction(
        ["chats", "messages", "tasks", "settings"],
        "readwrite",
      )
      transaction.objectStore("chats").put(snapshot.chat)
      for (const message of snapshot.messages) {
        transaction.objectStore("messages").put(message)
      }
      for (const task of snapshot.tasks) {
        transaction.objectStore("tasks").put(task)
      }
      transaction.objectStore("settings").put(snapshot.settings)
      await transactionDone(transaction)
    },

    async saveSelection(selection) {
      const settings = (await readSettings()) ?? {
        schemaVersion: CHAT_HISTORY_SCHEMA_VERSION,
        key: SETTINGS_KEY,
        lastChatId: null,
        selection: null,
      }
      const database = await open()
      const transaction = database.transaction("settings", "readwrite")
      transaction.objectStore("settings").put({
        ...settings,
        schemaVersion: CHAT_HISTORY_SCHEMA_VERSION,
        key: SETTINGS_KEY,
        selection,
      })
      await transactionDone(transaction)
    },

    async loadLastChat() {
      const settings = await readSettings()
      if (!settings?.lastChatId) {
        return null
      }
      return loadChat(settings.lastChatId)
    },

    loadChat,

    async loadSelection() {
      return (await readSettings())?.selection ?? null
    },

    async listChats() {
      const database = await open()
      const transaction = database.transaction("chats", "readonly")
      const values = await requestResult(
        transaction.objectStore("chats").getAll(),
      )
      await transactionDone(transaction)
      const chats: StoredChat[] = []
      for (const value of values as unknown[]) {
        assertChat(value)
        chats.push(value)
      }
      return chats.sort((left, right) => right.updatedAt - left.updatedAt)
    },

    async reset() {
      let database: IDBDatabase
      try {
        database = await open()
      } catch (cause) {
        if (cause instanceof LocalHistoryError && cause.code === "incompatible") {
          databasePromise = null
          await deleteDatabase()
          return
        }
        throw cause
      }
      const transaction = database.transaction(
        ["chats", "messages", "tasks", "settings"],
        "readwrite",
      )
      transaction.objectStore("chats").clear()
      transaction.objectStore("messages").clear()
      transaction.objectStore("tasks").clear()
      transaction.objectStore("settings").clear()
      await transactionDone(transaction)
    },

    close() {
      void databasePromise?.then(
        (database) => database.close(),
        () => undefined,
      )
      databasePromise = null
    },
  }
}

/** A production store, or null when this environment has no IndexedDB. */
export function browserChatHistory(): ChatHistoryStore | null {
  return typeof globalThis.indexedDB === "undefined"
    ? null
    : createChatHistory()
}

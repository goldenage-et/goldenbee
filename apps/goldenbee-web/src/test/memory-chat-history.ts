import {
  createChatSnapshot,
  stateFromStored,
  type ChatHistoryStore,
  type ChatSnapshot,
  type RestoredChat,
  type StoredChat,
} from "@/features/chat/local-chat-history"
import type { ChatState } from "@/features/chat/chat-types"

/** In-memory ChatHistoryStore for shell and chat tests. */
export class MemoryChatHistory implements ChatHistoryStore {
  readonly loadChatCalls: string[] = []
  loadLastCalls = 0
  private readonly chats = new Map<string, RestoredChat>()
  private lastChatId: string | null = null

  seed(chatId: string, title: string, state: ChatState): void {
    this.write(
      createChatSnapshot({
        chatId,
        title,
        state,
      }),
    )
  }

  async saveChat(snapshot: ChatSnapshot): Promise<void> {
    this.write(snapshot)
  }

  async saveSelection(): Promise<void> {
    // The workspace tests do not depend on a separate selection record.
  }

  async loadLastChat(): Promise<RestoredChat | null> {
    this.loadLastCalls += 1
    return this.lastChatId ? this.loadChat(this.lastChatId) : null
  }

  async loadChat(chatId: string): Promise<RestoredChat | null> {
    this.loadChatCalls.push(chatId)
    return this.chats.get(chatId) ?? null
  }

  async loadSelection(): Promise<null> {
    return null
  }

  async listChats(): Promise<StoredChat[]> {
    return [...this.chats.values()]
      .map((entry) => entry.chat)
      .sort((left, right) => right.updatedAt - left.updatedAt)
  }

  async reset(): Promise<void> {
    this.chats.clear()
    this.lastChatId = null
  }

  close(): void {
    // No resource to release.
  }

  private write(snapshot: ChatSnapshot): void {
    const messages = [...snapshot.messages]
    this.chats.set(snapshot.chat.id, {
      chat: snapshot.chat,
      messages,
      tasks: snapshot.tasks,
      activeTask: null,
      selection: snapshot.settings.selection,
      state: stateFromStored(snapshot.chat, messages),
    })
    this.lastChatId = snapshot.chat.id
  }
}

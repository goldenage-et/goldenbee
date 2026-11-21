import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { describe, expect, it, vi } from "vitest"

import { createInitialChatState, type ChatState } from "@/features/chat/chat-types"
import { TaskBrowserDialog } from "@/features/inspector/task-browser-dialog"
import {
  type TaskDetailGateway,
} from "@/features/tasks/task-detail-page"
import { MemoryChatHistory } from "@/test/memory-chat-history"
import type { GatewayTask, StreamPage } from "@goldenbee/sdk"

const task: GatewayTask = {
  task_id: "task-1",
  worker: "general-a",
  worker_instance: "general-a",
  connectivity: "online",
  task: {
    id: "task-1",
    namespace: "default",
    caller: "caller-a",
    agent: "general",
    session_id: "session-1",
    state: "succeeded",
    profile: "default",
    attempt: 1,
    output: "Task complete",
    usage: {
      prompt_tokens: 10,
      candidate_tokens: 4,
      model_calls: 1,
      tool_calls: 0,
      estimated_cost_micros: 0,
    },
    created_at: 1_000,
    updated_at: 2_000,
  },
}

const client: TaskDetailGateway = {
  task: async () => task,
  events: async () =>
    ({
      events: [],
      stream_id: "task:task-1",
      next_seq: 0,
      oldest_seq: 0,
      cursor_expired: false,
    }) as StreamPage,
  approvals: async () => [],
}

function stateWithTask(): ChatState {
  return {
    ...createInitialChatState("session-1"),
    status: "succeeded",
    activeTaskId: "task-1",
    activeAssistantId: "assistant-1",
    messages: [
      {
        id: "assistant-1",
        role: "assistant",
        createdAt: 2_000,
        taskId: "task-1",
        parts: [
          { type: "task", state: "succeeded", attempt: 1 },
          { type: "text", text: "Task complete", streaming: false },
        ],
      },
    ],
  }
}

function renderDialog(history: MemoryChatHistory, open = true) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={queryClient}>
      <TaskBrowserDialog
        history={history}
        client={client}
        chatId="chat-1"
        open={open}
        onClose={vi.fn()}
      />
    </QueryClientProvider>,
  )
}

describe("task browser", () => {
  it("shows observed tasks in a list and full detail in the right pane", async () => {
    const history = new MemoryChatHistory()
    history.seed("chat-1", "Test chat", stateWithTask())
    const user = userEvent.setup()
    renderDialog(history)

    const dialog = await screen.findByRole("dialog", { name: "Task records" })
    expect(await screen.findByRole("button", { name: "Task 1, succeeded" }))
      .toBeInTheDocument()
    expect(screen.queryByText("task-1")).not.toBeInTheDocument()

    await user.click(
      screen.getByRole("button", { name: "Task 1, succeeded" }),
    )

    expect(await screen.findByText("Task state")).toBeInTheDocument()
    expect(screen.getAllByText("succeeded").length).toBeGreaterThan(0)
    expect(screen.getByText("Task complete")).toBeInTheDocument()
    expect(dialog).toBeInTheDocument()
  })

  it("shows an honest empty state before the session runs a task", async () => {
    const history = new MemoryChatHistory()
    history.seed("chat-1", "Test chat", createInitialChatState("session-1"))
    renderDialog(history)

    expect(
      await screen.findByText(/No task records yet/i),
    ).toBeInTheDocument()
  })
})

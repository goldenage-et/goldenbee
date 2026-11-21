import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { render, screen, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { describe, expect, it } from "vitest"
import type { ReactNode } from "react"
import { GatewayError, type Artifact } from "@goldenbee/sdk"

import {
  createChatInspectionStore,
  type ChatInspectionStore,
} from "@/features/chat/chat-inspection-store"
import { createInitialChatState } from "@/features/chat/chat-types"
import { MemoryChatHistory } from "@/test/memory-chat-history"
import {
  SessionInspector,
  type ArtifactGateway,
} from "@/features/inspector/session-inspector"

const taskState = {
  ...createInitialChatState("session-inspect"),
  messages: [
    {
      id: "assistant-1",
      role: "assistant" as const,
      createdAt: 2,
      taskId: "exec-ticket",
      parts: [
        { type: "task" as const, state: "succeeded" as const, attempt: 1 },
        { type: "text" as const, text: "done", streaming: false },
      ],
    },
  ],
  status: "succeeded" as const,
  activeTaskId: "exec-ticket",
  activeAssistantId: "assistant-1",
  lastDurableSeq: 9,
}

function seededHistory(): MemoryChatHistory {
  const history = new MemoryChatHistory()
  history.seed("chat-inspect", "Alpha task", taskState)
  return history
}

function inspectionStore(): ChatInspectionStore {
  const store = createChatInspectionStore()
  store.getState().publish({
    chatId: "chat-inspect",
    sessionId: "session-inspect",
    title: "Alpha task",
    status: "succeeded",
    taskId: "exec-ticket",
    activeAssistantId: "assistant-1",
    lastDurableSeq: 9,
    selection: {
      worker: "general-a",
      agent: "general",
      profile: "default",
    },
    usage: {
      prompt_tokens: 219,
      candidate_tokens: 11,
      model_calls: 1,
      tool_calls: 0,
    },
    gap: null,
    recovery: null,
  })
  return store
}

function artifactsClient(
  result: Artifact[] | Error,
): ArtifactGateway & { calls: string[] } {
  const calls: string[] = []
  return {
    calls,
    async artifacts(taskId) {
      calls.push(taskId)
      if (result instanceof Error) {
        throw result
      }
      return result
    },
  }
}

function renderInspector(
  options: {
    history?: MemoryChatHistory
    inspections?: ChatInspectionStore
    client?: ArtifactGateway | null
  } = {},
) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  function wrapper({ children }: { children: ReactNode }) {
    return (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    )
  }
  return render(
    <SessionInspector
      history={options.history ?? seededHistory()}
      inspections={options.inspections ?? inspectionStore()}
      artifactsClient={
        options.client === undefined
          ? artifactsClient([])
          : options.client
      }
      chatId="chat-inspect"
      title="Alpha task"
      open
      onClose={() => undefined}
    />,
    { wrapper },
  )
}

describe("focused session inspector", () => {
  it("shows Artifacts and Details without a Files tree", () => {
    renderInspector()

    expect(screen.getByRole("tab", { name: "Artifacts" })).toBeInTheDocument()
    expect(screen.getByRole("tab", { name: "Details" })).toHaveAttribute(
      "aria-selected",
      "true",
    )
    expect(
      screen.queryByRole("tab", { name: "Tasks" }),
    ).not.toBeInTheDocument()
    expect(
      screen.queryByRole("tab", { name: "Files" }),
    ).not.toBeInTheDocument()
    expect(screen.queryByText(/workspace root/i)).not.toBeInTheDocument()
  })

  it("lists artifacts with type, size, digest, and finalization", async () => {
    const client = artifactsClient([
      {
        id: "art-1",
        task_id: "exec-ticket",
        native_invocation_id: "inv-1",
        attempt: 1,
        version: 1,
        namespace: "local",
        owner: "general",
        name: "report.md",
        content_type: "text/markdown",
        size: 2048,
        digest: `sha256:${"a".repeat(64)}`,
        finalized: true,
        created_at: 1,
      },
    ])
    const user = userEvent.setup()
    renderInspector({ client })

    await user.click(screen.getByRole("tab", { name: "Artifacts" }))

    const panel = await screen.findByRole("tabpanel", { name: "Artifacts" })
    expect(await within(panel).findByText("report.md")).toBeInTheDocument()
    expect(within(panel).getByText(/text\/markdown · 2.0 KiB/)).toBeInTheDocument()
    expect(within(panel).getByText(`sha256:${"a".repeat(64)}`)).toBeInTheDocument()
    expect(within(panel).getByText("Finalized")).toBeInTheDocument()
    expect(client.calls).toEqual(["exec-ticket"])
  })

  it("shows an honest empty state when the task has no artifacts", async () => {
    const user = userEvent.setup()
    renderInspector({ client: artifactsClient([]) })

    await user.click(screen.getByRole("tab", { name: "Artifacts" }))

    expect(
      await screen.findByText("No artifacts reported for this task."),
    ).toBeInTheDocument()
  })

  it("surfaces a typed scope denial for artifacts", async () => {
    const user = userEvent.setup()
    renderInspector({
      client: artifactsClient(
        new GatewayError({
          kind: "forbidden",
          status: 403,
          code: "scope.artifact.read",
          message: "The caller token lacks the artifact:read scope.",
          retryable: false,
        }),
      ),
    })

    await user.click(screen.getByRole("tab", { name: "Artifacts" }))

    expect(
      await screen.findByText(
        "The caller token cannot read artifacts for this task.",
      ),
    ).toBeInTheDocument()
    expect(
      screen.getByText(/artifact:read scope/),
    ).toBeInTheDocument()
  })

  it("shows runtime facts and usage details", async () => {
    const user = userEvent.setup()
    renderInspector()

    await user.click(screen.getByRole("tab", { name: "Details" }))

    const panel = screen.getByRole("tabpanel", { name: "Details" })
    expect(within(panel).getByText("general-a")).toBeInTheDocument()
    expect(within(panel).getByText("general")).toBeInTheDocument()
    expect(within(panel).getByText("default")).toBeInTheDocument()
    expect(within(panel).getByText("chat-inspect")).toBeInTheDocument()
    expect(
      within(panel).getByText(/continues when this panel is hidden/),
    ).toBeInTheDocument()
    expect(within(panel).getByText("Prompt tokens")).toBeInTheDocument()
    expect(within(panel).getByText("219")).toBeInTheDocument()
    expect(within(panel).getByText("Candidate tokens")).toBeInTheDocument()
    expect(within(panel).getByText("11")).toBeInTheDocument()
  })
})

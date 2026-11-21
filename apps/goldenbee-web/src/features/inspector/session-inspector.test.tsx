import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { render, screen, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { describe, expect, it, vi } from "vitest"
import type { ReactNode } from "react"
import {
  GatewayError,
  type Artifact,
  type StreamPage,
  type WorkspaceEntry,
} from "@goldenbee/sdk"

import {
  createChatInspectionStore,
  type ChatInspectionStore,
} from "@/features/chat/chat-inspection-store"
import { createInitialChatState } from "@/features/chat/chat-types"
import { MemoryChatHistory } from "@/test/memory-chat-history"
import {
  SessionInspector,
  type ArtifactGateway,
  type WorkspaceFileGateway,
} from "@/features/inspector/session-inspector"
import type { EventsGateway } from "@/features/inspector/workspace-tree"

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

function workspaceClient(
  entries: WorkspaceEntry[],
): WorkspaceFileGateway & { treeCalls: string[] } {
  const treeCalls: string[] = []
  return {
    treeCalls,
    async workspaceTree(_worker, path = "") {
      treeCalls.push(path)
      return {
        path,
        entries,
        truncated: false,
      }
    },
  }
}

function eventsClient(pages: StreamPage[] = []): EventsGateway {
  return {
    async events(_taskId, after) {
      return (
        pages.find((page) => page.next_seq > after) ?? {
          stream_id: "task:exec-ticket",
          events: [],
          next_seq: after,
          oldest_seq: 0,
          cursor_expired: false,
        }
      )
    },
  }
}

function renderInspector(
  options: {
    history?: MemoryChatHistory
    inspections?: ChatInspectionStore
    client?: ArtifactGateway | null
    workspace?: WorkspaceFileGateway | null
    events?: EventsGateway | null
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
  const openFile = vi.fn()
  const rendered = render(
    <SessionInspector
      history={options.history ?? seededHistory()}
      inspections={options.inspections ?? inspectionStore()}
      artifactsClient={
        options.client === undefined
          ? artifactsClient([])
          : options.client
      }
      workspaceClient={
        options.workspace === undefined ? null : options.workspace
      }
      eventsClient={options.events === undefined ? null : options.events}
      chatId="chat-inspect"
      title="Alpha task"
      open
      liveVersion={0}
      liveStatus="live"
      onOpenFile={openFile}
      onClose={() => undefined}
    />,
    { wrapper },
  )
  return { ...rendered, openFile }
}

describe("focused session inspector", () => {
  it("shows Files, Artifacts, and Details", () => {
    renderInspector()

    expect(screen.getByRole("tab", { name: "Files" })).toHaveAttribute(
      "aria-selected",
      "true",
    )
    expect(screen.getByRole("tab", { name: "Artifacts" })).toBeInTheDocument()
    expect(screen.getByRole("tab", { name: "Details" })).toBeInTheDocument()
    expect(
      screen.queryByRole("tab", { name: "Tasks" }),
    ).not.toBeInTheDocument()
  })

  it("opens a workspace file preview from the tree", async () => {
    const user = userEvent.setup()
    const workspace = workspaceClient([
      {
        name: "index.html",
        path: "index.html",
        kind: "file",
        size: 12,
        modified_at: 1,
        hidden: false,
      },
    ])
    const { openFile } = renderInspector({
      workspace,
      events: eventsClient(),
    })

    expect(await screen.findByText("index.html")).toBeInTheDocument()
    await user.click(screen.getByRole("button", { name: /index\.html/ }))

    expect(openFile).toHaveBeenCalledWith("index.html")
    expect(workspace.treeCalls).toEqual([""])
  })

  it("shows a disabled workspace without blocking Artifacts", async () => {
    const user = userEvent.setup()
    const workspace = workspaceClient([])
    workspace.workspaceTree = async () => {
      throw new GatewayError({
        kind: "forbidden",
        status: 403,
        code: "workspace.disabled",
        message: "Workspace browsing is disabled for this Worker.",
        retryable: false,
      })
    }
    renderInspector({ workspace, events: eventsClient() })

    expect(
      await screen.findByText("Workspace browsing is disabled for this Worker."),
    ).toBeInTheDocument()

    await user.click(screen.getByRole("tab", { name: "Artifacts" }))
    expect(screen.getByRole("tab", { name: "Artifacts" })).toHaveAttribute(
      "aria-selected",
      "true",
    )
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

import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { describe, expect, it, vi } from "vitest"

import {
  TaskDetailPage,
  type TaskDetailGateway,
} from "@/features/tasks/task-detail-page"
import type {
  GatewayApproval,
  GatewayTask,
  StreamEvent,
  StreamPage,
} from "@goldenbee/sdk"

const task: GatewayTask = {
  task_id: "exec-1",
  worker: "general-a",
  worker_instance: "general-a",
  connectivity: "online",
  task: {
    id: "exec-1",
    namespace: "default",
    caller: "caller-a",
    agent: "general",
    session_id: "session-1",
    state: "failed",
    profile: "default",
    attempt: 2,
    output: "partial answer",
    error: {
      code: "provider.timeout",
      message: "The provider timed out.",
      retryable: true,
      recovery: "Retry the task.",
    },
    usage: {
      prompt_tokens: 120,
      candidate_tokens: 80,
      model_calls: 2,
      tool_calls: 1,
      estimated_cost_micros: 750,
    },
    evidence: {
      operation: "http.post",
      target: "https://api.example.test/orders/42",
      receipt: "receipt-42",
      nested: { hidden: true },
    },
    created_at: 1_000,
    updated_at: 2_000,
  },
}

const requestedEvent: StreamEvent = {
  event_id: "evt-1",
  source: "worker",
  incarnation: "inc-1",
  stream_id: "task:exec-1",
  seq: 1,
  schema_version: 1,
  kind: "approval.requested",
  durable: true,
  payload: {
    approval_id: "approval-1",
    operation: "http.post",
    target: "https://api.example.test/orders/42",
    digest: "sha256:abc123",
    risk: "external_write",
    requested_by: "caller-a",
    expires_at: 5_000,
  },
  at: 1_100,
}

const failedEvent: StreamEvent = {
  event_id: "evt-2",
  source: "worker",
  incarnation: "inc-1",
  stream_id: "task:exec-1",
  seq: 2,
  schema_version: 1,
  kind: "task.failed",
  durable: true,
  payload: {
    state: "failed",
    uncertainty: "provider stream closed",
  },
  at: 1_200,
}

const approval: GatewayApproval = {
  task_id: "exec-1",
  worker: "general-a",
  approval: {
    id: "approval-1",
    task_id: "exec-1",
    namespace: "default",
    operation: "http.post",
    target: "https://api.example.test/orders/42",
    digest: "sha256:abc123",
    risk: "external_write",
    requested_by: "caller-a",
    expires_at: 5_000,
    status: "approved",
    decided_by: "caller-b",
    decided_at: 1_300,
    receipt: "receipt-42",
    created_at: 1_100,
  },
}

function page(events: StreamEvent[]): StreamPage {
  return {
    stream_id: "task:exec-1",
    events,
    next_seq: events.reduce((highest, event) => Math.max(highest, event.seq), 0),
    oldest_seq: 1,
    cursor_expired: false,
  }
}

function detailGateway(options: {
  events?: StreamEvent[]
  approvals?: GatewayApproval[]
} = {}): TaskDetailGateway & {
  eventsCall: ReturnType<typeof vi.fn>
  approvalsCall: ReturnType<typeof vi.fn>
} {
  const eventsCall = vi.fn(async () => page(options.events ?? [requestedEvent, failedEvent]))
  const approvalsCall = vi.fn(async () => options.approvals ?? [approval])
  return {
    task: async () => task,
    events: eventsCall,
    approvals: approvalsCall,
    eventsCall,
    approvalsCall,
  }
}

function renderPage(client: TaskDetailGateway) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const result = render(
    <QueryClientProvider client={queryClient}>
      <TaskDetailPage client={client} taskId="exec-1" />
    </QueryClientProvider>,
  )
  return { ...result, queryClient }
}

describe("task detail page", () => {
  it("renders the canonical projection, usage, error, evidence, events, and approvals", async () => {
    const client = detailGateway()
    renderPage(client)

    expect(
      await screen.findByRole("heading", { name: "exec-1" }),
    ).toBeInTheDocument()
    expect(await screen.findAllByText("failed")).not.toHaveLength(0)
    expect(screen.getByText("The provider timed out.")).toBeInTheDocument()
    expect(screen.getByText("provider.timeout")).toBeInTheDocument()
    expect(screen.getByText("120")).toBeInTheDocument()
    expect(screen.getByText("750")).toBeInTheDocument()
    expect(screen.getAllByText("receipt-42").length).toBeGreaterThan(0)
    expect(screen.getAllByText("http.post").length).toBeGreaterThan(0)
    expect(screen.getAllByText("sha256:abc123").length).toBeGreaterThan(0)
    expect(screen.getByText(/#1 approval\.requested/)).toBeInTheDocument()
    expect(screen.getByText(/#2 task\.failed/)).toBeInTheDocument()
    expect(screen.getByText("caller-b")).toBeInTheDocument()

    await waitFor(() =>
      expect(client.eventsCall).toHaveBeenCalledWith("exec-1", 0, 500),
    )
    expect(client.approvalsCall).toHaveBeenCalledWith("exec-1")
  })

  it("keeps retained history and approvals readable and refreshable without a live stream", async () => {
    const user = userEvent.setup()
    const client = detailGateway({ events: [requestedEvent] })
    const { rerender, queryClient } = renderPage(client)

    expect(
      await screen.findByText(/#1 approval\.requested/),
    ).toBeInTheDocument()
    expect(
      screen.getByText(/does not depend on the live chat stream/),
    ).toBeInTheDocument()

    client.events = vi.fn(async () => page([requestedEvent, failedEvent]))
    rerender(
      <QueryClientProvider client={queryClient}>
        <TaskDetailPage client={client} taskId="exec-1" />
      </QueryClientProvider>,
    )
    await user.click(screen.getByRole("button", { name: "Refresh" }))

    expect(
      await screen.findByText(/#2 task\.failed/),
    ).toBeInTheDocument()
    expect(screen.getAllByText("sha256:abc123").length).toBeGreaterThan(0)
  })

  it("does not render credential fields or payload HTML as markup", async () => {
    const injected = "<img src=x onerror=alert(1)>"
    const base = detailGateway({
      events: [
        {
          ...requestedEvent,
          payload: {
            ...(requestedEvent.payload as Record<string, unknown>),
            note: injected,
            token: "super-secret-token",
          },
        },
      ],
      approvals: [
        {
          ...approval,
          approval: { ...approval.approval, receipt: "redacted-receipt" },
        },
      ],
    })
    const client: TaskDetailGateway = {
      ...base,
      task: async () => ({
        ...task,
        task: {
          ...task.task,
          evidence: {
            operation: "http.post",
            token: "super-secret-token",
            password: "hunter2",
          },
        },
      }),
    }
    const { container } = renderPage(client)

    expect(await screen.findAllByText("http.post")).not.toHaveLength(0)
    expect(screen.queryByText("hunter2")).toBeNull()
    expect(screen.getByText(injected)).toBeInTheDocument()
    expect(container.querySelector("img")).toBeNull()
    expect(screen.queryByText("super-secret-token")).toBeNull()
    expect(screen.queryByText(/super-secret/)).toBeNull()
  })
})

import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { describe, expect, it, vi } from "vitest"

import {
  ApprovalCard,
  type ApprovalGateway,
} from "@/features/approvals/approval-card"
import type { ChatApprovalPart } from "@/features/chat/chat-types"
import {
  GatewayError,
  type ApprovalDecision,
  type GatewayApproval,
} from "@goldenbee/sdk"

const pending: ChatApprovalPart = {
  type: "approval",
  approvalId: "approval-1",
  operation: "http.post",
  target: "https://api.example.test/orders/42",
  digest: "sha256:abc123",
  risk: "external_write",
  requestedBy: "caller-a",
  expiresAt: 10_000,
  status: "pending",
  digestChanged: false,
}

function approvalRecord(
  overrides: Partial<GatewayApproval["approval"]> = {},
): GatewayApproval {
  return {
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
      expires_at: 10_000,
      status: "approved",
      decided_by: "caller-b",
      decided_at: 900,
      created_at: 800,
      ...overrides,
    },
  }
}

function gateway(
  decide: (decision: ApprovalDecision) => Promise<GatewayApproval>,
  revoke: () => Promise<GatewayApproval> = async () =>
    approvalRecord({ status: "revoked" }),
) {
  const decisions: Array<{
    taskId: string
    approvalId: string
    decision: ApprovalDecision
  }> = []
  const revocations: Array<{ taskId: string; approvalId: string }> = []
  const client: ApprovalGateway = {
    async decideApproval(taskId, approvalId, decision) {
      decisions.push({ taskId, approvalId, decision })
      return decide(decision)
    },
    async revokeApproval(taskId, approvalId) {
      revocations.push({ taskId, approvalId })
      return revoke()
    },
  }
  return { client, decisions, revocations }
}

function renderCard(
  overrides: {
    part?: Partial<ChatApprovalPart>
    client: ApprovalGateway
    taskId?: string
    now?: () => number
  },
) {
  const updated = vi.fn()
  const result = render(
    <ApprovalCard
      part={{ ...pending, ...overrides.part }}
      taskId={overrides.taskId ?? "exec-1"}
      client={overrides.client}
      onUpdated={updated}
      now={overrides.now ?? (() => 1_000)}
    />,
  )
  return { updated, unmount: result.unmount }
}

async function click(
  user: ReturnType<typeof userEvent.setup>,
  name: string,
) {
  await user.click(screen.getByRole("button", { name }))
}

describe("approval card", () => {
  it("approves with the exact observed digest and applies the Gateway response", async () => {
    const user = userEvent.setup()
    const { client, decisions } = gateway(async () => approvalRecord())
    const { updated } = renderCard({ client })

    expect(screen.getByText("sha256:abc123")).toBeInTheDocument()
    expect(screen.getByText("http.post")).toBeInTheDocument()
    expect(screen.getByText("https://api.example.test/orders/42")).toBeInTheDocument()
    expect(screen.getByText("external_write")).toBeInTheDocument()
    expect(screen.getByText("caller-a")).toBeInTheDocument()

    await click(user, "Approve")

    await waitFor(() => expect(updated).toHaveBeenCalledTimes(1))
    expect(decisions).toEqual([
      {
        taskId: "exec-1",
        approvalId: "approval-1",
        decision: { decision: "approved", digest: "sha256:abc123" },
      },
    ])
    expect(updated.mock.calls[0]?.[0]).toMatchObject({
      approval: { status: "approved", digest: "sha256:abc123" },
    })
  })

  it("rejects with the same exact digest", async () => {
    const user = userEvent.setup()
    const { client, decisions } = gateway(async () =>
      approvalRecord({ status: "rejected" }),
    )
    const { updated } = renderCard({ client })

    await click(user, "Reject")

    await waitFor(() => expect(updated).toHaveBeenCalledTimes(1))
    expect(decisions[0]?.decision).toEqual({
      decision: "rejected",
      digest: "sha256:abc123",
    })
  })

  it("revokes the pending approval through the Gateway", async () => {
    const user = userEvent.setup()
    const { client, revocations } = gateway(async () => approvalRecord())
    const { updated } = renderCard({ client })

    await click(user, "Revoke")

    await waitFor(() => expect(updated).toHaveBeenCalledTimes(1))
    expect(revocations).toEqual([
      { taskId: "exec-1", approvalId: "approval-1" },
    ])
  })

  it("keeps a scope denial visible without changing the card", async () => {
    const user = userEvent.setup()
    const { client } = gateway(async () => {
      throw new GatewayError({
        kind: "forbidden",
        status: 403,
        code: "scope.approval_decide",
        message: "the caller token lacks approval:decide",
        retryable: false,
        recovery: "Use a token that can decide approvals.",
      })
    })
    const { updated } = renderCard({ client })

    await click(user, "Approve")

    expect(
      await screen.findByText("Not authorized to decide this approval"),
    ).toBeInTheDocument()
    expect(screen.getByText("the caller token lacks approval:decide")).toBeInTheDocument()
    expect(screen.getByText("scope.approval_decide")).toBeInTheDocument()
    expect(updated).not.toHaveBeenCalled()
    expect(screen.getByText("pending")).toBeInTheDocument()
  })

  it("disables decisions after expiry", () => {
    const { client } = gateway(async () => approvalRecord())
    renderCard({
      client,
      part: { expiresAt: 900 },
      now: () => 1_000,
    })

    expect(screen.getByText("expiry passed")).toBeInTheDocument()
    expect(screen.getByRole("button", { name: "Approve" })).toBeDisabled()
    expect(screen.getByRole("button", { name: "Reject" })).toBeDisabled()
    expect(screen.getByRole("button", { name: "Revoke" })).toBeDisabled()
  })

  it("disables decisions after a decision", () => {
    const { client } = gateway(async () => approvalRecord())
    renderCard({ client, part: { status: "approved" } })

    expect(screen.getByRole("button", { name: "Approve" })).toBeDisabled()
    expect(screen.getByRole("button", { name: "Revoke" })).toBeDisabled()
  })

  it("disables decisions after the digest changes", () => {
    const { client } = gateway(async () => approvalRecord())
    renderCard({ client, part: { digestChanged: true } })

    expect(screen.getByText("digest changed")).toBeInTheDocument()
    expect(screen.getByRole("button", { name: "Approve" })).toBeDisabled()
  })

  it("shows a digest mismatch without reusing or changing the observed digest", async () => {
    const user = userEvent.setup()
    const { client, decisions } = gateway(async () => {
      throw new GatewayError({
        kind: "http",
        status: 409,
        code: "approval.digest_mismatch",
        message: "decision digest does not match the requested operation",
        retryable: false,
      })
    })
    const { updated } = renderCard({ client })

    await click(user, "Approve")

    expect(
      await screen.findByText("Approval action failed"),
    ).toBeInTheDocument()
    expect(
      screen.getByText("decision digest does not match the requested operation"),
    ).toBeInTheDocument()
    expect(screen.getByText("sha256:abc123")).toBeInTheDocument()
    expect(updated).not.toHaveBeenCalled()
    expect(decisions[0]?.decision.digest).toBe("sha256:abc123")
  })

  it("links to the task detail page when a task id exists", () => {
    const { client } = gateway(async () => approvalRecord())
    renderCard({ client, taskId: "exec-1" })

    expect(screen.getByRole("link", { name: "Open task" })).toHaveAttribute(
      "href",
      "#/tasks/exec-1",
    )
  })
})

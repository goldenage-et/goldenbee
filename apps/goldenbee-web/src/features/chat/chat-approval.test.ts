import { describe, expect, it } from "vitest"

import { chatReducer } from "@/features/chat/chat-reducer"
import {
  createInitialChatState,
  type ChatAction,
  type ChatPart,
  type ChatSelection,
  type ChatState,
} from "@/features/chat/chat-types"
import type { GatewayApproval, StreamEvent } from "@goldenbee/sdk"

const selection: ChatSelection = {
  worker: "general-a",
  agent: "general",
  profile: "default",
}

function event(
  seq: number,
  kind: string,
  payload: unknown,
  durable = true,
): StreamEvent {
  return {
    event_id: `evt-${seq}`,
    source: "worker",
    incarnation: "inc-1",
    stream_id: "task:exec-1",
    seq,
    schema_version: 1,
    kind,
    durable,
    payload,
    at: seq,
  }
}

function apply(state: ChatState, action: ChatAction): ChatState {
  return chatReducer(state, action)
}

function started(): ChatState {
  const state = apply(createInitialChatState("chat-1"), {
    type: "send",
    userMessageId: "u1",
    assistantMessageId: "a1",
    text: "deploy the change",
    selection,
    idempotencyKey: "key-1",
    createdAt: 1,
  })
  return apply(state, {
    type: "submitted",
    assistantMessageId: "a1",
    taskId: "exec-1",
    state: "accepted",
  })
}

function streamed(state: ChatState, next: StreamEvent): ChatState {
  return apply(state, { type: "event", assistantMessageId: "a1", event: next })
}

function approvals(state: ChatState) {
  const message = state.messages.find((candidate) => candidate.id === "a1")
  if (!message) {
    throw new Error("assistant message missing")
  }
  return message.parts.filter(
    (part): part is Extract<ChatPart, { type: "approval" }> =>
      part.type === "approval",
  )
}

const requested = {
  approval_id: "approval-1",
  operation: "http.post",
  target: "https://api.example.test/orders/42",
  digest: "sha256:abc123",
  expires_at: 1_000,
  risk: "external_write",
  requested_by: "caller-a",
}

describe("chat approval events", () => {
  it("maps a request to one approval part keyed by the approval id", () => {
    const state = streamed(started(), event(1, "approval.requested", requested))

    expect(approvals(state)).toEqual([
      {
        type: "approval",
        approvalId: "approval-1",
        operation: "http.post",
        target: "https://api.example.test/orders/42",
        digest: "sha256:abc123",
        risk: "external_write",
        requestedBy: "caller-a",
        expiresAt: 1_000,
        status: "pending",
        digestChanged: false,
      },
    ])
  })

  it("dedupes repeated request, decision, and receipt observations", () => {
    let state = streamed(started(), event(1, "approval.requested", requested))
    state = streamed(state, event(2, "approval.requested", requested))
    state = streamed(
      state,
      event(3, "approval.approved", {
        approval_id: "approval-1",
        decided_by: "caller-b",
        digest: "sha256:abc123",
      }),
    )
    state = streamed(
      state,
      event(4, "approval.approved", {
        approval_id: "approval-1",
        decided_by: "caller-b",
        digest: "sha256:abc123",
      }),
    )
    state = streamed(
      state,
      event(5, "approval.receipt", {
        approval_id: "approval-1",
        operation: "http.post",
        receipt: "receipt-42",
      }),
    )
    state = streamed(
      state,
      event(6, "approval.receipt", {
        approval_id: "approval-1",
        operation: "http.post",
        receipt: "receipt-42",
      }),
    )

    expect(approvals(state)).toHaveLength(1)
    expect(approvals(state)[0]).toMatchObject({
      status: "approved",
      decidedBy: "caller-b",
      receipt: "receipt-42",
      digest: "sha256:abc123",
    })
    expect(state.lastDurableSeq).toBe(6)
  })

  it("maps rejected, revoked, and expired outcomes without reopening a decision", () => {
    let rejected = streamed(started(), event(1, "approval.requested", requested))
    rejected = streamed(
      rejected,
      event(2, "approval.rejected", {
        approval_id: "approval-1",
        decided_by: "caller-b",
        digest: "sha256:abc123",
      }),
    )
    rejected = streamed(
      rejected,
      event(3, "approval.requested", requested),
    )
    expect(approvals(rejected)[0]?.status).toBe("rejected")

    let revoked = streamed(started(), event(1, "approval.requested", requested))
    revoked = streamed(
      revoked,
      event(2, "approval.revoked", {
        approval_id: "approval-1",
        revoked_by: "caller-c",
        digest: "sha256:abc123",
      }),
    )
    expect(approvals(revoked)[0]).toMatchObject({
      status: "revoked",
      decidedBy: "caller-c",
    })

    let expired = streamed(started(), event(1, "approval.requested", requested))
    expired = streamed(
      expired,
      event(2, "approval.expired", { approval_id: "approval-1" }),
    )
    expect(approvals(expired)[0]).toMatchObject({
      status: "expired",
      operation: "http.post",
      digest: "sha256:abc123",
    })
  })

  it("marks a changed digest and never lets a new request reopen a decision", () => {
    let state = streamed(started(), event(1, "approval.requested", requested))
    state = streamed(
      state,
      event(
        2,
        "approval.requested",
        { ...requested, digest: "sha256:changed" },
      ),
    )

    expect(approvals(state)[0]).toMatchObject({
      status: "pending",
      digest: "sha256:changed",
      digestChanged: true,
    })

    state = streamed(
      state,
      event(3, "approval.approved", {
        approval_id: "approval-1",
        decided_by: "caller-b",
        digest: "sha256:changed",
      }),
    )
    state = streamed(
      state,
      event(
        4,
        "approval.requested",
        { ...requested, digest: "sha256:third" },
      ),
    )

    expect(approvals(state)[0]).toMatchObject({
      status: "approved",
      digestChanged: true,
    })
  })

  it("applies the Gateway approval response in place", () => {
    let state = streamed(started(), event(1, "approval.requested", requested))
    const response: GatewayApproval = {
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
        expires_at: 1_000,
        status: "approved",
        decided_by: "caller-b",
        decided_at: 900,
        created_at: 800,
      },
    }

    state = apply(state, {
      type: "approval-snapshot",
      taskId: "exec-1",
      approval: response,
    })

    expect(approvals(state)).toHaveLength(1)
    expect(approvals(state)[0]).toMatchObject({
      status: "approved",
      decidedBy: "caller-b",
    })
  })
})

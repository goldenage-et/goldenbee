import { describe, expect, it } from "vitest"

import { chatReducer } from "@/features/chat/chat-reducer"
import { taskStateView } from "@/features/chat/chat-status"
import {
  createInitialChatState,
  type ChatAction,
  type ChatPart,
  type ChatSelection,
  type ChatState,
} from "@/features/chat/chat-types"
import type { StreamEvent } from "@goldenbee/sdk"

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
    text: "hello",
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

function assistant(state: ChatState) {
  const message = state.messages.find((candidate) => candidate.id === "a1")
  if (!message) {
    throw new Error("assistant message missing")
  }
  return message
}

function partsOf<T extends ChatPart["type"]>(
  state: ChatState,
  type: T,
): Extract<ChatPart, { type: T }>[] {
  return assistant(state).parts.filter(
    (part): part is Extract<ChatPart, { type: T }> => part.type === type,
  )
}

function textOf(state: ChatState): string {
  return partsOf(state, "text")
    .map((part) => part.text)
    .join("")
}

function streamed(state: ChatState, next: StreamEvent): ChatState {
  return apply(state, { type: "event", assistantMessageId: "a1", event: next })
}

describe("chat reducer submission", () => {
  it("adds one user turn and one assistant reply with the selected target", () => {
    const state = started()

    expect(state.messages).toHaveLength(2)
    expect(state.messages[0]).toMatchObject({
      id: "u1",
      role: "user",
      selection,
      idempotencyKey: "key-1",
    })
    expect(state.messages[0]?.parts).toEqual([
      { type: "text", text: "hello", streaming: false },
    ])
    expect(state.messages[1]).toMatchObject({
      id: "a1",
      role: "assistant",
      replyTo: "u1",
      taskId: "exec-1",
    })
    expect(state).toMatchObject({
      status: "streaming",
      activeTaskId: "exec-1",
      activeAssistantId: "a1",
    })
  })

  it("keeps a 202 unknown submission as uncertainty rather than failure", () => {
    let state = apply(createInitialChatState("chat-1"), {
      type: "send",
      userMessageId: "u1",
      assistantMessageId: "a1",
      text: "hello",
      selection,
      idempotencyKey: "key-1",
      createdAt: 1,
    })
    state = apply(state, {
      type: "submit-uncertain",
      assistantMessageId: "a1",
      error: {
        code: "task.unknown_submission",
        message: "The Gateway could not confirm whether the task was accepted.",
        recovery: "Inspect the task before sending it again.",
        retryable: false,
        details: { execution_id: "exec-9", worker: "general-a" },
      },
    })

    expect(state.status).toBe("uncertain")
    expect(partsOf(state, "uncertainty")).toEqual([
      {
        type: "uncertainty",
        code: "task.unknown_submission",
        message: "The Gateway could not confirm whether the task was accepted.",
        recovery: "Inspect the task before sending it again.",
        details: { execution_id: "exec-9", worker: "general-a" },
      },
    ])
    expect(partsOf(state, "error")).toHaveLength(0)
  })

  it("shows a rejected submission as a retryable failure", () => {
    let state = apply(createInitialChatState("chat-1"), {
      type: "send",
      userMessageId: "u1",
      assistantMessageId: "a1",
      text: "hello",
      selection,
      idempotencyKey: "key-1",
      createdAt: 1,
    })
    state = apply(state, {
      type: "submit-failed",
      assistantMessageId: "a1",
      error: {
        code: "task.rejected",
        message: "The Worker rejected the task.",
        recovery: "Choose another Worker.",
        retryable: false,
      },
    })

    expect(state.status).toBe("failed")
    expect(partsOf(state, "error")[0]).toMatchObject({
      code: "task.rejected",
      kind: "submission",
      retryable: false,
    })
  })
})

describe("chat reducer streaming text", () => {
  it("streams deltas in order and reconciles the final output without duplicates", () => {
    let state = started()
    state = streamed(state, event(1, "task.accepted", { state: "accepted" }))
    state = streamed(state, event(2, "model.delta", { text: "Hel" }, false))
    state = streamed(state, event(3, "model.delta", { text: "lo" }, false))
    state = streamed(
      state,
      event(4, "model.delta.complete", { text: "Hello" }, false),
    )
    state = streamed(
      state,
      event(5, "task.succeeded", {
        state: "succeeded",
        output: "Hello world",
        usage: { model_calls: 1, estimated_tokens: 4 },
      }),
    )

    expect(textOf(state)).toBe("Hello world")
    expect(partsOf(state, "text")).toHaveLength(1)
    expect(partsOf(state, "text")[0]).toMatchObject({ streaming: false })
    expect(partsOf(state, "task")[0]).toMatchObject({
      state: "succeeded",
      output: "Hello world",
      usage: { model_calls: 1, estimated_tokens: 4 },
    })
    expect(state.status).toBe("succeeded")
    expect(state.lastDurableSeq).toBe(5)
    expect(state.lastSeq).toBe(5)
  })

  it("replaces streamed text that differs from the authoritative output", () => {
    let state = started()
    state = streamed(state, event(1, "task.running", { attempt: 1 }))
    state = streamed(state, event(2, "model.delta", { text: "draft" }, false))
    state = streamed(
      state,
      event(3, "task.succeeded", { state: "succeeded", output: "final" }),
    )

    expect(textOf(state)).toBe("final")
    expect(partsOf(state, "text")).toHaveLength(1)
  })

  it("ignores out-of-order and duplicate sequences", () => {
    let state = started()
    state = streamed(state, event(1, "model.delta", { text: "a" }, false))
    state = streamed(state, event(3, "model.delta", { text: "c" }, false))
    state = streamed(state, event(2, "model.delta", { text: "b" }, false))
    state = streamed(state, event(3, "model.delta", { text: "c" }, false))

    expect(textOf(state)).toBe("ac")
    expect(state.lastSeq).toBe(3)
  })

  it("advances the cursor for durable kinds owned by later tickets", () => {
    let state = started()
    state = streamed(
      state,
      event(1, "tool.call", {
        id: "tool-1",
        name: "exec_command",
        agent: "general",
        server: "",
      }),
    )
    expect(partsOf(state, "task")).toHaveLength(1)
    expect(state.lastDurableSeq).toBe(1)

    state = streamed(state, event(2, "model.delta", { text: "after tool" }, false))
    expect(textOf(state)).toBe("after tool")
  })

  it("keeps stream control notices out of the sequence guard", () => {
    let state = started()
    state = streamed(state, event(4, "model.delta", { text: "x" }, false))
    state = streamed(
      state,
      event(4, "stream.lagged", {
        reason: "subscriber_lagged",
        from_seq: 2,
        recovery: "reconnect",
      }, false),
    )

    expect(state.gap).toEqual({ reason: "subscriber_lagged", fromSeq: 2 })
    expect(state.activeTaskId).toBe("exec-1")
  })

  it("records a draining notice without failing the stream", () => {
    let state = started()
    state = apply(state, {
      type: "stream-draining",
      assistantMessageId: "a1",
      phase: "draining",
    })

    expect(partsOf(state, "diagnostic")[0]).toMatchObject({
      eventKind: "stream.draining",
    })
    expect(state.status).toBe("streaming")
  })
})

describe("chat reducer task lifecycle", () => {
  it("reports queued, running, and cancelling states truthfully", () => {
    let state = started()
    state = streamed(state, event(1, "task.queued", { attempt: 2 }))
    expect(partsOf(state, "task")[0]).toMatchObject({
      state: "queued",
      attempt: 2,
    })

    state = streamed(state, event(2, "task.running", { attempt: 2 }))
    expect(partsOf(state, "task")[0]).toMatchObject({
      state: "running",
      attempt: 2,
    })

    state = streamed(
      state,
      event(3, "task.cancellation_requested", { confirmed: true, state: "cancelling" }),
    )
    expect(state.status).toBe("stopping")
    expect(partsOf(state, "task")[0]).toMatchObject({ state: "cancelling" })
  })

  it("applies repeated terminal events once and enriches output and usage", () => {
    let state = started()
    state = streamed(state, event(1, "task.cancelled", { reason: "requested" }))
    expect(state.status).toBe("cancelled")

    state = streamed(
      state,
      event(2, "task.cancelled", {
        state: "cancelled",
        output: "partial answer",
        usage: { tool_calls: 2 },
      }),
    )

    expect(state.status).toBe("cancelled")
    expect(state.messages).toHaveLength(2)
    expect(partsOf(state, "task")).toHaveLength(1)
    expect(partsOf(state, "task")[0]).toMatchObject({
      state: "cancelled",
      output: "partial answer",
      usage: { tool_calls: 2 },
    })
    expect(partsOf(state, "text")).toHaveLength(1)
    expect(textOf(state)).toBe("partial answer")
  })

  it("surfaces a failed task with its error and recovery guidance", () => {
    let state = started()
    state = streamed(
      state,
      event(1, "task.failed", {
        state: "failed",
        error: {
          code: "worker.crashed",
          message: "The Worker stopped before answering.",
          retryable: true,
          recovery: "Retry the task or choose another Worker.",
        },
      }),
    )

    expect(state.status).toBe("failed")
    expect(partsOf(state, "task")[0]?.error).toMatchObject({
      code: "worker.crashed",
      recovery: "Retry the task or choose another Worker.",
    })
    expect(taskStateView(partsOf(state, "task")[0]?.state)).toMatchObject({
      label: "Task failed",
      retryable: true,
    })
  })

  it("never turns an unknown task outcome into success", () => {
    let state = started()
    state = streamed(
      state,
      event(1, "task.unknown", {
        state: "unknown",
        uncertainty: "The Worker did not confirm completion.",
      }),
    )

    expect(state.status).toBe("unknown")
    expect(partsOf(state, "task")[0]).toMatchObject({
      state: "unknown",
      uncertainty: "The Worker did not confirm completion.",
    })
    expect(taskStateView("unknown")).toMatchObject({
      label: "Outcome unknown",
    })
  })

  it("shows a recovering task with reconciliation guidance", () => {
    let state = started()
    state = streamed(
      state,
      event(1, "task.recovery_required", { reason: "worker restarted" }),
    )

    expect(state.status).toBe("recovering")
    expect(partsOf(state, "task")[0]).toMatchObject({ state: "recovering" })
    expect(partsOf(state, "diagnostic")[0]?.detail).toContain("worker restarted")
    expect(taskStateView("recovering")).toMatchObject({
      label: "Recovering",
    })
  })
})

describe("chat reducer reconnect replay", () => {
  it("replays from a durable cursor without duplicating text or task parts", () => {
    let state = started()
    state = streamed(state, event(1, "task.accepted", { state: "accepted" }))
    state = streamed(state, event(2, "model.delta", { text: "partial " }, false))
    const durableCursor = state.lastDurableSeq
    expect(durableCursor).toBe(1)
    expect(state.lastSeq).toBe(2)

    // A reconnect replays the durable event at the cursor and then continues.
    state = streamed(state, event(1, "task.accepted", { state: "accepted" }))
    state = streamed(state, event(3, "model.delta", { text: "answer" }, false))
    state = streamed(
      state,
      event(4, "task.succeeded", { state: "succeeded", output: "partial answer" }),
    )

    expect(textOf(state)).toBe("partial answer")
    expect(partsOf(state, "task")).toHaveLength(1)
    expect(partsOf(state, "text")).toHaveLength(1)
  })

  it("records a cursor gap instead of pretending the replay is complete", () => {
    let state = started()
    state = apply(state, { type: "gap", reason: "cursor_expired", fromSeq: 12 })

    expect(state.gap).toEqual({ reason: "cursor_expired", fromSeq: 12 })
    expect(state.status).toBe("streaming")
  })
})

describe("chat reducer stop and retry", () => {
  it("applies the cancellation projection", () => {
    let state = started()
    state = apply(state, { type: "stop-requested" })
    expect(state.status).toBe("stopping")

    state = apply(state, {
      type: "stop-applied",
      assistantMessageId: "a1",
      task: {
        task_id: "exec-1",
        worker: "general-a",
        worker_instance: "general-a",
        connectivity: "online",
        task: {
          id: "worker-task",
          namespace: "local",
          caller: "operator",
          agent: "general",
          session_id: "chat-1",
          state: "cancelled",
          output: "stopped early",
        },
      },
    })

    expect(state.status).toBe("cancelled")
    expect(state.activeTaskId).toBeNull()
    expect(partsOf(state, "task")[0]).toMatchObject({
      state: "cancelled",
      output: "stopped early",
    })
    expect(textOf(state)).toBe("stopped early")
  })

  it("reuses the user turn on retry with a new idempotency key", () => {
    let state = started()
    state = streamed(
      state,
      event(1, "task.failed", {
        state: "failed",
        error: {
          code: "worker.crashed",
          message: "boom",
          retryable: true,
          recovery: "retry",
        },
      }),
    )

    state = apply(state, {
      type: "retry-started",
      userMessageId: "u1",
      assistantMessageId: "a1",
      idempotencyKey: "key-2",
    })
    expect(assistant(state).parts).toEqual([])

    state = apply(state, {
      type: "submitted",
      assistantMessageId: "a1",
      taskId: "exec-2",
      state: "accepted",
    })

    expect(state.messages).toHaveLength(2)
    expect(state.messages.filter((message) => message.role === "user")).toHaveLength(1)
    expect(state.messages[0]).toMatchObject({ id: "u1", idempotencyKey: "key-2" })
    expect(assistant(state).parts).toHaveLength(1)
    expect(assistant(state).parts[0]).toMatchObject({ type: "task", state: "accepted" })
    expect(assistant(state).taskId).toBe("exec-2")
    expect(state).toMatchObject({
      status: "streaming",
      activeTaskId: "exec-2",
      lastSeq: 0,
      lastDurableSeq: 0,
    })
  })
})

describe("chat reducer task guidance", () => {
  it("gives failed, cancelled, unknown, and recovering states distinct guidance", () => {
    const views = {
      failed: taskStateView("failed"),
      cancelled: taskStateView("cancelled"),
      unknown: taskStateView("unknown"),
      recovering: taskStateView("recovering"),
    }

    expect(new Set(Object.values(views).map((view) => view.label)).size).toBe(4)
    for (const view of Object.values(views)) {
      expect(view.guidance).toBeTruthy()
    }
    expect(views.unknown.label).toBe("Outcome unknown")
    expect(views.recovering.label).toBe("Recovering")
  })
})

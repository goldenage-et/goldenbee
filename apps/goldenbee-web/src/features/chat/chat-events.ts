import type {
  ApprovalStatus,
  GatewayApproval,
  StreamEvent,
  TaskError,
  TaskState,
} from "@goldenbee/sdk"

import type {
  ChatApprovalPart,
  ChatChildObservation,
  ChatDiagnosticPart,
  ChatErrorInfo,
  ChatMessage,
  ChatPart,
  ChatPlanPart,
  ChatPlanStep,
  ChatPlanStepStatus,
  ChatReasoningPart,
  ChatRunStatus,
  ChatState,
  ChatStreamGap,
  ChatTaskPart,
  ChatTextPart,
  ChatToolPart,
  ChatToolStatus,
  ChatUsagePart,
} from "./chat-types"

const taskStates: readonly TaskState[] = [
  "accepted",
  "queued",
  "running",
  "waiting_approval",
  "cancelling",
  "recovering",
  "unknown",
  "succeeded",
  "failed",
  "cancelled",
]

const approvalEventKinds = new Set([
  "approval.requested",
  "approval.approved",
  "approval.rejected",
  "approval.revoked",
  "approval.expired",
  "approval.receipt",
])

const approvalStatuses: readonly ApprovalStatus[] = [
  "pending",
  "approved",
  "rejected",
  "expired",
  "revoked",
]

const textKinds = new Set([
  "model.delta",
  "model.delta.complete",
  "graph.output",
])

const reasoningKinds = new Set(["model.thinking", "model.thinking.complete"])

const toolKinds = new Set(["tool.call", "tool.result", "tool.outcome_unknown"])

const planKinds = new Set(["master.plan_created", "master.child_observed"])

const usageKinds = new Set(["budget.exhausted", "budget.usage_unavailable"])

const diagnosticKinds = new Set([
  "artifact.finalized",
  "artifact.recording_failed",
  "model.fallback",
  "context.compacted",
  "graph.interrupted",
  "graph.thread_bound",
  "team.receipt",
  "operation.reconciled",
  "hook.after_task_failed",
])

const usagePriority: Record<ChatUsagePart["source"], number> = {
  projection: 0,
  budget: 1,
  terminal: 2,
}

export const terminalKinds = new Set([
  "task.succeeded",
  "task.failed",
  "task.cancelled",
  "task.unknown",
])

export function isTerminalStatus(status: ChatRunStatus): boolean {
  return (
    status === "succeeded" ||
    status === "failed" ||
    status === "cancelled" ||
    status === "unknown"
  )
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function textField(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined
}

function numberField(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined
}

function taskStateField(value: unknown): TaskState | undefined {
  return typeof value === "string" && taskStates.includes(value as TaskState)
    ? (value as TaskState)
    : undefined
}

function taskErrorField(value: unknown): TaskError | undefined {
  if (!isRecord(value)) {
    return undefined
  }
  const code = textField(value.code)
  const message = textField(value.message)
  if (!code || !message) {
    return undefined
  }
  return {
    code,
    message,
    retryable: value.retryable === true,
    recovery: typeof value.recovery === "string" ? value.recovery : "",
    details: isRecord(value.details)
      ? Object.fromEntries(
          Object.entries(value.details).map(([key, detail]) => [
            key,
            String(detail),
          ]),
        )
      : undefined,
  }
}

function usageField(value: unknown): ChatTaskPart["usage"] | undefined {
  return isRecord(value) ? value : undefined
}

export function messageWithParts(
  state: ChatState,
  messageId: string,
  update: (message: ChatMessage) => ChatMessage,
): ChatState {
  let changed = false
  const messages = state.messages.map((message) => {
    if (message.id !== messageId) {
      return message
    }
    changed = true
    return update(message)
  })
  return changed ? { ...state, messages } : state
}

function replacePart(message: ChatMessage, part: ChatPart): ChatMessage {
  const index = message.parts.findIndex((candidate) => candidate.type === part.type)
  if (index === -1) {
    return { ...message, parts: [...message.parts, part] }
  }
  const parts = message.parts.map((candidate, position) =>
    position === index ? part : candidate,
  )
  return { ...message, parts }
}

export function appendPart(
  message: ChatMessage,
  part: ChatPart,
): ChatMessage {
  return { ...message, parts: [...message.parts, part] }
}

function appendDiagnostic(
  message: ChatMessage,
  diagnostic: ChatDiagnosticPart,
): ChatMessage {
  const duplicate = message.parts.some(
    (part) =>
      part.type === "diagnostic" &&
      part.eventKind === diagnostic.eventKind &&
      part.detail === diagnostic.detail,
  )
  return duplicate ? message : appendPart(message, diagnostic)
}

function lastReasoningPart(
  message: ChatMessage,
): ChatReasoningPart | undefined {
  for (let index = message.parts.length - 1; index >= 0; index -= 1) {
    const part = message.parts[index]
    if (part?.type === "reasoning") {
      return part
    }
  }
  return undefined
}

function appendReasoning(
  message: ChatMessage,
  text: string,
  author?: string,
): ChatMessage {
  const current = lastReasoningPart(message)
  if (current?.streaming && (author === undefined || current.author === author)) {
    const index = message.parts.lastIndexOf(current)
    const parts = message.parts.map((part, position) =>
      position === index ? { ...current, text: current.text + text } : part,
    )
    return { ...message, parts }
  }
  return appendPart(message, {
    type: "reasoning",
    text,
    author,
    streaming: true,
    complete: false,
  })
}

function completeReasoning(
  message: ChatMessage,
  text: string,
  author?: string,
): ChatMessage {
  const current = lastReasoningPart(message)
  if (current?.streaming) {
    const index = message.parts.lastIndexOf(current)
    const parts = message.parts.map((part, position) =>
      position === index
        ? { ...current, text, streaming: false, complete: true }
        : part,
    )
    return { ...message, parts }
  }
  if (current?.complete && current.text === text) {
    return message
  }
  return appendPart(message, {
    type: "reasoning",
    text,
    author,
    streaming: false,
    complete: true,
  })
}

/** Reasoning is not durable; a terminal task closes the observed run. */
function closeReasoning(message: ChatMessage): ChatMessage {
  let changed = false
  const parts = message.parts.map((part) => {
    if (part.type !== "reasoning" || !part.streaming) {
      return part
    }
    changed = true
    return { ...part, streaming: false, complete: true }
  })
  return changed ? { ...message, parts } : message
}

function closeText(message: ChatMessage): ChatMessage {
  let changed = false
  const parts = message.parts.map((part) => {
    if (part.type !== "text" || !part.streaming) {
      return part
    }
    changed = true
    return { ...part, streaming: false }
  })
  return changed ? { ...message, parts } : message
}

function findTool(message: ChatMessage, id: string): ChatToolPart | undefined {
  return message.parts.find(
    (part): part is ChatToolPart => part.type === "tool" && part.id === id,
  )
}

function upsertTool(message: ChatMessage, next: ChatToolPart): ChatMessage {
  const current = findTool(message, next.id)
  if (!current) {
    return appendPart(message, next)
  }
  const index = message.parts.lastIndexOf(current)
  const parts = message.parts.map((part, position) =>
    position === index ? next : part,
  )
  return { ...message, parts }
}

function applyToolEvent(
  message: ChatMessage,
  event: StreamEvent,
): ChatMessage {
  const payload = isRecord(event.payload) ? event.payload : {}
  if (event.kind === "tool.outcome_unknown") {
    const reason = textField(payload.reason) ?? "the effect outcome is unknown"
    return appendDiagnostic(message, {
      type: "diagnostic",
      eventKind: event.kind,
      detail: `Tool outcome unknown: ${reason}.`,
    })
  }
  const id = textField(payload.id)
  if (!id) {
    return appendDiagnostic(message, {
      type: "diagnostic",
      eventKind: event.kind,
      detail: `Ignored malformed ${event.kind} event without a tool id.`,
    })
  }

  const current = findTool(message, id)
  const name = textField(payload.name) ?? current?.name
  if (!name) {
    return appendDiagnostic(message, {
      type: "diagnostic",
      eventKind: event.kind,
      detail: `Ignored malformed ${event.kind} event without a tool name.`,
    })
  }
  const agent = textField(payload.agent) ?? current?.agent
  const server = textField(payload.server) ?? current?.server

  if (event.kind === "tool.call") {
    return upsertTool(message, {
      type: "tool",
      id,
      name,
      agent,
      server,
      // A replay must not reopen a terminal tool result.
      status: current && current.status !== "running" ? current.status : "running",
      errorCode: current?.errorCode,
    })
  }

  const status: ChatToolStatus =
    payload.ok === true ? "ok" : payload.ok === false ? "error" : "unknown"
  const errorCode =
    payload.ok === false ? textField(payload.error_code) : undefined
  return upsertTool(message, {
    type: "tool",
    id,
    name,
    agent,
    server,
    status,
    errorCode,
  })
}

function approvalStatusField(value: unknown): ApprovalStatus | undefined {
  return typeof value === "string" &&
    approvalStatuses.includes(value as ApprovalStatus)
    ? (value as ApprovalStatus)
    : undefined
}

function findApproval(
  message: ChatMessage,
  approvalId: string,
): ChatApprovalPart | undefined {
  return message.parts.find(
    (part): part is ChatApprovalPart =>
      part.type === "approval" && part.approvalId === approvalId,
  )
}

function upsertApproval(
  message: ChatMessage,
  next: ChatApprovalPart,
): ChatMessage {
  const current = findApproval(message, next.approvalId)
  if (!current) {
    return appendPart(message, next)
  }
  const index = message.parts.lastIndexOf(current)
  const parts = message.parts.map((part, position) =>
    position === index ? next : part,
  )
  return { ...message, parts }
}

function isTerminalApproval(status: ApprovalStatus): boolean {
  return status !== "pending"
}

function applyApprovalRequest(
  message: ChatMessage,
  payload: Record<string, unknown>,
): ChatMessage {
  const approvalId = textField(payload.approval_id)
  if (!approvalId) {
    return appendDiagnostic(message, {
      type: "diagnostic",
      eventKind: "approval.requested",
      detail: "Ignored malformed approval.requested event without an approval id.",
    })
  }
  const digest = textField(payload.digest)
  const current = findApproval(message, approvalId)
  if (!digest) {
    return appendDiagnostic(message, {
      type: "diagnostic",
      eventKind: "approval.requested",
      detail: `Ignored approval.requested event for ${approvalId} without a digest.`,
    })
  }

  const changed = current !== undefined && current.digest !== digest
  const next: ChatApprovalPart = {
    type: "approval",
    approvalId,
    operation: textField(payload.operation) ?? current?.operation,
    target: textField(payload.target) ?? current?.target,
    digest,
    risk: textField(payload.risk) ?? current?.risk,
    requestedBy: textField(payload.requested_by) ?? current?.requestedBy,
    expiresAt: numberField(payload.expires_at) ?? current?.expiresAt,
    // A replay must not reopen a decision the Gateway already recorded.
    status:
      current && isTerminalApproval(current.status) ? current.status : "pending",
    decidedBy: current?.decidedBy,
    receipt: current?.receipt,
    digestChanged: changed || current?.digestChanged === true,
  }
  return upsertApproval(message, next)
}

function applyApprovalDecision(
  message: ChatMessage,
  event: StreamEvent,
  payload: Record<string, unknown>,
): ChatMessage {
  const approvalId = textField(payload.approval_id)
  if (!approvalId) {
    return appendDiagnostic(message, {
      type: "diagnostic",
      eventKind: event.kind,
      detail: `Ignored malformed ${event.kind} event without an approval id.`,
    })
  }
  const status = approvalStatusField(
    event.kind === "approval.approved"
      ? "approved"
      : event.kind === "approval.rejected"
        ? "rejected"
        : event.kind === "approval.revoked"
          ? "revoked"
          : event.kind === "approval.expired"
            ? "expired"
            : undefined,
  )
  if (!status) {
    return message
  }

  const current = findApproval(message, approvalId)
  const digest = textField(payload.digest)
  const decidedBy =
    textField(payload.decided_by) ??
    textField(payload.revoked_by) ??
    current?.decidedBy
  const next: ChatApprovalPart = {
    type: "approval",
    approvalId,
    operation: textField(payload.operation) ?? current?.operation,
    target: textField(payload.target) ?? current?.target,
    // Keep the digest already shown unless this is the first observation. A
    // decision event cannot silently replace a newer request digest.
    digest: current?.digest || digest || "",
    risk: textField(payload.risk) ?? current?.risk,
    requestedBy: textField(payload.requested_by) ?? current?.requestedBy,
    expiresAt: numberField(payload.expires_at) ?? current?.expiresAt,
    // The first recorded outcome wins. An out-of-order replay cannot flip it.
    status:
      current && isTerminalApproval(current.status) ? current.status : status,
    decidedBy,
    receipt: current?.receipt,
    digestChanged:
      current?.digestChanged === true ||
      (digest !== undefined && current !== undefined && current.digest !== digest),
  }
  return upsertApproval(message, next)
}

function applyApprovalReceipt(
  message: ChatMessage,
  payload: Record<string, unknown>,
): ChatMessage {
  const approvalId = textField(payload.approval_id)
  if (!approvalId) {
    return appendDiagnostic(message, {
      type: "diagnostic",
      eventKind: "approval.receipt",
      detail: "Ignored malformed approval.receipt event without an approval id.",
    })
  }
  const current = findApproval(message, approvalId)
  if (!current) {
    return appendDiagnostic(message, {
      type: "diagnostic",
      eventKind: "approval.receipt",
      detail: `Ignored approval.receipt for ${approvalId} before its request was observed.`,
    })
  }
  const receipt = textField(payload.receipt)
  if (!receipt) {
    return message
  }
  return upsertApproval(message, { ...current, receipt })
}

function applyApprovalEvent(
  message: ChatMessage,
  event: StreamEvent,
): ChatMessage {
  const payload = isRecord(event.payload) ? event.payload : {}
  switch (event.kind) {
    case "approval.requested":
      return applyApprovalRequest(message, payload)
    case "approval.receipt":
      return applyApprovalReceipt(message, payload)
    default:
      return applyApprovalDecision(message, event, payload)
  }
}

/** Apply the authoritative approval returned by a decision or revoke route. */
export function applyApprovalSnapshot(
  message: ChatMessage,
  approval: GatewayApproval,
): ChatMessage {
  const record = approval.approval
  const current = findApproval(message, record.id)
  const next: ChatApprovalPart = {
    type: "approval",
    approvalId: record.id,
    operation: record.operation || current?.operation,
    target: record.target || current?.target,
    digest: record.digest || current?.digest || "",
    risk: record.risk || current?.risk,
    requestedBy: record.requested_by || current?.requestedBy,
    expiresAt: record.expires_at ?? current?.expiresAt,
    status: record.status,
    decidedBy: record.decided_by ?? current?.decidedBy,
    receipt: record.receipt ?? current?.receipt,
    digestChanged:
      current?.digestChanged === true ||
      (current !== undefined && record.digest !== current.digest),
  }
  return upsertApproval(message, next)
}

function planStatusForState(state: string | undefined): ChatPlanStepStatus {
  switch (state) {
    case "succeeded":
      return "done"
    case "failed":
    case "cancelled":
      return "error"
    case "unknown":
      return "unknown"
    case "accepted":
    case "queued":
    case "running":
    case "waiting_approval":
    case "cancelling":
    case "recovering":
      return "running"
    default:
      return "unknown"
  }
}

function planChildren(payload: Record<string, unknown>): ChatPlanStep[] {
  const children = Array.isArray(payload.children) ? payload.children : []
  const steps: ChatPlanStep[] = []
  for (const child of children) {
    if (!isRecord(child)) {
      continue
    }
    const id = textField(child.id)
    if (!id) {
      continue
    }
    const target = textField(child.target) ?? id
    steps.push({
      id,
      target,
      capability: textField(child.capability),
      status: "pending",
    })
  }
  return steps
}

function findPlan(message: ChatMessage, planId: string): ChatPlanPart | undefined {
  return message.parts.find(
    (part): part is ChatPlanPart =>
      part.type === "plan" && part.planId === planId,
  )
}

function upsertPlan(message: ChatMessage, next: ChatPlanPart): ChatMessage {
  const current = findPlan(message, next.planId)
  if (!current) {
    return appendPart(message, next)
  }
  const index = message.parts.lastIndexOf(current)
  const parts = message.parts.map((part, position) =>
    position === index ? next : part,
  )
  return { ...message, parts }
}

function applyPlanEvent(message: ChatMessage, event: StreamEvent): ChatMessage {
  const payload = isRecord(event.payload) ? event.payload : {}
  const planId = textField(payload.plan_id)
  if (!planId) {
    return appendDiagnostic(message, {
      type: "diagnostic",
      eventKind: event.kind,
      detail: `Ignored malformed ${event.kind} event without a plan id.`,
    })
  }

  const current = findPlan(message, planId)
  if (event.kind === "master.plan_created") {
    const created = planChildren(payload)
    // A replay keeps statuses already learned from child observations.
    const steps = created.map((step) => {
      const observed = current?.steps.find((existing) => existing.id === step.id)
      return observed ? { ...step, status: observed.status } : step
    })
    return upsertPlan(message, {
      type: "plan",
      planId,
      steps,
      observed: current?.observed ?? [],
    })
  }

  const childId = textField(payload.child)
  if (!childId) {
    return appendDiagnostic(message, {
      type: "diagnostic",
      eventKind: event.kind,
      detail: `Ignored malformed ${event.kind} event without a child id.`,
    })
  }
  const state = textField(payload.state)
  const remoteTaskId = textField(payload.remote_task_id)
  const binding = textField(payload.binding)
  const artifactDigests = Array.isArray(payload.artifact_digests)
    ? payload.artifact_digests.filter(
        (digest): digest is string => typeof digest === "string",
      )
    : []
  const observation: ChatChildObservation = {
    child: childId,
    binding,
    remoteTaskId,
    state,
    artifactDigests,
  }

  const steps = current?.steps ?? []
  const existingStep = steps.find((step) => step.id === childId)
  const status = state
    ? planStatusForState(state)
    : (existingStep?.status ?? "unknown")
  const nextSteps = existingStep
    ? steps.map((step) => (step.id === childId ? { ...step, status } : step))
    : [...steps, { id: childId, target: childId, status }]
  const observed = current?.observed ?? []
  const nextObserved = observed.some((item) => item.child === childId)
    ? observed.map((item) => (item.child === childId ? observation : item))
    : [...observed, observation]

  return upsertPlan(message, {
    type: "plan",
    planId,
    steps: nextSteps,
    observed: nextObserved,
  })
}

function findUsage(message: ChatMessage): ChatUsagePart | undefined {
  return message.parts.find(
    (part): part is ChatUsagePart => part.type === "usage",
  )
}

export function upsertUsagePart(
  message: ChatMessage,
  usage: ChatTaskPart["usage"],
  source: ChatUsagePart["source"],
): ChatMessage {
  if (!usage) {
    return message
  }
  const current = findUsage(message)
  if (current && usagePriority[source] < usagePriority[current.source]) {
    return message
  }
  const next: ChatUsagePart = { type: "usage", usage, source }
  if (!current) {
    return appendPart(message, next)
  }
  const index = message.parts.lastIndexOf(current)
  const parts = message.parts.map((part, position) =>
    position === index ? next : part,
  )
  return { ...message, parts }
}

function applyUsageEvent(
  message: ChatMessage,
  event: StreamEvent,
): ChatMessage {
  const payload = isRecord(event.payload) ? event.payload : {}
  let next = upsertUsagePart(message, usageField(payload.usage), "budget")

  if (event.kind === "budget.exhausted") {
    const dimension = textField(payload.kind) ?? "limit"
    const limit = numberField(payload.limit)
    const used = numberField(payload.used)
    const partial = payload.partial_output === true ? " Partial output was kept." : ""
    const bounds =
      limit === undefined
        ? ""
        : ` (limit ${limit}${used === undefined ? "" : `, used ${used}`})`
    next = appendDiagnostic(next, {
      type: "diagnostic",
      eventKind: event.kind,
      detail: `Budget exhausted for ${dimension}${bounds}.${partial}`,
    })
  } else {
    const uncertainty = textField(payload.uncertainty)
    next = appendDiagnostic(next, {
      type: "diagnostic",
      eventKind: event.kind,
      detail: uncertainty
        ? `Usage is unavailable or inexact: ${uncertainty}`
        : "Usage is unavailable for part of this task.",
    })
  }
  return next
}

function diagnosticDetail(event: StreamEvent): string {
  const payload = isRecord(event.payload) ? event.payload : {}
  switch (event.kind) {
    case "artifact.finalized": {
      const name = textField(payload.name) ?? "artifact"
      const digest = textField(payload.digest)
      return digest ? `Artifact ${name} finalized (${digest}).` : `Artifact ${name} finalized.`
    }
    case "artifact.recording_failed": {
      const name = textField(payload.name) ?? "artifact"
      const error = textField(payload.error) ?? "recording failed"
      return `Artifact ${name} could not be recorded: ${error}.`
    }
    case "model.fallback":
      return `The task fell back to another model.`
    case "context.compacted":
      return "The context was compacted."
    case "graph.interrupted":
      return "The graph paused at a checkpoint."
    case "graph.thread_bound":
      return "The graph bound to a native thread."
    case "team.receipt":
      return "A native team receipt was observed."
    case "operation.reconciled": {
      const operation = textField(payload.operation) ?? "effect"
      const status = textField(payload.status)
      return status
        ? `Effect operation ${operation} reconciled as ${status}.`
        : `Effect operation ${operation} reconciled.`
    }
    case "hook.after_task_failed": {
      const code = textField(payload.code)
      return code ? `After-task hook failed: ${code}.` : "After-task hook failed."
    }
    default:
      return `Unrecognized event "${event.kind}".`
  }
}

export function taskPart(message: ChatMessage): ChatTaskPart | undefined {
  return message.parts.find(
    (part): part is ChatTaskPart => part.type === "task",
  )
}

export function ensureTaskPart(
  message: ChatMessage,
  state: TaskState,
  attempt = 1,
): ChatMessage {
  const current = taskPart(message)
  const next: ChatTaskPart = {
    type: "task",
    state,
    attempt,
    output: current?.output,
    error: current?.error,
    uncertainty: current?.uncertainty,
    usage: current?.usage,
  }
  return replacePart(message, next)
}

export function updateTaskPart(
  message: ChatMessage,
  update: (current: ChatTaskPart) => ChatTaskPart,
): ChatMessage {
  const current = taskPart(message)
  if (!current) {
    return message
  }
  return replacePart(message, update(current))
}

function lastTextPart(message: ChatMessage): ChatTextPart | undefined {
  for (let index = message.parts.length - 1; index >= 0; index -= 1) {
    const part = message.parts[index]
    if (part?.type === "text") {
      return part
    }
  }
  return undefined
}

function updateTextPart(
  message: ChatMessage,
  target: ChatTextPart,
  next: ChatTextPart,
): ChatMessage {
  const index = message.parts.lastIndexOf(target)
  if (index === -1) {
    return message
  }
  const parts = message.parts.map((part, position) =>
    position === index ? next : part,
  )
  return { ...message, parts }
}

function appendDelta(
  message: ChatMessage,
  text: string,
  author?: string,
): ChatMessage {
  const current = lastTextPart(message)
  if (current?.streaming && (author === undefined || current.author === author)) {
    return updateTextPart(message, current, {
      ...current,
      text: current.text + text,
    })
  }
  return appendPart(message, {
    type: "text",
    text,
    author,
    streaming: true,
  })
}

/** Close the current delta run with the completed text for that run. */
function completeDelta(
  message: ChatMessage,
  text: string,
  author?: string,
): ChatMessage {
  const current = lastTextPart(message)
  if (!current) {
    return appendPart(message, { type: "text", text, author, streaming: false })
  }
  if (current.text === text) {
    return updateTextPart(message, current, { ...current, streaming: false })
  }
  if (text.startsWith(current.text)) {
    return updateTextPart(message, current, {
      ...current,
      text,
      streaming: false,
    })
  }
  // The completed run disagrees with the deltas; replace just that run.
  return updateTextPart(message, current, { ...current, text, streaming: false })
}

/**
 * Apply the authoritative final output without duplicating streamed text.
 *
 * If the observed text is a prefix of the output, the single final part carries
 * the output. If it differs, the streamed draft is replaced.
 */
export function reconcileOutput(
  message: ChatMessage,
  output: string,
): ChatMessage {
  const textParts = message.parts.filter(
    (part): part is ChatTextPart => part.type === "text",
  )
  const withoutText: ChatMessage = {
    ...message,
    parts: message.parts.filter((part) => part.type !== "text"),
  }
  return appendPart(withoutText, {
    type: "text",
    text: output,
    author: textParts.at(-1)?.author,
    streaming: false,
  })
}

export function statusForState(state: TaskState): ChatRunStatus {
  switch (state) {
    case "accepted":
    case "queued":
    case "running":
    case "waiting_approval":
      return "streaming"
    case "cancelling":
      return "stopping"
    case "recovering":
      return "recovering"
    case "succeeded":
      return "succeeded"
    case "failed":
      return "failed"
    case "cancelled":
      return "cancelled"
    case "unknown":
      return "unknown"
  }
}

function applyTaskEvent(
  message: ChatMessage,
  event: StreamEvent,
): ChatMessage {
  const payload = isRecord(event.payload) ? event.payload : {}
  const payloadState = taskStateField(payload.state)
  const attempt = numberField(payload.attempt)
  const output = textField(payload.output)
  const error = taskErrorField(payload.error)
  const uncertainty = textField(payload.uncertainty)
  const usage = usageField(payload.usage)

  switch (event.kind) {
    case "task.accepted":
      return ensureTaskPart(message, payloadState ?? "accepted", attempt ?? 1)
    case "task.queued":
    case "task.running": {
      const fallback: TaskState =
        event.kind === "task.queued" ? "queued" : "running"
      return updateTaskPart(message, (current) => ({
        ...current,
        state: payloadState ?? fallback,
        attempt: attempt ?? current.attempt,
      }))
    }
    case "task.cancellation_requested": {
      const nextState: TaskState =
        payloadState ?? (payload.confirmed === true ? "cancelled" : "cancelling")
      return updateTaskPart(message, (current) => ({
        ...current,
        state: nextState,
      }))
    }
    case "task.recovery_required": {
      const reason = textField(payload.reason) ?? "The Worker is reconciling the task."
      return appendDiagnostic(
        updateTaskPart(message, (current) => ({
          ...current,
          state: "recovering",
        })),
        {
          type: "diagnostic",
          eventKind: event.kind,
          detail: `Recovery required: ${reason}`,
        },
      )
    }
    case "task.recovered": {
      const reason = textField(payload.reason) ?? "The Gateway reconciled the task."
      return appendDiagnostic(
        updateTaskPart(message, (current) => ({
          ...current,
          state: payloadState ?? "accepted",
          uncertainty: undefined,
        })),
        {
          type: "diagnostic",
          eventKind: event.kind,
          detail: `Recovered: ${reason}`,
        },
      )
    }
    case "task.updated": {
      if (!payloadState) {
        return message
      }
      let next = updateTaskPart(message, (current) => ({
        ...current,
        state: payloadState,
        attempt: attempt ?? current.attempt,
        output: output ?? current.output,
        error: error ?? current.error,
        uncertainty: uncertainty ?? current.uncertainty,
        usage: usage ?? current.usage,
      }))
      if (usage) {
        next = upsertUsagePart(next, usage, "projection")
      }
      return output ? reconcileOutput(next, output) : next
    }
    case "task.succeeded":
    case "task.failed":
    case "task.cancelled":
    case "task.unknown": {
      const fallback: TaskState =
        event.kind === "task.succeeded"
          ? "succeeded"
          : event.kind === "task.failed"
            ? "failed"
            : event.kind === "task.cancelled"
              ? "cancelled"
              : "unknown"
      const nextState = payloadState ?? fallback
      let next = ensureTaskPart(
        message,
        nextState,
        attempt ?? taskPart(message)?.attempt ?? 1,
      )
      next = updateTaskPart(next, (current) => ({
        ...current,
        state: nextState,
        output: output ?? current.output,
        error: error ?? current.error,
        uncertainty: uncertainty ?? current.uncertainty,
        usage: usage ?? current.usage,
      }))
      if (usage) {
        next = upsertUsagePart(next, usage, "terminal")
      }
      if (output) {
        next = reconcileOutput(next, output)
      }
      // A terminal event closes the observed live runs even when the Worker did
      // not emit a completion event before stopping.
      next = closeReasoning(closeText(next))
      if (event.kind === "task.unknown") {
        const code = error?.code ?? "task.unknown"
        const duplicate = next.parts.some(
          (part) => part.type === "uncertainty" && part.code === code,
        )
        if (!duplicate) {
          next = appendPart(next, {
            type: "uncertainty",
            code,
            message:
              uncertainty ??
              "The Gateway could not confirm the task outcome. Inspect the task before retrying.",
            recovery:
              error?.recovery ??
              "Check the task on the Gateway, then start a new task if needed.",
            details: error?.details,
          })
        }
      }
      return next
    }
    case "task.resume_unavailable": {
      const info: ChatErrorInfo = {
        code: textField(payload.code) ?? "task.resume_unavailable",
        message: textField(payload.detail) ?? "The task could not resume.",
        recovery: textField(payload.guidance),
        retryable: false,
      }
      return appendPart(message, {
        type: "error",
        ...info,
        kind: "task",
      })
    }
    case "task.transfer": {
      const target = textField(payload.target) ?? "another agent"
      return appendPart(message, {
        type: "diagnostic",
        eventKind: event.kind,
        detail: `Transferred to ${target}.`,
      })
    }
    case "task.resume_accepted":
    case "task.resumed_from_checkpoint": {
      const step = numberField(payload.step)
      return appendPart(message, {
        type: "diagnostic",
        eventKind: event.kind,
        detail:
          step === undefined
            ? "Resumed from a checkpoint."
            : `Resumed from checkpoint step ${step}.`,
      })
    }
    default:
      return message
  }
}

function applyTextEvent(
  message: ChatMessage,
  event: StreamEvent,
): ChatMessage {
  const payload = isRecord(event.payload) ? event.payload : {}
  const text = textField(payload.text)
  if (!text) {
    return message
  }
  const author = textField(payload.author)
  switch (event.kind) {
    case "model.delta":
      return appendDelta(message, text, author)
    case "model.delta.complete":
    case "graph.output":
      return completeDelta(message, text, author)
    default:
      return message
  }
}

function applyReasoningEvent(
  message: ChatMessage,
  event: StreamEvent,
): ChatMessage {
  const payload = isRecord(event.payload) ? event.payload : {}
  const text = textField(payload.text)
  if (!text) {
    return message
  }
  const author = textField(payload.author)
  return event.kind === "model.thinking.complete"
    ? completeReasoning(message, text, author)
    : appendReasoning(message, text, author)
}

function gapFromEvent(event: StreamEvent): ChatStreamGap | null {
  if (event.kind !== "stream.lagged") {
    return null
  }
  const payload = isRecord(event.payload) ? event.payload : {}
  return {
    reason: textField(payload.reason) ?? "subscriber_lagged",
    fromSeq: numberField(payload.from_seq) ?? event.seq,
  }
}

export interface StreamEventResult {
  message: ChatMessage
  gap: ChatStreamGap | null
}

/**
 * Apply one Gateway stream event to the assistant message.
 *
 * The caller owns sequence de-duplication and cursor state; this function owns
 * the kind-to-part mapping and never throws on an unknown kind.
 */
export function applyStreamEvent(
  message: ChatMessage,
  event: StreamEvent,
  status: ChatRunStatus,
): StreamEventResult {
  const gap = gapFromEvent(event)
  if (gap) {
    return { message, gap }
  }

  if (event.kind.startsWith("stream.")) {
    return { message, gap: null }
  }

  if (approvalEventKinds.has(event.kind)) {
    return { message: applyApprovalEvent(message, event), gap: null }
  }

  if (event.kind === "approval.resume_unavailable") {
    const payload = isRecord(event.payload) ? event.payload : {}
    const code = textField(payload.code)
    const detail = textField(payload.message)
    return {
      message: appendDiagnostic(message, {
        type: "diagnostic",
        eventKind: event.kind,
        detail: `Approval resume unavailable${code ? ` (${code})` : ""}${detail ? `: ${detail}` : "."}`,
      }),
      gap: null,
    }
  }

  if (textKinds.has(event.kind)) {
    if (isTerminalStatus(status)) {
      return { message, gap: null }
    }
    return { message: applyTextEvent(message, event), gap: null }
  }

  if (reasoningKinds.has(event.kind)) {
    return { message: applyReasoningEvent(message, event), gap: null }
  }

  if (toolKinds.has(event.kind)) {
    return { message: applyToolEvent(message, event), gap: null }
  }

  if (planKinds.has(event.kind)) {
    return { message: applyPlanEvent(message, event), gap: null }
  }

  if (usageKinds.has(event.kind)) {
    return { message: applyUsageEvent(message, event), gap: null }
  }

  if (event.kind.startsWith("task.")) {
    return { message: applyTaskEvent(message, event), gap: null }
  }

  if (diagnosticKinds.has(event.kind)) {
    return {
      message: appendDiagnostic(message, {
        type: "diagnostic",
        eventKind: event.kind,
        detail: diagnosticDetail(event),
      }),
      gap: null,
    }
  }

  // Unknown kinds are surfaced as a diagnostic, never thrown.
  return {
    message: appendDiagnostic(message, {
      type: "diagnostic",
      eventKind: event.kind,
      detail: diagnosticDetail(event),
    }),
    gap: null,
  }
}

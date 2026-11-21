import type { TaskState } from "@goldenbee/sdk"

import type { ChatMessage, ChatUncertaintyPart } from "./chat-types"

export interface TaskStateView {
  label: string
  tone: "neutral" | "working" | "success" | "error" | "warning"
  guidance?: string
  retryable: boolean
}

/**
 * Distinct operator-facing states for task progress, failure, cancellation,
 * unknown outcomes, and recovery. Guidance never claims a task succeeded when
 * the Gateway did not confirm it.
 */
export function taskStateView(state: TaskState | undefined): TaskStateView {
  switch (state) {
    case "accepted":
      return { label: "Task accepted", tone: "working", retryable: false }
    case "queued":
      return { label: "Task queued", tone: "working", retryable: false }
    case "running":
      return { label: "Task running", tone: "working", retryable: false }
    case "waiting_approval":
      return {
        label: "Waiting for approval",
        tone: "warning",
        guidance: "Open the approval request to continue this task.",
        retryable: false,
      }
    case "cancelling":
      return {
        label: "Cancelling",
        tone: "working",
        guidance: "The Gateway is asking the Worker to stop.",
        retryable: false,
      }
    case "recovering":
      return {
        label: "Recovering",
        tone: "warning",
        guidance:
          "The Gateway is reconciling the task with the Worker. Wait for it to settle before starting another task.",
        retryable: false,
      }
    case "succeeded":
      return { label: "Task succeeded", tone: "success", retryable: false }
    case "failed":
      return {
        label: "Task failed",
        tone: "error",
        guidance: "Retry the task, or choose another Worker before sending it again.",
        retryable: true,
      }
    case "cancelled":
      return {
        label: "Task cancelled",
        tone: "error",
        guidance: "Retry to run the same prompt as a new task.",
        retryable: true,
      }
    case "unknown":
      return {
        label: "Outcome unknown",
        tone: "warning",
        guidance:
          "The Gateway could not confirm whether the task finished. Inspect the task before sending it again.",
        retryable: false,
      }
    default:
      return { label: "Sending", tone: "working", retryable: false }
  }
}

/** Submission uncertainty has no retry action until the operator inspects. */
export function uncertaintyPart(
  message: ChatMessage,
): ChatUncertaintyPart | undefined {
  return message.parts.find(
    (part): part is ChatUncertaintyPart => part.type === "uncertainty",
  )
}

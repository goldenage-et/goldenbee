import type { GatewayWorker } from "@goldenbee/sdk"

import type { ChatSelection } from "./chat-types"

export function selectableWorkers(workers: GatewayWorker[]): GatewayWorker[] {
  return workers.filter(
    (worker) => worker.reachable && worker.status !== "unreachable",
  )
}

export function agentIds(worker: GatewayWorker | undefined): string[] {
  if (!worker) {
    return []
  }
  const advertised = [
    ...(worker.capabilities ?? []),
    ...(worker.declared_capabilities ?? []),
  ]
    .filter((capability) => capability.kind === "agent")
    .map((capability) => capability.id)
  return Array.from(new Set(advertised))
}

/**
 * Resolve a usable target from the current selection and caller-visible Workers.
 * A profile is optional; the Worker applies its active profile when omitted.
 */
export function effectiveSelection(
  workers: GatewayWorker[],
  selection: ChatSelection,
): ChatSelection {
  const available = selectableWorkers(workers)
  const worker =
    available.find((candidate) => candidate.id === selection.worker) ??
    available[0]
  if (!worker) {
    return { worker: "", agent: "", profile: selection.profile }
  }
  const agents = agentIds(worker)
  return {
    worker: worker.id,
    agent: selection.agent || agents[0] || "",
    profile: selection.profile?.trim() || undefined,
  }
}

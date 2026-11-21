import type {
  GatewayCapability,
  GatewayWorker,
  Profile,
  ProfileRevision,
} from "@goldenbee/sdk"

import type { ChatSelection } from "./chat-types"

/**
 * Selection policy for the next task.
 *
 * The persisted names are treated as an explicit operator choice. A name that
 * no longer resolves disables Send instead of silently falling back to another
 * Worker, agent, or profile.
 */

export type SelectionField = "worker" | "agent" | "profile"

export type SelectionIssueCode =
  | "worker.none"
  | "worker.unavailable"
  | "agent.none"
  | "agent.unavailable"
  | "profile.none_active"
  | "profile.loading"
  | "profile.unavailable"
  | "profile.inactive"
  | "profile.catalog_unavailable"

export interface SelectionIssue {
  field: SelectionField
  code: SelectionIssueCode
  message: string
}

export interface SelectionInput {
  workers: GatewayWorker[]
  selection: ChatSelection
  /** `null` until the Worker profile catalog resolves; `[]` is a resolved empty catalog. */
  profiles?: Profile[] | null
  /** Set when the profile catalog could not be read. */
  profileError?: string | null
}

export interface ResolvedSelection {
  /** Effective names to submit. Only meaningful when `ready` is true. */
  selection: ChatSelection
  worker: GatewayWorker | null
  agents: GatewayCapability[]
  profile: Profile | null
  issues: SelectionIssue[]
  ready: boolean
}

export function selectableWorkers(workers: GatewayWorker[]): GatewayWorker[] {
  return workers.filter(
    (worker) =>
      worker.reachable &&
      (worker.status === "approved" || worker.status === "registered"),
  )
}

/** Approved capabilities only; a declared-only capability cannot be submitted. */
export function agentCapabilities(
  worker: GatewayWorker | null | undefined,
): GatewayCapability[] {
  if (!worker) {
    return []
  }
  const seen = new Set<string>()
  const agents: GatewayCapability[] = []
  for (const capability of worker.capabilities ?? []) {
    if (capability.kind !== "agent" || seen.has(capability.id)) {
      continue
    }
    seen.add(capability.id)
    agents.push(capability)
  }
  return agents
}

export function activeRevision(profile: Profile): ProfileRevision | null {
  return (
    profile.revisions.find(
      (revision) => revision.revision === profile.active_revision,
    ) ?? null
  )
}

export function latestRevision(profile: Profile): ProfileRevision | null {
  return profile.revisions.reduce<ProfileRevision | null>((latest, revision) => {
    if (!latest || revision.revision > latest.revision) {
      return revision
    }
    return latest
  }, null)
}

export function latestRevisionNumber(profile: Profile): number {
  return latestRevision(profile)?.revision ?? 0
}

function workerIssue(
  workers: GatewayWorker[],
  requested: string,
): SelectionIssue {
  const known = workers.find((candidate) => candidate.id === requested)
  return {
    field: "worker",
    code: "worker.unavailable",
    message: known
      ? `Worker ${requested} is unreachable or not accepting tasks. Choose another Worker.`
      : `Worker ${requested} is no longer visible to this caller. Reconnect with a token that can reach it.`,
  }
}

export function resolveSelection({
  workers,
  selection,
  profiles,
  profileError = null,
}: SelectionInput): ResolvedSelection {
  const issues: SelectionIssue[] = []
  const available = selectableWorkers(workers)
  const requestedWorker = selection.worker.trim()
  let worker: GatewayWorker | null = null
  let workerName = requestedWorker

  if (available.length === 0) {
    issues.push({
      field: "worker",
      code: "worker.none",
      message:
        "No reachable Worker is visible to this caller. Reconnect with a token that can reach at least one Worker.",
    })
  } else if (!requestedWorker) {
    worker = available[0] ?? null
    workerName = worker?.id ?? ""
  } else {
    worker = available.find((candidate) => candidate.id === requestedWorker) ?? null
    if (!worker) {
      issues.push(workerIssue(workers, requestedWorker))
    }
  }

  const agents = agentCapabilities(worker)
  let agent = selection.agent.trim()

  if (worker) {
    if (agents.length === 0) {
      issues.push({
        field: "agent",
        code: "agent.none",
        message: `Worker ${worker.id} advertises no agent capability. Register an agent on that Worker before sending.`,
      })
    } else if (!agent) {
      agent = agents[0]?.id ?? ""
    } else if (!agents.some((capability) => capability.id === agent)) {
      issues.push({
        field: "agent",
        code: "agent.unavailable",
        message: `Agent ${agent} is not offered by Worker ${worker.id}. Choose an advertised agent capability.`,
      })
    }
  }

  let profileName = selection.profile?.trim() ?? ""
  let profile: Profile | null = null

  if (!worker) {
    // A Worker issue already explains the blocked target; profile names belong
    // to a Worker and cannot be reconciled until one resolves.
  } else if (profiles === undefined) {
    // Profile validation was not requested; the caller still owns the name.
  } else if (profiles === null) {
    if (profileError) {
      if (profileName) {
        issues.push({
          field: "profile",
          code: "profile.catalog_unavailable",
          message: `Runtime Profiles for Worker ${workerName} could not be read, so ${profileName} cannot be validated. Choose the Worker default or reconnect with the profile:read scope.`,
        })
      }
    } else {
      issues.push({
        field: "profile",
        code: "profile.loading",
        message: "Loading the Worker's Runtime Profiles…",
      })
    }
  } else if (profileName) {
    profile = profiles.find((candidate) => candidate.name === profileName) ?? null
    if (!profile) {
      issues.push({
        field: "profile",
        code: "profile.unavailable",
        message: `Runtime Profile ${profileName} is not available on Worker ${workerName}. Choose another profile or activate one on the Worker.`,
      })
    } else if (profile.active_revision < 1) {
      issues.push({
        field: "profile",
        code: "profile.inactive",
        message: `Runtime Profile ${profileName} has no active revision. Activate revision ${latestRevisionNumber(profile)} before sending.`,
      })
    }
  } else {
    profile = profiles.find((candidate) => candidate.active_revision > 0) ?? null
    if (profile) {
      profileName = profile.name
    } else {
      issues.push({
        field: "profile",
        code: "profile.none_active",
        message: `Worker ${workerName} has no active Runtime Profile. Activate one on the Worker before sending.`,
      })
    }
  }

  return {
    selection: {
      worker: workerName,
      agent,
      profile: profileName || undefined,
    },
    worker,
    agents,
    profile,
    issues,
    ready: Boolean(worker && agent) && issues.length === 0,
  }
}

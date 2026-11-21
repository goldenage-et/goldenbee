import type {
  CreateTeamRequest,
  TeamMember,
  UpdateTeamRequest,
} from "@goldenbee/sdk"

export type TeamLocality = "any" | "local" | "remote"

export interface TeamMemberDraft {
  worker: string
  role: string
  /** Comma-separated capability ids. */
  capabilities: string
  maxDepth: string
}

export interface TeamFormValues {
  name: string
  master: string
  locality: TeamLocality
  /** Roster members, including the Master when the operator pins its capabilities. */
  members: TeamMemberDraft[]
  maxDepth: string
  maxFanout: string
}

export type TeamFormField =
  | "name"
  | "master"
  | "members"
  | "max_depth"
  | "max_fanout"
  | `member-${number}-worker`
  | `member-${number}-capabilities`
  | `member-${number}-max_depth`

export interface TeamFormIssue {
  field: TeamFormField
  message: string
}

export interface TeamRequestBuild<T> {
  request?: T
  issues: TeamFormIssue[]
}

export function emptyTeamMemberDraft(): TeamMemberDraft {
  return { worker: "", role: "", capabilities: "", maxDepth: "" }
}

export function emptyTeamFormValues(): TeamFormValues {
  return {
    name: "",
    master: "",
    locality: "any",
    members: [emptyTeamMemberDraft()],
    maxDepth: "",
    maxFanout: "",
  }
}

function hasInvalidTeamNameCharacter(value: string): boolean {
  for (const character of value) {
    const code = character.codePointAt(0) ?? 0
    if (character === "/" || character === "\\" || code <= 0x1f || code === 0x7f) {
      return true
    }
  }
  return false
}

function splitList(value: string): string[] {
  return value
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean)
}

function parseWhole(
  raw: string,
): { value: number } | { error: "not-a-number" | "negative" } | undefined {
  const trimmed = raw.trim()
  if (!trimmed) {
    return undefined
  }
  const value = Number(trimmed)
  if (!Number.isInteger(value)) {
    return { error: "not-a-number" }
  }
  if (value < 0) {
    return { error: "negative" }
  }
  return { value }
}

/**
 * Validate the operator's roster before a Gateway request.
 *
 * The Gateway remains authoritative for Worker visibility, declared
 * capabilities, and locality. This catches the documented request shape so an
 * obviously invalid form never becomes a network round trip.
 */
export function validateTeamForm(values: TeamFormValues): TeamFormIssue[] {
  const issues: TeamFormIssue[] = []
  const name = values.name.trim()
  if (!name) {
    issues.push({ field: "name", message: "Enter a team name." })
  } else if (name.length > 100) {
    issues.push({
      field: "name",
      message: "Team names are limited to 100 characters.",
    })
  } else if (hasInvalidTeamNameCharacter(name)) {
    issues.push({
      field: "name",
      message: "Team names cannot contain path separators or control characters.",
    })
  }

  const master = values.master.trim()
  if (!master) {
    issues.push({ field: "master", message: "Designate a Master Worker." })
  }

  let teamDepth: number | undefined
  const depth = parseWhole(values.maxDepth)
  if (depth && "error" in depth) {
    issues.push({
      field: "max_depth",
      message: "Max depth must be a whole number.",
    })
  } else if (depth && depth.value > 16) {
    issues.push({
      field: "max_depth",
      message: "Max depth cannot exceed 16.",
    })
  } else if (depth) {
    teamDepth = depth.value
  }

  const seen = new Set<string>()
  let subworkers = 0
  values.members.forEach((member, index) => {
    const worker = member.worker.trim()
    if (!worker) {
      issues.push({
        field: `member-${index}-worker`,
        message: "Enter a Worker id for this member.",
      })
    } else if (seen.has(worker)) {
      issues.push({
        field: `member-${index}-worker`,
        message: `Worker '${worker}' is listed more than once.`,
      })
    } else {
      seen.add(worker)
      if (worker !== master) {
        subworkers += 1
      }
    }

    if (splitList(member.capabilities).length === 0) {
      issues.push({
        field: `member-${index}-capabilities`,
        message: "Authorize at least one capability for this member.",
      })
    }

    const memberDepth = parseWhole(member.maxDepth)
    if (memberDepth && "error" in memberDepth) {
      issues.push({
        field: `member-${index}-max_depth`,
        message: "Member depth must be a whole number.",
      })
    } else if (
      memberDepth &&
      memberDepth.value > 16
    ) {
      issues.push({
        field: `member-${index}-max_depth`,
        message: "Member depth cannot exceed 16.",
      })
    } else if (
      memberDepth &&
      teamDepth !== undefined &&
      teamDepth > 0 &&
      memberDepth.value > 0 &&
      memberDepth.value > teamDepth
    ) {
      issues.push({
        field: `member-${index}-max_depth`,
        message: "Member depth cannot exceed the team max depth.",
      })
    }
  })

  if (master && subworkers === 0) {
    issues.push({
      field: "members",
      message: "Add at least one member that is not the Master.",
    })
  }

  const fanout = parseWhole(values.maxFanout)
  if (fanout && "error" in fanout) {
    issues.push({
      field: "max_fanout",
      message: "Max fanout must be a whole number.",
    })
  } else if (fanout && fanout.value < subworkers) {
    issues.push({
      field: "max_fanout",
      message: "Max fanout cannot be below the number of subworkers.",
    })
  }

  return issues
}

function buildMembers(values: TeamFormValues): TeamMember[] {
  return values.members
    .map((member) => {
      const maxDepth = parseWhole(member.maxDepth)
      const role = member.role.trim()
      return {
        worker: member.worker.trim(),
        ...(role ? { role } : {}),
        capabilities: splitList(member.capabilities),
        ...(maxDepth && "value" in maxDepth && maxDepth.value > 0
          ? { max_depth: maxDepth.value }
          : {}),
      }
    })
    .filter((member) => member.worker.length > 0)
}

function buildSpec(values: TeamFormValues): {
  master: string
  members: TeamMember[]
  locality?: "local" | "remote"
  limits?: { max_depth?: number; max_fanout?: number }
} {
  const depth = parseWhole(values.maxDepth)
  const fanout = parseWhole(values.maxFanout)
  const limits = {
    ...(depth && "value" in depth && depth.value > 0
      ? { max_depth: depth.value }
      : {}),
    ...(fanout && "value" in fanout && fanout.value > 0
      ? { max_fanout: fanout.value }
      : {}),
  }
  return {
    master: values.master.trim(),
    members: buildMembers(values),
    ...(values.locality === "any" ? {} : { locality: values.locality }),
    ...(Object.keys(limits).length > 0 ? { limits } : {}),
  }
}

export function buildCreateTeamRequest(
  values: TeamFormValues,
): TeamRequestBuild<CreateTeamRequest> {
  const issues = validateTeamForm(values)
  if (issues.length > 0) {
    return { issues }
  }
  return {
    issues,
    request: {
      name: values.name.trim(),
      ...buildSpec(values),
    },
  }
}

export function buildUpdateTeamRequest(
  values: TeamFormValues,
  expectedVersion: number,
): TeamRequestBuild<UpdateTeamRequest> {
  const issues = validateTeamForm(values)
  if (issues.length > 0) {
    return { issues }
  }
  return {
    issues,
    request: {
      expected_version: expectedVersion,
      ...buildSpec(values),
    },
  }
}

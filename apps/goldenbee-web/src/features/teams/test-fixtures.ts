import {
  GatewayError,
  type CreateTeamRequest,
  type Team,
  type TeamMember,
  type TeamVersionRequest,
  type UpdateTeamRequest,
} from "@goldenbee/sdk"

import type { TeamAdminClient } from "./team-client"

export function teamMemberFixture(
  overrides: Partial<TeamMember> = {},
): TeamMember {
  return {
    worker: "research-b",
    role: "analyst",
    capabilities: ["research"],
    max_depth: 2,
    ...overrides,
  }
}

export function teamFixture(overrides: Partial<Team> = {}): Team {
  return {
    namespace: "local",
    name: "research",
    version: 4,
    master: "general-a",
    members: [
      teamMemberFixture({
        worker: "general-a",
        role: "master",
        capabilities: ["general"],
      }),
      teamMemberFixture(),
    ],
    limits: { max_depth: 3, max_fanout: 2 },
    locality: "local",
    active: true,
    created_at: 1_700_000_000_000,
    updated_at: 1_700_000_050_000,
    ...overrides,
  }
}

export interface FakeTeamClient extends TeamAdminClient {
  calls: {
    teams: number
    team: string[]
    create: CreateTeamRequest[]
    update: Array<{ name: string; request: UpdateTeamRequest }>
    validate: Array<{ name: string; version?: number }>
    activate: Array<{ name: string; version?: number }>
    deactivate: Array<{ name: string; version?: number }>
  }
}

function observed(team: Team, request?: TeamVersionRequest): Team {
  if (request?.version !== undefined && request.version !== team.version) {
    throw teamVersionConflict()
  }
  return team
}

/** Whole-feature fake for the team administration surface. */
export function createFakeTeamClient(
  initial: Team[] = [teamFixture()],
): FakeTeamClient {
  let teams = initial.map((team) => structuredClone(team))
  const calls: FakeTeamClient["calls"] = {
    teams: 0,
    team: [],
    create: [],
    update: [],
    validate: [],
    activate: [],
    deactivate: [],
  }

  function find(name: string): Team {
    const found = teams.find((team) => team.name === name)
    if (!found) {
      throw teamNotFound()
    }
    return found
  }

  function replace(next: Team): Team {
    teams = teams.map((team) => (team.name === next.name ? next : team))
    return structuredClone(next)
  }

  return {
    calls,
    async teams() {
      calls.teams += 1
      return teams.map((team) => structuredClone(team))
    },
    async team(name) {
      calls.team.push(name)
      return structuredClone(find(name))
    },
    async createTeam(request) {
      calls.create.push(request)
      if (teams.some((team) => team.name === request.name)) {
        throw teamConflict()
      }
      const created = teamFixture({
        name: request.name,
        version: 1,
        master: request.master,
        members: request.members,
        limits: request.limits,
        locality: request.locality,
        active: false,
        created_at: 1_700_000_100_000,
        updated_at: 1_700_000_100_000,
      })
      teams = [...teams, created]
      return structuredClone(created)
    },
    async updateTeam(name, request) {
      calls.update.push({ name, request })
      const current = find(name)
      if (request.expected_version !== current.version) {
        throw teamVersionConflict()
      }
      return replace({
        ...current,
        version: current.version + 1,
        master: request.master,
        members: request.members,
        limits: request.limits,
        locality: request.locality,
        updated_at: 1_700_000_200_000,
      })
    },
    async validateTeam(name, request) {
      calls.validate.push({ name, version: request?.version })
      return structuredClone(observed(find(name), request))
    },
    async activateTeam(name, request) {
      calls.activate.push({ name, version: request?.version })
      return replace({ ...observed(find(name), request), active: true })
    },
    async deactivateTeam(name, request) {
      calls.deactivate.push({ name, version: request?.version })
      return replace({ ...observed(find(name), request), active: false })
    },
  }
}

export function teamNotFound(): GatewayError {
  return sdkFailure({
    kind: "not-found",
    status: 404,
    code: "team.not_found",
    message: "team is not visible",
    recovery: "refresh the team list",
  })
}

export function teamConflict(): GatewayError {
  return sdkFailure({
    kind: "conflict",
    status: 409,
    code: "team.conflict",
    message: "team already exists",
    recovery: "choose another team name",
  })
}

export function teamVersionConflict(): GatewayError {
  return sdkFailure({
    kind: "conflict",
    status: 409,
    code: "team.version_conflict",
    message: "team was updated by another writer",
    recovery: "reload the roster and reapply the change",
    details: { expected_version: "4", current_version: "5" },
  })
}

export function scopeDenied(): GatewayError {
  return sdkFailure({
    kind: "forbidden",
    status: 403,
    code: "scope.team_write",
    message: "caller token is not allowed to administer teams",
    recovery: "use a caller token with the team:write scope",
  })
}

export function sdkFailure(options: {
  kind:
    | "not-found"
    | "conflict"
    | "forbidden"
    | "unauthorized"
    | "server"
    | "network"
    | "invalid-response"
    | "protocol"
  status?: number
  code: string
  message: string
  recovery?: string
  details?: Record<string, string>
  retryable?: boolean
}): GatewayError {
  return new GatewayError(options)
}

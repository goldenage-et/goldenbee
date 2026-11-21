import type {
  CreateTeamRequest,
  Team,
  TeamVersionRequest,
  UpdateTeamRequest,
} from "@goldenbee/sdk"

/**
 * The team administration slice of the Gateway SDK.
 *
 * The feature depends on this narrow interface rather than the full client so
 * pages can be exercised against a whole-feature fake without a network.
 */
export interface TeamAdminClient {
  teams(): Promise<Team[]>
  team(name: string): Promise<Team>
  createTeam(request: CreateTeamRequest): Promise<Team>
  updateTeam(name: string, request: UpdateTeamRequest): Promise<Team>
  validateTeam(name: string, request?: TeamVersionRequest): Promise<Team>
  activateTeam(name: string, request?: TeamVersionRequest): Promise<Team>
  deactivateTeam(name: string, request?: TeamVersionRequest): Promise<Team>
}

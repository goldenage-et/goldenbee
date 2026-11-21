import {
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query"
import type {
  CreateTeamRequest,
  Team,
  TeamVersionRequest,
  UpdateTeamRequest,
} from "@goldenbee/sdk"

import type { TeamAdminClient } from "./team-client"

export const teamKeys = {
  all: ["teams"] as const,
  list: () => [...teamKeys.all, "list"] as const,
  detail: (name: string) => [...teamKeys.all, "detail", name] as const,
}

export function useTeamsQuery(client: TeamAdminClient) {
  return useQuery({
    queryKey: teamKeys.list(),
    queryFn: () => client.teams(),
    retry: false,
  })
}

export function useTeamQuery(client: TeamAdminClient, name: string) {
  return useQuery({
    queryKey: teamKeys.detail(name),
    queryFn: () => client.team(name),
    enabled: name.length > 0,
    retry: false,
  })
}

/**
 * A team mutation always changes a fact the list renders (version, state, or
 * membership), so both projections are refetched from Gateway instead of being
 * guessed locally.
 */
function refresh(queryClient: ReturnType<typeof useQueryClient>, team: Team) {
  queryClient.setQueryData(teamKeys.detail(team.name), team)
  void queryClient.invalidateQueries({ queryKey: teamKeys.list() })
}

export function useCreateTeam(client: TeamAdminClient) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (request: CreateTeamRequest) => client.createTeam(request),
    retry: false,
    onSuccess: (team) => {
      queryClient.setQueryData(teamKeys.detail(team.name), team)
      void queryClient.invalidateQueries({ queryKey: teamKeys.list() })
    },
  })
}

export function useUpdateTeam(client: TeamAdminClient) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({
      name,
      request,
    }: {
      name: string
      request: UpdateTeamRequest
    }) => client.updateTeam(name, request),
    retry: false,
    onSuccess: (team) => refresh(queryClient, team),
    onError: (_error, variables) => {
      // A stale roster is the one failure where the observed version must be
      // replaced with Gateway's current one before the next attempt.
      void queryClient.invalidateQueries({
        queryKey: teamKeys.detail(variables.name),
      })
      void queryClient.invalidateQueries({ queryKey: teamKeys.list() })
    },
  })
}

type TeamVersionInput = { name: string; request?: TeamVersionRequest }

function useTeamVersionMutation(
  run: (input: TeamVersionInput) => Promise<Team>,
) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: run,
    retry: false,
    onSuccess: (team) => refresh(queryClient, team),
    onError: (_error, variables) => {
      void queryClient.invalidateQueries({
        queryKey: teamKeys.detail(variables.name),
      })
      void queryClient.invalidateQueries({ queryKey: teamKeys.list() })
    },
  })
}

export function useValidateTeam(client: TeamAdminClient) {
  return useTeamVersionMutation(({ name, request }) =>
    client.validateTeam(name, request),
  )
}

export function useActivateTeam(client: TeamAdminClient) {
  return useTeamVersionMutation(({ name, request }) =>
    client.activateTeam(name, request),
  )
}

export function useDeactivateTeam(client: TeamAdminClient) {
  return useTeamVersionMutation(({ name, request }) =>
    client.deactivateTeam(name, request),
  )
}

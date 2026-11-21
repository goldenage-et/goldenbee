import {
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query"
import type {
  CreateProfileRequest,
  Profile,
  ProfileVersionRequest,
  ReviseProfileRequest,
} from "@goldenbee/sdk"

import type { RuntimeProfileClient } from "./profile-client"

export const profileKeys = {
  all: ["runtime-profiles"] as const,
  list: (worker: string) => [...profileKeys.all, "list", worker] as const,
  detail: (worker: string, name: string) =>
    [...profileKeys.all, "detail", worker, name] as const,
}

export function useProfilesQuery(
  client: RuntimeProfileClient,
  worker: string,
) {
  return useQuery({
    queryKey: profileKeys.list(worker),
    queryFn: () => client.profiles(worker),
    enabled: worker.length > 0,
    retry: false,
  })
}

export function useProfileQuery(
  client: RuntimeProfileClient,
  worker: string,
  name: string,
) {
  return useQuery({
    queryKey: profileKeys.detail(worker, name),
    queryFn: () => client.profile(worker, name),
    enabled: worker.length > 0 && name.length > 0,
    retry: false,
  })
}

function refresh(
  queryClient: ReturnType<typeof useQueryClient>,
  worker: string,
  profile: Profile,
) {
  queryClient.setQueryData(profileKeys.detail(worker, profile.name), profile)
  void queryClient.invalidateQueries({ queryKey: profileKeys.list(worker) })
}

export function useCreateProfile(client: RuntimeProfileClient) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({
      worker,
      request,
    }: {
      worker: string
      request: CreateProfileRequest
    }) => client.createProfile(worker, request),
    retry: false,
    onSuccess: (profile, variables) =>
      refresh(queryClient, variables.worker, profile),
  })
}

export function useReviseProfile(client: RuntimeProfileClient) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({
      worker,
      name,
      request,
    }: {
      worker: string
      name: string
      request: ReviseProfileRequest
    }) => client.reviseProfile(worker, name, request),
    retry: false,
    onSuccess: (profile, variables) =>
      refresh(queryClient, variables.worker, profile),
    onError: (_error, variables) => {
      // A stale revision must be replaced with the Worker's observed revision
      // before the operator can retry.
      void queryClient.invalidateQueries({
        queryKey: profileKeys.detail(variables.worker, variables.name),
      })
      void queryClient.invalidateQueries({
        queryKey: profileKeys.list(variables.worker),
      })
    },
  })
}

type VersionInput = {
  worker: string
  name: string
  request?: ProfileVersionRequest
}

function useVersionMutation(run: (input: VersionInput) => Promise<Profile>) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: run,
    retry: false,
    onSuccess: (profile, variables) =>
      refresh(queryClient, variables.worker, profile),
    onError: (_error, variables) => {
      void queryClient.invalidateQueries({
        queryKey: profileKeys.detail(variables.worker, variables.name),
      })
      void queryClient.invalidateQueries({
        queryKey: profileKeys.list(variables.worker),
      })
    },
  })
}

export function useValidateProfile(client: RuntimeProfileClient) {
  return useVersionMutation(({ worker, name, request }) =>
    client.validateProfile(worker, name, request),
  )
}

export function useActivateProfile(client: RuntimeProfileClient) {
  return useVersionMutation(({ worker, name, request }) =>
    client.activateProfile(worker, name, request),
  )
}

export function useDeactivateProfile(client: RuntimeProfileClient) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ worker, name }: { worker: string; name: string }) =>
      client.deactivateProfile(worker, name),
    retry: false,
    onSuccess: (profile, variables) =>
      refresh(queryClient, variables.worker, profile),
    onError: (_error, variables) => {
      void queryClient.invalidateQueries({
        queryKey: profileKeys.detail(variables.worker, variables.name),
      })
      void queryClient.invalidateQueries({
        queryKey: profileKeys.list(variables.worker),
      })
    },
  })
}

export function useDeleteProfile(client: RuntimeProfileClient) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ worker, name }: { worker: string; name: string }) =>
      client.deleteProfile(worker, name),
    retry: false,
    onSuccess: (_deleted, variables) => {
      queryClient.removeQueries({
        queryKey: profileKeys.detail(variables.worker, variables.name),
      })
      void queryClient.invalidateQueries({
        queryKey: profileKeys.list(variables.worker),
      })
    },
  })
}

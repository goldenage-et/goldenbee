import {
  useMutation,
  useQuery,
  useQueryClient,
  type QueryClient,
} from "@tanstack/react-query"
import type {
  CreateProviderRequest,
  ProviderCredentialSummary,
  ProviderMetadata,
  UpdateProviderRequest,
} from "@goldenbee/sdk"

import { isRevisionConflict } from "./provider-errors"
import type { ProviderAdminClient } from "./provider-client"

export const providerKeys = {
  all: ["providers"] as const,
  list: () => [...providerKeys.all, "list"] as const,
  detail: (id: string) => [...providerKeys.all, "detail", id] as const,
}

export function useProvidersQuery(client: ProviderAdminClient) {
  return useQuery({
    queryKey: providerKeys.list(),
    queryFn: () => client.providers(),
    retry: false,
  })
}

export function useProviderQuery(client: ProviderAdminClient, id: string) {
  return useQuery({
    queryKey: providerKeys.detail(id),
    queryFn: () => client.provider(id),
    retry: false,
  })
}

function project(
  queryClient: QueryClient,
  provider: ProviderMetadata,
): void {
  queryClient.setQueryData(providerKeys.detail(provider.id), provider)
  // A mutation always advances discovery, credential, or revision state that a
  // list row renders, so the list projection is refetched from Gateway.
  void queryClient.invalidateQueries({ queryKey: providerKeys.list() })
}

/** Refetch after a stale revision so the next attempt carries the observed one. */
function reconcile(queryClient: QueryClient, id: string): void {
  void queryClient.invalidateQueries({ queryKey: providerKeys.detail(id) })
  void queryClient.invalidateQueries({ queryKey: providerKeys.list() })
}

export function useUpdateProvider(client: ProviderAdminClient) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({
      id,
      request,
    }: {
      id: string
      request: UpdateProviderRequest
    }) => client.updateProvider(id, request),
    onSuccess: (provider) => project(queryClient, provider),
    onError: (error, variables) => {
      if (isRevisionConflict(error)) {
        reconcile(queryClient, variables.id)
      }
    },
  })
}

export function useCreateProvider(client: ProviderAdminClient) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (request: CreateProviderRequest) => client.createProvider(request),
    onSuccess: (provider) => project(queryClient, provider),
  })
}

export function useDiscoverProvider(client: ProviderAdminClient) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => client.discoverProvider(id),
    onSuccess: (provider) => project(queryClient, provider),
  })
}

export interface CredentialWrite {
  id: string
  key: string
}

export function useSetProviderCredential(client: ProviderAdminClient) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, key }: CredentialWrite) =>
      client.setProviderCredential(id, key),
    onSuccess: (_summary, variables) => {
      // The credential value lives only in the request body. The projection is
      // refetched; the summary contains a fingerprint and never the key.
      void queryClient.invalidateQueries({
        queryKey: providerKeys.detail(variables.id),
      })
      void queryClient.invalidateQueries({ queryKey: providerKeys.list() })
    },
  })
}

export function useRevokeProviderCredential(client: ProviderAdminClient) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => client.revokeProviderCredential(id),
    onSuccess: (summary: ProviderCredentialSummary) => {
      void queryClient.invalidateQueries({
        queryKey: providerKeys.detail(summary.provider),
      })
      void queryClient.invalidateQueries({ queryKey: providerKeys.list() })
    },
  })
}

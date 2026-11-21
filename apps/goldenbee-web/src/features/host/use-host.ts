import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"

import type {
  GatewayClient,
  HostProvisionRequest,
  HostRollbackRequest,
  HostUpgradeRequest,
  PackageInstallRequest,
} from "@goldenbee/sdk"

export const hostQueryKeys = {
  all: ["host"] as const,
  packages: ["host", "packages"] as const,
  instances: ["host", "instances"] as const,
  instance: (name: string) => ["host", "instances", name] as const,
  logs: (name: string, tail: number) =>
    ["host", "instances", name, "logs", tail] as const,
}

export function usePackages(client: GatewayClient) {
  return useQuery({
    queryKey: hostQueryKeys.packages,
    queryFn: ({ signal }) => client.packages({ signal }),
    retry: false,
  })
}

export function useInstances(client: GatewayClient) {
  return useQuery({
    queryKey: hostQueryKeys.instances,
    queryFn: ({ signal }) => client.instances({ signal }),
    retry: false,
  })
}

export function useInstance(client: GatewayClient, name: string) {
  return useQuery({
    queryKey: hostQueryKeys.instance(name),
    queryFn: ({ signal }) => client.localInstance(name, { signal }),
    enabled: name.length > 0,
    retry: false,
  })
}

export function useInstanceLogs(
  client: GatewayClient,
  name: string,
  tail: number,
  enabled = true,
) {
  return useQuery({
    queryKey: hostQueryKeys.logs(name, tail),
    queryFn: ({ signal }) => client.instanceLogs(name, tail, { signal }),
    enabled: enabled && name.length > 0,
    retry: false,
  })
}

function useHostMutation<TVariables, TResult>(
  run: (variables: TVariables) => Promise<TResult>,
) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: run,
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: hostQueryKeys.all })
    },
  })
}

export function useInstallPackage(client: GatewayClient) {
  return useHostMutation((request: PackageInstallRequest) =>
    client.installPackage(request),
  )
}

export function useProvisionInstance(client: GatewayClient) {
  return useHostMutation((request: HostProvisionRequest) =>
    client.provisionLocalInstance(request),
  )
}

export function useStartInstance(client: GatewayClient) {
  return useHostMutation((name: string) => client.startInstance(name))
}

export function useStopInstance(client: GatewayClient) {
  return useHostMutation((name: string) => client.stopInstance(name))
}

export function useRestartInstance(client: GatewayClient) {
  return useHostMutation((name: string) => client.restartInstance(name))
}

export function useUpgradeInstance(client: GatewayClient) {
  return useHostMutation(
    (input: { name: string; request: HostUpgradeRequest }) =>
      client.upgradeLocalInstance(input.name, input.request),
  )
}

export function useRollbackInstance(client: GatewayClient) {
  return useHostMutation(
    (input: { name: string; request: HostRollbackRequest }) =>
      client.rollbackLocalInstance(input.name, input.request),
  )
}

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"

import { useGatewayClient } from "@/features/connection/use-gateway-client"
import type {
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

export function usePackages() {
  const client = useGatewayClient()
  return useQuery({
    queryKey: hostQueryKeys.packages,
    queryFn: ({ signal }) => client.packages({ signal }),
    retry: false,
  })
}

export function useInstances() {
  const client = useGatewayClient()
  return useQuery({
    queryKey: hostQueryKeys.instances,
    queryFn: ({ signal }) => client.instances({ signal }),
    retry: false,
  })
}

export function useInstance(name: string) {
  const client = useGatewayClient()
  return useQuery({
    queryKey: hostQueryKeys.instance(name),
    queryFn: ({ signal }) => client.localInstance(name, { signal }),
    enabled: name.length > 0,
    retry: false,
  })
}

export function useInstanceLogs(name: string, tail: number, enabled = true) {
  const client = useGatewayClient()
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

export function useInstallPackage() {
  const client = useGatewayClient()
  return useHostMutation((request: PackageInstallRequest) =>
    client.installPackage(request),
  )
}

export function useProvisionInstance() {
  const client = useGatewayClient()
  return useHostMutation((request: HostProvisionRequest) =>
    client.provisionLocalInstance(request),
  )
}

export function useStartInstance() {
  const client = useGatewayClient()
  return useHostMutation((name: string) => client.startInstance(name))
}

export function useStopInstance() {
  const client = useGatewayClient()
  return useHostMutation((name: string) => client.stopInstance(name))
}

export function useRestartInstance() {
  const client = useGatewayClient()
  return useHostMutation((name: string) => client.restartInstance(name))
}

export function useUpgradeInstance() {
  const client = useGatewayClient()
  return useHostMutation(
    (input: { name: string; request: HostUpgradeRequest }) =>
      client.upgradeLocalInstance(input.name, input.request),
  )
}

export function useRollbackInstance() {
  const client = useGatewayClient()
  return useHostMutation(
    (input: { name: string; request: HostRollbackRequest }) =>
      client.rollbackLocalInstance(input.name, input.request),
  )
}

import {
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query"
import {
  isGatewayError,
  type GatewayRequestOptions,
  type Profile,
  type ProfileVersionRequest,
} from "@goldenbee/sdk"

/**
 * The Worker-owned Runtime Profile slice the selection screen needs.
 *
 * Profile catalogs are read from the selected Worker and refreshed after an
 * activation. A profile is never inferred from a different Worker.
 */
export interface SelectionGateway {
  profiles(
    worker: string,
    options?: GatewayRequestOptions,
  ): Promise<Profile[]>
  activateProfile(
    worker: string,
    name: string,
    request?: ProfileVersionRequest,
    options?: GatewayRequestOptions,
  ): Promise<Profile>
}

export const chatProfileKeys = {
  all: ["chat", "profiles"] as const,
  worker: (worker: string) => [...chatProfileKeys.all, worker] as const,
}

export function useWorkerProfiles(
  client: SelectionGateway | null,
  worker: string,
) {
  return useQuery({
    queryKey: chatProfileKeys.worker(worker),
    queryFn: ({ signal }) => client!.profiles(worker, { signal }),
    enabled: client !== null && worker.length > 0,
    retry: false,
  })
}

export interface ActivateProfileInput {
  worker: string
  name: string
  revision: number
}

export function useActivateProfile(client: SelectionGateway | null) {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: (input: ActivateProfileInput) => {
      if (!client) {
        return Promise.reject(new Error("No Gateway client is available."))
      }
      return client.activateProfile(input.worker, input.name, {
        revision: input.revision,
      })
    },
    retry: false,
    onSuccess: (profile, input) => {
      queryClient.setQueryData<Profile[]>(
        chatProfileKeys.worker(input.worker),
        (profiles) => {
          if (!profiles) {
            return [profile]
          }
          return profiles.map((candidate) =>
            candidate.name === profile.name ? profile : candidate,
          )
        },
      )
    },
    onError: (_error, input) => {
      // Activation can fail on a stale revision. Refresh the observed catalog
      // so the next attempt carries the revision the Worker actually has.
      void queryClient.invalidateQueries({
        queryKey: chatProfileKeys.worker(input.worker),
      })
    },
  })
}

export interface ProfileErrorView {
  code?: string
  message: string
  recovery?: string
  /** The caller token cannot perform this profile action. */
  denied: boolean
}

export function profileErrorView(cause: unknown): ProfileErrorView {
  if (isGatewayError(cause)) {
    return {
      code: cause.code,
      message: cause.message,
      recovery: cause.recovery,
      denied:
        cause.kind === "forbidden" ||
        cause.kind === "unauthorized" ||
        Boolean(cause.code?.startsWith("scope.")),
    }
  }
  return {
    message:
      cause instanceof Error && cause.message
        ? cause.message
        : "The Runtime Profile request failed.",
    denied: false,
  }
}

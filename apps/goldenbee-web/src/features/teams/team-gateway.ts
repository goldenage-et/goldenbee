import { createGatewayClient } from "@goldenbee/sdk"

import type { TeamAdminClient } from "./team-client"

export interface TeamClientOptions {
  baseUrl: string
  token: string
}

export type TeamClientFactory = (options: TeamClientOptions) => TeamAdminClient

/** Browser factory for team administration; the SDK stays platform-neutral. */
export function createTeamGatewayClient(
  options: TeamClientOptions,
): TeamAdminClient {
  return createGatewayClient({
    baseUrl: options.baseUrl,
    token: options.token,
    fetch: (input, init) => fetch(input, init),
  })
}

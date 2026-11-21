import { createGatewayClient } from "@goldenbee/sdk"

import type { RuntimeProfileClient } from "./profile-client"

export interface ProfileClientOptions {
  baseUrl: string
  token: string
}

export type ProfileClientFactory = (
  options: ProfileClientOptions,
) => RuntimeProfileClient

/** Browser factory for Worker-owned Runtime Profile administration. */
export function createProfileGatewayClient(
  options: ProfileClientOptions,
): RuntimeProfileClient {
  return createGatewayClient({
    baseUrl: options.baseUrl,
    token: options.token,
    fetch: (input, init) => fetch(input, init),
  })
}

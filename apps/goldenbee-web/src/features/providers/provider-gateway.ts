import { createGatewayClient } from "@goldenbee/sdk"

import type { ProviderAdminClient } from "@/features/providers/provider-client"

export interface ProviderClientOptions {
  baseUrl: string
  token: string
}

export type ProviderClientFactory = (
  options: ProviderClientOptions,
) => ProviderAdminClient

/**
 * Browser factory for provider administration. The SDK stays platform-neutral;
 * the browser supplies fetch here.
 */
export function createProviderGatewayClient(
  options: ProviderClientOptions,
): ProviderAdminClient {
  return createGatewayClient({
    baseUrl: options.baseUrl,
    token: options.token,
    fetch: (input, init) => fetch(input, init),
  })
}

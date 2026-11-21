import type {
  CreateProviderRequest,
  ProviderCredentialSummary,
  ProviderMetadata,
  UpdateProviderRequest,
} from "@goldenbee/sdk"

/**
 * The provider administration slice of the Gateway SDK.
 *
 * The feature depends on this narrow interface rather than the full client, so
 * screens can be exercised against recorded projections without a network.
 */
export interface ProviderAdminClient {
  providers(): Promise<ProviderMetadata[]>
  provider(id: string): Promise<ProviderMetadata>
  createProvider(request: CreateProviderRequest): Promise<ProviderMetadata>
  updateProvider(
    id: string,
    request: UpdateProviderRequest,
  ): Promise<ProviderMetadata>
  discoverProvider(id: string): Promise<ProviderMetadata>
  setProviderCredential(
    id: string,
    key: string,
  ): Promise<ProviderCredentialSummary>
  revokeProviderCredential(id: string): Promise<ProviderCredentialSummary>
}

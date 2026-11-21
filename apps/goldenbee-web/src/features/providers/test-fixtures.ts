import {
  GatewayError,
  type CreateProviderRequest,
  type ProviderCredentialSummary,
  type ProviderMetadata,
  type UpdateProviderRequest,
} from "@goldenbee/sdk"

import type { ProviderAdminClient } from "./provider-client"

export function providerFixture(
  overrides: Partial<ProviderMetadata> = {},
): ProviderMetadata {
  return {
    id: "openrouter",
    adapter: "openrouter",
    model: "openai/gpt-4.1",
    endpoint: "https://openrouter.ai/api/v1",
    modalities: ["text"],
    tool_calling: true,
    context_window: 128_000,
    local: false,
    residency: "remote",
    allowed_workers: ["general-a"],
    models: ["openai/gpt-4.1", "anthropic/claude-sonnet-4"],
    models_source: "discovered",
    discovered_at: 1_700_000_000_000,
    allowed_models: ["openai/gpt-4.1", "anthropic/claude-sonnet-4"],
    allow_auto_default: false,
    ready: true,
    revision: 4,
    active: true,
    credential_version: 2,
    credential_fingerprint: "sha256:8f14e45fceea167a",
    credential_revoked: false,
    created_at: 1_700_000_000_000,
    updated_at: 1_700_000_010_000,
    ...overrides,
  }
}

export function credentialFixture(
  overrides: Partial<ProviderCredentialSummary> = {},
): ProviderCredentialSummary {
  return {
    provider: "openrouter",
    version: 3,
    fingerprint: "sha256:deadbeefcafebabe",
    created_at: 1_700_000_020_000,
    revision: 5,
    ready: true,
    models: ["openai/gpt-4.1"],
    ...overrides,
  }
}

export interface FakeProviderClient extends ProviderAdminClient {
  calls: {
    providers: number
    provider: string[]
    create: CreateProviderRequest[]
    update: Array<{ id: string; request: UpdateProviderRequest }>
    discover: string[]
    credential: Array<{ id: string; key: string }>
    revoke: string[]
  }
}

/**
 * Test seam: a whole-feature fake for the provider administration surface.
 * Behaviour is overridden per test with a plain SDK-shaped failure.
 */
export function createFakeProviderClient(
  initial: ProviderMetadata[] = [providerFixture()],
): FakeProviderClient {
  let catalog = initial.map((provider) => ({ ...provider }))
  const calls: FakeProviderClient["calls"] = {
    providers: 0,
    provider: [],
    create: [],
    update: [],
    discover: [],
    credential: [],
    revoke: [],
  }

  const client: FakeProviderClient = {
    calls,
    async providers() {
      calls.providers += 1
      return catalog.map((provider) => ({ ...provider }))
    },
    async provider(id) {
      calls.provider.push(id)
      const found = catalog.find((provider) => provider.id === id)
      if (!found) {
        throw providerNotFound(id)
      }
      return { ...found }
    },
    async createProvider(request) {
      calls.create.push(request)
      const created = providerFixture({
        id: request.id,
        adapter: request.adapter,
        model: request.model ?? "",
        endpoint: request.endpoint,
        residency: request.residency,
        active: true,
        revision: 1,
        ready: false,
        models: [],
        models_source: "pending",
        allowed_models: request.allowed_models,
        allow_auto_default: request.allow_auto_default ?? false,
        allowed_workers: request.allowed_workers ?? [],
        modalities: request.modalities ?? ["text"],
        tool_calling: request.tool_calling ?? false,
        context_window: request.context_window,
        local: request.local ?? false,
      })
      catalog = [...catalog, created]
      return { ...created }
    },
    async updateProvider(id, request) {
      calls.update.push({ id, request })
      const index = catalog.findIndex((provider) => provider.id === id)
      if (index < 0) {
        throw providerNotFound(id)
      }
      const current = catalog[index]
      if (request.revision !== current.revision) {
        throw revisionConflict()
      }
      const updated: ProviderMetadata = {
        ...current,
        ...request,
        revision: current.revision + 1,
      }
      catalog = catalog.map((provider, position) =>
        position === index ? updated : provider,
      )
      return { ...updated }
    },
    async discoverProvider(id) {
      calls.discover.push(id)
      const index = catalog.findIndex((provider) => provider.id === id)
      if (index < 0) {
        throw providerNotFound(id)
      }
      const current = catalog[index]
      const updated: ProviderMetadata = {
        ...current,
        models: ["openai/gpt-4.1", "google/gemini-2.5-pro"],
        models_source: "discovered",
        ready: true,
        discovery_error: undefined,
        discovery_error_code: undefined,
        revision: current.revision + 1,
      }
      catalog = catalog.map((provider, position) =>
        position === index ? updated : provider,
      )
      return { ...updated }
    },
    async setProviderCredential(id, key) {
      calls.credential.push({ id, key })
      const index = catalog.findIndex((provider) => provider.id === id)
      if (index < 0) {
        throw providerNotFound(id)
      }
      const current = catalog[index]
      const summary = credentialFixture({ provider: id })
      catalog = catalog.map((provider, position) =>
        position === index
          ? {
              ...current,
              credential_version: summary.version,
              credential_fingerprint: summary.fingerprint,
              credential_revoked: false,
              revision: summary.revision,
              ready: summary.ready,
              models: summary.models,
            }
          : provider,
      )
      return summary
    },
    async revokeProviderCredential(id) {
      calls.revoke.push(id)
      const index = catalog.findIndex((provider) => provider.id === id)
      if (index < 0) {
        throw providerNotFound(id)
      }
      const current = catalog[index]
      catalog = catalog.map((provider, position) =>
        position === index
          ? {
              ...current,
              credential_revoked: true,
              ready: false,
              revision: current.revision + 1,
            }
          : provider,
      )
      return credentialFixture({
        provider: id,
        revoked_at: 1_700_000_030_000,
        ready: false,
      })
    },
  }

  return client
}

export function providerNotFound(id: string): GatewayError {
  return sdkFailure({
    kind: "not-found",
    status: 404,
    code: "provider.not_found",
    message: `provider '${id}' is not configured`,
    recovery: "refresh the provider catalog",
  })
}

export function revisionConflict(): GatewayError {
  return sdkFailure({
    kind: "conflict",
    status: 409,
    code: "provider.revision_conflict",
    message: "provider metadata was changed by another decision",
    recovery: "reload the provider and reapply the change",
    details: { expected: "5", observed: "4" },
  })
}

export function scopeDenied(): GatewayError {
  return sdkFailure({
    kind: "forbidden",
    status: 403,
    code: "scope.provider_admin",
    message: "caller token is not allowed to administer model providers",
    recovery: "use a caller token with the provider:admin scope",
  })
}

export function sdkFailure(options: {
  kind:
    | "not-found"
    | "conflict"
    | "forbidden"
    | "server"
    | "network"
    | "invalid-response"
    | "protocol"
  status?: number
  code: string
  message: string
  recovery?: string
  details?: Record<string, string>
  retryable?: boolean
}): GatewayError {
  return new GatewayError(options)
}

import {
  GatewayError,
  type CreateProfileRequest,
  type DeletedProfile,
  type Profile,
  type ProfileRevision,
  type ReviseProfileRequest,
} from "@goldenbee/sdk"

import type { RuntimeProfileClient } from "./profile-client"

export function profileRevisionFixture(
  overrides: Partial<ProfileRevision> = {},
): ProfileRevision {
  return {
    revision: 1,
    instruction: "Research the question and cite the strongest evidence.",
    model: "openai/gpt-4.1",
    capabilities: ["general", "research"],
    grants: ["fs.read", "net.read"],
    restrictions: ["fs.write"],
    composition: "single",
    members: [],
    skills: [],
    mcp: [],
    workflow: null,
    limits: { max_model_calls: 4 },
    compaction: null,
    secret_refs: ["env:SEARCH_KEY"],
    created_at: 1_700_000_000_000,
    ...overrides,
  }
}

export function profileFixture(overrides: Partial<Profile> = {}): Profile {
  return {
    name: "research",
    active_revision: 2,
    revisions: [
      profileRevisionFixture(),
      profileRevisionFixture({
        revision: 2,
        composition: "sequential",
        members: ["planner", "writer"],
        skills: ["citation"],
        mcp: ["search"],
        limits: { max_model_calls: 6 },
        compaction: {
          strategy: "truncation",
          context_budget: 64_000,
          preserve_recent: 8,
          max_retries: 2,
        },
        created_at: 1_700_000_050_000,
      }),
    ],
    ...overrides,
  }
}

export interface FakeProfileClient extends RuntimeProfileClient {
  calls: {
    profiles: string[]
    profile: Array<{ worker: string; name: string }>
    create: Array<{ worker: string; request: CreateProfileRequest }>
    revise: Array<{ worker: string; name: string; request: ReviseProfileRequest }>
    validate: Array<{ worker: string; name: string; revision?: number }>
    activate: Array<{ worker: string; name: string; revision?: number }>
    deactivate: Array<{ worker: string; name: string; revision?: number }>
    delete: Array<{ worker: string; name: string }>
  }
}

/** Whole-feature fake for one or more Worker-owned profile catalogs. */
export function createFakeProfileClient(
  initial: Record<string, Profile[]> = { "general-a": [profileFixture()] },
): FakeProfileClient {
  const catalogs = new Map<string, Profile[]>()
  for (const [worker, profiles] of Object.entries(initial)) {
    catalogs.set(worker, profiles.map((profile) => structuredClone(profile)))
  }
  const calls: FakeProfileClient["calls"] = {
    profiles: [],
    profile: [],
    create: [],
    revise: [],
    validate: [],
    activate: [],
    deactivate: [],
    delete: [],
  }

  function catalog(worker: string): Profile[] {
    return catalogs.get(worker) ?? []
  }

  function find(worker: string, name: string): Profile {
    const found = catalog(worker).find((profile) => profile.name === name)
    if (!found) {
      throw profileNotFound(name)
    }
    return found
  }

  function replace(worker: string, next: Profile): Profile {
    catalogs.set(
      worker,
      catalog(worker).map((profile) =>
        profile.name === next.name ? next : profile,
      ),
    )
    return structuredClone(next)
  }

  function revisionOf(profile: Profile, number: number): ProfileRevision {
    const revision = profile.revisions.find(
      (candidate) => candidate.revision === number,
    )
    if (!revision) {
      throw revisionNotFound(profile.name, number)
    }
    return revision
  }

  return {
    calls,
    async profiles(worker) {
      calls.profiles.push(worker)
      return catalog(worker).map((profile) => structuredClone(profile))
    },
    async profile(worker, name) {
      calls.profile.push({ worker, name })
      return structuredClone(find(worker, name))
    },
    async createProfile(worker, request) {
      calls.create.push({ worker, request })
      if (catalog(worker).some((profile) => profile.name === request.name)) {
        throw profileConflict(request.name)
      }
      const created: Profile = {
        name: request.name,
        active_revision: 0,
        revisions: [
          {
            ...request.revision,
            revision: 1,
            created_at: 1_700_000_100_000,
          },
        ],
      }
      catalogs.set(worker, [...catalog(worker), created])
      return structuredClone(created)
    },
    async reviseProfile(worker, name, request) {
      calls.revise.push({ worker, name, request })
      const current = find(worker, name)
      if (request.expected_revision !== current.revisions.length) {
        throw revisionConflict(current.revisions.length, request.expected_revision)
      }
      const next: Profile = {
        ...current,
        revisions: [
          ...current.revisions,
          {
            ...request.revision,
            revision: current.revisions.length + 1,
            created_at: 1_700_000_200_000,
          },
        ],
      }
      return replace(worker, next)
    },
    async validateProfile(worker, name, request) {
      calls.validate.push({ worker, name, revision: request?.revision })
      const current = find(worker, name)
      if (request?.revision !== undefined) {
        revisionOf(current, request.revision)
      }
      return structuredClone(current)
    },
    async activateProfile(worker, name, request) {
      calls.activate.push({ worker, name, revision: request?.revision })
      const current = find(worker, name)
      const target =
        request?.revision ??
        current.revisions[current.revisions.length - 1]?.revision ??
        0
      const revision = revisionOf(current, target)
      return replace(worker, {
        ...current,
        active_revision: revision.revision,
      })
    },
    async deactivateProfile(worker, name) {
      calls.deactivate.push({ worker, name })
      const current = find(worker, name)
      return replace(worker, { ...current, active_revision: 0 })
    },
    async deleteProfile(worker, name): Promise<DeletedProfile> {
      calls.delete.push({ worker, name })
      const current = find(worker, name)
      if (current.active_revision !== 0) {
        throw profileActive(name)
      }
      catalogs.set(
        worker,
        catalog(worker).filter((profile) => profile.name !== name),
      )
      return { deleted: name }
    },
  }
}

export function profileNotFound(name: string): GatewayError {
  return sdkFailure({
    kind: "not-found",
    status: 404,
    code: "profile.not_found",
    message: `profile '${name}' is not visible`,
    recovery: "refresh the profile catalog",
  })
}

export function profileConflict(name: string): GatewayError {
  return sdkFailure({
    kind: "conflict",
    status: 409,
    code: "profile.conflict",
    message: `profile '${name}' already exists`,
    recovery: "choose another profile name",
  })
}

export function revisionConflict(
  current: number,
  expected: number,
): GatewayError {
  return sdkFailure({
    kind: "conflict",
    status: 409,
    code: "profile.revision_conflict",
    message: "profile was revised by another writer",
    recovery: "reload the profile and reapply the revision",
    details: { current: String(current), expected: String(expected) },
  })
}

export function revisionNotFound(
  name: string,
  revision: number,
): GatewayError {
  return sdkFailure({
    kind: "not-found",
    status: 404,
    code: "profile.revision_not_found",
    message: `profile '${name}' has no revision ${revision}`,
    recovery: "refresh the profile revisions",
  })
}

export function profileActive(name: string): GatewayError {
  return sdkFailure({
    kind: "conflict",
    status: 409,
    code: "profile.active",
    message: `profile '${name}' is active`,
    recovery: "deactivate the profile before removing it",
  })
}

export function scopeDenied(): GatewayError {
  return sdkFailure({
    kind: "forbidden",
    status: 403,
    code: "scope.profile_write",
    message: "caller token is not allowed to write Runtime Profiles",
    recovery: "use a caller token with the profile:write scope",
  })
}

export function sdkFailure(options: {
  kind:
    | "not-found"
    | "conflict"
    | "forbidden"
    | "unauthorized"
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

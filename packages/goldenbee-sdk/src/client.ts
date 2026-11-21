import {
  GatewayError,
  isGatewayError,
  sdkError,
} from "./errors.js"
import {
  GatewayTaskStream,
  openTaskStream,
  openWorkspaceStream,
} from "./sse.js"
import {
  connectSocket,
  type GatewaySocket,
  type GatewayWebSocketFactory,
} from "./socket.js"
import {
  encodeSegment,
  GatewayTransport,
  queryString,
  type GatewayFetch,
  type RetryOptions,
} from "./transport.js"
import type {
  ApprovalDecision,
  Artifact,
  ArtifactContent,
  ConnectionResult,
  ControlAuditEntry,
  CreateProfileRequest,
  CreateProviderRequest,
  CreateTeamRequest,
  DeletedProfile,
  DiagnosticBundle,
  GatewayApproval,
  GatewayHealth,
  GatewayRequestOptions,
  GatewaySubmitRequest,
  GatewaySubmitResponse,
  GatewayTask,
  GatewayWorker,
  HostDescriptor,
  HostInspection,
  HostInstanceView,
  HostOperationOutcome,
  HostProvisionRequest,
  HostRollbackRequest,
  HostUpgradeRequest,
  InstanceLogs,
  PackageInstallOutcome,
  PackageInstallRequest,
  PackageList,
  PairCodeResponse,
  Profile,
  ProfileVersionRequest,
  ProviderCredentialSummary,
  ProviderMetadata,
  RegistryAuditEntry,
  ReviseProfileRequest,
  SocketTicket,
  StreamPage,
  Team,
  TeamVersionRequest,
  UpdateProviderRequest,
  UpdateTeamRequest,
  WorkerDiscoveryFilter,
  WorkspaceContent,
  WorkspaceFile,
  WorkspaceTreePage,
} from "./types.js"
import {
  isApproval,
  isApprovalList,
  isArtifact,
  isArtifactList,
  isControlAuditList,
  isCredentialSummary,
  isDeletedProfile,
  isDiagnosticBundle,
  isHealth,
  isHostDescriptor,
  isHostDescriptorList,
  isHostInspection,
  isHostInstanceList,
  isHostOperationOutcome,
  isInstallOutcome,
  isInstanceLogs,
  isPackageList,
  isPairCode,
  isProfile,
  isProfileList,
  isProvider,
  isProviderList,
  isRegistryAuditList,
  isSocketTicket,
  isStreamPage,
  isSubmitResponse,
  isTask,
  isTeam,
  isTeamList,
  isWorkerList,
  isWorkspaceFile,
  isWorkspaceTreePage,
} from "./validate.js"
import { normalizeGatewayUrl } from "./url.js"

export type { GatewayFetch } from "./transport.js"
export type { GatewayTransportOptions } from "./transport.js"

export interface GatewayClientOptions {
  baseUrl: string
  /** Caller bearer token. It is held in memory and sent only in a header. */
  token?: string
  /**
   * Platform fetch implementation. The SDK never reaches for a global fetch,
   * so browser, Bun, Node, desktop, and mobile callers supply their own.
   */
  fetch: GatewayFetch
  timeoutMs?: number
  /** Bounded retries for idempotent transport failures. */
  retry?: RetryOptions
  /** Test seam; production waits with `setTimeout`. */
  sleep?: (milliseconds: number) => Promise<void>
  /**
   * Platform WebSocket factory. Required only for `socketConnect`/`openSocket`.
   * The SDK never constructs a global WebSocket.
   */
  webSocket?: GatewayWebSocketFactory
}

/**
 * The app-local Gateway SDK surface.
 *
 * UI code imports this interface and never constructs transport requests. HTTP
 * JSON, typed errors, bounded retries, SSE, socket tickets, and frames all live
 * behind this boundary.
 */
export interface GatewayClient {
  readonly baseUrl: string
  readonly hasToken: boolean

  // Connection, discovery, and diagnostics.
  health(options?: GatewayRequestOptions): Promise<GatewayHealth>
  workers(options?: GatewayRequestOptions): Promise<GatewayWorker[]>
  /** Backwards-compatible alias for {@link GatewayClient.workers}. */
  listWorkers(options?: GatewayRequestOptions): Promise<GatewayWorker[]>
  discover(
    filter?: WorkerDiscoveryFilter,
    options?: GatewayRequestOptions,
  ): Promise<GatewayWorker[]>
  diagnostics(
    worker: string,
    task?: string,
    options?: GatewayRequestOptions,
  ): Promise<DiagnosticBundle>
  workspaceTree(
    worker: string,
    path?: string,
    limit?: number,
    cursor?: string,
    options?: GatewayRequestOptions,
  ): Promise<WorkspaceTreePage>
  workspaceFile(
    worker: string,
    path: string,
    options?: GatewayRequestOptions,
  ): Promise<WorkspaceFile>
  workspaceContent(
    worker: string,
    path: string,
    range?: string,
    options?: GatewayRequestOptions,
  ): Promise<WorkspaceContent>
  workspaceEvents(
    worker: string,
    options?: GatewayRequestOptions,
  ): Promise<GatewayTaskStream>
  workerAudit(
    worker?: string,
    options?: GatewayRequestOptions,
  ): Promise<RegistryAuditEntry[]>
  workerAuditLegacy(
    worker?: string,
    options?: GatewayRequestOptions,
  ): Promise<RegistryAuditEntry[]>
  controlAudit(
    worker: string,
    session: string,
    options?: GatewayRequestOptions,
  ): Promise<ControlAuditEntry[]>
  checkConnection(options?: GatewayRequestOptions): Promise<ConnectionResult>

  // Tasks and streams.
  submit(
    request: GatewaySubmitRequest,
    options?: GatewayRequestOptions,
  ): Promise<GatewaySubmitResponse>
  task(taskId: string, options?: GatewayRequestOptions): Promise<GatewayTask>
  events(
    taskId: string,
    after?: number,
    limit?: number,
    options?: GatewayRequestOptions,
  ): Promise<StreamPage>
  watch(
    taskId: string,
    after?: number,
    options?: GatewayRequestOptions,
  ): Promise<GatewayTaskStream>
  cancel(taskId: string, options?: GatewayRequestOptions): Promise<GatewayTask>
  approvals(
    taskId: string,
    pendingOnly?: boolean,
    options?: GatewayRequestOptions,
  ): Promise<GatewayApproval[]>
  approval(
    taskId: string,
    approvalId: string,
    options?: GatewayRequestOptions,
  ): Promise<GatewayApproval>
  decideApproval(
    taskId: string,
    approvalId: string,
    decision: ApprovalDecision,
    options?: GatewayRequestOptions,
  ): Promise<GatewayApproval>
  revokeApproval(
    taskId: string,
    approvalId: string,
    options?: GatewayRequestOptions,
  ): Promise<GatewayApproval>
  artifacts(taskId: string, options?: GatewayRequestOptions): Promise<Artifact[]>
  artifact(
    taskId: string,
    artifactId: string,
    options?: GatewayRequestOptions,
  ): Promise<Artifact>
  artifactContent(
    taskId: string,
    artifactId: string,
    options?: GatewayRequestOptions,
  ): Promise<ArtifactContent>

  // Socket tickets and frames.
  socketTicket(options?: GatewayRequestOptions): Promise<SocketTicket>
  socketConnect(ticket: SocketTicket): GatewaySocket
  openSocket(options?: GatewayRequestOptions): Promise<GatewaySocket>

  // Teams.
  teams(options?: GatewayRequestOptions): Promise<Team[]>
  team(name: string, options?: GatewayRequestOptions): Promise<Team>
  createTeam(
    request: CreateTeamRequest,
    options?: GatewayRequestOptions,
  ): Promise<Team>
  updateTeam(
    name: string,
    request: UpdateTeamRequest,
    options?: GatewayRequestOptions,
  ): Promise<Team>
  validateTeam(
    name: string,
    request?: TeamVersionRequest,
    options?: GatewayRequestOptions,
  ): Promise<Team>
  activateTeam(
    name: string,
    request?: TeamVersionRequest,
    options?: GatewayRequestOptions,
  ): Promise<Team>
  deactivateTeam(
    name: string,
    request?: TeamVersionRequest,
    options?: GatewayRequestOptions,
  ): Promise<Team>

  // Worker-owned runtime profiles.
  profiles(worker: string, options?: GatewayRequestOptions): Promise<Profile[]>
  profile(
    worker: string,
    name: string,
    options?: GatewayRequestOptions,
  ): Promise<Profile>
  createProfile(
    worker: string,
    request: CreateProfileRequest,
    options?: GatewayRequestOptions,
  ): Promise<Profile>
  reviseProfile(
    worker: string,
    name: string,
    request: ReviseProfileRequest,
    options?: GatewayRequestOptions,
  ): Promise<Profile>
  validateProfile(
    worker: string,
    name: string,
    request?: ProfileVersionRequest,
    options?: GatewayRequestOptions,
  ): Promise<Profile>
  activateProfile(
    worker: string,
    name: string,
    request?: ProfileVersionRequest,
    options?: GatewayRequestOptions,
  ): Promise<Profile>
  deactivateProfile(
    worker: string,
    name: string,
    options?: GatewayRequestOptions,
  ): Promise<Profile>
  deleteProfile(
    worker: string,
    name: string,
    options?: GatewayRequestOptions,
  ): Promise<DeletedProfile>

  // Provider catalog administration.
  providers(options?: GatewayRequestOptions): Promise<ProviderMetadata[]>
  provider(id: string, options?: GatewayRequestOptions): Promise<ProviderMetadata>
  createProvider(
    request: CreateProviderRequest,
    options?: GatewayRequestOptions,
  ): Promise<ProviderMetadata>
  updateProvider(
    id: string,
    request: UpdateProviderRequest,
    options?: GatewayRequestOptions,
  ): Promise<ProviderMetadata>
  discoverProvider(
    id: string,
    options?: GatewayRequestOptions,
  ): Promise<ProviderMetadata>
  setProviderCredential(
    id: string,
    key: string,
    options?: GatewayRequestOptions,
  ): Promise<ProviderCredentialSummary>
  revokeProviderCredential(
    id: string,
    options?: GatewayRequestOptions,
  ): Promise<ProviderCredentialSummary>
  providerPairCode(
    worker: string,
    instance: string,
    options?: GatewayRequestOptions,
  ): Promise<PairCodeResponse>

  // Packages and Gateway-owned local instances.
  packages(options?: GatewayRequestOptions): Promise<PackageList>
  installPackage(
    request: PackageInstallRequest,
    options?: GatewayRequestOptions,
  ): Promise<PackageInstallOutcome>
  instances(options?: GatewayRequestOptions): Promise<HostInstanceView[]>
  localInstance(
    name: string,
    options?: GatewayRequestOptions,
  ): Promise<HostInspection>
  provisionLocalInstance(
    request: HostProvisionRequest,
    options?: GatewayRequestOptions,
  ): Promise<HostOperationOutcome>
  startInstance(
    name: string,
    options?: GatewayRequestOptions,
  ): Promise<HostInspection>
  stopInstance(
    name: string,
    options?: GatewayRequestOptions,
  ): Promise<HostInspection>
  restartInstance(
    name: string,
    options?: GatewayRequestOptions,
  ): Promise<HostInspection>
  upgradeLocalInstance(
    name: string,
    request: HostUpgradeRequest,
    options?: GatewayRequestOptions,
  ): Promise<HostOperationOutcome>
  rollbackLocalInstance(
    name: string,
    request: HostRollbackRequest,
    options?: GatewayRequestOptions,
  ): Promise<HostOperationOutcome>
  instanceLogs(
    name: string,
    tail?: number,
    options?: GatewayRequestOptions,
  ): Promise<InstanceLogs>
  localHost(options?: GatewayRequestOptions): Promise<HostDescriptor>
  hosts(options?: GatewayRequestOptions): Promise<HostDescriptor[]>
}

const defaultTimeoutMs = 15_000
const defaultEventLimit = 500
const maxEventLimit = 1_000
const maxLogTail = 1_000
const defaultLogTail = 200

/** Generate a caller-scoped idempotency key once per user action. */
export function newIdempotencyKey(prefix = "gb"): string {
  const cryptoApi = globalThis.crypto
  if (cryptoApi && typeof cryptoApi.randomUUID === "function") {
    return `${prefix}-${cryptoApi.randomUUID()}`
  }
  return `${prefix}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`
}

function profileVersionBody(
  request: ProfileVersionRequest | undefined,
): Record<string, number> {
  return request?.revision === undefined ? {} : { revision: request.revision }
}

function teamVersionBody(
  request: TeamVersionRequest | undefined,
): Record<string, number> {
  return request?.version === undefined ? {} : { version: request.version }
}

/**
 * Create an app-local Gateway client. The returned surface is the only way UI
 * code talks to Gateway, including tickets 03-11.
 */
export function createGatewayClient(
  options: GatewayClientOptions,
): GatewayClient {
  const baseUrl = normalizeGatewayUrl(options.baseUrl)
  const token = options.token?.trim() || undefined
  const transport = new GatewayTransport({
    baseUrl,
    token,
    fetch: options.fetch,
    timeoutMs: options.timeoutMs ?? defaultTimeoutMs,
    retry: options.retry,
    sleep: options.sleep,
  })
  const webSocket = options.webSocket

  function requireWebSocket(): GatewayWebSocketFactory {
    if (!webSocket) {
      throw sdkError(
        "protocol",
        "This platform has no WebSocket factory. Pass `webSocket` in createGatewayClient options.",
        { code: "sdk.platform_missing" },
      )
    }
    return webSocket
  }

  function get<T>(
    path: string,
    validate: (value: unknown) => value is T,
    request: GatewayRequestOptions = {},
  ): Promise<T> {
    return transport.request({
      method: "GET",
      path,
      authenticated: true,
      signal: request.signal,
      validate,
    })
  }

  function post<T>(
    path: string,
    body: unknown,
    validate: (value: unknown) => value is T,
    request: GatewayRequestOptions = {},
    reconcile = false,
  ): Promise<T> {
    return transport.request({
      method: "POST",
      path,
      body,
      authenticated: true,
      signal: request.signal,
      validate,
      reconcile,
    })
  }

  function put<T>(
    path: string,
    body: unknown,
    validate: (value: unknown) => value is T,
    request: GatewayRequestOptions = {},
  ): Promise<T> {
    return transport.request({
      method: "PUT",
      path,
      body,
      authenticated: true,
      signal: request.signal,
      validate,
    })
  }

  function patch<T>(
    path: string,
    body: unknown,
    validate: (value: unknown) => value is T,
    request: GatewayRequestOptions = {},
  ): Promise<T> {
    return transport.request({
      method: "PATCH",
      path,
      body,
      authenticated: true,
      signal: request.signal,
      validate,
    })
  }

  function del<T>(
    path: string,
    validate: (value: unknown) => value is T,
    request: GatewayRequestOptions = {},
  ): Promise<T> {
    return transport.request({
      method: "DELETE",
      path,
      authenticated: true,
      signal: request.signal,
      validate,
    })
  }

  async function health(
    request: GatewayRequestOptions = {},
  ): Promise<GatewayHealth> {
    return transport.request({
      method: "GET",
      path: "/v1/health",
      authenticated: false,
      signal: request.signal,
      validate: isHealth,
    })
  }

  async function workers(
    request: GatewayRequestOptions = {},
  ): Promise<GatewayWorker[]> {
    return get("/v1/workers", isWorkerList, request)
  }

  async function discover(
    filter: WorkerDiscoveryFilter = {},
    request: GatewayRequestOptions = {},
  ): Promise<GatewayWorker[]> {
    return get(
      `/v1/workers${queryString({
        capability: filter.capability,
        protocol: filter.protocol,
        locality: filter.locality,
        health: filter.health,
      })}`,
      isWorkerList,
      request,
    )
  }

  async function checkConnection(
    request: GatewayRequestOptions = {},
  ): Promise<ConnectionResult> {
    let gatewayHealth: GatewayHealth
    try {
      gatewayHealth = await health(request)
    } catch (cause) {
      return { status: "unreachable", error: toGatewayError(cause) }
    }

    try {
      const visible = await workers(request)
      return { status: "connected", health: gatewayHealth, workers: visible }
    } catch (cause) {
      const error = toGatewayError(cause)
      if (error.kind === "unauthorized") {
        return { status: "unauthorized", health: gatewayHealth, error }
      }
      if (error.kind === "forbidden") {
        return { status: "forbidden", health: gatewayHealth, error }
      }
      return { status: "error", health: gatewayHealth, error }
    }
  }

  async function submit(
    request: GatewaySubmitRequest,
    requestOptions: GatewayRequestOptions = {},
  ): Promise<GatewaySubmitResponse> {
    // Capture the key once so every bounded retry reuses the exact identity.
    const body: GatewaySubmitRequest = {
      ...request,
      idempotency_key:
        request.idempotency_key?.trim() || newIdempotencyKey("submit"),
    }
    return post("/v1/tasks", body, isSubmitResponse, requestOptions, true)
  }

  async function events(
    taskId: string,
    after = 0,
    limit = defaultEventLimit,
    request: GatewayRequestOptions = {},
  ): Promise<StreamPage> {
    const bounded = Math.min(maxEventLimit, Math.max(1, Math.trunc(limit)))
    return get(
      `/v1/tasks/${encodeSegment(taskId)}/events${queryString({
        after,
        limit: bounded,
      })}`,
      isStreamPage,
      request,
    )
  }

  async function approvals(
    taskId: string,
    pendingOnly = false,
    request: GatewayRequestOptions = {},
  ): Promise<GatewayApproval[]> {
    return get(
      `/v1/tasks/${encodeSegment(taskId)}/approvals${queryString({
        pending: pendingOnly,
      })}`,
      isApprovalList,
      request,
    )
  }

  async function artifactContent(
    taskId: string,
    artifactId: string,
    request: GatewayRequestOptions = {},
  ): Promise<ArtifactContent> {
    const response = await transport.openResponse(
      `/v1/tasks/${encodeSegment(taskId)}/artifacts/${encodeSegment(artifactId)}/content`,
      "*/*",
      request.signal,
    )
    const length = response.headers.get("content-length")
    const parsedLength = length === null ? Number.NaN : Number(length)
    return {
      blob: await response.blob(),
      contentType: response.headers.get("content-type") ?? "application/octet-stream",
      contentLength: Number.isFinite(parsedLength) ? parsedLength : undefined,
      digest: response.headers.get("x-goldenbee-artifact-digest") ?? undefined,
      artifactId: response.headers.get("x-goldenbee-artifact-id") ?? undefined,
    }
  }

  async function instanceLogs(
    name: string,
    tail = defaultLogTail,
    request: GatewayRequestOptions = {},
  ): Promise<InstanceLogs> {
    const bounded = Math.min(maxLogTail, Math.max(1, Math.trunc(tail)))
    return get(
      `/v1/instances/${encodeSegment(name)}/logs${queryString({ tail: bounded })}`,
      isInstanceLogs,
      request,
    )
  }

  return {
    baseUrl,
    hasToken: Boolean(token),

    health,
    workers,
    listWorkers: workers,
    discover,
    diagnostics: (worker, task, request = {}) =>
      get(
        `/v1/workers/${encodeSegment(worker)}/diagnostics${queryString({ task })}`,
        isDiagnosticBundle,
        request,
      ),
    workspaceTree: (worker, path = "", limit = 500, cursor, request = {}) =>
      get(
        `/v1/workers/${encodeSegment(worker)}/workspace/tree${queryString({
          path,
          limit,
          cursor,
        })}`,
        isWorkspaceTreePage,
        request,
      ),
    workspaceFile: (worker, path, request = {}) =>
      get(
        `/v1/workers/${encodeSegment(worker)}/workspace/file${queryString({ path })}`,
        isWorkspaceFile,
        request,
      ),
    workspaceContent: async (worker, path, range, request = {}) => {
      const response = await transport.openResponse(
        `/v1/workers/${encodeSegment(worker)}/workspace/content${queryString({ path })}`,
        "*/*",
        request.signal,
        range ? { range } : undefined,
      )
      const length = response.headers.get("content-length")
      const parsedLength = length === null ? Number.NaN : Number(length)
      return {
        blob: await response.blob(),
        contentType:
          response.headers.get("content-type") ?? "application/octet-stream",
        contentLength: Number.isFinite(parsedLength) ? parsedLength : undefined,
        contentRange: response.headers.get("content-range") ?? undefined,
        digest: response.headers.get("x-goldenbee-workspace-digest") ?? undefined,
        status: response.status,
      }
    },
    workspaceEvents: (worker, request = {}) =>
      openWorkspaceStream({
        transport,
        worker,
        signal: request.signal,
      }),
    workerAudit: (worker, request = {}) =>
      get(
        `/v1/admin/workers/audit${queryString({ worker })}`,
        isRegistryAuditList,
        request,
      ),
    workerAuditLegacy: (worker, request = {}) =>
      get(
        `/v1/workers/audit${queryString({ worker })}`,
        isRegistryAuditList,
        request,
      ),
    controlAudit: (worker, session, request = {}) =>
      get(
        `/v1/workers/${encodeSegment(worker)}/sessions/${encodeSegment(session)}/control/audit`,
        isControlAuditList,
        request,
      ),
    checkConnection,

    submit,
    task: (taskId, request = {}) =>
      get(`/v1/tasks/${encodeSegment(taskId)}`, isTask, request),
    events,
    watch: (taskId, after = 0, request = {}) =>
      openTaskStream({
        transport,
        taskId,
        after,
        signal: request.signal,
      }),
    cancel: (taskId, request = {}) =>
      post(`/v1/tasks/${encodeSegment(taskId)}/cancel`, undefined, isTask, request),
    approvals,
    approval: (taskId, approvalId, request = {}) =>
      get(
        `/v1/tasks/${encodeSegment(taskId)}/approvals/${encodeSegment(approvalId)}`,
        isApproval,
        request,
      ),
    decideApproval: (taskId, approvalId, decision, request = {}) =>
      post(
        `/v1/tasks/${encodeSegment(taskId)}/approvals/${encodeSegment(approvalId)}/decision`,
        decision,
        isApproval,
        request,
      ),
    revokeApproval: (taskId, approvalId, request = {}) =>
      post(
        `/v1/tasks/${encodeSegment(taskId)}/approvals/${encodeSegment(approvalId)}/revoke`,
        {},
        isApproval,
        request,
      ),
    artifacts: (taskId, request = {}) =>
      get(
        `/v1/tasks/${encodeSegment(taskId)}/artifacts`,
        isArtifactList,
        request,
      ),
    artifact: (taskId, artifactId, request = {}) =>
      get(
        `/v1/tasks/${encodeSegment(taskId)}/artifacts/${encodeSegment(artifactId)}`,
        isArtifact,
        request,
      ),
    artifactContent,

    socketTicket: (request = {}) =>
      post("/v1/socket/tickets", undefined, isSocketTicket, request),
    socketConnect: (ticket) =>
      connectSocket(requireWebSocket(), baseUrl, ticket),
    openSocket: async (request = {}) => {
      const factory = requireWebSocket()
      const ticket = await post(
        "/v1/socket/tickets",
        undefined,
        isSocketTicket,
        request,
      )
      return connectSocket(factory, baseUrl, ticket)
    },

    teams: (request = {}) => get("/v1/teams", isTeamList, request),
    team: (name, request = {}) =>
      get(`/v1/teams/${encodeSegment(name)}`, isTeam, request),
    createTeam: (body, request = {}) =>
      post("/v1/teams", body, isTeam, request),
    updateTeam: (name, body, request = {}) =>
      put(`/v1/teams/${encodeSegment(name)}`, body, isTeam, request),
    validateTeam: (name, body, request = {}) =>
      post(
        `/v1/teams/${encodeSegment(name)}/validate`,
        teamVersionBody(body),
        isTeam,
        request,
      ),
    activateTeam: (name, body, request = {}) =>
      post(
        `/v1/teams/${encodeSegment(name)}/activate`,
        teamVersionBody(body),
        isTeam,
        request,
      ),
    deactivateTeam: (name, body, request = {}) =>
      post(
        `/v1/teams/${encodeSegment(name)}/deactivate`,
        teamVersionBody(body),
        isTeam,
        request,
      ),

    profiles: (worker, request = {}) =>
      get(
        `/v1/workers/${encodeSegment(worker)}/profiles`,
        isProfileList,
        request,
      ),
    profile: (worker, name, request = {}) =>
      get(
        `/v1/workers/${encodeSegment(worker)}/profiles/${encodeSegment(name)}`,
        isProfile,
        request,
      ),
    createProfile: (worker, body, request = {}) =>
      post(
        `/v1/workers/${encodeSegment(worker)}/profiles`,
        body,
        isProfile,
        request,
      ),
    reviseProfile: (worker, name, body, request = {}) =>
      put(
        `/v1/workers/${encodeSegment(worker)}/profiles/${encodeSegment(name)}`,
        body,
        isProfile,
        request,
      ),
    validateProfile: (worker, name, body, request = {}) =>
      post(
        `/v1/workers/${encodeSegment(worker)}/profiles/${encodeSegment(name)}/validate`,
        profileVersionBody(body),
        isProfile,
        request,
      ),
    activateProfile: (worker, name, body, request = {}) =>
      post(
        `/v1/workers/${encodeSegment(worker)}/profiles/${encodeSegment(name)}/activate`,
        profileVersionBody(body),
        isProfile,
        request,
      ),
    deactivateProfile: (worker, name, request = {}) =>
      post(
        `/v1/workers/${encodeSegment(worker)}/profiles/${encodeSegment(name)}/deactivate`,
        {},
        isProfile,
        request,
      ),
    deleteProfile: (worker, name, request = {}) =>
      del(
        `/v1/workers/${encodeSegment(worker)}/profiles/${encodeSegment(name)}`,
        isDeletedProfile,
        request,
      ),

    providers: (request = {}) =>
      get("/v1/admin/model-providers", isProviderList, request),
    provider: (id, request = {}) =>
      get(
        `/v1/admin/model-providers/${encodeSegment(id)}`,
        isProvider,
        request,
      ),
    createProvider: (body, request = {}) =>
      post("/v1/admin/model-providers", body, isProvider, request),
    updateProvider: (id, body, request = {}) =>
      patch(
        `/v1/admin/model-providers/${encodeSegment(id)}`,
        body,
        isProvider,
        request,
      ),
    discoverProvider: (id, request = {}) =>
      post(
        `/v1/admin/model-providers/${encodeSegment(id)}/discover`,
        undefined,
        isProvider,
        request,
      ),
    setProviderCredential: (id, key, request = {}) =>
      put(
        `/v1/admin/model-providers/${encodeSegment(id)}/credential`,
        { key },
        isCredentialSummary,
        request,
      ),
    revokeProviderCredential: (id, request = {}) =>
      del(
        `/v1/admin/model-providers/${encodeSegment(id)}/credential`,
        isCredentialSummary,
        request,
      ),
    providerPairCode: (worker, instance, request = {}) =>
      post(
        "/v1/admin/model-providers/pair-code",
        { worker_id: worker, instance },
        isPairCode,
        request,
      ),

    packages: (request = {}) => get("/v1/packages", isPackageList, request),
    installPackage: (body, request = {}) =>
      post("/v1/packages/install", body, isInstallOutcome, request),
    instances: (request = {}) =>
      get("/v1/instances", isHostInstanceList, request),
    localInstance: (name, request = {}) =>
      get(`/v1/instances/${encodeSegment(name)}`, isHostInspection, request),
    provisionLocalInstance: (body, request = {}) =>
      post("/v1/instances", body, isHostOperationOutcome, request),
    startInstance: (name, request = {}) =>
      post(
        `/v1/instances/${encodeSegment(name)}/start`,
        undefined,
        isHostInspection,
        request,
      ),
    stopInstance: (name, request = {}) =>
      post(
        `/v1/instances/${encodeSegment(name)}/stop`,
        undefined,
        isHostInspection,
        request,
      ),
    restartInstance: (name, request = {}) =>
      post(
        `/v1/instances/${encodeSegment(name)}/restart`,
        undefined,
        isHostInspection,
        request,
      ),
    upgradeLocalInstance: (name, body, request = {}) =>
      post(
        `/v1/instances/${encodeSegment(name)}/upgrade`,
        body,
        isHostOperationOutcome,
        request,
      ),
    rollbackLocalInstance: (name, body, request = {}) =>
      post(
        `/v1/instances/${encodeSegment(name)}/rollback`,
        body,
        isHostOperationOutcome,
        request,
      ),
    instanceLogs,
    localHost: (request = {}) => get("/v1/host", isHostDescriptor, request),
    hosts: (request = {}) => get("/v1/hosts", isHostDescriptorList, request),
  }
}

function toGatewayError(cause: unknown): GatewayError {
  if (isGatewayError(cause)) {
    return cause
  }
  return new GatewayError({
    kind: "network",
    message:
      cause instanceof Error && cause.message
        ? cause.message
        : "The Gateway could not be reached.",
    retryable: true,
  })
}

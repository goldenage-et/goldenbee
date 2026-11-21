import type { GatewayError } from "./errors.js"

/**
 * Curated wire types for Gateway protocol v1.
 *
 * `schema/gateway-v1.json` is authoritative where it covers a shape. The schema
 * is intentionally not exhaustive: task projections, provider metadata,
 * diagnostics, stream event kinds, and cost limits are curated here and are
 * covered by contract tests and fixtures.
 */

export interface GatewayHealth {
  status: string
  workers: number
  reachable: number
}

export interface GatewayCapability {
  id: string
  kind: "agent" | "tool"
  description: string
  permissions?: string[]
}

export interface GatewayLimits {
  max_duration_ms?: number
  max_model_calls?: number
  max_tool_calls?: number
  max_tokens?: number
  max_cost_micros?: number
  max_fanout?: number
  max_depth?: number
}

export interface WorkerCapacity {
  active: number
  limit: number
  queued: number
  queue_limit: number
}

export interface WorkerIncarnation {
  incarnation: string
  first_seen_at: number
  last_seen_at: number
  superseded_at?: number
}

/** `schema/gateway-v1.json#/definitions/workerSummary`. */
export interface WorkerSummary {
  id: string
  instance: string
  package: string
  version: string
  status:
    | "approved"
    | "registered"
    | "unreachable"
    | "disabled"
    | "decommissioned"
  reachable: boolean
  locality: "local" | "remote"
  endpoint?: string
  capabilities?: GatewayCapability[]
  declared_capabilities?: GatewayCapability[]
  approved_capabilities?: string[]
  protocol?: string
  incarnation?: string
  incarnations?: WorkerIncarnation[]
  capacity?: WorkerCapacity
  lease_expires_at?: number
  renewed_at?: number
}

/** Backwards-compatible name used by the connection feature. */
export type GatewayWorker = WorkerSummary

export type WorkerLocalityFilter = "local" | "remote"
export type WorkerHealthFilter = "healthy" | "unreachable" | "any"

export interface WorkerDiscoveryFilter {
  capability?: string
  protocol?: string
  locality?: WorkerLocalityFilter
  health?: WorkerHealthFilter
}

export type RegistryAuditOutcome = "accepted" | "denied"

/** One Gateway registry decision. */
export interface RegistryAuditEntry {
  at: number
  action: string
  worker: string
  principal: string
  outcome: RegistryAuditOutcome
  detail: string
  source?: string
  policy_revision?: string
  correlation?: string
}

/** One Worker interactive-control decision. */
export interface ControlAuditEntry {
  at: number
  action: string
  session_id: string
  principal: string
  generation: number
  outcome: "accepted" | "rejected"
  detail: string
}

export type TaskState =
  | "accepted"
  | "queued"
  | "running"
  | "waiting_approval"
  | "cancelling"
  | "recovering"
  | "unknown"
  | "succeeded"
  | "failed"
  | "cancelled"

export type TaskTerminalState = "succeeded" | "failed" | "cancelled" | "unknown"

export type GatewayConnectivity = "unknown" | "online" | "unreachable"

/** Terminal or current task failure. */
export interface TaskError {
  code: string
  message: string
  retryable: boolean
  recovery: string
  details?: Record<string, string>
}

/** Bounded usage projection. Missing fields mean unavailable, not zero. */
export interface TaskUsage {
  prompt_tokens?: number
  candidate_tokens?: number
  estimated_tokens?: number
  estimated_cost_micros?: number
  model_calls?: number
  tool_calls?: number
  [key: string]: unknown
}

/** Curated task projection. The Worker remains authoritative. */
export interface TaskProjection {
  id: string
  namespace: string
  caller: string
  agent: string
  session_id: string
  state: TaskState
  input?: string
  idempotency_key?: string
  request_digest?: string
  profile?: string
  profile_revision?: number
  asset_revision?: number
  trace_id?: string
  traffic?: "product" | "local" | string
  resolved_model?: Record<string, unknown> | null
  team?: string | null
  team_version?: number | null
  team_roster?: unknown
  scopes?: string[]
  delegation_path?: string[]
  authority_expires_at?: number | null
  attempt?: number
  native_invocation_id?: string
  native_thread_id?: string
  native_a2a_task_id?: string | null
  output?: string | null
  evidence?: Record<string, unknown> | null
  error?: TaskError | null
  uncertainty?: string | null
  limits?: GatewayLimits
  usage?: TaskUsage
  created_at?: number
  updated_at?: number
}

/** `schema/gateway-v1.json#/definitions/routedTask`. */
export interface GatewayTask {
  task_id: string
  worker: string
  worker_instance: string
  worker_incarnation?: string
  connectivity: GatewayConnectivity
  task: TaskProjection
}

/** `schema/gateway-v1.json#/definitions/submitRequest`. */
export interface GatewaySubmitRequest {
  worker?: string
  agent: string
  session_id: string
  input: string
  /** Caller-scoped idempotency key. Retries of one submit must reuse it. */
  idempotency_key?: string
  profile?: string
  team?: string
  team_version?: number
  limits?: GatewayLimits
}

/** `schema/gateway-v1.json#/definitions/submitResponse`. */
export interface GatewaySubmitResponse {
  task_id: string
  worker: string
  worker_instance: string
  worker_incarnation?: string
  agent: string
  profile: string
  state: TaskState
  replayed: boolean
  team_version?: number
}

/** Bounded and de-duplicated durable stream event envelope. */
export interface StreamEvent {
  event_id: string
  source: string
  incarnation: string
  stream_id: string
  seq: number
  schema_version: number
  kind: string
  durable: boolean
  payload: unknown
  trace_id?: string
  at: number
}

/** `schema/gateway-v1.json#/definitions/eventPage`. */
export interface StreamPage {
  stream_id: string
  events: StreamEvent[]
  next_seq: number
  oldest_seq: number
  /** True when the requested cursor fell outside retention. */
  cursor_expired: boolean
}

/**
 * One frame from the fetch-based task stream.
 *
 * `stream.draining` is the one event that is not a `StreamEvent` envelope; it is
 * modelled separately so a strict envelope parser cannot fail the stream.
 */
export type GatewayStreamFrame =
  | { type: "event"; event: StreamEvent }
  | { type: "draining"; phase: string }

export type ApprovalStatus = "pending" | "approved" | "rejected" | "expired" | "revoked"

/** Worker-owned approval record. */
export interface GatewayApprovalRecord {
  id: string
  task_id: string
  namespace: string
  operation: string
  target: string
  digest: string
  risk: string
  requested_by: string
  expires_at: number
  status: ApprovalStatus
  decided_by?: string
  decided_at?: number
  receipt?: string
  created_at: number
}

/** `schema/gateway-v1.json#/definitions/gatewayApproval`. */
export interface GatewayApproval {
  task_id: string
  worker: string
  approval: GatewayApprovalRecord
}

/** `schema/gateway-v1.json#/definitions/approvalDecision`. */
export interface ApprovalDecision {
  decision: "approved" | "rejected"
  /** Exact digest the approver observed. */
  digest: string
}

/** `schema/gateway-v1.json#/definitions/artifact`. */
export interface Artifact {
  id: string
  task_id: string
  native_invocation_id: string
  attempt: number
  version: number
  namespace: string
  owner: string
  name: string
  content_type: string
  size: number
  digest: string
  finalized: boolean
  created_at: number
}

export interface ArtifactContent {
  blob: Blob
  contentType: string
  contentLength: number | undefined
  digest: string | undefined
  artifactId: string | undefined
}

/** One read-only Worker workspace entry. Paths are workspace-relative. */
export interface WorkspaceEntry {
  name: string
  path: string
  kind: "file" | "directory"
  size: number
  modified_at: number
  hidden: boolean
}

/** One bounded directory page from a Worker workspace. */
export interface WorkspaceTreePage {
  path: string
  entries: WorkspaceEntry[]
  next_cursor?: string
  truncated: boolean
}

/** One bounded text preview or metadata projection from a Worker workspace. */
export interface WorkspaceFile {
  path: string
  kind: "file"
  size: number
  modified_at: number
  digest?: string
  content_type?: string
  binary: boolean
  truncated: boolean
  text?: string
}

/** Bounded raw bytes for media/PDF/binary preview. */
export interface WorkspaceContent {
  blob: Blob
  contentType: string
  contentLength?: number
  contentRange?: string
  digest?: string
  status: number
}

export interface DiagnosticFinding {
  kind: string
  source: string
  code: string
  message: string
  action: string
  trace_id?: string
}

export interface DiagnosticStorage {
  metadata: string
  artifacts: string
  degraded: string[]
}

export interface DiagnosticTask {
  task_id: string
  state: TaskState
  attempt: number
  trace_id?: string
  profile: string
  profile_revision: number
  asset_revision: number
  traffic: string
  native_invocation_id?: string
  error?: TaskError
  delegations?: Array<Record<string, unknown>>
  approvals?: Array<Record<string, unknown>>
}

export interface DiagnosticCorrelation {
  trace_id?: string
  task_id?: string
  attempt?: number
  native_invocation_id?: string
}

/**
 * Operator diagnostic export. Deliberately omits prompt text, credential
 * values, and raw paths.
 */
export interface DiagnosticBundle {
  generated_at: number
  health: Record<string, unknown>
  metrics: Array<Record<string, unknown>>
  storage: DiagnosticStorage
  findings: DiagnosticFinding[]
  task?: DiagnosticTask
  correlation: DiagnosticCorrelation
  audit: Array<Record<string, unknown>>
  traces: Array<Record<string, unknown>>
  subsystems: Record<string, string>
}

export interface TeamMember {
  worker: string
  role?: string
  capabilities: string[]
  max_depth?: number
  instance?: string
}

/** `schema/gateway-v1.json#/definitions/team`. */
export interface Team {
  namespace?: string
  name: string
  version: number
  master: string
  members: TeamMember[]
  limits?: GatewayLimits
  locality?: "local" | "remote"
  active: boolean
  created_at?: number
  updated_at?: number
}

export interface CreateTeamRequest {
  name: string
  master: string
  members: TeamMember[]
  limits?: GatewayLimits
  locality?: "local" | "remote"
}

export interface UpdateTeamRequest {
  expected_version?: number
  master: string
  members: TeamMember[]
  limits?: GatewayLimits
  locality?: "local" | "remote"
}

export interface TeamVersionRequest {
  version?: number
}

export interface ContextCompaction {
  strategy: "truncation"
  context_budget: number
  preserve_recent: number
  max_retries: number
}

export interface ProfileRevision {
  revision: number
  instruction: string
  model: string
  capabilities?: string[]
  grants?: string[]
  restrictions?: string[]
  composition?: string
  members?: string[]
  skills?: string[]
  mcp?: string[]
  workflow?: string | null
  limits?: GatewayLimits
  compaction?: ContextCompaction | null
  secret_refs?: string[]
  created_at: number
}

export interface ProfileSelection {
  name: string
  active_revision: number
  revisions: ProfileRevision[]
}

/** `schema/gateway-v1.json#/definitions/profile`. */
export type Profile = ProfileSelection

export interface CreateProfileRequest {
  name: string
  revision: ProfileRevision
}

export interface ReviseProfileRequest {
  expected_revision: number
  revision: ProfileRevision
}

export interface ProfileVersionRequest {
  revision?: number
}

export interface DeletedProfile {
  deleted: string
}

export interface ProviderMetadata {
  id: string
  adapter: string
  model: string
  endpoint?: string
  modalities: string[]
  tool_calling: boolean
  context_window?: number
  local: boolean
  residency: string
  allowed_workers: string[]
  models: string[]
  models_source: string
  discovered_at?: number
  allowed_models?: string[]
  allow_auto_default: boolean
  ready: boolean
  discovery_error?: string
  discovery_error_code?: string
  revision: number
  active: boolean
  credential_version?: number
  credential_fingerprint?: string
  credential_revoked: boolean
  created_at?: number
  updated_at?: number
}

export interface CreateProviderRequest {
  id: string
  adapter: string
  model?: string
  endpoint?: string
  allowed_models?: string[]
  allow_auto_default?: boolean
  modalities?: string[]
  tool_calling?: boolean
  context_window?: number
  local?: boolean
  residency: string
  allowed_workers?: string[]
}

export interface UpdateProviderRequest {
  revision: number
  adapter?: string
  model?: string
  endpoint?: string
  allowed_models?: string[]
  allow_auto_default?: boolean
  modalities?: string[]
  tool_calling?: boolean
  context_window?: number
  local?: boolean
  residency?: string
  allowed_workers?: string[]
  active?: boolean
}

export interface ProviderCredentialSummary {
  provider: string
  version: number
  fingerprint: string
  created_at: number
  revoked_at?: number
  revision: number
  ready: boolean
  models: string[]
  discovered_at?: number
  discovery_error?: string
  discovery_error_code?: string
}

export interface PairCodeResponse {
  code: string
  link: string
  worker_id: string
  instance: string
  expires_at: number
}

export type PackageSource =
  | { kind: "folder"; path: string }
  | { kind: "url"; url: string }

export interface PackageSummary {
  package: string
  version: string
  artifact_digest: string
  signer: string
  target: string
}

export interface PackageList {
  packages: PackageSummary[]
}

export interface PackageInstallRequest {
  source: PackageSource
  package?: string
  expected_digest?: string
}

export interface PackageInstallOutcome {
  package: PackageSummary
  installed_now: boolean
}

export interface HostLimits {
  max_active: number
  max_queue: number
  per_principal: number
}

export interface HostProfileSelection {
  name: string
  create?: boolean
  instruction?: string
  members?: string[]
  capabilities?: string[]
  grants?: string[]
}

export interface HostProvisionRequest {
  operation_id: string
  instance: string
  worker: string
  release_digest: string
  config?: string | null
  workspace?: string | null
  read_only?: boolean
  grants?: string[]
  limits?: HostLimits
  secret_refs?: string[]
  profile?: HostProfileSelection
  addr?: string | null
  control_token?: string | null
}

export interface HostUpgradeRequest {
  operation_id: string
  release_digest: string
  authorized_permissions?: string[]
  work_policy?: "drain" | "pin" | "migrate"
  verify_input?: string
  addr?: string
}

export interface HostRollbackRequest {
  operation_id: string
}

export interface HostCompatibility {
  target: string
  protocol: string
  runtime: string
  adk: string
  compatible: boolean
  issues: string[]
}

export interface HostTrustView {
  decision: "continue" | "restrict" | "quarantine"
  reason: string
}

export interface HostInstanceView {
  host: string
  instance: string
  package: string
  version: string
  artifact_digest: string
  binary_digest: string
  signer: string
  target: string
  protocol: string
  lifecycle: string
  process_health: string
  task_activity: string
  runtime_phase?: string
  incarnation?: string
  active_profile?: string
  endpoint?: string
  grants?: string[]
  limits: HostLimits
  workspace_write: boolean
  secret_refs?: string[]
  operator_config: boolean
  trust?: HostTrustView
  compatibility: HostCompatibility
}

export interface HostDescriptor {
  host: string
  protocol: string
  adapter_version: string
  supported?: Array<
    "provision" | "inspect" | "upgrade" | "rollback" | "start" | "stop" | "restart"
  >
  reachable?: boolean
  error?: string
}

export interface HostInspection {
  host: string
  view: HostInstanceView
  worker?: Record<string, unknown>
}

export interface HostFinding {
  code: string
  message: string
}

export interface HostUpgradeSummary {
  current_digest: string
  candidate_digest: string
  permissions_added?: string[]
  permissions_removed?: string[]
  blocked?: HostFinding[]
  authorization_satisfied: boolean
  config_preserved: boolean
  schema_status: string
  work_action: string
  launcher_modes?: string[]
}

export interface HostRecoverySummary {
  binary_restored: boolean
  data_restored: boolean
  quarantine: boolean
  journal_status: string
}

export type HostOperationKind =
  | "provision"
  | "inspect"
  | "upgrade"
  | "rollback"
  | "start"
  | "stop"
  | "restart"

export interface HostOperationOutcome {
  operation_id: string
  kind: HostOperationKind
  host: string
  instance: string
  status: "succeeded" | "recovered" | "quarantined"
  view?: HostInstanceView
  worker?: Record<string, unknown>
  upgrade?: HostUpgradeSummary
  recovery?: HostRecoverySummary
  replayed?: boolean
}

export interface InstanceLogs {
  instance: string
  lines: string[]
  truncated?: boolean
}

export interface SocketTicket {
  /** Opaque one-use upgrade ticket. Never a bearer credential. */
  ticket: string
  expires_at_ms: number
  path: string
}

export interface SocketScope {
  worker?: string
  task_id?: string
}

export type SocketCommand =
  | { kind: "subscribe"; task_id: string; after: number }
  | { kind: "submit"; request: GatewaySubmitRequest }
  | { kind: "cancel"; task_id: string }
  | {
      kind: "approval"
      task_id: string
      approval_id: string
      decision: ApprovalDecision
    }

export interface SocketClientFrame {
  schema_version: 1
  command_id: string
  scope: SocketScope
  command: SocketCommand
}

export type SocketOutcome =
  | { kind: "subscribed"; task_id: string; after: number }
  | { kind: "submitted"; response: GatewaySubmitResponse }
  | { kind: "cancelled"; task: GatewayTask }
  | { kind: "approval"; approval: GatewayApproval }

export type SocketServerFrame =
  | {
      type: "ack"
      command_id: string
      outcome: SocketOutcome
    }
  | {
      type: "error"
      command_id?: string
      error: {
        code: string
        message: string
        retryable: boolean
        recovery: string
        details?: Record<string, string>
      }
    }
  | {
      type: "event"
      command_id: string
      task_id: string
      event: StreamEvent
    }
  | { type: "heartbeat"; at: number }

/** Per-call options accepted by every SDK method. */
export interface GatewayRequestOptions {
  signal?: AbortSignal
}

export type ConnectionResult =
  | {
      status: "connected"
      health: GatewayHealth
      workers: GatewayWorker[]
    }
  | {
      status: "unauthorized"
      health: GatewayHealth
      error: GatewayError
    }
  | {
      status: "forbidden"
      health: GatewayHealth
      error: GatewayError
    }
  | {
      status: "error"
      health: GatewayHealth
      error: GatewayError
    }
  | {
      status: "unreachable"
      error: GatewayError
    }

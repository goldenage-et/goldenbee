import type {
  Artifact,
  ControlAuditEntry,
  DiagnosticBundle,
  GatewayApproval,
  GatewayHealth,
  GatewaySubmitResponse,
  GatewayTask,
  HostDescriptor,
  HostInspection,
  HostInstanceView,
  HostOperationOutcome,
  InstanceLogs,
  PackageInstallOutcome,
  PackageList,
  PairCodeResponse,
  Profile,
  ProviderCredentialSummary,
  ProviderMetadata,
  RegistryAuditEntry,
  SocketTicket,
  StreamEvent,
  StreamPage,
  Team,
  WorkerSummary,
  WorkspaceEntry,
  WorkspaceFile,
  WorkspaceTreePage,
} from "./types.js"

/** Minimal runtime guards. They reject wrong top-level shapes, not every field. */

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function isString(value: unknown): value is string {
  return typeof value === "string"
}

function isFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value)
}

function isArray(value: unknown): value is unknown[] {
  return Array.isArray(value)
}

function isArrayOf<T>(
  value: unknown,
  guard: (item: unknown) => item is T,
): value is T[] {
  return isArray(value) && value.every(guard)
}

export function isHealth(value: unknown): value is GatewayHealth {
  return (
    isRecord(value) &&
    isString(value.status) &&
    isFiniteNumber(value.workers) &&
    isFiniteNumber(value.reachable)
  )
}

const workerStatuses = new Set<string>([
  "approved",
  "registered",
  "unreachable",
  "disabled",
  "decommissioned",
])

export function isWorker(value: unknown): value is WorkerSummary {
  return (
    isRecord(value) &&
    isString(value.id) &&
    isString(value.instance) &&
    isString(value.package) &&
    isString(value.version) &&
    isString(value.status) &&
    workerStatuses.has(value.status) &&
    typeof value.reachable === "boolean" &&
    (value.locality === "local" || value.locality === "remote")
  )
}

export function isWorkerList(value: unknown): value is WorkerSummary[] {
  return isArrayOf(value, isWorker)
}

function isAuditEntry(value: unknown): value is RegistryAuditEntry {
  return (
    isRecord(value) &&
    isFiniteNumber(value.at) &&
    isString(value.action) &&
    (value.outcome === "accepted" || value.outcome === "denied")
  )
}

export function isRegistryAuditList(
  value: unknown,
): value is RegistryAuditEntry[] {
  return isArrayOf(value, isAuditEntry)
}

export function isControlAuditList(
  value: unknown,
): value is ControlAuditEntry[] {
  return isArrayOf(
    value,
    (entry): entry is ControlAuditEntry =>
      isRecord(entry) &&
      isFiniteNumber(entry.at) &&
      isString(entry.action) &&
      isString(entry.session_id) &&
      (entry.outcome === "accepted" || entry.outcome === "rejected"),
  )
}

export function isDiagnosticBundle(value: unknown): value is DiagnosticBundle {
  return (
    isRecord(value) &&
    isFiniteNumber(value.generated_at) &&
    isRecord(value.health) &&
    isArray(value.metrics) &&
    isRecord(value.storage) &&
    isArray(value.findings) &&
    isRecord(value.correlation)
  )
}

const taskStates = new Set<string>([
  "accepted",
  "queued",
  "running",
  "waiting_approval",
  "cancelling",
  "recovering",
  "unknown",
  "succeeded",
  "failed",
  "cancelled",
])

export function isSubmitResponse(value: unknown): value is GatewaySubmitResponse {
  return (
    isRecord(value) &&
    isString(value.task_id) &&
    isString(value.worker) &&
    isString(value.worker_instance) &&
    isString(value.agent) &&
    isString(value.profile) &&
    isString(value.state) &&
    taskStates.has(value.state) &&
    typeof value.replayed === "boolean"
  )
}

export function isTask(value: unknown): value is GatewayTask {
  return (
    isRecord(value) &&
    isString(value.task_id) &&
    isString(value.worker) &&
    isString(value.worker_instance) &&
    isString(value.connectivity) &&
    isRecord(value.task) &&
    isString(value.task.id) &&
    isString(value.task.state) &&
    taskStates.has(value.task.state)
  )
}

function isStreamEvent(value: unknown): value is StreamEvent {
  return (
    isRecord(value) &&
    isString(value.event_id) &&
    isString(value.stream_id) &&
    isFiniteNumber(value.seq) &&
    isString(value.kind) &&
    typeof value.durable === "boolean" &&
    isFiniteNumber(value.at)
  )
}

export function isStreamPage(value: unknown): value is StreamPage {
  return (
    isRecord(value) &&
    isString(value.stream_id) &&
    isArrayOf(value.events, isStreamEvent) &&
    isFiniteNumber(value.next_seq) &&
    isFiniteNumber(value.oldest_seq) &&
    typeof value.cursor_expired === "boolean"
  )
}

export function isApproval(value: unknown): value is GatewayApproval {
  return (
    isRecord(value) &&
    isString(value.task_id) &&
    isString(value.worker) &&
    isRecord(value.approval) &&
    isString(value.approval.id) &&
    isString(value.approval.digest) &&
    isString(value.approval.status)
  )
}

export function isApprovalList(value: unknown): value is GatewayApproval[] {
  return isArrayOf(value, isApproval)
}

export function isArtifact(value: unknown): value is Artifact {
  return (
    isRecord(value) &&
    isString(value.id) &&
    isString(value.task_id) &&
    isString(value.name) &&
    isString(value.content_type) &&
    isString(value.digest) &&
    isFiniteNumber(value.size)
  )
}

export function isArtifactList(value: unknown): value is Artifact[] {
  return isArrayOf(value, isArtifact)
}

export function isWorkspaceEntry(value: unknown): value is WorkspaceEntry {
  return (
    isRecord(value) &&
    isString(value.name) &&
    isString(value.path) &&
    (value.kind === "file" || value.kind === "directory") &&
    isFiniteNumber(value.size) &&
    isFiniteNumber(value.modified_at) &&
    typeof value.hidden === "boolean"
  )
}

export function isWorkspaceTreePage(value: unknown): value is WorkspaceTreePage {
  return (
    isRecord(value) &&
    isString(value.path) &&
    isArrayOf(value.entries, isWorkspaceEntry) &&
    (value.next_cursor === undefined || isString(value.next_cursor)) &&
    typeof value.truncated === "boolean"
  )
}

export function isWorkspaceFile(value: unknown): value is WorkspaceFile {
  return (
    isRecord(value) &&
    isString(value.path) &&
    value.kind === "file" &&
    isFiniteNumber(value.size) &&
    isFiniteNumber(value.modified_at) &&
    (value.digest === undefined || isString(value.digest)) &&
    (value.content_type === undefined || isString(value.content_type)) &&
    typeof value.binary === "boolean" &&
    typeof value.truncated === "boolean" &&
    (value.text === undefined || isString(value.text))
  )
}

export function isSocketTicket(value: unknown): value is SocketTicket {
  return (
    isRecord(value) &&
    isString(value.ticket) &&
    isFiniteNumber(value.expires_at_ms) &&
    isString(value.path)
  )
}

export function isTeam(value: unknown): value is Team {
  return (
    isRecord(value) &&
    isString(value.name) &&
    isFiniteNumber(value.version) &&
    isString(value.master) &&
    isArray(value.members) &&
    typeof value.active === "boolean"
  )
}

export function isTeamList(value: unknown): value is Team[] {
  return isArrayOf(value, isTeam)
}

export function isProfile(value: unknown): value is Profile {
  return (
    isRecord(value) &&
    isString(value.name) &&
    isFiniteNumber(value.active_revision) &&
    isArray(value.revisions)
  )
}

export function isProfileList(value: unknown): value is Profile[] {
  return isArrayOf(value, isProfile)
}

export function isDeletedProfile(value: unknown): value is { deleted: string } {
  return isRecord(value) && isString(value.deleted)
}

export function isProvider(value: unknown): value is ProviderMetadata {
  return (
    isRecord(value) &&
    isString(value.id) &&
    isString(value.adapter) &&
    isFiniteNumber(value.revision) &&
    typeof value.active === "boolean"
  )
}

export function isProviderList(value: unknown): value is ProviderMetadata[] {
  return isArrayOf(value, isProvider)
}

export function isCredentialSummary(
  value: unknown,
): value is ProviderCredentialSummary {
  return (
    isRecord(value) &&
    isString(value.provider) &&
    isFiniteNumber(value.version) &&
    isString(value.fingerprint)
  )
}

export function isPairCode(value: unknown): value is PairCodeResponse {
  return (
    isRecord(value) &&
    isString(value.code) &&
    isString(value.link) &&
    isString(value.worker_id) &&
    isString(value.instance)
  )
}

export function isPackageList(value: unknown): value is PackageList {
  return isRecord(value) && isArray(value.packages)
}

export function isInstallOutcome(value: unknown): value is PackageInstallOutcome {
  return (
    isRecord(value) &&
    isRecord(value.package) &&
    isString(value.package.package) &&
    typeof value.installed_now === "boolean"
  )
}

export function isHostDescriptor(value: unknown): value is HostDescriptor {
  return (
    isRecord(value) &&
    isString(value.host) &&
    isString(value.protocol) &&
    isString(value.adapter_version)
  )
}

export function isHostDescriptorList(value: unknown): value is HostDescriptor[] {
  return isArrayOf(value, isHostDescriptor)
}

export function isHostInstance(value: unknown): value is HostInstanceView {
  return (
    isRecord(value) &&
    isString(value.host) &&
    isString(value.instance) &&
    isString(value.lifecycle) &&
    isRecord(value.compatibility)
  )
}

export function isHostInstanceList(value: unknown): value is HostInstanceView[] {
  return isArrayOf(value, isHostInstance)
}

export function isHostInspection(value: unknown): value is HostInspection {
  return isRecord(value) && isString(value.host) && isHostInstance(value.view)
}

export function isHostOperationOutcome(
  value: unknown,
): value is HostOperationOutcome {
  return (
    isRecord(value) &&
    isString(value.operation_id) &&
    isString(value.kind) &&
    isString(value.host) &&
    isString(value.instance) &&
    isString(value.status)
  )
}

export function isInstanceLogs(value: unknown): value is InstanceLogs {
  return (
    isRecord(value) &&
    isString(value.instance) &&
    isArrayOf(value.lines, isString)
  )
}

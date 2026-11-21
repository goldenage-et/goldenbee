import type {
  CreateProfileRequest,
  GatewayLimits,
  Profile,
  ProfileRevision,
  ReviseProfileRequest,
} from "@goldenbee/sdk"

export type ProfileComposition =
  | "single"
  | "sequential"
  | "parallel"
  | "loop"
  | "team"

export const profileCompositions: ProfileComposition[] = [
  "single",
  "sequential",
  "parallel",
  "loop",
  "team",
]

export interface ProfileFormValues {
  name: string
  instruction: string
  model: string
  capabilities: string
  grants: string
  restrictions: string
  composition: ProfileComposition
  members: string
  skills: string
  mcp: string
  workflow: string
  secretRefs: string
  maxDurationMs: string
  maxModelCalls: string
  maxToolCalls: string
  maxTokens: string
  maxCostMicros: string
  maxFanout: string
  maxDepth: string
  compactionEnabled: boolean
  contextBudget: string
  preserveRecent: string
  maxRetries: string
}

export type ProfileFormField =
  | "name"
  | "instruction"
  | "model"
  | "composition"
  | "secret_refs"
  | "max_duration_ms"
  | "max_model_calls"
  | "max_tool_calls"
  | "max_tokens"
  | "max_cost_micros"
  | "max_fanout"
  | "max_depth"
  | "context_budget"
  | "preserve_recent"
  | "max_retries"

export interface ProfileFormIssue {
  field: ProfileFormField
  message: string
}

export interface ProfileRequestBuild<T> {
  request?: T
  issues: ProfileFormIssue[]
}

export function emptyProfileFormValues(): ProfileFormValues {
  return {
    name: "",
    instruction: "",
    model: "",
    capabilities: "",
    grants: "",
    restrictions: "",
    composition: "single",
    members: "",
    skills: "",
    mcp: "",
    workflow: "",
    secretRefs: "",
    maxDurationMs: "",
    maxModelCalls: "",
    maxToolCalls: "",
    maxTokens: "",
    maxCostMicros: "",
    maxFanout: "",
    maxDepth: "",
    compactionEnabled: false,
    contextBudget: "",
    preserveRecent: "8",
    maxRetries: "2",
  }
}

function splitList(value: string): string[] {
  return value
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean)
}

function parseWhole(
  raw: string,
): { value: number } | { error: "not-a-number" | "negative" } | undefined {
  const trimmed = raw.trim()
  if (!trimmed) {
    return undefined
  }
  const value = Number(trimmed)
  if (!Number.isInteger(value)) {
    return { error: "not-a-number" }
  }
  if (value < 0) {
    return { error: "negative" }
  }
  return { value }
}

function validateWhole(
  issues: ProfileFormIssue[],
  field: ProfileFormField,
  raw: string,
  label: string,
): number | undefined {
  const parsed = parseWhole(raw)
  if (!parsed) {
    return undefined
  }
  if ("error" in parsed) {
    issues.push({
      field,
      message: `${label} must be a whole number.`,
    })
    return undefined
  }
  return parsed.value
}

const secretReferencePrefixes = ["secret://", "env:", "vault:"]

function isSecretReference(reference: string): boolean {
  const looksLikeValue =
    reference.startsWith("sk-") ||
    reference.startsWith("Bearer ") ||
    reference.startsWith("AKIA")
  return (
    !looksLikeValue &&
    reference.length <= 256 &&
    secretReferencePrefixes.some((prefix) => reference.startsWith(prefix))
  )
}

/** Validate the operator's revision before it becomes a Gateway request. */
export function validateProfileForm(
  values: ProfileFormValues,
): ProfileFormIssue[] {
  const issues: ProfileFormIssue[] = []

  if (!values.name.trim()) {
    issues.push({ field: "name", message: "Enter a profile name." })
  }
  if (!values.instruction.trim()) {
    issues.push({ field: "instruction", message: "Enter an agent instruction." })
  }
  if (!values.model.trim()) {
    issues.push({ field: "model", message: "Enter a model." })
  }
  if (!profileCompositions.includes(values.composition)) {
    issues.push({
      field: "composition",
      message: "Choose a supported composition.",
    })
  }

  for (const reference of splitList(values.secretRefs)) {
    if (!isSecretReference(reference)) {
      issues.push({
        field: "secret_refs",
        message:
          "Secret references must use secret://, env:, or vault: names, never credential values.",
      })
      break
    }
  }

  validateWhole(issues, "max_duration_ms", values.maxDurationMs, "Max duration")
  validateWhole(issues, "max_model_calls", values.maxModelCalls, "Max model calls")
  validateWhole(issues, "max_tool_calls", values.maxToolCalls, "Max tool calls")
  validateWhole(issues, "max_tokens", values.maxTokens, "Max tokens")
  validateWhole(issues, "max_cost_micros", values.maxCostMicros, "Max cost")
  validateWhole(issues, "max_fanout", values.maxFanout, "Max fanout")
  validateWhole(issues, "max_depth", values.maxDepth, "Max depth")

  if (values.compactionEnabled) {
    const budget = parseWhole(values.contextBudget)
    if (!budget || ("value" in budget && budget.value === 0)) {
      issues.push({
        field: "context_budget",
        message: "Context budget must be a positive whole number.",
      })
    } else if ("error" in budget) {
      issues.push({
        field: "context_budget",
        message: "Context budget must be a positive whole number.",
      })
    }
    validateWhole(issues, "preserve_recent", values.preserveRecent, "Preserve recent")
    validateWhole(issues, "max_retries", values.maxRetries, "Max retries")
  }

  return issues
}

function buildLimits(values: ProfileFormValues): GatewayLimits | undefined {
  const limits: GatewayLimits = {
    ...(positive(values.maxDurationMs) !== undefined
      ? { max_duration_ms: positive(values.maxDurationMs) }
      : {}),
    ...(positive(values.maxModelCalls) !== undefined
      ? { max_model_calls: positive(values.maxModelCalls) }
      : {}),
    ...(positive(values.maxToolCalls) !== undefined
      ? { max_tool_calls: positive(values.maxToolCalls) }
      : {}),
    ...(positive(values.maxTokens) !== undefined
      ? { max_tokens: positive(values.maxTokens) }
      : {}),
    ...(positive(values.maxCostMicros) !== undefined
      ? { max_cost_micros: positive(values.maxCostMicros) }
      : {}),
    ...(positive(values.maxFanout) !== undefined
      ? { max_fanout: positive(values.maxFanout) }
      : {}),
    ...(positive(values.maxDepth) !== undefined
      ? { max_depth: positive(values.maxDepth) }
      : {}),
  }
  return Object.keys(limits).length > 0 ? limits : undefined
}

function positive(raw: string): number | undefined {
  const parsed = parseWhole(raw)
  return parsed && "value" in parsed && parsed.value > 0 ? parsed.value : undefined
}

function buildRevision(values: ProfileFormValues): ProfileRevision {
  const workflow = values.workflow.trim()
  const limits = buildLimits(values)
  const compaction = values.compactionEnabled
    ? {
        strategy: "truncation" as const,
        context_budget: Number(values.contextBudget),
        preserve_recent: Number(values.preserveRecent || 0),
        max_retries: Number(values.maxRetries || 0),
      }
    : undefined

  return {
    revision: 0,
    created_at: 0,
    instruction: values.instruction.trim(),
    model: values.model.trim(),
    capabilities: splitList(values.capabilities),
    grants: splitList(values.grants),
    restrictions: splitList(values.restrictions),
    composition: values.composition,
    members: splitList(values.members),
    skills: splitList(values.skills),
    mcp: splitList(values.mcp),
    ...(workflow ? { workflow } : {}),
    ...(limits ? { limits } : {}),
    ...(compaction ? { compaction } : {}),
    secret_refs: splitList(values.secretRefs),
  }
}

export function buildCreateProfileRequest(
  values: ProfileFormValues,
): ProfileRequestBuild<CreateProfileRequest> {
  const issues = validateProfileForm(values)
  if (issues.length > 0) {
    return { issues }
  }
  return {
    issues,
    request: { name: values.name.trim(), revision: buildRevision(values) },
  }
}

export function buildReviseProfileRequest(
  values: ProfileFormValues,
  expectedRevision: number,
): ProfileRequestBuild<ReviseProfileRequest> {
  const issues = validateProfileForm(values)
  if (issues.length > 0) {
    return { issues }
  }
  return {
    issues,
    request: {
      expected_revision: expectedRevision,
      revision: buildRevision(values),
    },
  }
}

/** Seed the editor from the revision the operator is about to replace. */
export function profileFormValues(
  profile: Profile,
  revision?: ProfileRevision,
): ProfileFormValues {
  const source =
    revision ??
    profile.revisions.find(
      (candidate) => candidate.revision === profile.active_revision,
    ) ??
    profile.revisions[profile.revisions.length - 1]
  const values = emptyProfileFormValues()
  if (!source) {
    return { ...values, name: profile.name }
  }
  return {
    ...values,
    name: profile.name,
    instruction: source.instruction,
    model: source.model,
    capabilities: (source.capabilities ?? []).join(", "),
    grants: (source.grants ?? []).join(", "),
    restrictions: (source.restrictions ?? []).join(", "),
    composition: (source.composition ?? "single") as ProfileComposition,
    members: (source.members ?? []).join(", "),
    skills: (source.skills ?? []).join(", "),
    mcp: (source.mcp ?? []).join(", "),
    workflow: source.workflow ?? "",
    secretRefs: (source.secret_refs ?? []).join(", "),
    maxDurationMs: source.limits?.max_duration_ms
      ? String(source.limits.max_duration_ms)
      : "",
    maxModelCalls: source.limits?.max_model_calls
      ? String(source.limits.max_model_calls)
      : "",
    maxToolCalls: source.limits?.max_tool_calls
      ? String(source.limits.max_tool_calls)
      : "",
    maxTokens: source.limits?.max_tokens
      ? String(source.limits.max_tokens)
      : "",
    maxCostMicros: source.limits?.max_cost_micros
      ? String(source.limits.max_cost_micros)
      : "",
    maxFanout: source.limits?.max_fanout
      ? String(source.limits.max_fanout)
      : "",
    maxDepth: source.limits?.max_depth ? String(source.limits.max_depth) : "",
    compactionEnabled: Boolean(source.compaction),
    contextBudget: source.compaction
      ? String(source.compaction.context_budget)
      : "",
    preserveRecent: source.compaction
      ? String(source.compaction.preserve_recent)
      : "8",
    maxRetries: source.compaction ? String(source.compaction.max_retries) : "2",
  }
}

/** Latest immutable revision number, used as `expected_revision`. */
export function latestRevision(profile: Profile): ProfileRevision | undefined {
  return profile.revisions[profile.revisions.length - 1]
}

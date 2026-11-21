# Worker protocol v1

A Worker Package composes the native ADK server with the harness management
routes. Native protocol surfaces are reused, not cloned. New management
behavior is versioned independently and uses the stable error object defined in
`crates/common`.

## Startup configuration

The packaged executable reads an operator-selected TOML file with
`--config <path>`. Relative agent, prompt, skill, and storage paths resolve
against that file's directory. See [Worker configuration](worker-config.md) for
the schema, automatic asset loading, hooks, native callbacks/plugins, and reload
behavior.

Profile revisions are managed through the Worker-native CLI (`profile
list|show|create|revise|activate|deactivate|export`) using the instance's
development token principal. Product profile management uses the SDK and
Gateway routes below; those routes forward to this Worker's existing immutable
revision store and never create a second profile loader or authority.

## Endpoint audit

| Path | Classification | Notes |
| --- | --- | --- |
| `/ui/`, `/` | Native | ADK web UI assets. |
| `/.well-known/agent.json` | Native | Public A2A discovery card. |
| `/api/health` | Native | Public liveness. |
| `/api/apps`, `/api/sessions*`, `/api/run*` | Native | ADK REST execution and session routes. |
| `/a2a`, `/a2a/stream` | Native | ADK A2A JSON-RPC transport. |
| `/v1/health` | New | Minimal public liveness. |
| `/v1/status` | New | Authenticated health, readiness, lifecycle, and capacity projection. |
| `/v1/capabilities` | New | Authenticated manifest, command inventory, and durability disclosure. |
| `/v1/commands` | New | List runtime and Worker-defined commands. Requires `task:read`. |
| `/v1/commands/{name}` | New | Read one command descriptor, or invoke it with `POST`. Invocation requires `task:submit`. |
| `/v1/drain` | New | Authorized bounded drain. Requires `instance:admin`. |
| `/v1/tasks` | New | Durable task admission with caller-scoped idempotency. |
| `/v1/tasks/{id}` | New | Durable task projection. |
| `/v1/tasks/{id}/events` | New | Retained task event page. |
| `/v1/tasks/{id}/events/stream` | New | Replays retained events, then follows the live broadcast. Uses a bounded cursor contract; model token deltas are non-durable. |
| `/v1/tasks/{id}/artifacts` | New | Artifact metadata for one visible task. |
| `/v1/tasks/{id}/cancel` | New | Precise task cancellation. |
| `/v1/approvals` | New | Exact-protected-operation requests visible in the caller's namespace. Requires `approval:read`. |
| `/v1/approvals/{id}` | New | One approval with its operation, target, digest, risk, expiry, decision, and receipt. Requires `approval:read`. |
| `/v1/approvals/{id}/decision` | New | Approve or reject the exact digest. Requires `approval:decide`. |
| `/v1/approvals/{id}/revoke` | New | Revoke a pending approval before a decision. Requires `approval:decide`. |
| `/v1/profiles` | New | List or create Runtime Profiles in the caller namespace. Read requires `profile:read`; create requires `profile:write`. |
| `/v1/profiles/{name}` | New | Fetch, revise, or delete one profile. Revision and deletion require `profile:write`. |
| `/v1/profiles/{name}/validate` | New | Validate one immutable revision without activating it. Requires `profile:read`. |
| `/v1/profiles/{name}/activate` | New | Validate then activate one exact revision. Requires `profile:write`. |
| `/v1/profiles/{name}/deactivate` | New | Stop new admissions without rewriting active work. Requires `profile:write`. |
| `/v1/artifacts/{id}` | New | Immutable artifact metadata. |
| `/v1/artifacts/{id}/content` | New | Authenticated, digest-verified byte transfer. |
| `/v1/artifacts/{id}/grant` | New | Short-lived scoped download grant. |
| `/v1/peers` | New | Authorized static peers with verified instance identity and effective capabilities. |
| `/v1/peers/{peer}` | New | One configured peer; `POST .../resolve` verifies endpoint identity before use. |
| `/v1/peers/{peer}/delegate` | New | Delegate one capability/objective through native A2A and persist the child binding. |
| `/v1/teams/{team}/members/{member}/delegate` | New | Delegate to a member of the Master task's signed, immutable Gateway roster. |
| `/v1/delegations` | New | Caller-scoped child delegation bindings. |
| `/v1/delegations/{id}` | New | Query or reattach the existing child; never resubmits. |
| `/v1/delegations/{id}/recover` | New | Explicit reattach after a broken stream or restart. |
| `/v1/delegations/{id}/cancel` | New | Cancel the exact remote A2A task. |
| `/v1/plans` | New | Create or list durable Master plans bound to a Master task. |
| `/v1/plans/{id}` | New | Durable plan context, child handles, observed evidence, and evaluation. |
| `/v1/plans/{id}/dispatch` | New | Bounded concurrent child dispatch under the frozen authority. |
| `/v1/plans/{id}/recover` | New | Query stored child handles after restart and re-evaluate; never replaces a child. |
| `/v1/plans/{id}/evaluate` | New | Re-evaluate criteria against observed evidence. |
| `/v1/plans/{id}/cancel` | New | Cancel every exact child handle owned by the plan. |
| `/v1/plans/{id}/escalate` | New | Explicit partial/uncertain escalation without widening authority. |
| `/v1/artifact-grants/{grant}` | New | Grant-only byte transfer. |
| `/v1/retention` | New | Apply the configured per-owner retention policy. Requires `instance:admin`. |
| `/v1/principals/{principal}/erase` | New | Authorized principal erasure with a durable deletion report. Requires `admin`; another principal requires `instance:admin`. |

The Gateway proxies only its documented task routes to the Worker. It does not
proxy Worker admin, native agent, session, A2A, artifact, or model routes.
Native routes keep their upstream behavior and schemas.
Harness routes never reimplement ADK session or A2A task storage.

## Authentication

- Every route except `/`, `/ui/*`, `/v1/health`, `/api/health`,
  `/.well-known/agent.json`, and `/v1/artifact-grants/{grant}` requires
  `Authorization: Bearer <token>`. A scoped artifact grant is itself the
  bearer credential for one artifact and expiry window; it grants no metadata
  or other route access.
- A local operator token resolves through the Worker token bindings.
- A Gateway delegation token is verified with the instance's delegation secret.
  Its `audience` must equal the Worker instance identity, its expiry and depth
  must be valid, and its capability list narrows admission.
- Native development routes accept local operator tokens only. Product traffic
  uses `/v1`.
- Explicit local UI development sets `GOLDENBEE_DEV_UI=1`. In that mode only, a
  loopback peer can call native `/api/*` and `/a2a` routes without a bearer
  token, because the upstream ADK web UI cannot attach one. Harness `/v1/*`
  routes still require a token. Non-loopback peers never get this exception,
  and the mode is off by default.
- Unauthenticated, wrong-audience, and capability-escaping requests fail with
  stable errors and no secret content in the response.

## Process-level team delegation

A Gateway team submission arrives with a signed roster snapshot and the
Master's frozen team version. `POST /v1/teams/{team}/members/{member}/delegate`
selects only a member present in that snapshot; it never consults Gateway
discovery or a static peer list. The requested capability must be bound to the
member. The Worker rejects the Master itself, any instance already in the
delegation path, a member outside the snapshot, and any request that would widen
the parent task's scopes, limits, expiry, depth, or fanout. A later roster
revision does not change the parent task's snapshot, so an already running
Master keeps delegating within the version it started under. Child authority is
carried to the member by a narrower signed A2A token and recorded in the normal
delegation binding.

## Master plans and evidence evaluation

A designated Master is an ordinary Worker Instance. It creates a durable plan
with `POST /v1/plans` bound to its own harness task. The plan copies the task's
frozen team roster, or uses the operator's authorized static peers when no team
is present, and records package, package version, instance, profile, profile
revision, and asset revision provenance.

- Children are planned before dispatch. `POST /v1/plans/{id}/dispatch` starts
  them under deterministic idempotency keys, so repeating the call or recovering
  after a crash reattaches the existing child instead of submitting the
  objective again. Each child is authorized by the same narrowed delegation path
  used by `/v1/delegations`; a roster plan cannot select a hidden member, a
  capability outside that member's grant, or a wider depth/fanout/limit.
- The Master records the observed remote task id, state, output, and immutable
  text artifact digests from the native A2A task. Criteria require observed
  content (`output_contains`) or an observed artifact with a `sha256:` digest
  (`artifact_digest`). A child's completed state alone can never verify an
  objective. Evaluation reports `satisfied`, `missing`, or `conflicting`
  evidence per criterion.
- `POST /v1/plans/{id}/recover` restores the persisted plan context and queries
  each stored binding by its exact id or deterministic context. A binding that
  exists is never replaced. A child with no persisted binding cannot have sent
  an objective and may be dispatched once. The response records queried handles,
  duplicate count zero, and any unresolved uncertainty.
- An unreachable or saturated child keeps its stored handle and truthful
  `peer.*`/`delegation.*` error. Dispatch and recovery accept a bounded
  `wait_ms`; an unresolved plan reports partial evidence. `POST .../escalate`
  records an explicit reason without changing the frozen roster, grants, or
  child authority. `POST .../cancel` addresses each exact remote task id.
- Gateway is not required for dispatch or recovery. A team task that already
  reached its Master continues over native A2A while Gateway is down.
## Commands

`GET /v1/commands` returns the same inventory the CLI prints: neutral
`session.*`, `context.info`, `permissions.info`, `skills.list`, and
`commands.list` entries plus Worker commands loaded from `paths.commands`.

- `GET /v1/commands/{name}` requires `task:read`.
- `POST /v1/commands/{name}` requires `task:submit` and accepts
  `{"input": "...", "session_id": "...", "idempotency_key": "...",
  "agent": "...", "limits": {...}}`.
- Neutral commands return `{"command", "kind": "neutral", "result"}`.
- Worker commands return `{"command", "kind": "worker", "result", "task"}` and
  admit a normal durable task for the prebuilt narrowed command agent.
- Malformed arguments, an agent override that differs from the command target,
  and an undeclared model or tool fail before task admission.

Worker commands can only narrow their target agent. Their `allowed_tools` must
be a subset of tools the target agent already binds, and their `model` must be a
model the Worker already declares. Command assets are resolved against the
operator-selected configuration directory.

## Runtime Profile management

Profiles remain Worker-owned configuration. Product Clients reach these routes
only through the Gateway and SDK; the routes expose the same immutable revision
store used by native startup and execution.

- `GET /v1/profiles` and `GET /v1/profiles/{name}` require `profile:read`.
- `POST /v1/profiles` creates revision 1 in the authenticated caller namespace.
  It requires `profile:write` and does not activate the profile.
- `PUT /v1/profiles/{name}` appends a revision only when
  `expected_revision` equals the observed revision count. A stale writer gets
  `profile.revision_conflict` with `expected` and `current` details.
- `POST /v1/profiles/{name}/validate` validates an existing revision against
  the same resolved file-loaded agents, skills, model providers, instance
  grants, restrictions, and MCP policy used by activation. It changes nothing.
- `POST /v1/profiles/{name}/activate` requires an exact `revision`, revalidates
  it, and leaves the previous active revision unchanged on any failure.
- `POST /v1/profiles/{name}/deactivate` stops new admission. Running work keeps
  the revision it already pinned.
- `DELETE /v1/profiles/{name}` removes an inactive profile without touching the
  installed package or its file assets.
- Responses contain profile state and secret references only. Instructions are
  redacted by Client presentation, never replaced by a second stored value.

## Static peers and native A2A delegation

A Worker can delegate to an explicitly authorized remote Worker Instance
without Gateway discovery. The transport is A2A v1 (`SendMessage`, `GetTask`,
`ListTasks`, `CancelTask`, and the streaming send used by the adapter); the
remote side remains the native `RequestHandler`/`V1Executor`. The shared
`common` crate and Gateway acquire no A2A engine.

- `GET /v1/peers` lists only configured peers. `POST /v1/peers/{id}/resolve`
  fetches the peer card, checks the advertised JSONRPC 1.0 interface stays on
  the configured origin, and matches `/v1/health` instance identity. Endpoint
  credentials, paths, and insecure non-loopback transports are rejected before
  a connection.
- A peer record exposes the installed instance id, package name/version, the
  card's advertised agent skills, and the effective capabilities (configured ∩
  advertised). Tools and toolsets are not advertised as A2A skills.
- A child delegation carries only the requested capability. Its audience,
  expiry, scopes, depth, fanout, and resource limits are narrowed from the
  parent authority. A parent that is itself delegated may not outlive or widen
  its own claims. Reusing a delegation token for a different message id is
  denied durably; the same message id remains the native idempotent replay.
- The binding (`/v1/delegations`) stores the deterministic message/context ids,
  remote task id, remote state, output, and parent task/instance correlation.
  After a broken stream or process restart, status, recovery, and cancellation
  list by context or read the exact remote task id; they never submit the
  objective again.
- Cancellation addresses the stored remote task id. A terminal task returns its
  authoritative state. Unreachable, saturated, denied, unavailable-capability,
  and not-cancelable conditions use stable `peer.*`/`delegation.*` codes with
  recovery guidance.

## Lifecycle, readiness, and drain

`/v1/health` is minimal liveness: `{status, instance, incarnation}`. It stays
`200` while a model provider or execution subsystem is degraded. Use it for
process supervision, not for routing decisions.

`/v1/status` requires `task:read` and returns the full projection:

| Field | Meaning |
| --- | --- |
| `phase`, `lifecycle` | `initializing`, `recovering`, `warming`, `ready`, `draining`, `stopped`, `stopped_degraded`, or `failed`. |
| `phase_history` | Ordered phases observed by this process. |
| `ready`, `readiness_reason` | Readiness derives from the phase and the admission gate only. |
| `admission` | `open` or `closed`. |
| `instance`, `incarnation`, `config_revision` | Instance and configuration identity. |
| `package`, `version` | Installed package identity. One executable is one package; Agents inside it are not roles silently combined from other packages. |
| `capabilities`, `capacity` | Advertised capability ids and finite active/queued capacity. |
| `subsystems` | Observed subsystem state. `provider` is `unknown` until a provider probe owns it; provider failure changes a task outcome, never liveness or readiness. |
| `degraded` | Explicit reasons such as `drain_timeout`, `shutdown_hook_failed`, or `startup_hook_failed`. Empty means no observed degradation. |

`POST /v1/drain` requires `instance:admin`. It is idempotent and returns the
current phase plus `already_draining`, `timed_out`, `forced_cancelled`,
`unfinished`, `active`, and `queued`.

Drain sequence:

1. Atomically close admission. A submission that already holds the admission
   read gate may finish its durable acceptance; every later submission receives
   retryable `lifecycle.draining` (`503`).
2. Readiness becomes false and connected event streams emit `stream.draining`
   and close before axum waits for connections.
3. Admitted work, including approval-paused tasks, may finish within
   `drain_timeout_ms`.
4. At the deadline, remaining execution tokens are cancelled. Work that never
   started stays accepted or queued for restart recovery. Work that was active
   becomes `recovering` with explicit uncertainty; it is never rewritten to
   `succeeded` or `failed`.
5. Package shutdown hooks run, then native plugin/executor close, then owned
   resources stop. A failed hook is recorded and the phase is
   `stopped_degraded`; failure cannot skip cleanup or fabricate a clean stop.

`SIGTERM` runs the same sequence before listener close. A second `SIGTERM`
forces remaining execution tokens. `SIGHUP` remains configuration reload.

## Correlated diagnostics, metrics, and audit

Every admitted task carries a bounded W3C trace identity. A `traceparent`
header on `POST /v1/tasks` is accepted when it is a valid 32-hex trace id;
missing, malformed, secret-bearing, or all-zero values are replaced rather than
propagated. The same identity appears on the durable task, every retained
event, artifact provenance, native A2A delegation metadata
(`goldenbee.trace_id`), audit correlation, and `GET /v1/diagnostics?task=<id>`.
Package, instance, profile revision, asset revision, attempt, and native
invocation identity remain attached across each hop.

`GET /v1/diagnostics` requires `task:read` and composes evidence the Worker
already owns; it is not a second execution authority. The bundle contains:

- Resolved status (`config_revision`, `asset_revision`, capacity, lifecycle,
  degraded reasons, subsystem observations) and bounded metrics.
- Actionable findings such as `unreachable_child`, `blocked_approval`,
  `hook_failure`, `native_error`, `recovery`, `saturation`, or
  `degraded_storage`.
- The selected task without prompt text, plus its pending approvals (identity,
  operation, exact digest, status, expiry), child delegations, and any failed
  Master plan child retained after an endpoint-resolution failure.
- Redacted audit tail and sampled trace evidence. Audit records carry principal,
  action, resource, outcome, source, policy revision, and correlation, and are
  retained independently of trace sampling.

Metrics use the bounded label vocabulary `instance`, `package`, `phase`,
`admission`, `state`, `capability`, `outcome`, and `quality`. Task ids, session
ids, principals, message content, and secret values never become labels.
Provider-confirmed prompt/candidate counts, local estimates, and `unknown`
values stay distinct; an unavailable value is absent rather than zero.

Inspectable failures preserve the native error or short-circuit outcome. The
diagnostic bundle never exports prompt content, secret values, another
instance's paths, or the approval target text. Local development is
distinguishable from product traffic through the task `traffic` field
(`local` for a direct operator/Launcher submission, `product` when admitted
through a Gateway delegation).

## Gateway registration and lease heartbeat

Gateway is optional. A Worker with no `[gateway]` configuration executes and
serves exactly as before. When configured, `serve` starts a background
registration and lease heartbeat after the listener is bound:

```toml
[gateway]
url = "http://127.0.0.1:8088"
control_token = "..."
renew_interval_ms = 15000
advertise_url = "http://127.0.0.1:8099"
```

- The registered endpoint is the actual bound listener unless `advertise_url`
  is set.
- Registration and renewal include the process incarnation and live capacity.
- The Gateway still verifies `/v1/health` and authenticated `/v1/capabilities`.
  Registration never supplies trusted identity or capabilities from the body.
- Heartbeat work is non-blocking and retries with bounded backoff. Local task
  execution never waits for registration.
- `401` or `403` stops the heartbeat and prints an actionable status error. It
  does not crash the Worker. `409 worker.disabled` keeps retrying so an
  operator's later enable can recover automatically; `403` and
  `worker.decommissioned` are terminal for this process.
- A Gateway restart or outage changes only registration and reachability.
  Lease expiry never rewrites a task outcome.
- A new process incarnation re-registers and is reconciled against retained
  incarnation history. Routed tasks keep the incarnation frozen when they were
  accepted.

## Durability disclosure

`/v1/capabilities` reports `durable_tasks: true` for harness tasks and
`a2a_task_storage: "in_memory"` with `a2a_durable: false`. Native A2A task
persistence is claimed only after its own acceptance gate passes.

## Execution budgets and truthful usage

Task `limits` bound one task attempt. `max_duration_ms`, `max_model_calls`,
`max_tool_calls`, `max_tokens`, and `max_cost_micros` are narrowed at admission;
a profile, team, or delegated request cannot widen them. Zero means unset.

- Wall-clock, model-call, and tool-call stops are selected before the next unit
  of work. A model response that requests tools after the model-call allowance
  is spent is not dispatched. A requested tool call beyond the tool allowance
  is not dispatched.
- Provider-reported token counts and operator-priced cost are enforced after
  the usage is observed. The Worker does not claim an exact preemptive token or
  monetary hard cap: `budget.exhausted` carries `exact_preemptive: false` and
  an `uncertainty` statement when a provider call may already have completed.
- `budget.exhausted` is durable and carries the budget kind, limit, consumed
  basis, effective limits, partial-output presence, recovery guidance, and the
  enforcement ordering. The task becomes `failed` with the stable
  `budget.<kind>_exceeded` error; it is never rewritten to objective success.
  Completed model deltas and tool evidence remain in the durable event stream.
- When a token or cost budget is configured but the provider reports no counts
  (or no pricing is declared), `budget.usage_unavailable` records the
  uncertainty. Missing usage is unavailable, not zero.
- Provider fallback, retries, and native delegation share the task allowance;
  a fallback provider selection does not reset spend. The task `usage` record
  is the authoritative per-task accounting. Gateway projections and Clients
  display it; they do not maintain a second authoritative total.

## Task recovery

- Admission commits the task projection, its idempotency binding, the first
  durable event, and the audit record in one atomic metadata write. The caller
  is acknowledged only after that commit succeeds.
- On restart, a durably accepted task that never requested native execution is
  resumed under the same external handle. Its attempt counter increases; its
  identity does not change.
- A task that was already running may have produced an external effect. Restart
  projects it as `recovering` with explicit uncertainty and records
  `task.recovery_required`. It is never dispatched again automatically.
- The native invocation identity is persisted as soon as the Runner exposes the
  first event, so a recovery review can map the handle to the invocation that
  may already have run.
- Conformance tests inject crashes at durable reservation, native start, first
  native event, and acceptance acknowledgment through `GOLDENBEE_FAULT_DIR`.
  That variable exists for tests only; it is not operator configuration.

## Protected external effects

A package registers a side-effecting native tool through
`Worker::effect_tool(id, tool, reconciler, policy)` instead of `tool(...)`.

- The runtime persists an operation identity before dispatch and a redacted
  receipt only after a confirmed response. The operation record stays in the
  Worker Instance that owns the effect.
- The package supplies an `EffectReconciler`. Its `status` method is the
  external status or idempotency interface used after a crash or ambiguous
  response. `Unknown` is a valid answer and is never converted to success,
  definite failure, or an automatic retry.
- `EffectPolicy` records the policy name, attempt bound, and whether
  known-absent or definite-failed effects may be retried. Retries stay inside
  one admitted task execution and preserve the task and operation identities
  and the external idempotency key.
- The adapter injects a reserved `operation_key` argument with the operation
  identity. It is excluded from the digest and always overwrites a model-supplied
  value.
- `GET /v1/tasks/{id}/operations` returns read-only evidence under the same
  caller, namespace, and `task:read` scope as the task. It cannot retry or
  rewrite an operation.
- Startup reconciles reserved or unknown operations before readiness. A
  task with an unresolved effect is `unknown`; a known-absent or definite-failed
  effect is never reported as task success.

## Protected operation approvals

An effect policy may require exact approval before dispatch:

```rust
let policy = EffectPolicy::default().requiring_approval(
    EffectApprovalPolicy::new("external-write", 60_000).target_argument("path"),
);
worker = worker.effect_tool(id, tool, reconciler, policy);
```

- The Worker registers the native ADK confirmation requirement for that tool.
  `LlmAgent` pauses through the pinned `ToolConfirmationHandler` after the model
  names the exact call and before `Tool::execute`. Graph nodes use the same
  native handler. The adapter also rechecks the approval at the tool boundary,
  so an agent that bypasses native confirmation cannot dispatch the effect.
- The request records the task, native operation, exact target argument,
  canonical argument digest, risk note, requester, and expiry. It is durable
  before the wait. Repeated calls for the same task, operation, and digest
  return the recorded request instead of creating another one.
- `GET /v1/approvals` and `GET /v1/approvals/{id}` expose that record to an
  authorized approver in the task namespace. `POST .../decision` requires the
  exact digest the approver saw. The task caller or an administrator may
  decide; another caller in the same namespace is rejected.
- Approval resumes the same native invocation in-process. After a process
  restart, approval of a native graph task resumes the same graph thread from
  its committed checkpoint. Work without a resumable native checkpoint stays
  `recovering` with an explicit `approval.resume_unavailable` event rather than
  being reported as approved execution.
- Rejection, expiry, revocation, a changed digest, and a wrong approver block
  the effect. Repeating the same decision is idempotent and emits no duplicate
  decision event or audit record. A conflicting decision is rejected.
- An approval is evidence for one configured operation, not a new capability
  grant. The pinned profile revision and native resume checks still run, and the
  approval never widens the effect policy, tool set, namespace, or caller.
- Waiting releases the execution slot; only the per-session writing order is
  retained.

## Task event streams

`GET /v1/tasks/{id}/events/stream?after=<seq>` replays retained events and then
follows the live broadcast. The route is GET-only; watching a task never
submits a prompt or creates another task.

- Each envelope carries `event_id`, `source`, `incarnation`, `stream_id`,
  `seq`, `schema_version`, `kind`, `durable`, `payload`, and `at`. `seq` is
  monotonic within one `task:<id>` stream. Workers do not publish a global
  ordering across tasks.
- `after` is a base-10 sequence cursor. Omitted means `0`. A cursor is only
  valid for the task stream that issued it; reusing one across tasks is not
  meaningful.
- `after=0` means "from the earliest retained event" only when retention has
  not removed history. If the retained stream starts after sequence 1, the
  route returns `stream.cursor_expired` with `details.oldest_seq` instead of
  silently presenting a truncated stream as complete.
- A cursor older than the retained window returns `stream.cursor_expired`. A
  cursor newer than any sequence issued for the task, a negative value, a
  non-numeric value, or a repeated parameter returns `stream.cursor_invalid`.
  Both errors carry recovery guidance and no event payload from another stream.
- The Worker subscribes to the live broadcast before reading the retained page,
  so a durable event committed during the handoff is either replayed or
  delivered live. Terminal outcomes and approval transitions remain durable;
  `model.delta` and `model.thinking` token fragments are explicitly
  `durable: false` and are not replayed. Thinking fragments are separate from
  answer text and never enter the task output.
- A terminal task closes the stream after the terminal event. Lossy ephemeral
  fragments may be missing after reconnect; reconnect from the last durable
  sequence to recover canonical results.
- Keepalive comments use the configured interval. A bounded broadcast buffer
  protects execution: a slow viewer receives one `stream.lagged` control event
  (`durable: false`) and the stream closes; the client reconnects from its last
  observed cursor. The control event never rewrites task state.
- Concurrent streams are bounded by `max_streams`. Capacity exhaustion returns
  retryable `stream.subscriber_limit` (`429`) before a live receiver is opened.
  Closing a viewer releases its slot even when the task is still running.
- Event payloads are redacted before persistence and broadcast. Secrets such as
  `sk-` canaries do not appear in the SSE bytes even though the event kind still
  arrives. The canonical task projection remains authoritative for the model
  output the caller supplied.
- `Last-Event-ID` is not interpreted. Clients resume with the explicit
  `after` query parameter so cursor precedence is unambiguous.

## Artifacts

Artifact bytes and immutable versions are stored by the pinned native
`adk_artifact::ArtifactService` (file-backed for each Worker Instance). The
runtime records only harness metadata: ownership, namespace, task id, native
invocation id, attempt, version, content type, size, and SHA-256 digest.

- A native invocation's `EventActions.artifact_delta` names each version it
  saved. The executor finalizes metadata from that native event, so
  task-to-invocation provenance is recorded at creation.
- References are opaque artifact ids or grants. Requests never carry a shared
  filesystem path, a `file:` URI, or a server-side URL. Non-inline native
  parts are refused by the storage adapter; no deployment policy currently
  enables remote fetching.
- Metadata and authenticated content require `artifact:read`, the owning
  namespace, and the owning caller (or `admin`). Task listing additionally
  requires visibility of the owning task.
- `POST /v1/artifacts/{id}/grant` issues a grant for 1 ms to 300 s. The grant is
  stored only in that Worker Instance and is invalid after restart data loss,
  expiry, retention, or on another instance.
- Content responses carry `x-goldenbee-artifact-digest` and are chunked in
  64 KiB body frames. Bytes are never added to event payloads. Dropping the
  response cancels the remaining transfer. Per-transfer buffering is bounded
  by the advertised 64 MiB artifact ceiling and the runtime rejects larger
  content.
- A digest mismatch, altered reference, altered grant, expired grant, missing
  credential, foreign caller, or foreign instance produces a stable error and
  no byte transfer.
- Gateway may keep only metadata or controlled relay state. It does not store
  artifact bytes and does not expose a second artifact service.

## Retention and authorized erasure

Retention is not one universal TTL. Each owning store applies its own window:
conversations (native sessions), native graph checkpoints, task metadata,
durable events, artifacts, native long-term memory, ingested documents and
derived graph facts, security audit, and telemetry traces.

- `POST /v1/retention` requires `instance:admin` and returns a
  `LifecycleReport`. Stages persist progress before moving on. An unfinished
  task keeps its session, checkpoint, task projection, durable events, and
  referenced evidence; those obligations are listed in `retained` with a
  machine-readable reason.
- `POST /v1/principals/{principal}/erase` requires `admin`. Erasing another
  principal requires `instance:admin`; the target is always resolved inside the
  authenticated caller's namespace, so a foreign principal cannot address or
  inspect it. Erasure reaches authoritative content, Gateway projections,
  document/derived-memory indexes, native caches, and artifact grants.
- Terminal task metadata and events are removed. An unfinished task keeps its
  recovery structure while user input, output, event payloads, approval target,
  and operation receipts are redacted. Evidence references required for
  recovery are retained and reported.
- Artifact cleanup does not delete content protected by an unexpired grant or
  referenced as unfinished-task evidence. A failed native deletion leaves
  metadata in place and is reported under `blocked`; it is never marked erased.
- `LifecycleReport` is `{job_id, resumed, completed, removed, retained,
  blocked, cursor_expiry}`. Retrying the same operation after an interruption
  resumes the recorded job. A completed erasure is idempotent.
- Independently installed packages register a `LifecycleOwner`. The Knowledge
  Package uses it to remove its own job catalogue, source bytes, vector chunks,
  and derived memory ids; the shared runtime only records the package's count
  or blocking error.
- Removing retained event history makes the next read return the existing
  typed `stream.cursor_expired` guidance with `details.oldest_seq`.
- Security audit stays append-only. Erasure records a lifecycle entry with
  counts only and redacts prior payload-bearing detail for the target.

## Errors

Errors use the shared object:

```json
{
  "code": "scope.capability_denied",
  "message": "delegation does not grant this capability",
  "retryable": false,
  "recovery": "Request a token or grant that includes the required scope.",
  "details": {}
}
```

The HTTP status is derived from the error category: `401` for unauthenticated
or invalid delegation, `403` for scope denial, `404` for absent resources,
`409` for conflicts, `429` for capacity, and `400` for malformed input.

## Schema

`schema/worker-v1.json` publishes the request, response, task, and event
envelopes for the new routes. It is a JSON Schema document, not a Rust
dependency, so non-Rust Gateways and test drivers can validate the same wire
shape.

## Bounds

- Task input: 1 MB management limit enforced by the runtime.
- Event page: `limit` clamped to 1–1000.
- Request body: bounded by the serving framework's default body limit.
- Artifact version: 64 MiB ceiling, 64 KiB response chunks, grant lifetime
  clamped to 1–300000 ms.
- Listeners bind loopback unless an approved remote configuration is supplied.

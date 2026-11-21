# Gateway protocol v1

The Gateway is the product access point between a Client SDK and approved
Worker Instances. It reuses the Worker `/v1` management protocol on the
Gateway-to-Worker hop and adds only client authentication, explicit routing,
delegation issuance, and routing metadata. It never executes an agent.

## Packaging and deployment

One control plane serves two deployment shapes:

```text
standalone-server: Client -> SDK -> goldenbee-gateway -> Worker
direct-core:       business backend -> gateway-core -> Worker
```

- `crates/gateway-core` is the library. It owns Worker approvals,
  registration and leases, routing, dispatch identity and idempotency,
  delegation issuance, discovery, administration, audit, retained and live
  event forwarding, and the durable registry and dispatch stores. It accepts an
  already-authenticated caller and has no Axum, caller-token, CLI, user,
  payment, subscription, entitlement, or business tenant dependency.
- `apps/goldenbee-gateway` is the standalone adapter. It owns the listener,
  caller bearer-token configuration and authentication, Axum routing and
  middleware, JSON and SSE serialization, and operator configuration loading.
  The package is `goldenbee-gateway`; the executable remains `gateway`.
- A business backend may depend on `gateway-core` directly and supply a
  principal, scopes, limits, and an optional Worker allow-list. It keeps its own
  authentication, user, payment, entitlement, tenant policy, and end-user
  streaming. It does not add a Gateway network hop.
- Workers remain separate processes in both shapes. The Gateway-to-Worker hop
  is HTTP with SSE, and neither shape links Worker runtime or native ADK
  execution.
- Direct use of `gateway-core` alone is not Gateway high availability. Multiple
  replicas require the Postgres store and separate deployment work such as
  load balancing, database failover, and pool sizing. The JSON registry and
  dispatch stores are single-writer. The tested shared-store claims and the
  file-to-Postgres migration path are in
  [Operations](operations.md#shared-gateway-store).

## Endpoint audit

The embedded browser Client may call the authenticated `/v1` surface without a
bearer token only when all of these hold: the request carries the
`x-goldenbee-ui` marker, the TCP peer is loopback, and the `Host` header is
loopback (`localhost`, `127.0.0.1`, or `::1`). That local session uses the first
configured caller's scopes and Worker allow-list. An explicit token must still
be valid. Set `"ui": { "local_auth": false }` to disable this path.

| Path | Classification | Notes |
| --- | --- | --- |
| `/`, `/index.html`, `/assets/*`, `/favicon.svg` | New, public | Static browser Client embedded in release binaries. Unknown non-API paths do not receive the HTML shell; unknown `/v1/*` paths stay JSON 404s. |
| `/v1/health` | New, public | Minimal liveness plus approved and reachable Worker counts. |
| `GET /v1/workers` | New, authenticated | Discovery filtered by caller visibility, capability, protocol, locality, and lease-derived health. |
| `POST /v1/workers/register` | New, Worker control token | Authenticated registration; all descriptors are verified against the live Worker endpoint. |
| `POST /v1/workers/{id}/lease` | New, Worker control token | Authenticated lease renewal using Gateway-side expiry. |
| `GET /v1/admin/workers` | New, `admin` | Full approved inventory, including disabled and decommissioned identities. |
| `POST /v1/admin/workers` | New, `admin` | Add an approval. A generated control token is returned exactly once. |
| `GET /v1/admin/workers/{id}` | New, `admin` | One approval, lease, and lifecycle projection. |
| `PATCH /v1/admin/workers/{id}` | New, `admin` | Revision-checked approval update. |
| `POST /v1/admin/workers/{id}/probe` | New, `admin` | Verify the live endpoint and reconcile registration. |
| `POST /v1/admin/workers/{id}/disable` | New, `admin` | Clear the lease and block registration, renewal, discovery, and new routing. |
| `POST /v1/admin/workers/{id}/enable` | New, `admin` | Allow registration again without restoring the old lease. |
| `DELETE /v1/admin/workers/{id}` | New, `admin` | Terminal decommission. Task history is retained. |
| `GET /v1/admin/workers/audit` | New, `admin` | Retained admin decisions, optionally filtered by Worker. |
| `GET /v1/workers/audit` | Legacy alias, `admin` | Same audit response as `/v1/admin/workers/audit`. |
| `DELETE /v1/workers/{id}` | Legacy alias, `admin` | Same decommission behavior as `/v1/admin/workers/{id}`. |
| `GET /v1/workers/{id}/workspace/tree` | New, `workspace:read` or `admin` | One bounded directory page from a visible Worker workspace. |
| `GET /v1/workers/{id}/workspace/file` | New, `workspace:read` or `admin` | Bounded text preview or binary metadata from a visible Worker workspace. |
| `GET /v1/workers/{id}/workspace/events/stream` | New, `workspace:read` or `admin` | SSE relay of `workspace.changed` notifications from a visible Worker. |
| `GET /v1/teams` | New, `team:read` or `task:read` | Team rosters visible in the caller namespace. |
| `POST /v1/teams` | New, `team:write` or `admin` | Create an inactive version-1 Master/subworker roster. Members must be registered, visible, and capability-compatible. |
| `GET /v1/teams/{id}` | New, `team:read` or `task:read` | Fetch the current versioned roster. |
| `PUT /v1/teams/{id}` | New, `team:write` or `admin` | Append a version when `expected_version` matches. Stale writers receive `409 team.version_conflict`. |
| `POST /v1/teams/{id}/validate` | New, `team:read` or `task:read` | Revalidate the current or named version against live membership and capabilities. |
| `POST /v1/teams/{id}/activate` | New, `team:write` or `admin` | Revalidate, then admit new team executions. A membership race fails activation. |
| `POST /v1/teams/{id}/deactivate` | New, `team:write` or `admin` | Stop new team admissions. Active executions keep their frozen roster. |
| `GET /v1/workers/{worker}/profiles` | New, `profile:read` or `admin` | Lists Runtime Profiles owned by one visible, reachable Worker. |
| `POST /v1/workers/{worker}/profiles` | New, `profile:write` or `admin` | Creates revision 1 on the owning Worker. Creation does not activate. |
| `GET /v1/workers/{worker}/profiles/{name}` | New, `profile:read` or `admin` | Fetches one Worker-owned profile. |
| `PUT /v1/workers/{worker}/profiles/{name}` | New, `profile:write` or `admin` | Appends a revision when `expected_revision` matches. Stale writers get `409 profile.revision_conflict` from the owning Worker. |
| `DELETE /v1/workers/{worker}/profiles/{name}` | New, `profile:write` or `admin` | Removes an inactive profile without touching the installed Worker. |
| `POST /v1/workers/{worker}/profiles/{name}/validate` | New, `profile:read` or `admin` | Validates an existing revision without changing activation state. |
| `POST /v1/workers/{worker}/profiles/{name}/activate` | New, `profile:write` or `admin` | Revalidates and activates an exact revision on the owning Worker. |
| `POST /v1/workers/{worker}/profiles/{name}/deactivate` | New, `profile:write` or `admin` | Stops new profile admissions while active work keeps its pinned revision. |
| `POST /v1/tasks` | New, authenticated | Requires an explicit Worker or an active team and a capability the target advertises. Persists caller-scoped intent before dispatch and returns a Gateway external execution id. |
| `GET /v1/tasks/{id}` | New, authenticated proxy | Resolves a Gateway external execution id to the Worker projection, frozen incarnation, and separate connectivity. |
| `GET /v1/tasks/{id}/events` | New, authenticated proxy | Returns the Worker retained event page. |
| `GET /v1/tasks/{id}/events/stream` | New, authenticated proxy | Proxies the Worker replay-then-live Server-Sent Event stream without resubmitting the task. Cursors and typed errors are forwarded unchanged. |
| `GET /v1/tasks/{id}/approvals` | New, authenticated proxy | Lists Worker-owned approvals for one routed task. Requires `approval:read`. Only the caller's namespace and that exact Worker task are included. |
| `GET /v1/tasks/{id}/approvals/{approval}` | New, authenticated proxy | Returns one Worker-owned approval bound to the routed task. Requires `approval:read`. |
| `POST /v1/tasks/{id}/approvals/{approval}/decision` | New, authenticated proxy | Forwards `approved` or `rejected` with the exact digest the approver observed. Requires `approval:read` and `approval:decide`. |
| `POST /v1/tasks/{id}/approvals/{approval}/revoke` | New, authenticated proxy | Revokes a still-pending approval through the owning Worker. Requires `approval:read` and `approval:decide`. |
| `GET /v1/tasks/{id}/artifacts` | New, authenticated proxy | Lists immutable artifacts belonging to one routed task. Requires `artifact:read`. |
| `GET /v1/tasks/{id}/artifacts/{artifact}` | New, authenticated proxy | Returns one immutable artifact metadata record for the routed task. |
| `GET /v1/tasks/{id}/artifacts/{artifact}/content` | New, authenticated proxy | Streams digest-verified artifact bytes in bounded Worker chunks. |
| `POST /v1/tasks/{id}/cancel` | New, authenticated proxy | Returns the Worker cancellation projection. |
| `POST /v1/socket/tickets` | New, authenticated | Mint one short-lived one-use duplex upgrade ticket. |
| `GET /v1/socket?ticket=...` | New, ticket-authenticated | Duplex typed command subscription, not a second event journal. |
| `GET /v1/admin/model-providers` | New, `admin` or `provider:admin` | Non-secret provider metadata, versions, and fingerprints. |
| `POST /v1/admin/model-providers` | New, `admin` or `provider:admin` | Create provider metadata. No credential is accepted here. |
| `GET /v1/admin/model-providers/{id}` | New, `admin` or `provider:admin` | One non-secret provider entry. |
| `PATCH /v1/admin/model-providers/{id}` | New, `admin` or `provider:admin` | Revision-checked metadata update; endpoint or filter changes rediscover. |
| `POST /v1/admin/model-providers/{id}/discover` | New, `admin` or `provider:admin` | Refresh discovered models and advance the provider revision. |
| `PUT /v1/admin/model-providers/{id}/credential` | New, `admin` or `provider:admin` | Write-only credential version. The request body is the only accepted transport. Runs discovery for supported adapters. |
| `DELETE /v1/admin/model-providers/{id}/credential` | New, `admin` or `provider:admin` | Revoke the active version. New envelopes omit it. |
| `POST /v1/admin/model-providers/pair-code` | New, `admin` or `provider:admin` | Mint a one-time, five-minute Worker enrollment code bound to one Worker and instance. |
| `POST /v1/model-providers/enroll` | New, Worker control token | Redeem a pairing code and bind the Worker's base64 X25519 public key. |
| `GET /v1/model-providers` | New, Worker control token | Assigned non-revoked providers with short-lived encrypted envelopes. |

## Provider vault

Provider credential custody is Gateway-owned; model execution stays
Worker-owned. The Gateway never proxies prompts, completions, streaming, or tool
calls.

- An operator writes metadata first, then a credential. The credential route is
  write-only: the response contains `provider`, `version`, `fingerprint`,
  timestamps, `ready`, discovered `models`, and any redacted discovery error.
  `fingerprint` is `sha256:<hex>`. No route returns the value.
- Supported adapters discover models before becoming ready:
  `openrouter` and `openai-compatible` call authenticated `GET
  {endpoint}/models`; `ollama` calls `GET {endpoint}/api/tags`.
  `http-json` and `deterministic` keep the explicit single model.
- `model` is the preferred default. It may be omitted for a discovering adapter
  when `allow_auto_default` is explicitly true; otherwise a discovered model
  must be selected. `allowed_models`, when present, is the only set retained
  from discovery. The Gateway never invents a model.
- Catalog metadata includes `models`, `models_source` (`explicit`, `discovered`,
  or `pending`), `discovered_at`, `allowed_models`, `allow_auto_default`,
  `ready`, `discovery_error`, and `discovery_error_code`. These fields are
  non-secret and also appear in the Worker bundle.
- Discovery failures keep the provider not-ready and record a stable code such
  as `provider.discovery_auth`, `provider.discovery_invalid`,
  `provider.discovery_empty`, or `provider.default_model_unavailable`.
  Credential values are never logged, audited, returned, or copied into a
  discovery error.
- `POST /v1/admin/model-providers/{id}/discover` recomputes the model set from
  the current endpoint and credential and advances `revision`. Repeated calls
  converge on the provider response.
- Credential writes append a version and revoke older versions. Revocation
  removes the provider from new Worker bundles; a task already running keeps
  the adapter it pinned.
- At rest, each version is sealed with XChaCha20-Poly1305 under
  `GOLDENBEE_VAULT_MASTER_KEY`. The master key must be base64 for exactly 32
  bytes. A missing or malformed key fails Gateway startup when the vault is
  enabled. The key is never read from Gateway JSON.
- `POST /v1/admin/model-providers/pair-code` requires an approved, enabled
  Worker and an instance matching its configured identity. The code is high
  entropy, bound to that Worker and instance, single-use, and expires after five
  minutes. Only its SHA-256 hash is stored.
- `POST /v1/model-providers/enroll` authenticates the Worker control token,
  confirms the body Worker id and instance match the approved identity, and
  redeems the code once. Enrollment stores the public key, never a private key.
- `GET /v1/model-providers` returns only active providers assigned to the
  calling Worker. Each envelope contains an ephemeral X25519 public key, a
  24-byte nonce, ciphertext, and an expiry of at most five minutes. Associated
  data binds provider, version, Worker, instance, and expiry.
- Missing `vault` configuration disables provider management and preserves the
  prior environment-backed provider mode. The file store is single-writer;
  multi-replica vault operation requires a shared adapter that this release does
  not claim.

Operator CLI examples (the credential key is read from stdin, never argv):

```sh
gb-gateway provider add --url http://127.0.0.1:8080 --token "$GOLDENBEE_ADMIN_TOKEN" \
  --id openrouter --adapter openrouter --model "$OPENROUTER_MODEL_KEY" \
  --residency remote --tool-calling --context-window 128000 --worker general-a
# Without OPENROUTER_MODEL_KEY, an explicit dev policy may use:
#   --auto-default
gb-gateway provider discover --url http://127.0.0.1:8080 --token "$GOLDENBEE_ADMIN_TOKEN" \
  --id openrouter
printf '%s' "$OPENROUTER_API_KEY" | gb-gateway provider credential set \
  --url http://127.0.0.1:8080 --token "$GOLDENBEE_ADMIN_TOKEN" --id openrouter
gb-gateway provider pair --url http://127.0.0.1:8080 --token "$GOLDENBEE_ADMIN_TOKEN" \
  --worker general-a --instance general-a
general enroll --config /etc/goldenbee/workers/general-a.toml --code "$PAIR_CODE"
```

## Teams, admission, and routing

A team is a Gateway-owned, versioned roster. It is configuration, not a
process, and it never creates a separate Master binary. The designated Master
and every subworker are ordinary approved Worker Instances. Role labels are
descriptive and grant no capability; capability authority comes only from each
member's explicit subset of the Worker's verified, approved capabilities.

- `POST /v1/teams` requires at least one subworker distinct from the Master.
  `members` may list subworkers only, or include the Master with an explicit
  Master capability subset. An omitted Master binding is inferred from the
  registry using its approved capabilities. Every listed member must be
  registered, visible through the caller's Worker allow-list, share the
  declared locality when one is set, and authorize at least one capability the
  Worker actually advertises.
- `PUT /v1/teams/{id}` requires the observed `expected_version`. The update
  appends a new version and preserves the active flag. A stale writer receives
  `409 team.version_conflict`; it never overwrites a newer roster.
- `validate` and `activate` re-check live membership, lease reachability, and
  capability compatibility. Activation fails with `team.member_unavailable`
  when a member was disabled or stopped after setup.
- `max_depth`, `max_fanout`, and member depth are bounded. A roster larger than
  its fanout, a Master outside the member list, duplicate members or instances,
  unsupported capabilities, and incompatible locality are rejected before
  storage.
- Deactivation stops new admissions only. Existing executions retain the
  roster snapshot frozen at routing time.

`POST /v1/tasks` may name `team` instead of `worker`; `worker` may be supplied
only when it equals that team's Master. An optional `team_version` must match
the current active version. The Gateway resolves the roster once, checks every
member is still reachable and allowed, and issues a short-lived signed
delegation whose claims carry the immutable roster version, Master, member
endpoints/instances/capabilities, depth, fanout, and narrowed limits. It then
submits to the Master and stores the frozen team version in the dispatch
mapping. Later roster updates or deactivation do not rewrite that mapping, and
retrying the same caller idempotency key replays it instead of selecting a new
roster.

The Master Worker owns execution. It delegates to a roster member through its
native A2A path. Every child token is audience-bound to the member instance,
narrowed to the requested capability, and intersected with the parent task's
scopes, limits, expiry, depth, and delegation path. A member already present in
the path, the current Worker itself, a member outside the frozen roster, and a
capability outside the member's binding are rejected before a child is
prepared. This path is cycle-safe across teams that share Worker Instances.

## Runtime Profile management

Runtime Profiles are Worker-owned. The Gateway exposes typed management routes
that name the owning Worker and forwards them through a short-lived delegation
scoped only to `profile:read` or `profile:write`. It stores no profile revision,
active flag, or second revision counter.

- The caller must hold `profile:read`/`profile:write` or `admin` and the Worker
  must be visible through its allow-list. Disabled, decommissioned, or
  unreachable Workers are rejected before any profile request.
- The Worker assigns immutable revision numbers and creation times. `PUT`
  requires the observed `expected_revision`; stale writes return the Worker's
  `profile.revision_conflict` unchanged with expected and current details.
- `validate` and `activate` use the owning Worker's resolved file-loaded agents,
  skills, providers, grants, restrictions, and policy. Validation changes
  nothing; a failed activation leaves the previous active revision in place.
- Profiles are namespace-scoped by the authenticated caller, and another
  caller's profile cannot appear through the Worker route.
- The CLI and SDK use these routes. No Client maintains a parallel profile
  loader, retry loop, or profile authority.

## Stream forwarding and ownership

`GET /v1/tasks/{id}/events/stream` is a forward-only relay of the Worker's
replay-then-live SSE route. The Gateway does not keep an event journal and does
not resubmit a task while opening or reconnecting a viewer.

- The Gateway maps its external execution id to the stored Worker handle and
  reopens the Worker stream with the caller's `after` cursor. Reconnect works
  after a Gateway restart because the dispatch mapping is durable.
- Cursor parsing happens at the Gateway edge. Malformed, repeated, or too-new
  cursors return typed `stream.cursor_invalid`; retention gaps return the
  Worker's `stream.cursor_expired` unchanged. Neither error rewrites Worker
  connectivity or task outcome.
- The Gateway bounds its own forwarded viewers with `max_streams` (default 256,
  optional in operator configuration). Exhaustion returns retryable
  `stream.subscriber_limit` (`429`) before a Worker connection is opened. The
  slot is held by the returned response and released on disconnect.
- Worker-side retention, sequence allocation, and canonical event production
  remain exclusively Worker-owned. `common` holds the envelope and cursor
  representation, not a journal implementation.
- A typed client error (cursor, authorization, capacity) does not mark the
  Worker unreachable. Only a Worker server failure or transport failure changes
  the connectivity projection.

No Worker agent, session, A2A, or model route is exposed by the Gateway in this
slice. Artifact routes are task-scoped relays over the Worker artifact service,
not a second artifact store. The native A2A adapter is not used yet; the
Gateway-to-Worker hop is the versioned Worker management HTTP protocol. The
pinned A2A card builder (`adk_server::a2a::v1::card::build_v1_agent_card`) and
pinned registry store (`adk_server::registry::store::AgentRegistryStore`) are
reference shapes, not linked dependencies: the Gateway never imports native
ADK or Worker execution.

## Authenticated duplex socket

The socket adapter is an optional duplex control surface over the same task,
stream, cancellation, and approval behavior. It owns no task state, event
journal, decision, or protected effect. HTTP remains canonical and available
when the socket is absent.

- `POST /v1/socket/tickets` requires the caller bearer and returns one
  short-lived, one-use opaque ticket. The default lifetime is 30 seconds and the
  pending ticket set is bounded. A ticket is not a bearer credential.
- `GET /v1/socket?ticket=<ticket>` upgrades the connection. The caller bearer
  token never appears in the query string. The ticket is removed at redemption,
  so a repeat, expired, or unknown value returns `401 auth.socket_ticket` before
  the upgrade.
- The Gateway binds the redeemed ticket to its authenticated caller identity and
  Worker allow-list. Every command is executed through `gateway-core` with that
  caller, so foreign tasks and Workers are rejected exactly as on HTTP.

Schema version 1 frames are JSON objects. A client command frame carries
`schema_version`, a caller-assigned `command_id`, a `scope` (`worker` and/or
`task_id`), and one typed command: `subscribe`, `submit`, `cancel`, or
`approval`. The scope must match the command target; a lying frame returns
`socket.scope_mismatch`. A successful command returns `ack` with the same
`command_id` and a typed outcome; failures return `error` with the stable
`ApiError` shape. Subscriptions then stream `event` frames carrying the shared
durable envelope. `heartbeat` frames keep a quiet connection observable.

- `subscribe` reopens the Worker replay-then-live stream with the supplied
  retained cursor and never resubmits work. An expired cursor forwards
  `stream.cursor_expired`; the canonical `GET /v1/tasks/{id}` projection remains
  the resynchronization source.
- `submit` uses the existing caller idempotency key and durable dispatch path. A
  repeated frame after a reconnect returns the recorded outcome or reconciles
  the same task without creating a second prompt.
- `cancel` returns the Worker cancellation projection. `approval` forwards the
  exact observed digest and the Worker remains the authority for its recorded
  decision; repeating an acknowledged decision converges on one protected
  effect.

Bounds are operator-configurable under `socket` in the Gateway JSON:

| Field | Default | Bound |
| --- | --- | --- |
| `max_frame_bytes` | 65536 | 1 KiB-1 MiB inbound and outbound frame ceiling |
| `subscriber_queue` | 64 | 1-4096 buffered subscription frames per connection |
| `max_subscriptions` | 4 | 1-256 concurrent task subscriptions per connection |
| `idle_timeout_ms` | 60000 | 100-600000 ms silent-client timeout |
| `heartbeat_ms` | 20000 | 100-600000 ms, and shorter than the idle timeout |
| `send_timeout_ms` | 5000 | 10-60000 ms per-frame send bound |
| `ticket_ttl_ms` | 30000 | 100-300000 ms one-use ticket lifetime |

An oversized frame is rejected with `socket.frame_too_large`; an oversized
forwarded event ends that subscription rather than growing without bound. A
consumer that cannot drain its bounded queue is disconnected with a typed
slow-consumer or idle error and resumes from its last durable cursor. Malformed
JSON, unknown command kinds, unsupported schema versions, binary frames, missing
tickets, and expired tickets all fail with typed errors and never bypass
authorization.

The Rust SDK exposes `sdk::Socket` and `sdk::SocketSession`. `Socket::ticket`
uses authenticated HTTP; `Socket::connect` redeems one ticket; `SocketSession`
sends typed command frames, awaits matching acknowledgments, and streams events.
The `sdk-client` example consumes this adapter with `--socket` and does not open
a parallel raw WebSocket or HTTP event stream for submit/watch.

## Workspace relay

The Gateway forwards read-only workspace tree and file requests to a visible
Worker. It does not read the filesystem itself and does not store file content.

- `GET /v1/workers/{id}/workspace/tree?path=&limit=&cursor=` lists one directory.
- `GET /v1/workers/{id}/workspace/file?path=` returns a bounded text preview or
  binary metadata.
- `GET /v1/workers/{id}/workspace/events/stream` relays the Worker's
  best-effort SSE change stream. It is not a durable task journal; reconnecting
  Clients refetch the tree.
- Both routes require `workspace:read` and caller visibility of the Worker.
- The delegation carries only `workspace:read`; Worker path containment,
  hidden-file policy, and bounds remain authoritative.
- `workspace.disabled`, `workspace.unavailable`, and `workspace.path_denied`
  are forwarded as typed Worker errors.

## Artifact relay

Artifact access is task-scoped because the Gateway maps an external execution id
to the owning Worker. The caller must hold `artifact:read`, and the Gateway
rejects a task that is not visible to that caller before contacting any Worker.

- `GET /v1/tasks/{id}/artifacts` forwards the Worker's immutable artifact list.
- `GET /v1/tasks/{id}/artifacts/{artifact}` forwards one metadata record and
  rejects a record whose `task_id` does not match the routed Worker task.
- `GET /v1/tasks/{id}/artifacts/{artifact}/content` reads metadata first, then
  streams content with the expected digest, content type, and length headers.
  Content is forwarded in the Worker's bounded 64 KiB chunks. The Gateway keeps
  no artifact bytes, no duplicate journal, and no second artifact service.
- The Gateway issues a narrow delegation containing only `task:read` and
  `artifact:read` for the authenticated principal and the frozen task agent.
- Concurrent transfers share the Gateway's bounded fan-out permit pool, so a
  caller cannot hold unbounded relay resources. Exhaustion returns retryable
  `capacity.artifact_transfer`. Dropping a response closes the Worker transfer
  and releases the permit without cancelling the producing task.
- Denied or foreign artifacts return a stable error and no bytes. Worker digest
  mismatch, expiry, altered reference, and tampered content are forwarded as
  typed errors. The Rust SDK verifies size and `sha256:` digest again after
  download and never retries a digest mismatch.

## Approval routing

Approvals belong to the Worker Instance that owns the protected operation. The
Gateway holds only the durable task route and a redacted audit projection; it
never stores a pending approval, decision, expiry outcome, or protected effect.

- A caller reaches approvals only through a visible Gateway execution id. A
  foreign namespace or another caller's task is rejected as
  `route.task.not_found` before any Worker request. A Worker approval returned
  for a different task handle is rejected as `approval.not_found`.
- Listing forwards `pending=1|true` to the Worker, which filters by the
  authenticated namespace; the Gateway additionally binds every record to the
  routed Worker task.
- A decision requires `approval:read` and `approval:decide`. The Gateway reads
  the exact Worker record, rejects a changed digest with
  `approval.digest_mismatch`, then forwards the unchanged decision and digest.
  The Worker independently revalidates caller identity, authority, expiry,
  digest, and task ownership before resuming its native workflow.
- A repeated decision is forwarded again so a lost acknowledgment, duplicate
  delivery, or Gateway restart converges on the Worker's recorded outcome and
  one protected effect. The Gateway stores no decision that could disagree with
  the Worker.
- An unreachable Worker returns `route.worker_unavailable`; the Gateway does not
  mark the approval approved or executed. Approval requests and terminal events
  replay from the Worker's retained event stream after reconnect.
- Rejections, changed digests, foreign references, and unauthorized decisions
  are retained in the Gateway audit projection with a redacted detail. The
  Worker also records its own decision audit and external receipt.
- The SDK methods `approvals`, `approval`, `decide_approval`, and
  `revoke_approval`, and the CLI `approvals`, `approve`, `reject`, and `revoke`
  commands use these routes. No Client implements another approval transport.

## Operator configuration

`gb-gateway serve --config <path>` reads a JSON operator file. Listener and caller
fields belong to `goldenbee-gateway`; the control-plane fields are the
`gateway-core` configuration. A business backend using the core directly constructs that core
configuration directly and do not need a caller token. The fields are:

- `listen` — listener address, loopback by default.
- `data_dir` — Gateway-owned durable dispatch state. Relative paths resolve
  against the configuration file's directory. The default is `gateway-state`.
- `registry` — optional path for persistent Gateway-owned registry state. A
  relative path resolves against the configuration file's directory. Omitted
  keeps registry state in memory for one process lifetime. The file is written
  with mode `0600`. It stores approvals, control-token hashes, leases,
  incarnations, and audit; plaintext control tokens are never persisted.
- `lease_ttl_ms` — Gateway-measured lease duration. Default 300000.
- `callers[]` — `token`, `id`, `namespace`, `scopes`, optional `workers`
  allow-list. Omitted `workers` allows every approved Worker.
- `workers[]` — `id`, `endpoint`, optional expected `instance`, optional
  expected `package`, optional approved `capabilities` subset, optional
  `control_token` for authenticated registration and renewal, and optional
  `preflight` (default true). These entries seed approvals on first startup.
  Afterwards the registry is authoritative. A restart updates only
  `source: "config"` approvals that have not been changed through the admin API;
  runtime edits are not overwritten.
- `delegation_secret` — optional test-only fallback;
  `GOLDENBEE_DELEGATION_SECRET` wins and is the recommended source.

A safe example is `config/gateway.example.json`. On startup the Gateway loads
persisted registry state first, seeds config-owned approvals, and expires stale
leases from Gateway time. It probes only approved Workers with no persisted
record: `/v1/health` learns the instance and incarnation, then
`/v1/capabilities` is read with a short-lived read delegation. The expected
`instance` and `package`, when configured, must match. Failures mark the Worker
unreachable; they do not fail open or select another Worker.

## Registration, leases, and identity

- Worker identity is the Gateway-assigned `id`. The Worker instance identity,
  process incarnation, package name/version, protocol version, and capability
  descriptors come from the live endpoint, never from the registration body.
- `POST /v1/workers/register` authenticates with the Worker's configured
  `control_token`. The body carries only the expected endpoint, the current
  incarnation, and live capacity. The endpoint must equal the approved endpoint
  before the Gateway contacts it.
- The Gateway then verifies `/v1/health` and authenticated `/v1/capabilities`.
  A body incarnation that differs from the verified endpoint is rejected as
  spoofing. A second approved identity that verifies to an already registered
  instance is rejected as a duplicate identity.
- `POST /v1/workers/{id}/lease` authenticates the same control token, requires
  it to be bound to `{id}`, and re-verifies the live endpoint before renewing.
  Expiry is always `Gateway clock + lease_ttl_ms`; a remote clock is never
  trusted. Live capacity is stored separately from the static capability card.
- A verified new incarnation on the same endpoint reconciles: the previous
  incarnation is retained with `superseded_at`, the new one becomes current,
  and routed task records keep the incarnation frozen at submission.
- `DELETE /v1/workers/{id}` requires the caller `admin` scope. Missing scope,
  unknown control tokens, endpoint changes, spoofed incarnations, duplicate
  identities, and unauthorized decommissioning are recorded in the audit trail.
- Registry state is Gateway-owned; `crates/common` carries only wire types and
  Worker runtime state is not imported.

## Correlated diagnostics

`GET /v1/workers/{worker}/diagnostics?task=<id>` requires `task:read` and caller
visibility of the Worker. The Gateway forwards a short-lived `task:read`
delegation to that Worker and returns the Worker-owned bundle unchanged; it
stores no diagnostic copy and creates no second authority.

- A Gateway execution id is resolved to the exact Worker task handle before
  forwarding. A handle that belongs to another Worker is reported as
  `route.task.not_found`.
- `POST /v1/tasks` accepts a valid W3C `traceparent` header and forwards the
  same trace identity to the selected Worker through the existing dispatch
  request. The Gateway projection exposes the Worker-owned trace identity and
  the `product` traffic marker.
- Registry audit entries carry principal, action, Worker resource, outcome,
  source, policy revision, and correlation. Sampling trace spans never removes
  an audit decision.
- Metric labels remain bounded; raw task ids, session ids, tokens, and prompt
  content are not labels.

## Worker administration

`admin` scope owns Worker inventory and lifecycle. All admin decisions include
the authenticated principal, action, Worker, outcome, and a redacted detail.
No route returns a stored control token.

| State | Meaning |
| --- | --- |
| `approved` | Approval exists; no live registration has been observed. |
| `registered` | The current incarnation holds a non-expired Gateway lease. |
| `unreachable` | The lease expired or an explicit probe could not verify the endpoint. |
| `disabled` | Registration, renewal, discovery, and new routing are blocked. |
| `decommissioned` | Terminal identity. A replacement uses a new id. |

Rules:

- Config `workers[]` seeds approvals on first startup. The registry store is
  authoritative afterwards. Config updates apply only to config-owned records
  during startup.
- `POST /v1/admin/workers` is idempotent by id plus the canonical request
  digest. The same digest replays the approval; a different digest for the same
  id returns `409 worker.idempotency_conflict`.
- When `control_token` is omitted, the Gateway generates one and returns it in
  the add response exactly once. Later inventory, discovery, audit, and error
  responses expose only `control_token_set`. Because the value cannot be shown
  again, a lost generated-token response requires a replacement identity; there
  is no token-recovery route.
- `PATCH /v1/admin/workers/{id}` requires the observed `revision`. A stale
  revision returns `409 worker.revision_conflict`. Repeating an update that
  already committed with the same idempotency key replays the committed
  revision, so a lost acknowledgment converges.
- An endpoint change is rejected while a live incarnation still owns the
  endpoint (`409 worker.endpoint_in_use`). Disable or decommission first.
- Expanding the approved capability set requires a successful probe and is
  audited. Narrowing does not.
- `probe` verifies `/v1/health` and authenticated `/v1/capabilities` through the
  normal verification path, then reconciles registration. A mismatch is an
  explicit error and never reports reachability.
- Disable clears the lease immediately. Existing task projections, events, and
  outcomes remain readable and are never rewritten.
- Enable allows registration again. The identity stays unavailable until the
  Worker registers or an explicit probe succeeds; the previous lease is not
  restored.
- Decommission is terminal. Task history is retained and caller discovery
  excludes the identity.
- Host process start, stop, install, upgrade, and rollback are outside this
  Gateway control plane. It manages approvals and connectivity, not host
  lifecycle.

## Discovery

`GET /v1/workers` accepts optional query filters:

- `capability=<id>` — the effective capability card must contain the identifier.
- `protocol=<version>` — exact management protocol match.
- `locality=local|remote` — loopback endpoints are `local`.
- `health=healthy|unreachable|any` — lease-derived status, never a task outcome.

Results always intersect the caller's Worker allow-list. Disabled and
decommissioned identities are excluded. Admin inventory still shows them. `declared_capabilities` are the verified static card,
`approved_capabilities` are the operator authorization, and `capabilities` are
the effective intersection; `capacity` is the live lease value.
Package identity (`package`, `version`), independent instance identity
(`instance`), Gateway identity (`id`), and current/retained incarnations remain
distinct fields.

## Authentication and delegation

- Every client route except `/v1/health` requires
  `Authorization: Bearer <token>`. Registration and renewal instead require the
  Worker's bound `control_token`; the Gateway never accepts identity claims
  from the body.
- Caller tokens resolve to a `Principal` with a namespace and management
  scopes. The Gateway checks `task:submit`, `task:read`, `task:cancel`, or
  `admin` per route.
- A submission must name `worker`. The Gateway rejects a missing, unknown,
  unauthorized, unreachable, or capability-mismatched target before dispatch.
- For the Worker hop the Gateway issues an HMAC delegation token through the
  shared `common::delegation` primitive. The audience is the Worker instance
  identity, the principal and namespace are the authenticated caller, the
  capability list contains only the requested agent, and the scope list
  contains only the operation being performed.
- The Worker independently verifies the signature, audience, expiry, depth, and
  capability scope. Changing the token body invalidates the signature.
- Routing submissions are keyed by authenticated namespace, caller, and the
  caller's idempotency key. The key returns one Gateway external execution id
  (`exec-…`) for its lifetime. A changed canonical payload — including the
  explicit Worker target — conflicts before dispatch. Another namespace or
  caller cannot read or inherit the mapping.

## Fleet lifecycle adapters

Authorized lifecycle uses the Gateway-owned local host or a configured remote
Gateway host, never caller filesystem access. The caller needs `instance:admin`
for mutations and `instance:read` or `instance:admin` for inspection. The
Gateway also checks the host, instance allow-list, and operation allow-list.

The Gateway also owns its local package cache and Worker processes. These
routes do not name a host:

| Route | Purpose |
| --- | --- |
| `POST /v1/packages/install` | Install one verified package from a folder or URL |
| `GET /v1/packages` | List verified packages |
| `POST /v1/instances` | Provision and register one verified instance |
| `GET /v1/instances` | List local instances |
| `GET /v1/instances/{instance}` | Digest, identity, lifecycle, health, compatibility |
| `POST /v1/instances/{instance}/start` | Start a stopped instance |
| `POST /v1/instances/{instance}/stop` | Stop a running instance |
| `POST /v1/instances/{instance}/restart` | Stop, start, and re-register |
| `POST /v1/instances/{instance}/upgrade` | Managed verified upgrade |
| `POST /v1/instances/{instance}/rollback` | Recover a compatible interrupted activation |
| `GET /v1/instances/{instance}/logs` | Bounded process log tail |

The host-qualified compatibility routes remain:

| Route | Purpose |
| --- | --- |
| `GET /v1/hosts` | Local Gateway host plus configured remote hosts |
| `GET /v1/hosts/{host}` | One adapter descriptor |
| `POST /v1/hosts/{host}/instances` | Provision and register one verified instance |
| `GET /v1/hosts/{host}/instances/{instance}` | Digest, identity, lifecycle, health, compatibility |
| `POST /v1/hosts/{host}/instances/{instance}/upgrade` | Managed verified upgrade |
| `POST /v1/hosts/{host}/instances/{instance}/rollback` | Recover a compatible interrupted activation |

Provision requests select a verified `release_digest`, an `instance`, a
Gateway `worker` identity, and an optional profile activation separately. They
never carry a host endpoint, host credential, or executable path. A host-local
`config` or `workspace` string is resolved by the Gateway-owned host against
its own permitted roots. See [goldenbee-composer.md](goldenbee-composer.md) for
local host policy and reconciliation.

Host loss returns `503 host.unreachable`. An operation the local host or remote
Gateway does not implement returns `host.unsupported`, and an unknown release digest returns
`host.release_unavailable`. Repeated operation ids return the recorded outcome
with `replayed: true`; a different payload under the same id returns
`host.operation_conflict`.

## Errors

Errors use the shared object from `crates/common`:

```json
{
  "code": "scope.capability_denied",
  "message": "Worker 'general-a' does not advertise capability 'missing'",
  "retryable": false,
  "recovery": "Request a token or grant that includes the required scope.",
  "details": {"worker": "general-a"}
}
```

Stable Gateway codes:

| Code | HTTP | Meaning |
| --- | --- | --- |
| `auth.unauthenticated` | 401 | Missing or unrecognized caller token. |
| `route.worker_required` | 400 | No explicit Worker target. |
| `route.worker.not_found` | 404 | Target is not approved by this Gateway. |
| `scope.worker_denied` | 403 | Caller allow-list excludes the target. |
| `route.worker_unavailable` | 503 | Approved Worker is unreachable or instance-mismatched. |
| `scope.capability_denied` | 403 | Worker does not advertise the requested agent. |
| `route.task.not_found` | 404 | Routed task is unknown or not visible. |
| `route.invalid_worker_response` | 400 | Worker payload does not match protocol 1.0. |
| `task.idempotency_conflict` | 409 | The caller reused a key with a different canonical request. |
| `task.unknown_submission` | 202 | Worker acceptance is unresolved. Body details include `execution_id` and `worker`. |
| `storage.unavailable` | 503 | Gateway dispatch or registry state could not be read or committed. No acceptance is claimed. |
| `storage.corrupt` | 400 | Gateway dispatch state is unreadable or internally inconsistent. |
| `storage.schema_unsupported` | 400 | Dispatch state was written by a newer Gateway schema. |
| `worker.endpoint_forbidden` | 403 | Registration names an endpoint other than the approved one, or a live identity tries to move. |
| `worker.instance_mismatch` | 403 | Verified instance identity differs from the approved identity. |
| `worker.package_mismatch` | 403 | Verified package identity differs from the approved package. |
| `worker.capability_mismatch` | 403 | An operator-approved capability is not advertised by the Worker. |
| `worker.incarnation_mismatch` | 403 | Caller-supplied incarnation differs from the live endpoint. |
| `worker.duplicate_identity` | 409 | Another approved identity already owns the verified instance. |
| `worker.decommissioned` | 409 | A decommissioned identity tried to register or renew. |
| `worker.endpoint_unavailable` | 503 | Registration, renewal, or probe could not verify the live endpoint. |
| `worker.filter.invalid` | 400 | Discovery locality or health filter is not recognized. |
| `worker.disabled` | 409 | A disabled identity tried to register, renew, probe, or route new work. |
| `worker.endpoint_in_use` | 409 | An update tried to move an endpoint owned by a live incarnation. |
| `worker.idempotency_conflict` | 409 | The same Worker id was added with a different request digest. |
| `worker.revision_conflict` | 409 | An update used a stale approval revision. |
| `worker.control_conflict` | 409 | A control token is already bound to another identity. |
| `worker.not_found` | 404 | The admin target is not an approved identity. |
| `artifact.not_found` | 404 | The artifact is absent, foreign, or not visible for the routed task. |
| `artifact.digest_mismatch` | 400 | Artifact content does not match its finalized digest. |
| `artifact.grant_expired` | 400 | A scoped artifact grant has expired. |
| `capacity.artifact_transfer` | 429 | The Gateway reached its bounded concurrent artifact transfers. |
| `artifact.transfer_interrupted` | 503 | A Worker artifact transfer ended before the expected bytes arrived. |
| `auth.socket_ticket` | 401 | The socket ticket is missing, unknown, expired, repeated, or already used. |
| `socket.invalid_frame` | 400 | The frame is not valid schema-1 JSON, uses a binary frame, or lacks a command identity. |
| `socket.unsupported_protocol` | 400 | The frame schema version is not supported. |
| `socket.scope_mismatch` | 403 | The frame scope does not match the command's task or Worker target. |
| `socket.frame_too_large` | 400 | A client or forwarded frame exceeds the configured bound. |
| `capacity.socket_subscriptions` | 429 | The connection reached its bounded concurrent subscription count. |
| `capacity.socket_slow_consumer` | 429 | The connection could not drain its bounded subscriber queue. |
| `socket.idle_timeout` | 400 | The connection was silent beyond the configured idle bound. |

Upstream `ApiError` bodies are forwarded unchanged. Typed errors never contain
caller tokens, control tokens, delegation tokens, or signing secrets.

## Schema

`schema/gateway-v1.json` publishes the new client-facing request, response,
task, Worker summary, discovery, registration, lease, audit, health, and host
lifecycle envelopes as JSON Schema. It is independent of any Rust dependency
and covers the same wire shapes the Rust SDK consumes.

## Durability and restart boundary

The Gateway owns a small durable dispatch file at
`<data_dir>/dispatch.json`. It is Gateway-local persistence, separate from the
Worker store. It records only:

- caller-scoped request identity: namespace, caller, idempotency key, canonical
  Gateway digest, explicit Worker, and frozen agent;
- one external execution id;
- dispatch status (`intent`, `dispatching`, `accepted`, `unknown`);
- the Worker task handle and frozen acceptance projection once confirmed;
- connectivity and the last observed Worker projection;
- a redacted transition audit for each recovery outcome.

Worker task state, native invocation identity, retained events, cancellation,
and the final outcome remain authoritative in the Worker.

The dispatch file and the optional persistent registry are committed through a
temporary file: write, flush and `sync_all`, rename, then sync the containing
directory on Unix. Temporary files are removed on a pre-rename failure. A
commit failure returns `storage.unavailable` and no acceptance is claimed. This
closes the process-restart durability gap; actual power-loss behavior is tested
separately and is not implied by a process-only test. Gateway and Worker stores
remain separate owners.

### Persist-before-dispatch

A new submission validates the target and capability, commits identity and
intent, then commits a `dispatching` attempt before the HTTP request. The
Gateway returns `task_id` as the external execution id, not the Worker handle.
A persistence failure before intent returns `storage.unavailable` and performs
no remote request.

### Reconciliation outcomes

| Boundary | Durable Gateway state | Result |
| --- | --- | --- |
| Crash after intent, before remote request | `intent` | Retry with the original key reuses the external id and dispatches once. |
| Worker accepted, acknowledgment lost | `unknown` | `202 task.unknown_submission` with `execution_id`; retry with the original key resolves the original Worker task. |
| Crash after Worker acceptance, before mapping commit | `dispatching` | Retry with the original key reuses the same Worker idempotency key and converges on the accepted Worker task. |
| Gateway accepted mapping committed | `accepted` | Retries return the stored external id without remote dispatch. `GET` refreshes the Worker projection. |
| Worker unreachable after acceptance | `unreachable` connectivity | `GET` returns the last Worker projection unchanged with `connectivity: "unreachable"`. |
| Worker outcome later observed | `online` connectivity | `GET` returns the live Worker projection. Connectivity never rewrites a task outcome. |
| Worker reports `task.not_found` / `stream.not_found` | projection cleared | The owning store erased the content (retention or authorized erasure). The Gateway returns the typed not-found error and never serves its last cached projection. |

Recovery never creates a fresh uncorrelated submission. Ambiguous acceptance is
retried with the caller's original idempotency key, and the Worker's own
durable idempotency binding returns the original handle. A Worker handle is
used directly once the Gateway has persisted it.

The Gateway projection is a cache of owner-reported state, not a copy of
authority. When the owning Worker reports a task or retained stream absent, the
Gateway clears the cached projection durably before returning the typed error.
A restart does not resurrect it. Task, event, and stream reads observe this
same rule, so SDK consumers receive the owner's deletion outcome instead of a
stale successful task.

The Gateway does not run an agent and does not claim exactly-once execution
beyond these reconciled scenarios. A process crash during native execution is
still the Worker's `recovering`/`unknown` boundary, not a Gateway retry.

Registry state is durable when `registry` is configured: approved
identities, leases, incarnation history, capacity, and audit entries survive a
Gateway restart. Expiry is recomputed from Gateway time on the first request
after restart. A Gateway restart also reloads the durable dispatch file, so an
execution id and its Worker mapping remain resolvable.
# Goldenbee Gateway server

`goldenbee-gateway` is the standalone HTTP adapter over the `gateway-core`
library. It owns listener startup, caller bearer-token authentication, Axum
routing and middleware, JSON and Server-Sent Event serialization, operator
configuration loading, the verified package cache, and local Worker lifecycle.
Worker approvals, registration, leases, routing, idempotency, delegation,
discovery, administration, audit, and stream forwarding come from
`gateway-core`.

The package is `goldenbee-gateway`; the executable is `gb-gateway`.

## Module layout

```text
src/
  main.rs      binary entry only
  lib.rs       public exports
  config.rs    operator configuration
  server.rs    preparation, listener, graceful shutdown
  state.rs     control plane and caller bindings
  router.rs    route composition by authentication class
  auth.rs      caller bearer authentication
  http.rs      JSON results, failures, bearer parsing
  health.rs    public liveness
  host/        verified package cache and local instance lifecycle
  workers/     discovery, control, and administration
  tasks/       task proxy and Server-Sent Event routes
```

`router.rs` composes three route classes. `/v1/health` is public.
`/v1/workers/register` and `/v1/workers/{id}/lease` are Worker control routes;
their handlers authenticate the Worker's own control token, so caller identity
is never accepted there. Discovery, audit, admin, and task routes receive caller
bearer authentication from one middleware on the caller router.

Two deployment shapes use the same control plane:

```text
standalone-server: Client -> SDK -> goldenbee-gateway -> Worker
direct-core:       business backend -> gateway-core -> Worker
```

A business backend may depend on `gateway-core` directly, authenticate its own
users, and supply an already-authenticated `Caller`. That skips the extra
Gateway network hop without changing routing or registry behavior. It is not
Gateway high availability: multiple replicas still require a shared durable
store, which is future work.

## Run

```sh
export GOLDENBEE_DELEGATION_SECRET='shared with each Worker process'
cargo run -p goldenbee-gateway -- serve --config config/gateway.example.json
```

The built binary is named `gb-gateway`.

`--config` is required. `--addr` overrides the configured listener. Prefer
`GOLDENBEE_DELEGATION_SECRET`; the operator file may carry
`delegation_secret` for isolated tests, but committed examples never hold a
real secret.

`data_dir` names the Gateway-owned durable dispatch store. Relative paths
resolve against the configuration file's directory. It stores only dispatch
intent, the external execution id, the Worker task mapping, connectivity, and
the last observed Worker projection. Worker task state remains authoritative.

Startup loads persisted registry state, seeds config-owned approvals, and
expires stale leases. It then probes only approved Workers with no record.
Unreachable or identity-mismatched Workers are reported unreachable; they are
never silently replaced at dispatch time. Registry state is durable when
`registry` is configured and is written with mode `0600`. Control tokens are
stored only as SHA-256 hashes.

## Routes

| Path | Authentication | Behavior |
| --- | --- | --- |
| `/v1/health` | Public | Liveness plus approved and reachable Worker counts. |
| `POST /v1/packages/install` | `instance:admin` | Install a verified package from a folder or URL. |
| `GET /v1/packages` | `instance:read` | List verified packages. |
| `POST /v1/instances` | `instance:admin` | Provision and register a local instance. |
| `GET /v1/instances` | `instance:read` | List local instances. |
| `GET /v1/instances/{instance}` | `instance:read` | Inspect one local instance. |
| `POST /v1/instances/{instance}/start`, `/stop`, `/restart` | `instance:admin` | Control the local process. |
| `GET /v1/instances/{instance}/logs` | `instance:read` | Read a bounded process log tail. |
| `GET /v1/workers` | `task:read` | Discovery filtered by allow-list, capability, protocol, locality, and health. |
| `POST /v1/workers/register` | Worker `control_token` | Verified registration of endpoint, incarnation, package, protocol, and capabilities. |
| `POST /v1/workers/{id}/lease` | Worker `control_token` | Gateway-clock lease renewal and incarnation reconciliation. |
| `GET /v1/admin/workers` | `admin` | Full admin inventory, including disabled and decommissioned identities. |
| `POST /v1/admin/workers` | `admin` | Add an approval. A generated control token is shown exactly once. |
| `GET /v1/admin/workers/{id}` | `admin` | One approval and lease projection. |
| `PATCH /v1/admin/workers/{id}` | `admin` | Revision-checked update. Endpoint moves require a non-live target. |
| `POST /v1/admin/workers/{id}/probe` | `admin` | Verify the endpoint and reconcile registration. |
| `POST /v1/admin/workers/{id}/disable` | `admin` | Block registration, renewal, discovery, and new routing. |
| `POST /v1/admin/workers/{id}/enable` | `admin` | Allow registration again; the old lease is not restored. |
| `DELETE /v1/admin/workers/{id}` | `admin` | Terminal decommission; task history is retained. |
| `GET /v1/admin/workers/audit` | `admin` | Admin decision audit, filtered by Worker. |
| `GET /v1/workers/audit` | `admin` | Legacy alias for admin audit. |
| `DELETE /v1/workers/{id}` | `admin` | Legacy alias for decommission. |
| `POST /v1/tasks` | `task:submit` | Explicit target, capability check, durable intent, delegation, Worker submission. Returns a Gateway external execution id. |
| `GET /v1/tasks/{id}` | `task:read` | Worker projection with separate connectivity; `{id}` is the Gateway external execution id. |
| `GET /v1/tasks/{id}/events` | `task:read` | Proxied retained event page. |
| `GET /v1/tasks/{id}/events/stream` | `task:read` | Proxied replay-then-live Server-Sent Event stream. |
| `POST /v1/tasks/{id}/cancel` | `task:cancel` | Proxied precise Worker cancellation. |

Dispatch intent and mapping survive a Gateway restart. A retry uses the
caller's original idempotency key, so an ambiguous remote acceptance converges
on the Worker's original task instead of creating another. Unresolved
acceptance returns `202 task.unknown_submission` with recovery guidance.
Connectivity is reported separately and never rewrites a Worker outcome.

## Worker administration

The Gateway is the Worker control plane for approvals, leases, discovery, and
routing. It does not start, stop, install, or upgrade host processes.

Worker states are `approved`, `registered`, `unreachable`, `disabled`, and
`decommissioned`. Disabled and decommissioned identities remain visible to
admin inventory and audit but not to caller discovery. Disabling clears the
lease, blocks registration, renewal, discovery, and new submits, and never
cancels or rewrites existing tasks. Enabling permits registration again without
restoring the old lease. Decommission is terminal; a replacement uses a new id.

Add is idempotent by id and request digest. Updates require the observed
revision. Capability expansion requires a successful probe. Control tokens are
returned only when an add generates one; every ordinary view exposes only
`control_token_set`. See [Gateway protocol v1](../../docs/gateway-api.md) for
the full state and failure rules.

## Stable errors

Errors use `common::ApiError` with `code`, `message`, `retryable`, `recovery`,
and redacted `details`:

- `auth.unauthenticated` — missing or unrecognized caller/control token.
- `route.worker_required` — no explicit Worker was named.
- `route.worker.not_found` — the named Worker is not approved here.
- `scope.worker_denied` — the caller's allow-list excludes the target, or a control token is bound to another identity.
- `route.worker_unavailable` — the approved endpoint is unreachable.
- `scope.capability_denied` — the Worker does not advertise the agent.
- `route.task.not_found` — no routed task is visible to this caller.
- `task.idempotency_conflict` — the key was reused with a changed request.
- `task.unknown_submission` — remote acceptance is unresolved; repeat the
  original key.
- `storage.unavailable` — dispatch or registry state could not be committed.
- `worker.endpoint_forbidden` / `worker.incarnation_mismatch` — spoofed registration claims.
- `worker.duplicate_identity` — another identity already owns the verified instance.
- `worker.decommissioned` — a decommissioned identity tried to register or renew.
- `worker.endpoint_unavailable` — registration or renewal verification failed.

Host autostart and process supervision are outside the Gateway control plane.
The example systemd units in [`deploy/systemd`](../../deploy/systemd) use
`Type=exec`, SIGTERM, bounded restarts, and journald; container deployments use
container restart and readiness policies instead. See
[Operations](../../docs/operations.md) for drain, backup, and recovery guidance.

See [the Gateway protocol audit](../../docs/gateway-api.md) and
[`schema/gateway-v1.json`](../../schema/gateway-v1.json).

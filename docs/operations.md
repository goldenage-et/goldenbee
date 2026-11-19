# Operations

This guide covers host deployment, boot, drain, restart, upgrade, backup, and
recovery. The example systemd units live in [`deploy/systemd`](../deploy/systemd).
Container deployments use their supervisor instead of these units.

Two responsibilities stay separate:

- **Autostart and process supervision** belong to systemd, a container
  runtime, or another host supervisor. Goldenbee does not run a custom daemon
  supervisor, and the CLI does not install units or write to `/etc`.
- **Durability** belongs to the application stores. The Worker runtime store,
  native session store, Gateway dispatch store, and Gateway registry each write
  a temporary file, flush and `sync_all` it, rename it, and sync the containing
  directory on Unix. That closes the known process-restart gap. Power-loss
  testing is a separate exercise and is not claimed by a process-only test.

Gateway and Worker stores are separate owners. The Gateway never reads or
mutates Worker task state, and the Worker never reads Gateway dispatch or
registry state. A restart of one process does not require the other to restart.

## Host layout

The examples use these paths:

| Path | Owner | Purpose |
| --- | --- | --- |
| `/usr/local/bin/gb-gateway` | root | Standalone Gateway executable (`goldenbee-gateway`). |
| `/usr/local/bin/goldenbee-worker` | root | Symlink to the installed Worker package binary. |
| `/etc/goldenbee/gateway.json` | root, `0600` | Gateway listener, caller tokens, Worker approvals. |
| `/etc/goldenbee/gateway.env` | root, `0600` | Gateway secrets such as `GOLDENBEE_DELEGATION_SECRET`. |
| `/etc/goldenbee/workers/<instance>.toml` | root, `0600` | Worker instance configuration. |
| `/etc/goldenbee/workers/<instance>.env` | root, `0600` | Worker instance secrets. |
| `/var/lib/goldenbee/gateway/` | `goldenbee`, `0700` | Gateway dispatch and registry state. |
| `/var/lib/goldenbee/workers/<instance>/` | `goldenbee`, `0700` | Worker task, session, artifact, memory, and sandbox state. |

Configuration contains tokens. Keep it out of world-readable locations and
never put secrets in `ExecStart`. The units set `UMask=0077`, `ProtectSystem=strict`,
`ProtectHome=yes`, `PrivateTmp=yes`, `NoNewPrivileges=yes`, and limit writes to
the instance data directory.

## Release installation and restricted deployment

Release artifacts are verified and installed by local operator tooling; a
running registry is not required. Installing, creating an instance, activating a
profile, starting a Worker, and authorizing a team all work with registry access
disabled. `gb-install install` reads the signed OCI layout from local storage and
never contacts the registry. The optional registry is a distribution hop only.

- Restrict listeners to loopback or an operator-managed private interface. The
  Worker `serve` command refuses a non-loopback `--addr` unless remote operation
  is explicitly configured; the example Gateway configuration binds loopback.
- Terminate TLS at a reviewed reverse proxy or service mesh. The bundled
  listeners do not implement TLS, certificate rotation, or public exposure.
- Provision secrets through files or environment injection owned by the host
  supervisor, never in `ExecStart`, repository files, release artifacts, audit
  output, or command-line arguments. The example units use root-owned `0600`
  environment files.
- Install the sandbox prerequisites recorded in [Worker configuration](worker-config.md)
  and [Sandbox prerequisites](install.md). Installation and execution fail
  closed when the sandbox is unavailable; a signature is never a substitute for
  isolation or explicit grants.
- Drain before restart or upgrade: stop admission, wait for active work, keep
  uncertain work `recovering`, then stop the listener. A failed drain or hook is
  reported truthfully and never rewritten as success.
- Back up each Worker instance separately from the Gateway projection. Restore
  into a clean root with the matching verified release and configuration.
  Missing releases, incompatible schemas, unavailable secret references, and
  binary drift restore into `quarantined` state and do not start.
- Quarantine is an operator-visible holding state, not a rollback claim. Review
  the lifecycle report, preserved data, and required external secrets before
  returning an instance to service.

Release assembly, compatibility, artifact verification, and the single-node
scope are documented in [Release](release.md). Multi-replica Gateway deployment
requires the shared Postgres store and the active-active evidence recorded in
[Shared Gateway store](#shared-gateway-store) and ticket 56. A library
dependency on `gateway-core` is not Gateway high availability.

## Provider vault

Provider management is optional. Without a `vault` section the Gateway keeps the
previous environment-backed provider mode. When enabled, the Gateway needs
`GOLDENBEE_VAULT_MASTER_KEY` (base64, exactly 32 bytes) before it will start.
Supply it through the Gateway environment file, not `ExecStart` or Gateway JSON.

```json
"vault": {
  "enabled": true,
  "directory": "/var/lib/goldenbee/gateway/vault",
  "master_key_env": "GOLDENBEE_VAULT_MASTER_KEY"
}
```

- The vault directory is `0700`; `provider-vault.json` is `0600`. Back it up
  together with the Gateway registry but keep the master key separate from the
  backup.
- A missing or wrong master key fails closed. Losing the key makes stored
  credentials unrecoverable; rewrite each credential with
  `gb-gateway provider credential set`.
- Write credentials from stdin only:
  `printf '%s' "$KEY" | gb-gateway provider credential set --url ... --token ... --id ...`.
  The response contains a version and `sha256:` fingerprint, never the value.
- Create provider metadata before the credential. Discovery runs on create,
  credential write, endpoint/filter change, and explicit
  `gb-gateway provider discover`. The catalog reports `ready`, discovered
  `models`, and a redacted `discovery_error`; only ready, assigned providers
  reach a Worker bundle. A failed discovery does not invent a model. Assign the
  provider to the exact Worker ids that may use it (`*` is allowed but broader).
  Revoke a credential to stop new envelope delivery; running tasks keep their
  pinned adapter.
- Pair one Worker instance with
  `gb-gateway provider pair --worker <id> --instance <instance>`, then run
  `worker enroll --config <path> --code <code>`. Codes are single-use and expire
  after five minutes. The link contains no provider key.
- The master key is a single-host file/KMS input, not a Gateway high
  availability feature. The file store is single-writer; do not share one vault
  directory across replicas. Multi-replica provider vault operation requires a
  shared adapter that this release does not claim.
- A Gateway compromise with permission to read the master key can read provider
  credentials. The vault protects storage and per-Worker transport, not a fully
  compromised Gateway.

## Boot and autostart

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now goldenbee.target
sudo systemctl enable --now goldenbee-worker@general-a.service
```

`goldenbee.target` starts the Gateway. Each Worker instance is enabled
separately, so Gateway and Worker processes start independently and there is no
`Requires=` coupling. Additional instances are enabled with
`goldenbee-worker@<instance>.service`.

Both units use `Type=exec` and `Restart=on-failure` with a bounded `RestartSec`
and `StartLimitBurst`. No readiness notification protocol is implemented, so
the units do not claim `Type=notify`; use the HTTP health probes below.

The Worker heartbeat retries registration while the Gateway is unavailable. A
Worker can also run standalone without any Gateway configuration.

## Health probes

```sh
curl -fsS http://127.0.0.1:8080/v1/health   # Gateway
curl -fsS http://127.0.0.1:8081/v1/health   # Worker instance
```

The Gateway health response reports liveness plus approved and reachable
Worker counts. It is public, like the Worker health route. Consumer discovery
and task routes still require bearer authentication. Treat health as a process
and lease projection, not as proof that a remote model provider is available.

## Drain and restart

SIGTERM is the supported stop signal. `systemctl stop` and `systemctl restart`
send it.

- The Worker atomically closes admission and makes readiness false, then waits
  up to `drain_timeout_ms` (default 10 seconds) for active, queued, and
  approval-paused work to finish. It emits `stream.draining` to connected event
  streams so graceful listener shutdown cannot wait on an open SSE connection.
- At the bound, remaining execution tokens are cancelled. Work that never
  started stays recoverable; work that was active becomes `recovering` with
  explicit uncertainty. A forced stop never reports task success or failure.
- Package shutdown hooks run after the drain, then native plugins close and
  owned resources stop. A failed hook or expired drain produces
  `stopped_degraded` and a non-zero process exit, not a clean stop.
- `POST /v1/drain` with an `instance:admin` token starts the same bounded drain
  while leaving the process and listener alive for inspection.
- The Gateway stops accepting new HTTP requests and lets in-flight requests
  finish. Durable dispatch state is already committed by the time acceptance is
  acknowledged.
- `TimeoutStopSec=60s` leaves room beyond the Worker drain window before systemd
  escalates. Raise it in a drop-in if package shutdown hooks can run longer.

Restart a single Worker without stopping the Gateway:

```sh
sudo systemctl restart goldenbee-worker@general-a.service
```

Restart the Gateway without stopping Workers:

```sh
sudo systemctl restart goldenbee-gateway.service
```

Restarting never silently re-submits a task. See [Recovery](#recovery) for the
state matrix.

## Logs

The units log to the journal. Use the instance name for template units:

```sh
sudo journalctl -u goldenbee-gateway.service -f
sudo journalctl -u goldenbee-worker@general-a.service -f
sudo journalctl -u 'goldenbee-worker@*' --since '1 hour ago'
sudo journalctl -u goldenbee.target --since today
```

Logs are redacted by the application. Do not add secret values to command-line
arguments or unit descriptions.

## Upgrade

Install the candidate release first, then let `gb-instance` plan and activate
one instance at a time so other Workers remain available:

```sh
gb-install install --artifact ./release --trust ./trust.json --root /srv/goldenbee

# Dry run: platform, signature, protocol, runtime, profile, capability, schema,
# permission, team, work, configuration, and launcher differences.
gb-instance upgrade --root /srv/goldenbee/instances --instance general-a \
  --release /srv/goldenbee/packages/sha256/<digest> [--json]

# Activate only after reviewing the report and explicitly authorizing any
# candidate-only permission.
gb-instance upgrade --root /srv/goldenbee/instances --instance general-a \
  --release /srv/goldenbee/packages/sha256/<digest> \
  --authorize exec:command --work-policy drain --apply
```

Upgrade rules:

1. `--dry-run` is the default; it does not modify the instance. It executes the
   verified candidate's own `probe`, `capabilities --config`, `dev --probe-only`,
   and `serve --probe-only` in an isolated temporary environment to validate
   configuration references, compiled hooks, capabilities, and launcher modes.
2. A candidate that adds a permission stops until `--authorize` names it.
   Incompatible profiles, team assignments, platform, runtime, protocol, or
   persisted schema stop with an actionable report.
3. `--work-policy pin` refuses while admitted or queued work exists.
   `drain` stops admission, waits for work, and refuses replacement if tasks
   remain non-terminal. `migrate` allows recoverable queued tasks to resume on
   the candidate when the persisted schema is compatible.
4. Activation snapshots the complete `data_dir`, copies only missing signed
   seed files, keeps the operator configuration location and external
   agent/skill/prompt directories, and preserves instance identity, profile
   revisions, history, workspace authorization, grants, limits, and credential
   references.
5. Completion requires the candidate's readiness phase and a real capability
   task. A failed readiness or capability check restores the previous binary
   record and the data snapshot and reports both results separately.
6. A killed activation is recovered with
   `gb-instance recover --root <root> --instance <id>`. Data that cannot be
   restored is moved to `<instance>/quarantine/` and the lifecycle becomes
   `quarantined`; never describe that outcome as a successful rollback.

Production restart and supervision remain the host supervisor's
responsibility. Gateway inventory does not install or replace executable code.

Keep Worker data and configuration in place across a binary upgrade. A newer
persisted schema is refused by an older binary rather than silently rewritten,
so a downgrade across a schema change requires the matching backup.

## Backup and restore

Worker and Gateway state are owned separately. Take one Worker backup per
instance and one Gateway backup for the control-plane projection; do not merge
them into one database or archive.

```sh
# Cold Worker backup. Stop the instance first so its JSON and SQLite stores are
# quiescent; the snapshot also archives SQLite -wal and -shm companions.
gb-instance stop --root /srv/goldenbee/instances --instance general-a
gb-instance backup --root /srv/goldenbee/instances --instance general-a \
  --output /srv/backups/general-a-2026-11-19 --json
gb-instance backup-verify --backup /srv/backups/general-a-2026-11-19 --json

# Restore onto a clean instances root. The verified release must already be
# present; restore never rebuilds a Worker or substitutes bundled seeds.
gb-instance restore --backup /srv/backups/general-a-2026-11-19 \
  --root /srv/goldenbee/instances-restored \
  --release /srv/goldenbee/packages/sha256/<digest> \
  --config /srv/goldenbee/config/general-a.toml \
  --map /srv/old/prompts=/srv/goldenbee/assets/prompts \
  --secret-available env:MODEL_KEY --json

# Gateway projection backup and restore. The Gateway file store is separate
# from every Worker data directory.
systemctl stop goldenbee-gateway.service
gb-gateway backup --config /etc/goldenbee/gateway.json --output /srv/backups/gateway-2026-11-19
gb-gateway backup-verify --backup /srv/backups/gateway-2026-11-19 --json
gb-gateway restore --backup /srv/backups/gateway-2026-11-19 \
  --config /etc/goldenbee/gateway.json --not-within /srv/goldenbee/instances --json
```

A Worker backup records the instance identity, verified package and binary
digests, native data schema, immutable profile revisions, task bindings,
artifact references, grants, limits, secret *references*, external
agent/prompt/skill directories, and the configuration digest. It does not put a
credential value in the manifest. The archive can still contain the Worker token
and user content, so encrypt it and restrict access.

Restore validates every file digest, manifest and data schema, the verified
package digest and compiled hooks, required external secret references, explicit
configuration and asset paths, instance identity, and workspace/grants before
reporting readiness. An unfinished task resumes only when its package, profile
revision, authority, session, and checkpoint are present; otherwise the instance
is restored into `quarantined` state and will not start. A missing release,
asset, path mapping, or credential blocks readiness. Restore does not create a
new package or fall back to seed defaults.

SQLite files are copied only as a cold snapshot with their `-wal` and `-shm`
companions. This tool does not perform an online SQLite backup; use a filesystem
snapshot only when it is consistent across every file. A backup taken while the
Worker is running is refused.

`gb-instance backup-migrate --backup <dir> --output <copy>` exercises a
resumable migration on a copy. Completed steps are journaled in
`migration.json`; rerunning resumes. A manifest or data schema newer than the
tool understands is reported as `migration.schema_unsupported` or
`migration.irreversible` and the copy is not rewritten. Neither command mutates
the source backup.

The Gateway file store snapshot is taken under its single-writer lock. Shared
Postgres stores use `pg_dump` and are not imported into this file backup; keep
database dumps separate from Worker state as described above. Caller tokens are
not stored in the Gateway database, so restore requires the operator caller
configuration and refuses readiness when no usable caller binding is present.
The SDK is the client access path; the SDK cache is not an authoritative backup
store.

## Recovery

The runtime already persists admission before acknowledgment. On restart it
applies this matrix; it does not invent a new workflow engine:

| Persisted state | Restart behavior |
| --- | --- |
| Accepted or queued work | Re-admitted under the same external handle and dispatched again. |
| Running or cancelling work | Marked `recovering` with explicit uncertainty. It is not dispatched automatically. |
| Running work with a confirmed external effect | Reconcile the persisted operation receipt before any retry. |
| Running work with an unknown external effect | Remains `recovering` or `unknown` until an operator probes or reviews it. |
| Terminal work | Unchanged. It is never resumed or rewritten. |

The operation store records `reserved`, `committed`, `absent`, `failed`, and
`unknown` statuses. A committed operation is not repeated; an ambiguous one is
not converted into success.

Protected effects use the response when dispatch fails. An unavailable status
interface leaves the operation `unknown` and the task `unknown`; it never
becomes a definite failure or an automatic retry. Only a status interface that
reports `absent` or `failed` can authorize another attempt, and only when the
registered `EffectPolicy` permits that status within its attempt bound. The
retry keeps the task identity, operation identity, and external idempotency key.

`GET /v1/tasks/{id}/operations` returns the task attempt, confirmed native
invocation, and each operation's tool, status, attempt, policy, redacted
external reference, and redacted receipt. The route uses the same caller,
namespace, and `task:read` scope as the task itself. It is read-only evidence:
it cannot retry, complete, or rewrite an operation.

Gateway dispatch recovery is also explicit:

- `intent` before a remote request: a retry with the original key reuses the
  external id and dispatches once.
- `dispatching`: a retry reuses the Worker idempotency key and converges on the
  original Worker task.
- `unknown`: the Gateway returns `202 task.unknown_submission` with the external
  id instead of creating a fresh uncorrelated task.
- `accepted`: the stored execution id and Worker mapping are returned without a
  remote dispatch.

Review `recovering` and `unknown` records before retrying. Do not delete or
rewrite the state files to force a retry; that discards the evidence used to
avoid duplicate effects.

## Upgrade and recovery limits

- The pinned native Runner does not expose a durable invocation-status probe.
  An uncertain native start stays `recovering` with its bound invocation id for
  operator review.
- The Gateway's JSON registry and dispatch stores are single-writer state. A
  second process that opens the same data directory fails with
  `storage.single_writer`. Multi-replica deployment uses the Postgres store
  described in [Shared Gateway store](#shared-gateway-store).
- A post-rename parent-directory sync failure is reported as
  `storage.unavailable`; the rename may already be visible on the live
  filesystem even though it is not confirmed durable. Treat the error as an
  I/O fault and retry after checking the filesystem.
- Process restart durability is tested. Power-loss durability, filesystem
  corruption, and disk-quota exhaustion are separate operational events.

## Containers

Container deployments do not use these units. Use the container runtime's
restart policy, health or readiness probes, stop grace period, and secret
injection instead:

- Mount durable volumes for `/var/lib/goldenbee/gateway` and each Worker state
  directory. Ephemeral container filesystems lose accepted tasks and sessions.
- Set the stop grace period longer than the Worker drain window.
- Probe the same `/v1/health` routes.
- Send SIGTERM as the stop signal.
- Keep one Worker instance per container process unless you provide separate
  state directories and identities.
- For multiple Gateway replicas, supply a shared durable store. The file-backed
  registry and dispatch stores are single-replica only.

Autostart and restart remain the container supervisor's responsibility, just
as they remain systemd's responsibility on a host.

## Shared Gateway store

Use the Postgres store when more than one Gateway replica serves the same
Worker inventory, caller identities, tasks, leases, or streams. The file store
remains the default for standalone single-node use.

```json
{
  "store": {
    "kind": "postgres",
    "url_env": "GOLDENBEE_GATEWAY_DATABASE_URL",
    "max_connections": 8,
    "migrate": true
  },
  "replica_id": "gateway-a"
}
```

- Put `GOLDENBEE_GATEWAY_DATABASE_URL` in `gateway.env` (mode `0600`) or a
  credential file. The operator JSON names the environment variable; it never
  contains the DSN.
- `replica_id` is stable per process. It is recorded on dispatch claims and
  lease renewals. If omitted, `GOLDENBEE_GATEWAY_REPLICA_ID` is used, then a
  process-derived local fallback.
- Build `gateway-core`, and the `gateway` binary, with the `postgres` feature
  (`cargo build -p goldenbee-gateway --features postgres`). A build without it refuses
  `store.kind = postgres` at startup instead of silently using files.
- On open, the adapter applies the idempotent schema migration under an
  advisory lock, then verifies the schema version. A newer schema fails closed
  with `storage.schema_unsupported`.
- Every replica must be able to reach each approved Worker endpoint, because
  the replica that receives registration or renewal performs the live
  `/v1/health` and `/v1/capabilities` verification. Load-balance the Gateway,
  not the Worker's per-instance endpoint.

What is shared:

- Caller-scoped request identity and dispatch intent, with a unique
  `(namespace, caller, idempotency_key)` constraint.
- Atomic claims with a lease-style expiry. A claim is acquired in one
  conditional update; a crashed replica's claim becomes reclaimable after
  `claim_ttl_ms` (default 30 seconds, above the Gateway request timeout).
- First-writer-wins acceptance mappings, optimistic row revisions, and
  `worker_task_id`/instance/incarnation stream mappings. A second replica reads
  the mapping and reattaches with the caller's cursor; it never submits again.
- Approvals, Worker records, control-token hashes, lease timestamps, incarnation
  history, and audit decisions. Lease and claim timestamps use the database
  clock, not replica wall clocks.
- No process-local registry or dispatch copy is authoritative. Each control
  operation loads a transaction, mutates, verifies, and commits; read paths
  reload from the database.

What remains deployment-owned:

- Active-active safety is tested for the schema and Postgres 17 against
  concurrent submissions, replica restart, cross-replica renewal, stream
  reattachment, lost acknowledgment convergence, admin propagation, and expiry
  sweep. It is not a claim about connection-pool exhaustion, database failover,
  network partitions, or untested Postgres versions.
- A replica that cannot reach Postgres returns store failures; it is not a
  healthy read/write member. `gateway-core` exposes `Gateway::ready()` for a
  store and schema probe. `goldenbee-gateway` health remains liveness plus Worker
  counts; a deployment that needs readiness gating calls `ready()` from its own
  probe or supervisor.
- Size the pool and the claim TTL for the deployment. The claim TTL is the only
  guard that prevents two replicas from dispatching at once during a crash.

### Migration from the file store

There is no automatic import on startup; two replicas would race and an
accidental import could resurrect decommissioned identities. The supported
migration is explicit:

1. Drain and stop the file-mode Gateway. The file writer lock proves no process
   still owns the directory.
2. Save the complete `data_dir` snapshot. It is the rollback artifact. Keep it
   outside the new database host and record its checksum.
3. Start the shared store with the same operator configuration. Config-owned
   approvals are seeded idempotently. Runtime approvals, disable/decommission
   decisions, and dispatch history are not copied by the Gateway. Recreate the
   runtime decisions through the admin API before routing traffic, or keep the
   old file-mode deployment until those decisions are no longer needed.
4. Verify `GET /v1/workers` and `/v1/health` on every replica, submit one
   borrowed-key request, renew a Worker through a different replica, and open a
   stream from a replica that did not accept the task.
5. Keep the JSON snapshot read-only until the new deployment is accepted.

Do not dual-write to the JSON and Postgres stores. Two authoritative stores make
rollback nondeterministic.

### Backup and restore

- Back up with a single consistent snapshot, for example
  `pg_dump --format=custom --no-owner "$GOLDENBEE_GATEWAY_DATABASE_URL" > gateway.dump`.
- Restore into an empty database, then start one replica and verify the schema
  version before adding the others.
- Encrypt the dump. It contains control-token hashes, audit decisions, task
  metadata, and Worker endpoints. Caller tokens and the delegation secret are
  not stored in this database.
- A dump older than the binary's schema is migrated on startup. A database
  newer than the binary is refused; restore the matching binary or a database
  backup from that version.
- Do not export a live JSON directory into a running shared deployment.

### Rollback

1. Stop every shared-store replica.
2. Start the previous binary and configuration against the untouched file
   snapshot from step 2 of the migration.
3. Leave the Postgres database intact but offline. Do not point the old binary
   at it; schema downgrade requires the matching database backup.

The tests that support this section are
`concurrent_same_key_dispatch_once_across_replicas`,
`replica_restart_replays_without_dispatch`,
`lease_renewal_from_another_replica`,
`stream_reattaches_from_another_replica_without_submitting`,
`lost_acknowledgment_converges_on_one_worker_task`,
`admin_change_propagates_immediately`, and
`expiry_sweep_from_another_replica` in
`crates/gateway-core/tests/shared_store.rs`, plus the file adapter contract tests
in `crates/gateway-core/src/store/tests.rs`.

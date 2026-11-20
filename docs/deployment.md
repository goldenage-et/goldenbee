# Deployment and usage

This is the operator path from zero. It covers local development, a standalone
Worker, a Gateway + CLI stack, verified installation, optional instance and
fleet tools, systemd, and the Postgres-backed Gateway. For component ownership
and boundaries, read [Architecture](architecture.md) first.

Commands assume the release executables (`gb-gateway`, `gb-client`, `general`,
`coding`, `knowledge`, `gb-install`, `gb-instance`) are on `PATH`. From a source
checkout, use
`target/debug/<name>` or `cargo run --locked -p <package> -- ...`. The release
target is Linux `x86_64-unknown-linux-gnu`.

> **Scope.** The bundled listeners do not implement TLS. Keep them on loopback
> or a private interface unless a reviewed reverse proxy or service mesh
> terminates TLS. The JSON Gateway stores are single-writer; multiple Gateway
> replicas require PostgreSQL. Provider credentials are Worker-owned; there is
> no merged shared provider vault.

## Prerequisites

- Rust toolchain pinned by `rust-toolchain.toml`
- Python 3.11 or newer for validation and smoke tests
- GNU Make
- Linux for sandboxed execution and the systemd examples
- `bubblewrap` (`bwrap`) and `prlimit` for the Linux sandbox
- PostgreSQL for multiple Gateway replicas
- A TLS reverse proxy or service mesh for any non-loopback exposure
- systemd, a container runtime, or another host supervisor for restart and boot

The sandbox is not optional for an untrusted package. Installation and
execution fail closed when its prerequisites are unavailable. A package
signature proves provenance; it does not replace isolation or grants.

## Choose a path

| Goal | Start with |
| --- | --- |
| Try the full stack locally | `make setup && make start all`, then `make start gb-client` |
| Run one Worker with no control plane | `make worker:general` or a standalone `general --config ...` |
| Use the CLI through Gateway | Gateway + one Worker + `gb-client gw add` |
| Install a signed release on a host | `gb-install` then `gb-instance` |
| Distribute releases privately | external `gb-registry` from infrastructure plus `gb-install publish` |
| Manage Workers on several hosts | one `gb-gateway` per host, or a root Gateway with `hosts` |
| Serve multiple Gateway replicas | Postgres shared store + load balancer |
| Use the control plane from a backend | depend on `gateway-core` directly |

## Single-command operator install

Install the Gateway directly from GitHub:

```sh
curl -fsSL https://raw.githubusercontent.com/goldenage-et/goldenbee/main/install.sh | sh
```

Install the Client too, or install only the Client:

```sh
curl -fsSL https://raw.githubusercontent.com/goldenage-et/goldenbee/main/install.sh | sh -s -- all
curl -fsSL https://raw.githubusercontent.com/goldenage-et/goldenbee/main/install.sh | sh -s -- client
```

The script downloads target-specific assets, verifies `SHA256SUMS`, and writes
`gateway.json` and `trust.json` on first Gateway install. There is no separate
composer executable. `install.sh` is not `gb-install`; it installs the Gateway
and Client applications, not signed Worker packages. See
[Install Goldenbee](goldenbee-cli.md) for paths and environment overrides.

## Local development from zero

Prepare the isolated development workspace once:

```sh
make setup
```

This prepares development binaries, Gateway configuration, Worker
configurations and assets, caller/admin/control tokens, and a small Coding
workspace under `.goldenbee/dev`. It resets the local development state,
including the Gateway registry/dispatch projection, generated Worker state
directories, and CLI profiles. Development task history is not preserved across
this reset. It does not install system services or write outside the checkout.

If `.env` contains `OPENROUTER_API_KEY` and `OPENROUTER_MODEL_KEY`, the generated
development Workers select that provider. Otherwise they use the deterministic
diagnostic model. Never commit `.env` or a real token.

Start all three development Workers and Gateway:

```sh
make start all
```

Expected services:

| Service | Default address |
| --- | --- |
| Gateway | `127.0.0.1:18080` |
| General Worker | `127.0.0.1:18081` |
| Coding Worker | `127.0.0.1:18082` |
| Knowledge Worker | `127.0.0.1:18083` |

`make start all` also configures the development CLI profile for the generated
caller token and prints one `goldenbee://pair/dev?...` link containing the
Gateway, caller identity, token, and Worker endpoints. The link contains a
secret; do not paste it into a ticket or log.

Open the CLI, or use the one-command path that starts the stack and then opens
the chat:

```sh
make start gb-client
make dev                 # start all, then open the CLI
```

Start or stop pieces independently:

```sh
make start gateway          # auto-starts the General Worker if none is running
make start worker general
make start worker coding
make start worker knowledge
make start gb-client
make dev-status
make stop worker coding
make stop gateway
make stop all
```

Health checks:

```sh
curl -fsS http://127.0.0.1:18080/v1/health
curl -fsS http://127.0.0.1:18081/v1/health
```

The Worker web UI is at `http://127.0.0.1:18081/ui/`. The development targets set
`GOLDENBEE_DEV_UI=1`; production must not set it. These are debug binaries and
development tokens, not a production deployment.

### Local CLI examples

```sh
gb-client health
gb-client workers
gb-client submit --worker general-a --agent general --input "hello" --json
gb-client task <task-id> --json
gb-client watch <task-id>
```

If the CLI is not installed on `PATH`, use the development copy at
`.goldenbee/dev/bin/gb-client` or the source build at `target/debug/gb-client`.

`gb-client` without a command opens the active Gateway in a full-screen chat. The
context pane shows the selected profile, Gateway, Worker, agent, session, and task
status. Redirected stdin or stdout falls back to a line-oriented session. See
[CLI](gb-client.md).

## Standalone Worker

A Worker does not require Gateway, SDK, registry, or a Gateway-managed host.

From the source checkout:

```sh
make worker:general:run INPUT="hello"   # one task, JSON result
make worker:general                     # serve config/general.toml on 127.0.0.1:8080
make worker:general:dev                 # native ADK console launcher
make worker:general:stop
```

For another configuration or address:

```sh
CONFIG=/tmp/my-worker/worker.toml WORKER_ADDR=127.0.0.1:8099 make worker:general
```

Direct executable form:

```sh
general --config /path/to/worker.toml run --agent general --input "hello" --json
general --config /path/to/worker.toml dev --agent general
general --config /path/to/worker.toml serve --addr 127.0.0.1:8081
```

Relative agent, prompt, skill, storage, and workspace paths resolve against the
configuration file's directory, not the current directory. `--config` is
required for every command except `--help` and `--version`. Expected result:
`run` prints one JSON task result; `serve` answers `GET /v1/health` and serves the
native UI at `/ui/`. See [Worker configuration](worker-config.md).

## Gateway plus one Worker

This section uses the example listener on port 8080. The `make` local development
stack uses 18080, as listed above.

Create an operator configuration from the example:

```sh
cp config/gateway.example.json gateway.json
```

Replace the caller tokens, Worker `control_token`, endpoint, instance, package,
and capability values. Start Gateway:

```sh
gb-gateway serve --config ./gateway.json
```

Start the Worker with a matching `[gateway]` block in its TOML:

```toml
[gateway]
url = "http://127.0.0.1:8080"
control_token = "the-same-worker-control-token"
advertise_url = "http://127.0.0.1:8081"
renew_interval_ms = 15000
```

```sh
general --config /path/to/worker.toml serve --addr 127.0.0.1:8081
```

The Worker registers asynchronously and renews its lease. Gateway verifies the
live instance, package, and capabilities rather than trusting the registration
body. If the approval is not in the configuration file, an administrator can add
it. A generated control token is printed exactly once; put it in the Worker's
`[gateway].control_token` and restart the Worker, or supply the same token when
creating the approval. This command needs an `admin` scope:

```sh
gb-client --gateway http://127.0.0.1:8080 --token <admin-token> \
  worker add general-a http://127.0.0.1:8081 \
  --instance general-a --package general --capability general --no-preflight
```

Configure the CLI and verify the path:

```sh
gb-client gw add local http://127.0.0.1:8080 <caller-token>
gb-client gw switch local
gb-client health
gb-client workers
gb-client submit --worker general-a --agent general --input "hello" --json
```

Expected result: `gb-client health` reports the Gateway and Worker counts; `gb-client workers`
lists `general-a`; `gb-client submit` returns a Gateway task id, and `gb-client task <id>`
shows the terminal projection. `gb-client worker ...` requires an `admin` scope token;
`gb-client workers` is the ordinary caller view. The local dev stack generated by
`scripts/dev.sh` uses one unrestricted `local` caller instead, so its CLI and TUI
management commands need no profile switch. See [Gateway protocol](gateway-api.md).

## Verified install and instance

Installation and instance lifecycle are separate. `gb-install inspect` only reads
and hashes. `gb-install install` verifies provenance, digests, compatibility, and
permissions, runs a constrained sandbox smoke test, then publishes the release
under `<root>/packages/sha256/<manifest-digest>/`.

```sh
gb-install inspect \
  --artifact ./release/packages/general-0.1.0-x86_64-unknown-linux-gnu \
  --trust ./trust.json --json

gb-install install \
  --artifact ./release/packages/general-0.1.0-x86_64-unknown-linux-gnu \
  --trust ./trust.json --root /srv/goldenbee --json

gb-install list --root /srv/goldenbee --json
```

Use the OCI layout directory as `--artifact`, not the release root. The digest
from `inspect` or `list` is the immutable identity used by `gb-instance`.

Create, configure, and start one instance:

```sh
gb-instance create \
  --root /srv/instances \
  --release /srv/goldenbee/packages/sha256/<digest> \
  --id general-a \
  --grants task:submit,task:read,profile:read

gb-instance profile --root /srv/instances --instance general-a \
  create default \
  --instruction "You are a helpful agent." \
  --member general --capability general \
  --grant task:submit --activate

gb-instance start --root /srv/instances --instance general-a
gb-instance inspect --root /srv/instances --instance general-a --json
```

Expected result: `inspect --json` reports `lifecycle: running`,
`process_health: running`, and `task_activity: idle` after readiness. A failed
start writes `faulted`, never `running`. Stop with:

```sh
gb-instance stop --root /srv/instances --instance general-a
```

Without `--config`, `gb-instance` generates a `worker.toml` with a development
token. For a production-shaped host, supply `--config` pointing at an
operator-owned file and keep secrets in a root-owned, mode-`0600` environment
file. See [Instance lifecycle](instance.md) and [Install](install.md).

## Private registry

The registry is optional and never participates in execution. It stores
immutable releases; installation still runs local trust, compatibility,
permission, sandbox, and smoke checks.

```sh
The registry listener is a separate deployment, not part of this repository.
Point the harness at its URL and token.
```

Generate a token hash with:

```sh
printf '%s' "$TOKEN" | sha256sum
```

Publish one signed OCI layout and install it by digest:

```sh
gb-install publish \
  --artifact ./release/packages/general-0.1.0-x86_64-unknown-linux-gnu \
  --registry http://127.0.0.1:8085 \
  --token "$TOKEN" --publisher acme --json

gb-install registry list \
  --registry http://127.0.0.1:8085 \
  --token "$TOKEN" --package acme/general --json

gb-install registry install \
  --registry http://127.0.0.1:8085 \
  --token "$TOKEN" \
  --package acme/general \
  --digest sha256:<manifest-digest> \
  --trust ./trust.json --root /srv/goldenbee --json
```

Expected result: `publish` reports the immutable version and digest; `registry
list` shows it; `registry install` places the same digest in the local cache.
Stopping the registry does not affect installed Workers. Plain HTTP is accepted
only on loopback. See [Private registry](registry.md) and
[Trust and revocation](trust.md).

## Managed Workers on the Gateway host

Run one `gb-gateway` on each host. The Gateway owns that host's verified package
cache, instance root, allowed configuration/workspace roots, and host-local
policy. Client requests never carry host filesystem paths.

```sh
export GOLDENBEE_DELEGATION_SECRET='shared with each Worker process'
gb-gateway serve --config /etc/goldenbee/gateway.json
```

Install a signed package and deploy an instance with a caller token that carries
`instance:admin`:

```sh
gb-client package install --source /srv/goldenbee/releases/general-0.1.0
gb-client instance deploy \
  --source /srv/goldenbee/releases/general-0.1.0 \
  --package general \
  --instance general-a --worker general-a \
  --grant task:submit --grant task:read --json
gb-client instance list --json
gb-client instance logs general-a
gb-client instance stop general-a
gb-client instance start general-a
gb-client instance restart general-a
gb-client instance upgrade general-a \
  --operation upgrade-general-a-1 \
  --release sha256:<manifest-digest> --json
gb-client instance rollback general-a \
  --operation rollback-general-a-1 --json
```

Expected result: `package install` reports the package, version, digest, signer,
and target; `instance deploy` returns digest, lifecycle, process health,
compatibility, and the registered Worker; repeating a lifecycle operation id
returns the recorded outcome with `replayed: true`.

The Gateway accepts only packages that pass its trust policy and smoke checks. A
folder source must resolve under `allowed_package_sources`; a URL source is
downloaded and verified before publication. **The Gateway is not a task proxy or
a process supervisor for an external Worker.** When it owns the instance it
starts and stops the process, but systemd or the container runtime still owns
boot and crash restart policy.

A control-plane-only Gateway can proxy lifecycle operations to another Gateway
through `hosts`; the remote Gateway must accept the configured caller token with
`instance:admin`. See [Local host lifecycle](goldenbee-composer.md).

## systemd

Example units are in [`deploy/systemd`](../deploy/systemd). They are examples,
not an installer. Create the service account and directories, install the units,
and replace every placeholder token and path as described in that directory's
README.

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now goldenbee.target
sudo systemctl enable --now goldenbee-worker@general-a.service
```

`goldenbee.target` starts Gateway. Each Worker instance is enabled separately, so
Gateway and Workers start independently. Logs go to the journal:

```sh
sudo journalctl -u goldenbee-gateway.service -f
sudo journalctl -u goldenbee-worker@general-a.service -f
sudo journalctl -u 'goldenbee-worker@*' --since '1 hour ago'
```

SIGTERM is the stop signal. The Worker closes admission, waits up to its drain
bound, and reports uncertain active work as `recovering`. The Gateway stops
accepting new requests and lets in-flight requests finish. Restarting never
silently re-submits a task. See [Operations](operations.md).

## Multiple Gateway replicas with PostgreSQL

One Gateway process with the JSON registry and dispatch stores is the supported
single-node shape. A second process pointed at the same data directory fails with
`storage.single_writer`. Do not try to make the JSON files shared state.

Multiple replicas require the Postgres store and a binary built with the
`postgres` feature:

```json
{
  "store": {
    "kind": "postgres",
    "url_env": "GOLDENBEE_GATEWAY_DATABASE_URL",
    "max_connections": 16,
    "migrate": true
  },
  "replica_id": "gateway-a"
}
```

```sh
# Build or obtain a gateway binary with the feature enabled.
cargo build -p goldenbee-gateway --features postgres
```

Put `GOLDENBEE_GATEWAY_DATABASE_URL` in a root-owned `0600` environment file, not
in the JSON. Give every replica a distinct stable `replica_id`, keep the same
delegation secret, and put a load balancer in front of the replicas for caller
and Worker registration traffic. Each replica must be able to reach every
approved Worker endpoint because the receiving replica performs live identity
verification.

What the shared store covers is documented in
[Operations: shared Gateway store](operations.md#shared-gateway-store):
idempotent dispatch intent, atomic claims, first-writer-wins acceptance,
cross-replica stream reattachment, leases, approvals, and audit. The documented
active-active evidence is limited to the tested schema and PostgreSQL 17
scenarios. It is **not** a claim about database failover, connection-pool
exhaustion, network partitions, untested PostgreSQL versions, or general high
availability. The deployment still owns backups, migration timing, pool sizing,
TLS, and network policy. A replica that cannot reach Postgres is not a healthy
read/write member.

## Use cases

### Local agent workspace

`make setup && make start all`, then `make start gb-client`. Use the General Worker for
general chat, or switch to Coding or Knowledge with `/worker`. No registry or host
adapter is required.

### Private coding environment

Use the Coding Worker with an operator-selected repository and worktree. The
package edits and tests code through native ADK coding tools; the Client sees
tasks, diffs, and verification evidence. Configure the workspace, repository,
checks, and models under `[settings]`. See
[Worker configuration: Coding package](worker-config.md#coding-package).

### Knowledge assistant with citations

Use the Knowledge Worker with operator-selected source directories. Documents
are ingested through the package-owned source adapter and cited back to their
source revision or artifact digest.

### Team of specialists

Create a Gateway roster with a Master and subworkers. The managing token needs
`team:write` or `admin`; submitting to the team needs `task:submit`:

```sh
gb-client team create review-team --master general-a \
  --member coding-a:coding --member knowledge-a:knowledge \
  --max-depth 1 --max-fanout 2
gb-client team validate review-team
gb-client team activate review-team
gb-client submit --team review-team --agent general --input "Plan the review." --json
```

The Master delegates through native A2A within the frozen roster. Gateway stores
the roster and routing projection, not child execution state. See
[Gateway protocol](gateway-api.md).

### Managed private fleet

Run one Gateway per host, or a root Gateway with remote `hosts`, plus an optional
private registry. Operators install verified packages, provision instances, and
upgrade by digest. Workers continue running if Gateway, registry, or a remote
Gateway is unavailable; existing tasks are not automatically moved to another
Worker.

### Business backend using gateway-core

A trusted backend may depend on `gateway-core`, supply an already-authenticated
principal, scopes, limits, and an optional Worker allow-list, and submit, stream,
cancel, discover, and administer Workers without an extra network hop. Users,
login, payments, entitlements, and tenant policy remain in that backend.

### High-availability control plane

Run multiple Gateway replicas with PostgreSQL and a load balancer, then verify
cross-replica registration, submission, streaming, and recovery within the
documented tested scope. Keep remote Gateway hosts and Workers independent. Do
not call this generally safe without the Postgres store and the operational work
above.

## Decision table

| Need | Use | Avoid |
| --- | --- | --- |
| Try agents locally | `make setup && make start all` | Treating debug tokens as production |
| One Worker, no control plane | standalone Worker | Gateway and registry for no reason |
| CLI + Gateway on one host | Gateway + Worker + CLI | Direct Worker calls from a product Client |
| Install signed packages | `gb-install`, then `gb-instance` | Running an unverified binary |
| Distribute packages privately | registry | Making the registry an execution dependency |
| Manage instances on many hosts | one Gateway per host, or root Gateway + `hosts` | Turning the Gateway into a task proxy for a remote host |
| Serve more than one Gateway replica | Postgres shared store + load balancer | Sharing JSON files between writers |
| Use the control plane from a backend | `gateway-core` directly | Reimplementing Gateway routing |

## Health and troubleshooting

```sh
curl -fsS http://127.0.0.1:8080/v1/health     # Gateway example
curl -fsS http://127.0.0.1:8081/v1/health     # Worker example
gb-client health
make dev-status                               # local development stack
```

Health is liveness plus projections. It is not proof that a remote model
provider is available.

| Symptom or code | Meaning | Next check |
| --- | --- | --- |
| `auth.unauthenticated` | Missing or wrong caller token | CLI profile, Gateway `callers[]` |
| `route.worker.not_found` | Worker not approved by this Gateway | `gb-client worker list --all`, `gb-client workers` |
| `route.worker_unavailable` | Approved Worker is unreachable or identity-mismatched | Worker health, endpoint, instance/package |
| `worker.endpoint_unavailable` | Registration or probe could not verify the live endpoint | `gb-client worker probe <id>`, listener, firewall |
| `host.unreachable` | Gateway cannot reach a remote Gateway host | Remote Gateway process, endpoint, caller token |
| `install.smoke_failed` | Candidate failed constrained startup checks | Sandbox prerequisites, binary probe, trust policy |
| `lifecycle.draining` | Worker is closing admission | Retry after restart or drain completion |
| `provider.credential_unavailable` | Provider credential reference is not usable | `env:` variable on the Worker host |
| `storage.single_writer` | A second process opened a JSON Gateway store | Use one writer or the Postgres store |
| `lifecycle: faulted` | Start failed readiness | `gb-instance inspect`, `process.log`, configuration |

## Limits

- Default listeners are loopback-only. TLS and public exposure are operator
  responsibilities.
- A single Gateway process is not high availability. Multiple replicas require
  Postgres and the documented operational work.
- A Worker crash does not automatically move its task to another Worker.
- A Gateway-owned local host manages its own Worker processes; it is not a task proxy for a remote host.
- The registry is a distribution service, not an invocation hop.
- Provider credentials remain Worker-owned. `secret://` and `vault:` are
  recognized references but have no merged provider behind them; use `env:` for
  current behavior.
- The release candidate is incomplete. Do not treat a path, protocol surface, or
  optional native feature as production acceptance without the matching evidence.

## Related documents

- [Documentation index](README.md)
- [Architecture](architecture.md)
- [Operations](operations.md)
- [Install](install.md)
- [Instance lifecycle](instance.md)
- [Local host lifecycle](goldenbee-composer.md)
- [Private registry](registry.md)
- [Trust and revocation](trust.md)
- [Worker configuration](worker-config.md)
- [Gateway protocol](gateway-api.md)
- [Worker protocol](worker-api.md)
- [CLI](gb-client.md)
- [Release](release.md)
- [systemd examples](../deploy/systemd/README.md)

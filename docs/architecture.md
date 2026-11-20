# Architecture

Goldenbee is a Rust agent harness. It keeps four concerns separate:

- **Worker** processes execute agents with native ADK.
- **Gateway** coordinates approved Workers for Clients.
- **Client** applications reach Gateway through an **SDK**.
- **Operator tools** install, distribute, and provision Worker processes.

Goldenbee is not a unified application, a scheduler, a container platform, or a
model proxy. The [glossary](../GLOSSARY.md) defines the product terms, and
[repository layout](layout.md) records the accepted module ownership.

> **Status.** This page describes the accepted architecture and the implemented
> v1 surfaces. Release acceptance is still incomplete; see [Release](release.md)
> and [release notes](release-notes.md). A directory or route in this document is
> not proof that every optional capability is production-ready.

## One-minute view

```text
CLI / web / desktop / other Clients
                |
                |  product traffic
                v
              SDK  (client library)
                |
                v
             Gateway  ---------------------> approved Worker Instances
                |                                  |
                | control-plane state              +-- native ADK execution
                |                                  +-- tools, sessions, memory
                |                                  +-- durable tasks and events
                |
        JSON file stores (one writer)  or  PostgreSQL (multiple replicas)

optional lifecycle:     gb-gateway (local host) --> gb-instance --> Worker process
optional distribution:  gb-registry (infrastructure) / local OCI layout --> gb-install
```

The SDK is a connection library, not an application tier. Workers can run
without Gateway. Gateway never executes an agent.

## Building blocks

### Worker

A Worker is an independently deployable agent execution process. One process
hosts one **Worker Instance**. An instance has its own identity, configuration,
state, grants, limits, and Runtime Profiles.

The Worker owns:

- native ADK agents, tools, sessions, graphs, A2A, MCP, memory, RAG, coding, and
  browser execution where the package enables them;
- task admission, cancellation, durable task projections, and bounded drain;
- native session history, checkpoints, artifacts, and effect receipts;
- logical provider construction, credential resolution, model calls, usage, and
  explicit fallback;
- sandboxed command execution and its workspace policy.

A **Worker Package** is the executable. Packages share the **Worker runtime**
(`crates/runtime`), which supplies task admission, native ADK integration,
provider preflight, asset loading, hooks, and durable state behavior. The
runtime is a shared harness inside Worker processes, not another application
tier.

A **Runtime Profile** selects and constrains capabilities that the package
already compiled. A different instruction, model, skill, or agent role normally
needs a profile, not a new package. A Master or subworker is an instance role,
not a different binary.

Workers are long-lived stateful processes. They are not interchangeable
stateless replicas; losing a Worker leaves its in-flight work uncertain until
the documented recovery path runs.

### Gateway

Gateway is the control plane between Client traffic and approved Workers. It
owns:

- Worker approvals, registration, leases, discovery, disable/decommission, and
  audit;
- caller-scoped routing, Worker allow-lists, and delegation issuance;
- dispatch identity and idempotency;
- team rosters and profile routing;
- retained and live event forwarding;
- operator-visible lifecycle operations for its own local host and configured remote Gateway hosts.

Gateway does **not** execute agents, own Worker sessions or assets, run model
calls, hold provider credentials by default, or supervise host processes.

The implementation has two owners:

| Piece | Kind | Responsibility |
| --- | --- | --- |
| `crates/gateway-core` | Library | Routing, registry, leases, dispatch, teams, audit, event forwarding |
| `apps/goldenbee-gateway` | Application | HTTP/SSE listener, caller bearer authentication, JSON configuration, signal handling |

The executable is named `gb-gateway`. A business backend may use `gateway-core`
directly with an already-authenticated principal; that removes a network hop but
does not add users, payments, tenant policy, or high availability.

`gb-gateway` also owns the optional local host lifecycle. It installs verified
packages, provisions instances, and starts, stops, restarts, upgrades, and rolls
back Workers in its own roots. There is no composer executable and no host
credential. The independent `gb-client` remains a separate application.

### Client and SDK

A Client is an independent user-facing application. The CLI is the first Client;
web and desktop are separate future applications, not tabs of one mandatory
shell.

Every product Client reaches Workers through an SDK and Gateway. The Rust SDK is
a reusable client library. It carries protocol types and connection behavior,
not native ADK execution or presentation. The planned web Client uses a suitable
SDK without running Rust in the browser.

`gb-client` is not a Worker control tool. `gb-client workers` is caller-scoped discovery;
`gb-client worker ...` uses Gateway administrator routes and needs an `admin` token.
See [CLI](gb-client.md).

## Optional operator tools

These tools are not additional product tiers and are not required to run a
Worker.

| Tool | Owns | Does not own |
| --- | --- | --- |
| `gb-install` | Verified OCI release cache, signature and advisory checks, constrained smoke acceptance | Instance lifecycle, Gateway policy, Worker execution |
| `gb-instance` | On-host instance layout, profiles proxy, start/stop/inspect, upgrade, backup, restore, quarantine | Fleet orchestration, production supervision, Gateway routing |
| `gb-registry` (infrastructure) | Authenticated release distribution, immutable versions, content-addressed storage | Trust decisions, installation acceptance, task invocation |
| `gb-gateway` local host | Verified package cache, instance provisioning, start/stop/restart, upgrade/rollback, allowed roots | Task routing for an external host, model calls, Worker execution, caller identity |

The Gateway local host reuses `crates/instance` and `crates/install`; the
external `gb-registry` reuses `crates/registry`. They are adapters, not duplicate
managers.

## Planes and request flow

| Plane | Owner | Failure effect |
| --- | --- | --- |
| Client | CLI, web, SDK | Cannot submit; existing Workers keep running |
| Control | Gateway | No new routing or administration; Worker registration retries |
| Execution | Worker | That Worker's tasks become unreachable or `recovering` |
| Distribution | Registry | Installed Workers are unaffected |
| Lifecycle | `gb-gateway` local host | Running Workers are unaffected; new lifecycle operations fail |
| Host supervision | systemd or container runtime | Process restart policy is outside Goldenbee |

A normal task follows this path:

1. A Client calls the SDK, which submits to Gateway with a caller token.
2. Gateway persists the caller-scoped intent and idempotency key, checks the
   allow-list and capability, and selects an approved Worker or team.
3. Gateway issues a short-lived delegation and dispatches to the Worker.
4. The Worker admits the task, persists it before acknowledging, and runs the
   native ADK composition.
5. Retained events and the terminal projection flow back through Gateway. The
   Worker remains authoritative for execution state.
6. A retry with the same idempotency key converges on the same logical task.
   Recovery of interrupted work is explicit; it is never silent resubmission.

Teams are Gateway-owned rosters, not processes. The Master and every subworker
are ordinary Worker Instances. The Master delegates through native A2A within
the signed, frozen roster and narrowed authority. See
[Gateway protocol](gateway-api.md) and [Worker protocol](worker-api.md).

## Deployment shapes

### Standalone Worker

```text
operator or local native client --> Worker
```

No Gateway, SDK, registry, or Gateway-managed host is required. This is the native
development and offline-execution path.

### Single-host development stack

```text
CLI --> SDK --> Gateway --> Worker Instances
```

All processes use loopback listeners, generated development tokens, and
`.goldenbee/dev` state. This is a debug stack, not a production deployment.

### Managed single host

```text
Gateway ----------------> Worker (tasks, streams, health)
   |
   +--> local host --> gb-instance --> Worker process
```

The Gateway performs verified package installation, provisioning, inspection,
start/stop/restart, upgrade, and rollback. It still talks to the Worker directly
for task traffic. It is not a task proxy for an external host and not the crash
restart supervisor.

### Distributed fleet

```text
                        +-- remote Gateway A --> Worker A
Gateway ----------------+
   |                    +-- remote Gateway B --> Worker B
   |
   +---------------------- direct task traffic --> Workers
```

Each host runs one Gateway. A root Gateway selects from its operator
configuration; callers cannot supply a host endpoint, credential, executable, or
release path.

### Multiple Gateway replicas

```text
Clients --> load balancer --> Gateway replicas --> Workers
                                  |
                             shared PostgreSQL
```

Multiple replicas require the shared Postgres store. The JSON registry and
dispatch stores are single-writer and fail closed if a second process opens the
same data directory. Postgres provides the shared control-plane state instead,
but deployment still owns load balancing, database failover, pool sizing,
backups, network policy, and TLS. See
[Operations: shared Gateway store](operations.md#shared-gateway-store).

## Trust and security boundaries

- Gateway does not receive host root credentials, Worker asset directories, or
  provider secret values.
- A remote host credential is an ordinary Gateway caller token scoped to that
  Gateway and, optionally, an instance allow-list. It is not a caller task token.
  Gateway does not forward caller task tokens to the remote host.
- Caller tokens and Worker control tokens are different authorities. A Gateway
  caller token does not authenticate a Worker route, and a Client cannot turn a
  Gateway profile into direct Worker administration.
- The registry distributes bytes and metadata. It does not decide trust,
  installation acceptance, grants, or invocation.
- SDK-side checks are usability checks. Gateway and Worker enforce
  authentication, scope, isolation, and grants.
- A package signature proves provenance. It does not prove sandbox safety or
  grant permission to execute.
- Listeners are loopback/private by default. The bundled listeners do not
  terminate TLS; a reviewed reverse proxy or service mesh owns public exposure.
- Worker credentials are resolved in the Worker process. They are not copied
  onto tasks, events, or Gateway state.

## Provider credentials today

Provider credentials are **Worker-owned**. A Worker configuration declares
logical providers under `[providers.<id>]` and resolves a credential reference
such as `env:PROVIDER_API_KEY`. A profile, agent, or package registration selects
the logical provider name; the Worker records the concrete adapter, model,
locality, residency, and credential reference per invocation.

`secret://` and `vault:` references are recognized and validated as references,
but they currently fail with `provider.credential_unavailable` until a provider
integration is installed. `env:` is the working credential source. There is no
merged shared provider vault, and Gateway does not hold provider keys. Treat any
central vault design as future work, not current behavior. See
[Worker configuration: Providers](worker-config.md#providers).

## Failure behavior

| Event | Behavior |
| --- | --- |
| Gateway stops | Existing tasks on Workers keep running. New routing, discovery, and administration are unavailable until a Gateway returns. Workers retry registration. |
| Worker stops | Its tasks are unreachable or `recovering`; the Gateway does not silently move them to an unrelated Worker. |
| Registry stops | Installed Workers and all local execution continue. Installation from the registry is unavailable. |
| Remote Gateway host stops | Running Workers continue. New provision/inspect/upgrade/rollback calls return `host.unreachable`. |
| Second writer opens a JSON Gateway store | Startup fails with `storage.single_writer`; state is not shared or merged. |
| Postgres is unreachable | A replica cannot serve store operations reliably and is not a healthy read/write member. There is no silent file fallback. |
| Provider credential is missing | Admission fails with `provider.credential_unavailable`; no secret value appears in diagnostics. |
| Sandbox is unavailable | Installation or execution fails closed instead of running unsandboxed. |

## What Goldenbee is not

- Not a Kubernetes replacement, scheduler, or container orchestrator.
- Not a shared-memory compute cluster.
- Not automatic task failover between unrelated Workers.
- Not a model proxy or central provider-key service.
- Not a single fixed Worker binary or one mandatory Client application.
- Not a replacement for systemd, a container runtime, or database operations.
- Not a general multi-tenant business platform. Users, login, payments, and
  tenant policy remain with the application that uses `gateway-core`.

## Current limits

- The advertised release target is Linux `x86_64-unknown-linux-gnu`.
- The release candidate is incomplete; clean-install, production-signing, and
  full deployment-drill evidence remain separate gates.
- Process-restart durability is tested. Power-loss durability, filesystem
  corruption, and disk exhaustion are separate operational events.
- A single file-store Gateway is not high availability. Postgres is required for
  multiple replicas, and even then only the documented tested scope is claimed.
- No product web or desktop Client is released. Native Launcher web serving is a
  development tool.
- No public TLS listener, certificate rotation, DDoS protection, or internet
  exposure is shipped.
- Optional native capabilities are not guaranteed merely because the harness
  can expose them. Check the package's declared capabilities and the release
  evidence.

## Related documents

- [Documentation index](README.md)
- [Deployment and usage](deployment.md)
- [Repository layout](layout.md)
- [Glossary](../GLOSSARY.md)
- [Gateway protocol](gateway-api.md)
- [Worker protocol](worker-api.md)
- [Worker configuration](worker-config.md)
- [CLI](gb-client.md)
- [Install](install.md)
- [Instance lifecycle](instance.md)
- [Local host lifecycle](goldenbee-composer.md)
- [Private registry](registry.md)
- [Trust and revocation](trust.md)
- [Operations](operations.md)
- [Release](release.md)

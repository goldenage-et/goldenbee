# Repository layout

This is the accepted target structure, not a claim that every directory or application already exists. [ADR 0001](adr/0001-layout.md) records the decision; the [glossary](../GLOSSARY.md) defines its terms.

## Three building blocks

```text
CLI ---------+
Web ---------+---> SDK ---> Gateway ---> Worker Instances
Desktop -----+                              |
Other apps --+                              +-- general
                                            +-- coding
                                            +-- knowledge
                                            +-- other packages
```

Clients are independent applications. Every Client connects through an SDK. SDKs are reusable connection libraries, not another application tier, and may have language-specific implementations. Workers can also run standalone without Gateway.

## Worker development path

Every Worker Package uses native ADK-Rust Launcher to provide its development CLI and web server:

```text
ADK Launcher CLI --------+
ADK Launcher web server -+---> Worker Package ---> runtime ---> native ADK
```

This local development path needs neither SDK nor Gateway. It is distinct from the product Client path above. The product CLI and web application remain independent Clients that reach Gateway through SDKs.

Launcher development modes use the package's real agent composition and capabilities. Do not build a replacement Worker development CLI, web server, or model loop. Launcher feature flags and both modes require an executable check for each supported package; the standalone Runner proof alone does not establish launcher compatibility.

## Target structure

```text
goldenbee/
|-- workers/
|   |-- general/       # Initial Worker Package
|   |-- coding/        # Coding capabilities
|   |-- knowledge/     # Ingestion and retrieval capabilities
|   `-- research/      # Example future package, after capabilities are specified
|-- apps/
|   |-- goldenbee-cli/     # Operator bootstrap and component dispatcher
|   |-- goldenbee-gateway/ # Standalone HTTP adapter over gateway-core
|   |-- goldenbee-composer/ # Host-local lifecycle composer
|   |-- goldenbee-client-cli/ # First Client application
|   |-- web/           # Later: independent Client application
|   `-- desktop/       # Optional future Client application
|-- crates/
|   |-- gateway-core/  # Reusable Gateway control plane
|   |-- registry/      # Public registry client and wire types
|   |-- runtime/       # Shared Worker harness and native ADK integration
|   |-- install/       # Verified local release cache and installation
|   |-- instance/      # On-host Worker instance provisioning and lifecycle
|   |-- common/        # Small shared types and helpers
|   |-- provider-crypto/ # Provider secret encryption and per-Worker envelopes
|   `-- sdk/           # Rust Gateway client library
|-- packages/
|   `-- sdk/           # Later: TypeScript Gateway client library for web
|-- config/            # Safe example configuration
|-- schema/            # Versioned protocol schemas
|-- examples/          # Executable integrations
|-- tests/             # Public behavior and recovery checks
|-- docs/
|   |-- agents/
|   |-- adr/
|   |-- dev.md
|   |-- layout.md
|   |-- instance.md
|   `-- adk.md
|-- .goldenbee/        # Local runtime state (generated, ignored)
|-- Cargo.toml
|-- Cargo.lock
|-- Makefile
|-- AGENTS.md
|-- GLOSSARY.md
|-- README.md
`-- proposal.md
```

The package and application names show where independently implemented products belong. They do not authorize empty projects or imply that every illustrated Worker is required for the first release. Use short, generic crate names matching their directories, such as `general`, `runtime`, and `sdk`.

## Ownership

| Location | Owns | Does not own |
| --- | --- | --- |
| `workers/*` | Use-case capabilities, package composition, executable startup | A copied model loop or Gateway registry |
| `crates/runtime` | Shared Worker task coordination and verified native ADK integration | Gateway team configuration or Client presentation |
| `crates/install` | Release verification, immutable cache, constrained smoke acceptance | User profiles, instance lifecycle, or registry transport |
| `crates/instance` | On-host instance layout, atomic creation, local start/stop/inspect, profile proxying | Launcher implementation, Gateway management, or production supervision |
| `crates/gateway-core` | Worker approvals and leases, caller-scoped routing, dispatch identity and idempotency, delegation issuance, discovery, administration, audit, and event forwarding | Axum routers, caller bearer-token authentication, listeners, business users, payments, or Worker runtime |
| `apps/goldenbee-gateway` | Standalone HTTP listener, caller authentication, JSON/SSE serialization, operator configuration | Routing, registry, lease, idempotency, or Worker event semantics |
| `apps/goldenbee-composer` | One host's scoped lifecycle credential check, verified package cache resolution, instance provisioning/upgrade/rollback, host-local policy | Gateway caller identity, registry, routing, Client presentation, or another package format |
| `crates/registry` | Private release storage, bearer scopes, immutable version index, content-addressed upload/download | Trust decisions, installation acceptance, Worker execution, or Gateway routing |
| `apps/goldenbee-cli` | Operator bootstrap, component installation, default local config, transparent gateway/composer dispatch | Client commands, Gateway routing, Worker execution |
| `apps/goldenbee-client-cli` | CLI interaction and output | Authoritative execution state |
| `apps/web`, `apps/desktop` | Their own interaction and presentation | A mandatory shared UI or Worker engine |
| `crates/sdk` | Reusable Rust client connection behavior | Native ADK execution or UI |
| `crates/common` | Shared wire representations and small reusable helpers | Mutable runtime state or a universal policy engine |
| `crates/provider-crypto` | At-rest secret sealing and per-Worker X25519 envelope primitives | Provider catalog policy, transport, or Worker execution |
| `schema` | Language-neutral protocol representations | A required Rust crate dependency for non-Rust Clients |

`gateway-core` is the single routing and registry implementation. A business backend may use it directly with an already-authenticated principal; `goldenbee-gateway` is one HTTP deployment adapter. Keep `common` small; sharing a message shape does not transfer authority over the underlying task or session.

## Dependencies

```text
workers/general ---+
workers/coding ----+---> runtime ---> native ADK
workers/knowledge -+         |
                            +------> common

goldenbee-gateway -----------------> common
gb-client ---> sdk ----------------------> common
           |
           +--- network ---> Gateway --- network ---> Workers

web ---> TypeScript SDK ---> Gateway
desktop ---> suitable SDK
```

Only Worker execution modules carry native ADK execution dependencies. Gateway may use a verified native A2A adapter where needed without importing Worker execution. Client apps and client SDKs stay independent of ADK. Web uses a suitable SDK without running Rust inside the browser; it does not bypass the SDK with another direct Gateway transport.

## Package, instance, and profile

Publish a new Worker Package when it adds real executable capabilities. A package can run as several Worker Instances, each with separate identity, state, grants, and limits.

Different instructions, models, enabled skills, or agent roles normally belong in Runtime Profiles. A developer and a read-only reviewer can therefore use the same Coding Worker package with different profiles and grants. Master and subworker are instance roles, not different executable implementations.

## Delivery order

Establish real Worker execution, connect it through Gateway and the SDK, and complete a CLI journey first. Web follows as a separate application with an appropriate SDK. Desktop and other use-case applications can be added independently through a suitable SDK.

Worker package registration remains a Worker concern. It may be supplied by shared runtime modules; a separate authoring SDK is not a prerequisite for the three building blocks and must not take the client SDK's name or dependencies.

## Migration status

The accepted structure supersedes the original scaffold and conflicting illustrative paths in the source proposal. It is not fully implemented yet.

- Native proof code currently lives under `crates/sdk`. That is the legacy Worker authoring meaning of SDK, not the target client library. Its recorded commands remain documented in [ADK compatibility](adk.md) until code is relocated.
- Worker execution and package composition need to move into Worker-owned runtime modules before `crates/sdk` can become the ADK-free client library.
- `crates/common` is the selected replacement name for the earlier shared `contracts` crate. This naming change does not itself alter protocol semantics or move policy ownership.
- The Gateway split has two owners: `crates/gateway-core` holds control-plane behavior and storage, and `apps/goldenbee-gateway` is a thin standalone HTTP adapter. The executable is `gb-gateway`.
- The existing runtime mixes Worker and Gateway responsibilities. Implementation must reconcile those owners rather than make every process instantiate one universal runtime.
- Workspace membership, executable names, dependency checks, and examples must follow the actual code migration. These documentation changes do not claim that migration or feature acceptance is complete.

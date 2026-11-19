# Goldenbee AI Agent Harness — Detailed Architecture & Implementation Proposal

> **Current architecture:** This document preserves the original proposal. Later user clarifications define independent Worker Packages under `workers/`, separate Client applications, a client-facing SDK, and shared types under `crates/common`. See the [accepted layout](docs/layout.md), [architecture decision](docs/adr/0001-layout.md), and [glossary](GLOSSARY.md). Those decisions supersede conflicting scaffold paths and SDK naming below; original examples are not claims about current implementation.

**Revision:** 1.1 — Worker SDK, native packages, runtime profiles, marketplace and ASCII diagrams  
**Date:** 17 November 2026  
**Deliverable:** Reference implementation proposal, system contracts, Worker SDK and marketplace specification, repository plan, ADRs, security and reliability model, delivery backlog, and acceptance specifications  
**Scope:** Business-application-agnostic **Agent Worker + Agent Gateway + Harness Clients + Worker SDK + native Worker Packages + runtime profiles + optional Marketplace distribution**  
**Framework baseline:** **ADK-Rust 2.3.0**, verified published on docs.rs; pin exact compatible crate versions and Cargo.lock.  
**Status:** Proposed implementation specification; interfaces and wire contracts marked *Goldenbee-defined* are designs, not claims that ADK exposes those exact symbols.

> **Reading and sizing note:** Markdown does not have fixed pagination. This long-form manuscript is structured for approximately **50–100 conventional document pages**, depending on font, margins, code wrapping, and rendering. Headings, cross-references, acceptance criteria, API contracts, decision records, and appendices are designed to survive conversion to a formal paginated proposal. No separate PDF is requested.

## Executive decision

Goldenbee is a reusable AI-agent harness, **not** a CRM, ERP, organizational business server, or fixed Command Center UI. The harness has three architectural tiers: **Clients** (CLI/SDK/REST/SSE/WebSocket consumers); **Agent Gateway** (optional shared coordination, routing, team control, durable projections); and **Agent Worker** (independent ADK-Rust-powered runtime for multiple agents, sessions and concurrent tasks). An external business application can use the Gateway API, but that application is outside this repository's required runtime dependencies. The Worker runs standalone and must remain functional without the Gateway.

**Framework-first rule:** adopt existing, supported ADK-Rust features wherever they satisfy the requirement. Implement only glue, domain-neutral service contracts, storage adapters, and policies that are unique to the Goldenbee harness. A new homegrown agent loop, multi-agent engine, graph orchestrator, MCP client, A2A stack, coding toolset, memory backend, guardrail engine, or telemetry subsystem is explicitly prohibited unless a documented gap and an approved Architecture Decision Record (ADR) demonstrate necessity. Developer-built Worker Packages may add genuinely new capabilities through typed Rust code; this is *not* permission to reimplement upstream ADK capabilities.

**Terminology:** *worker* means one independently deployable Rust process hosting multiple ADK agents. *Worker Package* means developer-authored Rust source and released executable capabilities built with the Goldenbee Worker SDK; *Worker Instance* means an installed, independently identified executable process; *Runtime Profile* means user-owned agent, skill, workflow, prompt, model, MCP, connection, and policy configuration, separate from the package source.  *master* and *subworker* are **roles/relationships**, not different worker binaries. A *task* is a durable unit of execution. A *session* is interaction context; an *invocation* is one ADK Runner execution; an *attempt* is a worker-level retry/recovery unit. *Team* means a versioned, authorized roster of worker references, with one designated master for a particular execution or configured team.

### In-scope outputs

- Complete separation of agent execution, coordination, and caller interfaces.
- Executable Rust workspace layout and dependency boundaries.
- ADK-reuse map with documentation sources and proof gates.
- Standard REST/OpenAPI, SSE, WebSocket, A2A, and CLI contracts.
- Durable task, session, artifact, and event identity semantics.
- Local and distributed orchestration including master/subworker teams.
- Security, policy inheritance, sandbox, authorization, audit and secrets designs.
- SQLite and ADK-native persistence strategy; recovery and failover plans.
- Detailed module interfaces, configuration models, state machines and examples.
- Worker SDK, source-based native Worker Packages, package registry/Marketplace, runtime-profile separation, installation, verification, upgrade and rollback.
- Implementation sequence with tests, CI, benchmarks, acceptance gates, risks and ADRs.

### Out of scope

Business users, accounting, invoices, organizational hierarchies, employee records, customer data, business workflows, UI component systems, custom model training, a proprietary LLM inference engine, and a mandatory central cloud control plane. External systems can expose capabilities through MCP or gateway API callers, but they remain independently owned and authorized.

---

# Part I — Architecture and Design Constraints

## 1. System context

```text
                           GOLDENBEE AI AGENT HARNESS
+----------------------+                +---------------------------+
| CLI / SDK / API      |--------------->| Agent Gateway (optional)  |
| WS / SSE clients     |<---------------| registry, teams, routing  |
+----------------------+                +-------------+-------------+
                                                  |  |  |
                        +-------------------------+  |  +--------------------+
                        v                            v                       v
               +----------------+          +----------------+      +----------------+
               | Worker A       |<-------->| Worker B       |      | Worker C       |
               | master role    | A2A      | specialist     |      | specialist     |
               | ADK-Rust       |<-------------------------------->| ADK-Rust       |
               +--------+-------+          +----------------+      +----------------+
                        |
                  ADK models / MCP / sessions / memory / artifacts / checkpoints

+----------------------+        +---------------------------+
| Standalone CLI       |------->| Worker D (no Gateway)     |
+----------------------+        +---------------------------+

External business server (optional) ---> Gateway API
```

The Gateway **does not run the ADK agent's reasoning loop**. It registers and addresses worker instances, routes user inputs to the intended worker, manages master/subworker team configurations, enforces admission and delegated authority at the control-plane boundary, and presents a durable event projection to clients. The Worker owns concrete agent instances, model/tool calls, invocation lifecycle, local sessions and execution evidence. Local ADK subagents are not synonymous with network workers. A remote subworker is an independently running worker with its own API, credentials, resource budget and persistence.

## 2. Non-negotiable invariants

1. A Worker starts and accepts a local task when no Gateway is available.
2. A Worker process hosts many independent agents and concurrent task invocations.
3. A session's user/app/tenant identity is never inferred from an untrusted session ID.
4. The Master Worker retains responsibility for its plan and delegated task results, while each executing Worker owns its authoritative task state.
5. A Gateway cannot forge tool outcomes or impersonate an ADK invocation; it indexes and forwards verified events.
6. Delegation permissions only narrow as work moves down the hierarchy.
7. Workers can participate in multiple teams only if resource and authorization policy allows it.
8. Worker relationships are acyclic **for an active delegation DAG**; rosters may be non-tree graphs as long as task delegation is cycle-safe.
9. Existing ADK features are used before custom implementation is considered.
10. Every externally visible command has stable identity, idempotency semantics, cancellation behavior and an authorization rule.
11. Stalled or disconnected workers are not automatically assumed to have failed or completed tasks.
12. Credentials, sensitive files and tool output are redacted according to explicit policy before logging or event fan-out.
13. The system is usable with a local inference endpoint and no hosted model service when supported provider adapters are configured.
14. No agent decides its own permission level through a prompt or tool argument.
15. A successful tool call is evidence, not proof that the original objective's acceptance criteria are satisfied.

## 3. Goals and measured outcomes

| Goal | Quantitative acceptance direction | Verification |
|---|---|---|
| Standalone mode | Work with Gateway disconnected | Isolated integration test |
| Concurrent tasks | Bounded parallelism, no global serialized Runner lock | Multi-session soak |
| Stable public contracts | Versioned routes and schemas; compatibility tests | OpenAPI and wire conformance |
| Recoverable execution | No lost acknowledged task metadata on clean restart; explicit uncertainty on crash | Kill/restart matrix |
| Multi-worker team | Master delegates to >=2 remote workers and aggregates results | E2E exercise |
| Correct policy | Unauthorized requests fail at every boundary | Negative security tests |
| Framework-first | Every feature points to upstream docs and tests | ADR and dependency audit |
| Traceability | Task -> attempt -> ADK invocation -> tool event -> artifact | Correlation trace inspection |
| Application neutrality | No mandatory business server/schema in worker or gateway | Dependency inspection |
| Operability | Health, readiness, metrics, audit and drain | Deployment verification |

## 4. Separation of ownership

| Resource or decision | Authoritative owner | Cached/projection owner | Notes |
|---|---|---|---|
| Agent definition and active version | Worker | Gateway capability registry | Do not mutate running version |
| User-facing client credential | Calling app or Gateway auth service | None | Workers validate delegated claims |
| Team configuration | Gateway (managed); local config (standalone) | Master snapshots | Version every roster |
| User interaction session | Worker/ADK SessionService | Gateway session index | Distinguish client connection from ADK session |
| Concrete agent execution | Worker/ADK Runner | Gateway state projection | Never double-write ADK internals |
| Master execution plan | Master Worker/ADK graph or team | Gateway plan visualization | Plan may have revisions |
| Delegated task | Executing subworker | Master + Gateway | One execution authority |
| A2A task state | A2A owning worker/server | Gateway adapter | Prefer ADK server task lifecycle |
| Artifacts | ADK artifact service or explicitly configured object store | Indexed metadata | Stream large content separately |
| Long-term memory | ADK memory service | None by default | Tenant-scoped access |
| Guardrail decisions | ADK guardrail/tool policy at executing worker | Gateway audit view | Multiple layers of defense |
| Business policy | External application | Gateway delegated claims | Business app is optional |

## 5. System decomposition

The **Worker executable** composes ADK Runner, agent definitions, model adapters, session/memory/artifact services, existing graph/team implementations, MCP and native tools, optional coding/browser/realtime capabilities, and a thin Worker Control API. The **Gateway executable** composes worker registry, team manager, task/session routing, event relay/projection, policy administration, connectivity and health. The **Client family** includes a CLI, a typed Rust SDK, and OpenAPI schemas so third parties can generate TypeScript, Python and other clients. The gateway offers management access; the worker offers a bounded standalone service and A2A endpoint. Gateway and Worker can be co-located yet are separate processes and cargo packages.

## 6. Language, framework and platform

- Rust stable toolchain; edition and MSRV must follow the selected ADK-Rust release and lockfile constraints.
- Tokio for asynchronous networking and task supervision; avoid nested Tokio runtimes.
- ADK-Rust 2.3.0 for all supported native components; pin exact versions after scaffold compilation.
- Axum only where ADK's built-in server does **not** already satisfy the required Gateway-specific management routes; prefer ADK server for agent API/A2A.
- SQLite for the single-node worker's *harness-only* metadata where ADK native stores cannot cover it; use ADK SQLite backends for checkpoints and memory when available.
- OpenAPI 3.1 for custom management routes; JSON Schema for payloads; RFC 9457-like typed errors where practical.
- W3C Trace Context for propagated trace identifiers; OpenTelemetry via ADK telemetry integration.
- Semantic versioning for Rust crates; separate `/v1` protocol compatibility policy for HTTP.
- Linux first, with a portable core allowing macOS/Windows; platform-specific terminal/sandbox integrations must be optional.

---

# Part II — Official ADK-Rust Reference & Reuse Decision

## 7. Source-of-truth hierarchy

1. `https://docs.rs/crate/adk-rust/2.3.0` and matching specific-crate rustdoc for the published pinned version.
2. The matching Git tag/source for ADK-Rust 2.3.0, including examples and tests.
3. `https://adk-rust.com/en/docs` official guides, which may display a **v2.1.0** banner even though a newer 2.3.0 crate is published; guides explain design but symbols must be verified on the pinned release.
4. Official A2A/MCP/ACP protocol specifications for interop details when ADK exposes an adapter.
5. Local experiments and contract tests. No community snippet overrides a pinned rustdoc signature.

**Validated publication:** docs.rs lists `adk-rust` **2.3.0**, dated 2026-10-09. Historical GitHub indexed results may still describe 2.2.0 or a 2.3 release candidate. Do not use their freshness to override the published crate page. Reference: https://docs.rs/crate/adk-rust/latest

## 8. ADK capability adoption matrix

| Harness requirement | Native feature to reuse | Upstream documentation | Custom allowance |
|---|---|---|---|
| Agent runtime, transfers, streaming | `adk-runner::Runner` / RunnerConfig | https://adk-rust.com/en/docs/core/runner | Worker RPC adaptation only |
| LLM specialists and tool calling | LlmAgent, tools, callbacks | https://adk-rust.com/en/docs/agents/llm-agent | Configuration and agent catalog |
| Local sequential orchestration | SequentialAgent | https://adk-rust.com/en/docs/agents/workflow-agents | None unless documented gap |
| Local parallel orchestration | ParallelAgent | https://adk-rust.com/en/docs/agents/workflow-agents | None |
| Bounded repeated work | LoopAgent | https://adk-rust.com/en/docs/agents/workflow-agents | None |
| Handoffs, supervisor, team policies | CompiledTeam / team architecture | https://adk-rust.com/en/docs/agents/multi-agent | Worker roster mapping |
| Durable branching workflows | GraphAgent / StateGraph | https://adk-rust.com/en/docs/agents/graph-agents | External side-effect reconciliation only |
| Session management and state | ADK SessionService | https://adk-rust.com/en/docs/sessions/sessions | Identity mapping and retention config |
| Semantic and graph memory | ADK MemoryService / GraphMemoryService | https://adk-rust.com/en/docs/memory/backends | No replacement RAG/graph framework |
| Artifacts | ADK ArtifactService | https://adk-rust.com/en/docs/artifacts/artifacts | Secure download and index API |
| Local/offline model integrations | ADK model providers (e.g. Ollama) | https://adk-rust.com/en/docs/models/ollama | Credentials/config mapping only |
| Coding assistant | CodingAgent, adk-devtools, Workspace | https://adk-rust.com/en/docs/coding-agent/harness | External workspace provisioning |
| Browser automation | ADK browser tools | https://adk-rust.com/en/docs/tools/browser-tools | Browser session lifecycle if not covered |
| Tool catalog/remote tools | McpToolset, McpServerManager | https://adk-rust.com/en/docs/mcp | Tenant-scoped access policy adapter |
| Remote agent protocol | ADK A2A server/client | https://adk-rust.com/en/docs/deployment/a2a | Worker discovery/authorization overlay |
| Editor coding-agent interop | ADK ACP client/server | https://adk-rust.com/en/docs/acp | Optional app integration |
| Agent HTTP server | ADK server/Launcher | https://adk-rust.com/en/docs/deployment/server | Extra control routes only |
| Authentication/authorization | ADK auth/access control | https://adk-rust.com/en/docs/security/access-control | Gateway policy admission/claims |
| Guardrails | ADK guardrails/plugins | https://adk-rust.com/en/docs/security/guardrails | Policy configuration, not engine |
| Structured tracing | ADK telemetry | https://adk-rust.com/en/docs/observability/telemetry | Correlation enrichment |
| Evaluations | ADK eval | https://adk-rust.com/en/docs/evaluation/agent-evaluation | Harness scenario suites |
| Retry tool failures | ADK retry-and-reflect plugin | https://adk-rust.com/en/docs/tools/retry-reflect | No generic replacement retry loop |
| Realtime voice | ADK RealtimeAgent | https://adk-rust.com/en/docs/agents/realtime-agents | Transport bridge where needed |
| Durable managed run engine | ADK managed runtime (experimental) | https://adk-rust.com/en/docs/managed-agents | Research gate; do not assume stable |

Some guide URLs vary by release. Each is a **research pointer** rather than a guaranteed 2.3 symbol declaration; implementers must resolve the official site's current redirect and record the exact pinned rustdoc URL in the module README. If a referenced guide is missing, the source of truth is rustdoc and official tag source, not invention.

## 9. Framework-first implementation workflow

For every feature ticket the engineer must: (1) open matching ADK official guide; (2) identify existing exact public types and required Cargo flags; (3) inspect pinned 2.3.0 rustdoc and examples; (4) compose ADK objects with smallest adapter; (5) write a working proof test; (6) document only the uncovered application-neutral requirement; (7) obtain ADR approval before building a competing abstraction. A feature is not accepted merely because an API endpoint exists: prove an actual ADK invocation produced the expected events, side effects and evidence.

## 10. Framework version-gating and provenance

Maintain `docs/adk-compatibility.md` with `selected_version`, crate features, examples compiled, missing/experimental features, API differences, security patches and last verification date. Do not cargo-update all framework crates automatically. Maintain a smoke-crate for Runner, GraphAgent + SqliteCheckpointer, CompiledTeam policies, CodingAgent, MCP Toolset, A2A streaming, and telemetry. Gate upgrades by building and running the smoke suite in CI and test a live multi-worker scenario before rollout. Do not claim managed-agent durability is stable without demonstrating the documented experimental API and crash semantics.

---

# Part III — Worker Runtime Detailed Design

## 11. Worker process composition

The Worker is built by *composition*, not inheritance-heavy service objects. At startup the `WorkerBootstrap` reads validated configuration, resolves secrets through a `SecretProvider`, initializes ADK telemetry once, opens ADK session/memory/artifact/checkpoint services, creates model provider instances, registers tool capabilities, constructs versioned agent definitions and ADK Runners, and finally attaches the API server. Each subsystem has one ownership boundary. Runtime services implement small interfaces whose callers depend on capabilities, not concrete SQLite, HTTP or ADK internals. This applies SOLID principles while respecting Rust's trait/ownership style.

```text
+-----------------------+
| Worker executable     |
+-----------+-----------+
            |
            v
+-----------------------+    +-----------------------+
| Bootstrap / DI        |--->| Agent / Runner Catalog|
+-----------+-----------+    +-----------+-----------+
            |                            |
            +----------+                 v
            |          |         +----------------+
            v          v         | ADK Runner     |
+----------------+ +----------------+    |
| Worker         | | ADK Model      |    +----> Sessions / Checkpoints
| Supervisor     | | Providers      |    +----> ADK Tools / MCP
+--------+-------+ +----------------+    +----> Model Providers
         ^
         |
+---------------------------+
| Worker Control API / A2A  |
+---------------------------+
```

Intra-process tasks are asynchronous; external processes include browsers, tool sandboxes, MCP stdio servers, optional model runtimes and terminal child processes. *Single process Worker* describes control runtime ownership, not an unsafe demand to embed every privileged dependency or model server in-process.

## 12. Worker state machine

States: `initializing → recovering → warming → ready → draining → stopped`; `degraded` is an orthogonal health flag, and `failed` is a terminal boot state unless restarted. `initializing` validates configuration and data dir. `recovering` reopens persistent ADK stores and task metadata; uncertain external actions are reconciled. `warming` composes model and agent dependencies and binds internal services. `ready` admits new work. `draining` atomically denies new admissions and asks existing invocations to complete/checkpoint within policy. `stopped` confirms the HTTP listener and owned subprocesses are closed. Crash recovery never assumes in-flight tool calls were harmless.

**Readiness contract:** response contains process ID/instance incarnation, boot time, config hash, registered agent count, accepted protocols, capacity and degraded subsystem names. **Liveness contract:** checks that the process event loop is servicing requests; not a proof of external provider reachability. A graceful shutdown must remove or expire peer lease, stop accepting new A2A tasks, flush durable events, close ADK services and then stop. A forced shutdown marks unfinished invocations `unknown` for reconciliation at restart.

## 13. Agent definitions and versioning

The catalog indexes `AgentDescriptor { agent_id, version, agent_kind, display_name, capabilities, supported_modalities, model_ref, tool_set_ref, skill_refs, policy_ref, status }`. This is Goldenbee catalog metadata; the executable agent remains an `Arc<dyn adk_core::Agent>` or the verified pinned 2.3 equivalent. The factory uses ADK `LlmAgentBuilder`, workflow agents, `GraphAgent`, `CodingAgent`, or `CustomAgent` only for genuinely custom agent behavior. A definition update creates an immutable version; active runs stay pinned to the old version. New sessions can select latest compatible or explicitly pinned agent version. Do not hot-swap a model/tool definition under an active run.

**Activation:** load -> validate ADK agent graph -> validate tool permission set -> preflight model provider -> publish catalog descriptor. **Deactivation:** deny new sessions/tasks targeting version -> drain pending invocations -> retain historical reference. **Deletion:** soft delete catalog entry; never delete audit or checkpoint references without explicit retention policy. **Capability discovery:** separate `available` (built/installed), `configured` (credentials/settings present), `authorized` (principal allowed), and `healthy` (resource reachable). Advertise effective intersection, not optimistic marketing.

## 14. Runner management

Use `adk-runner` as the source of truth for model turns, streaming events, tool calls, transfers, session appends and memory injection. The Worker catalog maps `{agent_version, app_namespace, persistence_profile}` to a cached Runner or Runner factory. Document whether Runner and its configured providers are safe for simultaneous task invocations in the pinned version; if yes, share by `Arc`. Otherwise instantiate isolated runners while sharing safe ADK services. Never wrap all executions in a global mutex.

ADK documents `Runner::interrupt_identity(app_name,user_id,session_id)` for accurate cancellation. Prefer that over broad `interrupt(session_id)`, which may affect another caller with the same session identifier. Runner tracks concurrent identities; the Worker task registry still associates specific attempt IDs and cancellation token instances. If multiple active invocations share an identity, worker-level per-attempt cancellation must be proven or disallow concurrent turns for that identity. Runner's `ToolExecutionStrategy::Auto` can parallelize *safe read-only* tools; never force `Parallel` for arbitrary filesystem writes or browser control. See https://adk-rust.com/en/docs/core/runner.

## 15. Sessions and interaction identity

The Worker owns conversational sessions through ADK `SessionService`. A session identity includes `{application_namespace, authenticated_principal, session_id}`. Session IDs are opaque and client-generated values must be validated against authorization. Distinguish one session containing many turns from one execution request that may run long and emit thousands of events. Default policy: at most one active writing turn per session; distinct sessions can execute concurrently. Optionally permit concurrent read-only view operations, but test history consistency. Namespaces should map to an arbitrary application identifier, not Goldenbee business-specific entities.

Session APIs cover create, retrieve, list, append turn, close/archive, and retention/erasure where supported by ADK storage. Public session management routes can wrap the ADK server's existing routes rather than creating new duplicate ones. Streaming clients must supply the last received cursor to resume viewing; reconnecting must never implicitly submit the prompt again. Sensitive sessions require encryption at rest, tenant-scoped deletion and access audit. `SessionService` implementations should be reused directly, with only a small identity adapter if caller identity differs from ADK's native triplet.

## 16. Task and attempt semantics

The Worker-level *task* exists only where necessary to represent independently managed, potentially long-running execution beyond one streaming HTTP request. In A2A mode use the **ADK A2A task state machine and task store** as primary, and do not create a second competing A2A state machine. Non-A2A execution may use a slim generic `ExecutionHandle` mapped to ADK invocation, Graph thread or A2A task ID. Persist the mapping with acknowledged requests, attempts and completion envelope. Never duplicate the full ADK event history in a new table.

Suggested logical states for Goldenbee management projection: `accepted`, `queued`, `executing`, `input_required`, `paused`, `reconciling`, `succeeded`, `failed`, `cancelled`. These are **projection states**, not asserted ADK A2A wire enum values. State transitions require monotonic sequence and recorded cause. Re-submitting with the same caller and idempotency key must return the original handle if request hashes match; mismatching payload re-use returns conflict. A retry increments attempt and records new invocation ID, but preserves the original external task ID.

## 17. Concurrency, capacity and fairness

A shared Tokio process accepts many tasks and applies bounded concurrency through a small admission layer that is *not* an agent runner replacement. Resource classes include model-call budgets, active ADK Runner invocations, external tool subprocesses, local browsers, GPU-provider concurrency and workspace write locks. Use `Semaphore` and bounded queues; each admitted run holds a permit only while actually consuming active execution capacity. Awaiting human approval or offline dependencies should release slot claims while retaining state. A cancellation token cascades from root task through internal child invocations and remote delegations to the extent each protocol supports it.

Do not equate `max_concurrent_tasks` to safe parallel filesystem writes. On a shared repository, allocate separate Git worktrees or exclusive write leases. Tools may claim `is_read_only()` and `is_concurrency_safe()`; those flags must accurately describe implementation behavior before enabling ADK Auto parallel tool execution. Expose queue depth, running count, waiting count, per-agent saturation, tool semaphore saturation and provider rate limiting. Set finite queue lengths and return backpressure rather than unbounded memory growth. Fair scheduling can start FIFO with per-principal quotas; move to weighted fair scheduling only when measured starvation justifies it.

## 18. Native local multi-agent composition

Reuse ADK's built-in `LlmAgent` with `sub_agent` transfers for conversational handoffs; `AgentTool` where an agent invokes another as a tool and receives a result; `SequentialAgent` for static chains; `ParallelAgent` for independent same-input branches; `LoopAgent` for bounded iteration; `GraphAgent` for dynamic branching and restart-safe work; and ADK's portable team architecture/CompiledTeam for supervisor/router/hierarchical patterns, handoff/delegation policies and receipts. No custom orchestration interpreter is permitted for these intra-worker use cases. Upstream docs: https://adk-rust.com/en/docs/agents/multi-agent and https://adk-rust.com/en/docs/agents/graph-agents.

The worker-level `agent_kind` is only a selector for these preexisting implementations. Workers can advertise capabilities like `research`, `code`, `browser`, `verify`; the Master chooses a target based on descriptions and policy. An LLM may propose a specialist but deterministic permission/capability checks validate the actual call. Capture ADK team execution receipts and graph snapshots, rather than inventing an opaque proprietary plan format that discards native metadata.

## 19. Master Worker role

A Master Worker is a normal Worker process with at least one coordinating ADK agent and the right to invoke configured remote peers. It may host several local experts itself. An authorized external caller can select a single worker and submit directly, or choose a team with a designated Master and scoped subworker roster. The Master interprets the goal, proposes a plan, routes tasks, aggregates evidence, adapts strategy when results differ from expectations, and reports outcomes. Planning is an *agent capability*, not a global obligation for every request; direct specialist routing should avoid unnecessary coordinator tokens.

The Master must not become an unbounded recursively delegating agent. A delegation context includes root ID, parent ID, path, remaining depth, maximum fanout, total token/time/cost limits, capability allowlist, explicit authority, required result schema and trace context. Every child receives budget and rights no broader than its parent. Result acceptance is evidence-based: a text response describing success does not necessarily satisfy objective acceptance criteria. Record missing artifacts, unverified assertions and deployment uncertainty explicitly.

## 20. Remote subworker collaboration

ADK A2A is the default protocol between independent agent services when the native A2A implementation covers the needed operation. An agent card advertises authentic capabilities, endpoint, security schemes and supported modalities; clients negotiate and invoke through the ADK-provided protocol. A2A tasks and event subscriptions remain the source of truth for A2A interactions. The Master uses a small `PeerDirectory` to discover *authorized* cards and chooses remote targets; the actual send/stream/cancel transport is ADK's A2A client. Gateway may broker discovery and network reachability, but should not translate every tool call or text token into its own private worker RPC.

The official guide warns that in-memory A2A task stores/idempotency are unsuitable as a guarantee for long-running production work. Before release, verify a persistent task-store integration available in the pinned ADK version or implement only a documented store trait adapter; do not rebuild the A2A handler/state machine. Remote delegates may finish while the master is disconnected; on recovery query their task IDs and reconcile. Never replay a non-idempotent remote call merely because an SSE stream dropped. Official source: https://adk-rust.com/en/docs/deployment/a2a.

## 21. Durable graph workflows

Use `adk-graph` with native `SqliteCheckpointer` for branching, checkpointing, human approval interruption and restart-resumable graph state. A checkpoint saves graph state and frontier, not a universal transaction across external systems. If a node was in progress when interrupted, it may execute again on resume; side-effecting tools need deduplication or reconciliation. Time-travel functions are primarily inspection/fork facilities; never describe them as undoing deployments, emails, payments or filesystem edits. Prefer normal workflow agents for static sequences and GraphAgent only where conditional routing/durability is needed. See https://adk-rust.com/en/docs/agents/graph-agents.

Graph designs should include explicit state channels for objective, worker assignments, evidence references, review verdict and outstanding approval gates. Use strict channel validation if supported in pinned release; cap recursion/super-steps and retry attempts. Subgraphs should have matching checkpointers when interruption can happen inside them. Design graph nodes to be individually testable, include an output schema and record external idempotency keys. Persistent graph threads carry unique task IDs and tenant-scoped context.

## 22. Tools and tool execution

Prefer ADK `#[tool]` / `FunctionTool` for simple, typed native Rust tool implementations, built-in tools where available, `McpToolset` for external capabilities, `McpServerManager` for dynamic local stdio MCP connections, and existing browser/devtools/code tools for their respective domains. Do not create a second general tool-schema registry or JSON tool-call loop. All custom tool wrappers should represent *new capabilities* absent from ADK and integrate into ADK's Tool trait. Treat input validation, canonical path checking, allowed side effects, credentials and approval as policy outside prompt instructions.

Tool metadata must accurately distinguish read-only, concurrency-safe and long-running operations. For operations with mutable or external effects, establish an operation ID before dispatch and persist a receipt after response. A returned error might indicate definite failure, transient failure, or **unknown outcome**; do not automatically retry ambiguous writes. Link evidence to the corresponding ADK event, tool invocation, trace and artifact reference. For terminal/browser/file access, use ADK's available implementations and add external sandbox containment when security requires it.

## 23. Coding tools and workspace lifecycle

ADK's `CodingAgent` wraps LlmAgent and wires workspace-scoped `read_file`, `write_file`, `edit_file`, `glob`, `grep`, `bash`, plus `write_todos`. Use it instead of creating another coding agent harness. Worker-owned adapters provision workspace roots, immutable input snapshots, separate write worktrees, per-task quotas and cleanup. A workspace path is a locator, not an authorization token: check canonical path beneath an approved root, avoid symlink breakout, and secure subprocess environment. If ADK's workspace restriction is not OS sandboxing on a particular target, place untrusted shell commands behind a process/container isolation policy. Official source: https://adk-rust.com/en/docs/coding-agent/harness.

The default agent loop can explore/plan/edit/test. Goldenbee adds quality policy: inspect project instructions, implement all meaningful scenarios rather than converting alternatives into user-facing errors, validate typecheck/test/lint/e2e as appropriate, and report verified vs unverified results. Resource contention matters for Rust builds; distinct `CARGO_TARGET_DIR` per independent worktree or a build coordinator is safer than blindly parallel cargo commands in a shared checkout. Use ADK existing multi-review workflows for parallel code review instead of a custom reviewer engine.

## 24. Browser, terminal, computer use and sandbox

Start with ADK browser tools and official sandbox/code-execution facilities. Browser automation often involves separately running WebDriver/Chromium processes, not embedding them in the Worker binary. The Worker owns browser session authorization, connection lifecycle, observability, timeout and human takeover coordination. If a live browser streaming/control feature is not provided by ADK, record it as an explicit external integration gap; do not silently describe it as native. Terminal sessions use constrained PTYs/subprocesses outside the Rust agent loop. Shared human/agent control requires an exclusive lease with fencing; stale agent commands must be rejected after human takeover.

Risk-based isolation profiles: `read_only`, `workspace_write`, `sandboxed_exec`, `external_network`, `privileged_operation`. The actual execution mechanism may use Bubblewrap, namespaces, containers or remote executors depending on platform. Never interpret model instructions as permission to expand sandbox boundaries. Each profile defines permitted mounts, filesystem roots, network destinations, environment variables, command duration and maximum output. For privileged operations, require external authorization and immutable audit records. ADK tool policy/guardrail hooks should perform the gate nearest to the tool.

## 25. Models and provider routing

Use ADK model abstractions/providers for Gemini/OpenAI/Anthropic/Ollama and any providers verified in the pinned release. Local inference can be served through Ollama or a compatible endpoint. Provider configurations specify logical model ID, capabilities, context limits, concurrency/rate policy, credential reference, fallback policy and deployment locality. Do not write another LLM protocol client when ADK already ships one. Choose models by capabilities and cost only when the provider adapter supports the required modality/tool schema, and ensure the selected model's tokenizer/context behavior is validated for oversized inputs.

A single worker can host heterogeneous agents with different model providers, but the execution's provider choice must be recorded. Model fallback is not allowed if it violates residency, data handling, cost or tool-support restrictions. Token usage uses provider-reported metrics wherever ADK exposes them; missing counts remain `unknown`, not guessed as zero. Prompt caching follows provider integration behavior; avoid duplicating experimental Runner cache logic without proof. Rate limits should backpressure task admission and produce a meaningful retry-after signal.

## 26. Memory, knowledge and RAG

Use ADK `MemoryService` and `GraphMemoryService` for long-term agent memory, and built-in RAG integration for retrieval. Available backends documented include in-memory, SQLite, Postgres/pgvector, Redis, MongoDB, Neo4j and the separate bi-temporal graph memory service, with differing search semantics. The SQLite memory backend documents **keyword** retrieval; do not claim it is a native `sqlite-vec` semantic backend. If vector retrieval specifically requires `sqlite-vec`, first check ADK's `MemoryService`/RAG extension interfaces and implement only a storage adapter under a documented ADR; do not fork the full retrieval pipeline. Official source: https://adk-rust.com/en/docs/memory/backends.

Separate: session history, persistent semantic memory, document repository, artifact storage, graph relations and transient graph execution state. These differ in lifecycle and permissions. Every stored memory includes provenance, scope, timestamps, retention and deletion rights. Retrieval must filter by authorized namespace *before* result exposure and must not leak cross-tenant chunks through cached embeddings. User-approved memory updates should be validated rather than blindly self-curated. Apply token budget and source citation requirements at agent construction and testing levels.

## 27. Artifacts and large outputs

Use ADK ArtifactService for agent artifact create/get/list/version behaviors available in the installed release. The Worker API should return opaque artifact IDs, content type, size, checksum, ownership and time; large payloads are fetched or streamed separately, never pasted into SSE events. Support immutable finalized artifact versions and explicit retention. External shared object stores can be mounted through a documented ArtifactService implementation when appropriate; avoid duplicating an artifact subsystem in Gateway.

Artifact references passed between workers must be authorized, signed/short-lived or proxied, and independent of local machine file paths. Cross-machine execution cannot presume a shared filesystem. Use provenance `{source_worker, task, invocation, tool, digest}` and verify downloaded content. Result bundles should enumerate artifacts, verification status, errors and warnings. The Gateway stores metadata projections and access grants only, unless configured as a controlled relay.

## 28. Events and streaming

ADK Runner streams typed events; the Worker wraps only the stable metadata needed for cross-process correlation. The Gateway should adapt and forward/replay events, not recreate agent semantics. Distinguish **persistent domain events** (`task accepted`, `approval required`, `result ready`) from **ephemeral stream fragments** (partial model tokens, browser frame updates). Durable events have `{event_id, stream_id, stream_seq, producer_incarnation, occurred_at, traceparent, payload_version}`. Sequence is guaranteed only **within the declared stream**; global ordering between workers is not guaranteed. A Gateway projection uses worker ID + stream ID + sequence/id as dedupe key.

SSE: `id` corresponds to a cursor that maps to a retained event; `Last-Event-ID` resumes a stream. If requested history was compacted/expired, respond with explicit cursor-expired guidance and snapshot fallback. WebSocket is for duplex control, live input, binary streams and takeover; it also requires acknowledgment, heartbeat and backpressure. Token streams may be lossy/recoverable from persisted transcript or final answer, whereas approval decisions and task completions must be durably journaled. Use ADK telemetry for detailed traces rather than flooding all clients with per-token spans.

## 29. Approvals and human-in-the-loop

Prefer ADK GraphAgent interruptions and available approval/tool-confirmation facilities. Define policy decisions outside the model: if a tool requests protected operation, the runtime must pause or reject before execution. Approval request fields: task ID, exact operation digest, principal, capability, risk, human-readable effect, expiration, allowed decision types and audit correlation. Approval decision must match the operation digest; changed arguments invalidate old approval. Resume the native graph/checkpoint with the authorized decision. Revocation, expiry or target mismatch must fail closed.

Human questions are distinct from approvals: a question asks for data; an approval grants a bounded action. Client authentication and Gateway ownership checks apply to both. Ensure delegated authority does not allow a subworker to appoint itself an approver or bypass mandatory controls. The UI may represent approval requests, but no UI-specific schema or business rules live inside Worker core. A CLI can satisfy the same decision protocol as a web app.

## 30. Runtime recovery

Recovery sequence: validate local state and schema; load immutable agent definitions; restore ADK sessions and graph checkpoints; reconcile external A2A tasks and side-effect receipts; classify tasks into completed, resumable, retryable, blocked/unknown; reacquire resource slots; resume only safe work; publish a reconciliation report. The pending execution registry is worker-level bookkeeping and must not replace ADK checkpoints or A2A task lifecycle. Any operation with unknown external outcome requires probing its idempotency/status API or human review before resubmission. Unexpected restart never means automatic fresh execution of every pending node.

The Gateway follows similar projection reconciliation: if worker lease expires mark `unreachable` without altering authoritative task state. On return, request a snapshot and replay events since last cursor. A Master recovery queries delegated task handles from subworkers; it does not recreate children unless confirmed absent and safe. Drain shutdown flushes event journal and waits for bounded completion/checkpoint with timeout. A kill -9 experiment is a mandatory acceptance test.

---

# Part IV — Agent Gateway Design

## 31. Gateway boundary and topology

The Gateway is a **headless domain-neutral control and routing plane**, not a second ADK Runner and not a business server. It may sit between clients and workers, supply team registries, manage secure worker identities, and fan out event streams. Agent-to-agent work can use direct A2A paths where mutually reachable or an explicitly controlled relay where not. A successful gateway deployment does not need to preload any LLM weights. Worker registration is an administrative action that validates endpoint identity and effective capabilities; it must not trust arbitrary agent cards or self-declared privileges without policy.

Gateway modules: `registry`, `connectivity`, `team_catalog`, `routing`, `session_proxy`, `execution_index`, `event_relay`, `approval_router`, `authorization`, `credential_broker`, `audit`, `telemetry`, `api`, `persistence`. Each module is an interface and an implementation, not a new framework. Prefer ADK A2A client/server where relevant and Gateway-only REST for admin metadata. The Gateway knows **which worker**, **which team**, and **which task handle**; it does not need to deserialize every ADK-internal agent event to drive orchestration decisions.

## 32. Worker registration and discovery

A worker registers its stable `worker_id`, instance incarnation, endpoint(s), agent card(s), software version, protocol versions, feature flags and resource envelope. Gateway authenticates the identity (mTLS/workload token/API key as appropriate), validates that the endpoint's claimed identity matches the connection, and stores its last-known capability snapshot. A worker lease has bounded TTL and frequent renewal, with clocks handled server-side to avoid remote clock trust. The gateway should mark an expired lease `unreachable`, **not `dead`**, and require reconciliation on reconnect.

Discovery returns only workers visible to a caller and matching requested capability, team, protocol, data-locality and health constraints. Separate discovery descriptors from live capacity; an agent card is not proof that the model server is responsive. For a managed Worker reachable only outbound, use secure tunnel/reverse connection or relay and clearly record this deployment mode. Avoid depending on Kubernetes: local child processes, systemd workers, containers and remote machines all use the same identity contract.

## 33. Teams and master assignment

A `Team` has `team_id`, version, display name, master worker ID, subworker member list, role/capability bindings, delegation policy, allowed communication routes and lifecycle state. It is a *configuration resource*, not a process. A worker can join multiple compatible teams, but an execution must snapshot one team version. Team membership changes apply to new tasks or controlled plan revisions; do not mutate the delegated permissions of an already running child by overwriting a shared roster in place. The Master may access only effective authorized members and their approved capabilities.

Team setup validates: master is registered and capable; each member identity is resolvable; no prohibited self-delegation; policy grants sufficient but bounded rights; incompatible tenancy/locality is rejected; max members/fanout/depth are configured. Role definitions are descriptive and do not grant rights independently. To build a hierarchy, authorize subworker delegates and cap depth. Active task delegation graphs must reject cycles even where teams contain shared workers. ADK portable-team templates and receipts should implement **internal** agent teams; Gateway team resources represent **process-level** worker membership.

## 34. Routing and admission

A caller can specify `target.worker_id`, `target.team_id` or `target.capability`. Explicit worker targets should route directly after permission and availability checks. A team target routes to its designated Master, with the resolved team snapshot included in the invocation context. Capability routing selects a permitted worker according to explicit policy (health, configured provider, locality, load, priority). Never silently redirect a privileged request to a less trusted worker. Record routing decision and rationale in audit metadata.

Submission is at-least-once over unreliable networks; exactly-once network delivery is not guaranteed. Use idempotency key + canonical payload hash at the Gateway and at the executing Worker's supported submission boundary. After timeout, query the target task using the same ID before retrying. Client receives a stable external `execution_id` and status URL; Gateway maps this to concrete worker/A2A task IDs. If Gateway cannot confirm acceptance, return an `unknown_submission` status to be reconciled rather than generating an untracked second execution.

## 35. Session routing

A client session is a Gateway-level address and authentication context. It maps to one ADK Worker session (or a Master-facing session) plus endpoint ownership. A direct session binds to a selected worker; a team session binds to the Master and the versioned team. Client disconnection must not delete session state. Messages have client-side idempotency keys to avoid duplicates on retries, and turn ordering per session is enforced. Gateway does not reconstruct ADK chat history from partial SSE; it fetches canonical session history from worker when needed.

Support `POST /v1/sessions` and `POST /v1/sessions/{id}/messages` only as Gateway-specific contracts if ADK server routes don't already satisfy them; otherwise proxy ADK's routes while providing a versioned mapping. Each client request includes caller identity resolved from token, request context and desired access scope. Cancellation targets the current task attempt/handle; there is no unsafe global cancel-by-session shortcut.

## 36. Event relay and projections

The Gateway's event store is a *projection/replay layer*, not duplicate execution authority. Consume worker/A2A streams, attach standardized external envelopes, persist important domain transitions before acknowledging to downstream subscribers, then publish through SSE or WS. Reconnect using worker-specific cursor and reconcile any gap using canonical status/history. Large partial output can be stored in rolling buffers or canonical transcript references to avoid unbounded storage. A worker may produce several independent streams; aggregate dashboards cannot assume a globally ordered chronology.

Data retention policy distinguishes compliance audit, operational transitions, chat transcripts, streaming tokens, browser frames and telemetry. Persist durable categories with configurable TTL; do not retain raw secrets. Gateway restart loads event cursor and projections, establishes worker subscriptions and dedupes replay. A reconnect race must be tested under slow consumers and malicious invalid cursors. Prefer protocol-standard A2A streaming and subscriptions over inventing a remote-agent streaming protocol.

## 37. Gateway policy enforcement

The Gateway authenticates caller identities, checks access to target workers and teams, applies quotas/routing constraints and produces scoped delegation claims for Workers. The Worker **revalidates** claims at execution and tool boundaries; neither Gateway nor ADK model prompt alone is sufficient. An external business app can supply trusted subject claims only through a configured identity-federation/impersonation model, not arbitrary JSON. Gateway audits team changes, worker registration, approvals, credential grants and cancellations.

Use resource-scoped policies: `worker:read`, `worker:manage`, `team:write`, `session:start`, `execution:submit`, `execution:cancel`, `artifact:read`, `approval:decide`. Principals can be humans, applications or service identities. Capability tokens should be short-lived, audience-bound, non-forwardable except along explicit delegation chain, and limited to exact task/tool scopes. Avoid coupling to specific business role titles.

## 38. Failure handling

If Gateway is down, workers keep running existing authorized executions according to their lease/policy validity; remote A2A peer communication may continue if topology permits. New client submissions through Gateway fail clearly rather than queue invisibly. If worker is unreachable, Gateway marks that routing target unavailable and does not infer task failure. If Master fails, Gateway can expose state and pending child task IDs, but must not unilaterally select a replacement Master unless an explicit failover policy and safe task ownership transfer are defined. If event streaming breaks, clients reconnect with durable cursor rather than resubmit a task.

## 39. Gateway storage

Persist only Gateway-owned data: worker identities and endpoint leases, team manifests and versions, authorized memberships, external-to-worker task mapping, idempotency keys, client-facing session mapping, cursor/replay projection, approval routing metadata, audit entries and configured service tokens. Do not store complete worker model prompts, mutable tool states, ADK graph checkpoint internals or model embeddings. A single-node Gateway can begin with SQLite; HA topology will require a database and leader/lease model appropriate to multi-instance coordination. Do not claim SQLite local files form a safe distributed shared database.

---

# Part V — Public Interfaces and Protocol Contracts

## 40. Contract philosophy

Use native ADK server and A2A protocols wherever they fully meet the requirement. The **Goldenbee Gateway API** is an additional control-plane contract for worker registration, team configuration, cross-worker routing and normalized client sessions; it is not a replacement for ADK A2A or Agent Server. Public contracts are language-neutral and versioned, using OpenAPI 3.1 plus JSON Schema. API surface names in this proposal are *proposed*; freeze them after comparing with exact ADK native endpoints to eliminate duplicates. Every route specifies authentication, authorization, input schema, response status, idempotency, pagination, rate limit and errors.

## 41. Authentication and transport

Production TLS is mandatory for remote APIs. Support workload identities for worker-to-gateway and worker-to-worker channels; client auth may use OIDC/OAuth2 access tokens, service tokens or local restricted loopback tokens. `Authorization: Bearer` is appropriate for HTTP, while WebSocket auth should be established securely during handshake or via a short-lived one-use ticket; never log tokenized query URLs. CORS is explicit and restrictive. Request IDs and W3C `traceparent` propagate across hops. Sensitive write routes require CSRF defenses when accessed with browser cookies; bearer-token APIs use appropriate origin and token safeguards.

## 42. API naming and patterns

- Base `/v1` for Goldenbee-specific routes; maintain protocol version separately from `adk-rust` crate version.
- Resource identifiers are opaque UUIDv7/ULID-like values; document accepted format and never treat source-provided IDs as trusted identity.
- `GET` is safe/idempotent; mutating `POST` commands require `Idempotency-Key` where a retry can occur.
- Use `202 Accepted` for asynchronous execution start, `201 Created` for immediately persisted resources, `409 Conflict` for mismatched idempotency or concurrent version update, `422` for semantic request errors, `429` and `Retry-After` for capacity.
- Prefer `ETag`/`If-Match` for team and agent catalog version updates.
- Cursor pagination rather than offset for growing worker/event lists.
- API errors return stable machine-readable codes; avoid exposing raw LLM/provider errors to users.
- Large artifact bytes are separate resources with signed access or authenticated proxy.

## 43. Proposed Gateway REST catalog

| Method | Route | Purpose | Scope |
|---|---|---|---|
| GET | `/v1/health/live` | Liveness | Public restricted/ops |
| GET | `/v1/health/ready` | Readiness | Ops |
| GET | `/v1/capabilities` | Protocol and feature negotiation | Read |
| GET | `/v1/workers` | Authorized list, filter by capability/status | worker:read |
| POST | `/v1/workers` | Register managed worker | worker:manage |
| GET | `/v1/workers/{worker_id}` | Get worker descriptor and status | worker:read |
| PATCH | `/v1/workers/{worker_id}` | Change approved endpoint/config | worker:manage |
| DELETE | `/v1/workers/{worker_id}` | Decommission registration safely | worker:manage |
| POST | `/v1/workers/{worker_id}/heartbeat` | Authenticated lease renewal | worker:self |
| GET | `/v1/teams` | List visible teams | team:read |
| POST | `/v1/teams` | Create master/subworker team | team:write |
| GET | `/v1/teams/{team_id}` | Fetch versioned manifest | team:read |
| PUT | `/v1/teams/{team_id}` | Update team with If-Match | team:write |
| POST | `/v1/teams/{team_id}/activate` | Validate/activate team | team:write |
| POST | `/v1/teams/{team_id}/deactivate` | Stop new assignments | team:write |
| POST | `/v1/sessions` | Create target-bound session | session:start |
| GET | `/v1/sessions/{session_id}` | Get projection and worker binding | session:read |
| POST | `/v1/sessions/{session_id}/messages` | Submit user turn | session:write |
| GET | `/v1/sessions/{session_id}/events` | Event SSE with replay | session:read |
| POST | `/v1/executions` | Submit direct/team objective | execution:submit |
| GET | `/v1/executions/{execution_id}` | Execution snapshot | execution:read |
| GET | `/v1/executions/{execution_id}/events` | Durable execution events | execution:read |
| POST | `/v1/executions/{execution_id}/cancel` | Request cancellation | execution:cancel |
| POST | `/v1/executions/{execution_id}/resume` | Resume checkpointed task | execution:resume |
| GET | `/v1/executions/{execution_id}/children` | Delegation overview | execution:read |
| GET | `/v1/approvals` | Pending authorized requests | approval:read |
| POST | `/v1/approvals/{approval_id}/decision` | Human decision with digest | approval:decide |
| GET | `/v1/artifacts/{artifact_id}` | Artifact metadata | artifact:read |
| GET | `/v1/artifacts/{artifact_id}/content` | Artifact stream/proxy | artifact:read |
| GET | `/v1/events` | Global authorized event feed | audit:read |
| GET | `/v1/metrics` | Export ops metrics | Ops-only |

This catalog is a **design target**: before coding, review every route against ADK `adk-server` and A2A native endpoints and mark **native/proxy/new** in the OpenAPI source. Do not implement a clone of existing ADK session or A2A task routes merely to match the table.

## 44. Worker standalone API

A Worker exposes ADK's native REST/A2A server according to release capabilities and only the missing Worker-specific management endpoints: worker identity/capability snapshot, health, safe agent catalog browsing, runtime drain/status, and optional direct-task command adapter. Standalone CLI may call the Runner in-process without network. When Gateway-managed, restrict management APIs to Gateway/service identities. Worker A2A agent cards must advertise only supported capabilities; long-running task state requires persistent store rather than default in-memory prototype store. Bind loopback by default and require explicit configured listeners for remote access.

## 45. Example: execution submission JSON

```http
POST /v1/executions HTTP/1.1
Authorization: Bearer <redacted>
Content-Type: application/json
Idempotency-Key: req-2026-001
Traceparent: 00-11111111111111111111111111111111-2222222222222222-01
```

```json
{
  "target": { "type": "team", "id": "team-engineering", "version": 3 },
  "input": { "kind": "text", "text": "Implement a documented REST service and verify with tests" },
  "context": { "namespace": "my-application", "workspace_ref": "workspace-42" },
  "policy": { "approval_mode": "sensitive_only", "deadline_seconds": 3600 },
  "metadata": { "client_reference": "issue-127" }
}
```

Response (`202 Accepted`; this is a Goldenbee-defined contract, not the ADK A2A task wire type):

```json
{
  "execution_id": "exe_01JTESTABC",
  "state": "accepted",
  "target": { "worker_id": "worker-master-1", "team_version": 3 },
  "links": {
    "self": "/v1/executions/exe_01JTESTABC",
    "events": "/v1/executions/exe_01JTESTABC/events"
  }
}
```

**Validation cases:** absent target; team not visible; mismatched team version; unsupported modality; duplicate idempotency key with changed body; exhausted tenant quota; timeouts before worker acknowledgment; client disconnect during submission; expired authorization; invalid metadata exceeding allowed size. Each case gets a stable protocol error rather than a fictional success state.

## 46. Proposed normalized event schema

```json
{
  "id": "evt_01JTESTDEF",
  "schema_version": "1.0",
  "source": { "worker_id": "worker-2", "instance_id": "boot-17" },
  "stream": { "id": "exe_01JTESTABC", "sequence": 19 },
  "context": { "execution_id": "exe_01JTESTABC", "task_id": "task-7", "attempt": 1 },
  "type": "tool.completed",
  "occurred_at": "2026-11-17T10:00:00Z",
  "payload": { "tool_name": "bash", "status": "succeeded", "artifact_ref": "art-57" },
  "traceparent": "00-11111111111111111111111111111111-2222222222222222-01"
}
```

Do not flatten all ADK events into strings. Preserve an `adk_event` typed/passthrough payload where safe and documented, and use a small normalized envelope for routing and indexing. Client event types should include `execution.accepted`, `execution.started`, `agent.transfer`, `delegation.created`, `delegation.updated`, `approval.requested`, `approval.decided`, `tool.started`, `tool.completed`, `artifact.created`, `execution.completed`, `execution.failed`, `execution.cancelled`, `worker.status_changed`. Some event types are **Goldenbee-derived projections**, not necessarily native ADK event discriminants.

## 47. SSE specifics

```http
GET /v1/executions/exe_01JTESTABC/events
Authorization: Bearer <redacted>
Accept: text/event-stream
Last-Event-ID: evt_01JTESTXYZ
```

```text
event: execution.progress
id: evt_01JTESTDEF
data: {"id":"evt_01JTESTDEF","stream":{"id":"exe_01JTESTABC","sequence":19},"type":"execution.progress","payload":{"message":"Tests running"}}

```

Use keepalive comments, server-side bounded buffering and explicit cursor errors. A new subscriber receives a snapshot marker and retained events, then live events, with no gap at the handoff. Test reconnection during task completion, worker restart, Gateway restart, slow consumer, token refresh, event retention expiration, and network partition.

## 48. WebSocket contract

WebSocket is an optional duplex interface for chat, control, realtime audio/video/terminal proxy where enabled. It is not the sole source of durable state. Use `wss://.../v1/ws`, authenticated via short-lived ticket or approved header mechanism. Frames are typed: `hello`, `subscribe`, `unsubscribe`, `send_message`, `cancel`, `approval_decision`, `ack`, `ping`, `pong`, `event`, `error`, `snapshot`, `resync_required`. Every command carries command ID and task/session scope. Explicitly configure maximum frame size, idle timeout, heartbeat interval and send queue. Binary high-bandwidth streams require their own channels and flow control.

```json
{
  "type": "subscribe",
  "command_id": "cmd-101",
  "resource": { "type": "execution", "id": "exe_01JTESTABC" },
  "cursor": "evt_01JTESTXYZ"
}
```

The gateway returns an acknowledgment containing negotiated protocol version and whether replay was possible. Never assume a socket reconnect is equivalent to session resumption; callers must supply stable session/execution identifiers.

## 49. A2A and MCP wire boundaries

A2A is the default remote **agent** communication protocol. Worker Gateway discovery can expose or proxy compatible agent cards, but does not invent competing task semantics. MCP is the external **tool/capability** protocol; it is not a replacement for A2A task ownership. ACP is optional editor/coding-agent protocol. Keep the protocol adapters separate and validate authenticity, origin, role, and transport security at each boundary. Preserve upstream version negotiation rather than remapping every field to a simplified private schema.

## 50. CLI contract

```text
goldenbee worker run --config worker.toml
goldenbee worker doctor
goldenbee gateway run --config gateway.toml
goldenbee gateway doctor
goldenbee workers list --gateway <url>
goldenbee workers inspect <id>
goldenbee teams create --file team.json
goldenbee teams show <id>
goldenbee session start --worker <id>
goldenbee session start --team <id>
goldenbee task submit --team <id> --text "..."
goldenbee task watch <execution_id> --resume
goldenbee task cancel <execution_id>
goldenbee approvals list
goldenbee approvals decide <id> --approve --digest <digest>
```

This is a **proposed command grammar**; prioritize ADK-provided `cargo adk`/launcher commands for internal agent scaffolding and running rather than rebuilding their existing functionality. The Goldenbee CLI primarily manages Gateway resources and cross-process operation. Return machine-readable JSON mode, meaningful nonzero exit codes, and a redaction-safe verbose mode.

---

# Part VI — Repository, Rust Interfaces and SOLID Engineering Standard

## 51. Cargo workspace design

The workspace is modular to allow the Worker and Gateway to build independently and to prevent the core domain from importing HTTP, SQLite or ADK implementations. This is a *target* layout; begin with a minimal vertical slice and add crates only when a dependency boundary justifies the split. Avoid a crate per trivial struct or a large single `common` dumping ground.

```text
goldenbee/
├── Cargo.toml                         # workspace members, lint profiles
├── Cargo.lock                         # committed reproducible lock
├── rust-toolchain.toml
├── .cargo/config.toml
├── Makefile / justfile
├── .github/workflows/
│   ├── check.yml
│   ├── integration.yml
│   ├── security.yml
│   └── release.yml
├── apps/
│   ├── agent-worker/
│   │   ├── Cargo.toml
│   │   └── src/{main.rs,bootstrap.rs,commands.rs}
│   ├── agent-gateway/
│   │   ├── Cargo.toml
│   │   └── src/{main.rs,bootstrap.rs,commands.rs}
│   └── goldenbee-cli/
│       ├── Cargo.toml
│       └── src/{main.rs,command_tree.rs,output.rs}
├── crates/
│   ├── harness-contracts/             # DTOs, typed IDs, errors, JSON schemas
│   ├── harness-config/                # configuration validation
│   ├── harness-security/              # claims, policy interface, decisions
│   ├── worker-domain/                 # domain-neutral worker/task metadata
│   ├── worker-runtime/                # supervisor, admission, task mapping
│   ├── worker-adk/                    # ONLY adapter that knows native ADK types
│   ├── worker-api/                    # standalone control HTTP adapter
│   ├── worker-persistence/            # minimal metadata/outbox adapter
│   ├── gateway-domain/                # registration, team, route semantics
│   ├── gateway-application/           # orchestration use cases
│   ├── gateway-api/                   # HTTP/SSE/WS interface
│   ├── gateway-persistence/           # Gateway-owned tables
│   ├── gateway-connectivity/          # ADK A2A clients, peer discovery
│   ├── sdk-rust/                      # typed API client
│   └── test-support/                  # fixtures, fake providers, test clocks
├── schemas/
│   ├── openapi/gateway-v1.yaml
│   ├── jsonschema/{worker,team,execution,event}.json
│   └── compatibility/
├── configs/
│   ├── worker.example.toml
│   ├── gateway.example.toml
│   └── team.example.json
├── docs/
│   ├── architecture/
│   ├── adr/
│   ├── adk-compatibility.md
│   ├── api/
│   ├── security/
│   ├── operations/
│   ├── testing/
│   └── runbooks/
├── examples/
│   ├── standalone-worker/
│   ├── direct-gateway/
│   ├── master-two-workers/
│   ├── durable-graph-approval/
│   └── mcp-and-a2a/
└── tests/
    ├── contracts/
    ├── cross-process/
    ├── disaster/
    └── evaluation/
```

The `worker-adk` crate should not copy ADK data types into parallel mutable implementations. It constructs ADK components, translates public DTOs at the API edge, and preserves original ADK event content where allowed. Never place `adk_rust::*` imports in `harness-contracts`, `gateway-domain`, or user-facing Rust SDK. `worker-domain` is a domain-neutral coordination model, not a re-implementation of ADK Agent and Runner traits.

## 52. Direction of dependencies

```text
                   SHARED PUBLIC CONTRACTS
                            |
              +-------------+-------------+
              |                           |
              v                           v
+-------------------------+  +-------------------------+
| Gateway domain / app    |  | Worker domain / runtime |
+------------+------------+  +------------+------------+
             |                            |
             v                            v
+-------------------------+  +-------------------------+
| Gateway ports           |  | Worker ports            |
+------------+------------+  +------------+------------+
             ^                            ^
             |                            |
    +--------+---------+        +---------+---------+
    |                  |        |                   |
+---------+    +-------------+ +---------------+ +-------------+
| Storage |    | ADK A2A     | | ADK adapter   | | Worker      |
| adapter |    | connectivity| | (native reuse)| | metadata DB |
+---------+    +-------------+ +---------------+ +-------------+

Note: adapters implement ports; domain modules do not import ADK types.
```

High-level application logic depends on small domain ports; adapters depend inward on ports. The composition root creates concrete instances and injects trait objects/generic implementations. Do not require the domain layer to know an ORM entity, web request, protobuf message or crate-specific ADK Runner configuration. Use feature flags for optional costly functionality rather than building every provider into every executable.

## 53. SOLID in idiomatic Rust

**S — Single Responsibility:** `WorkerRegistry` handles identities and capability snapshots, not event streaming or billing; `TaskAdmissionService` handles resource availability, not agent reasoning; `A2aTaskClient` handles protocol mechanics, not policy decisions. **O — Open/Closed:** add model providers, transport adapters or metadata stores via trait implementations/configuration instead of editing one enormous enum match. **L — Liskov Substitution:** a fake `WorkerTransport` must fulfill the same submission, error and idempotency contract as remote A2A adapter; avoid surprising unsupported behavior hidden behind generic success types. **I — Interface Segregation:** separate `WorkerDirectoryReader` from `WorkerRegistrar`, `ExecutionSubmitter` from `ExecutionCanceller`, and `ArtifactReader` from `ArtifactWriter`. **D — Dependency Inversion:** orchestration use cases depend on trait ports, while Axum, SQLite and ADK adapters implement them.

**OOP-like Rust style:** use encapsulated structs for services, private mutable fields, constructor validation, `Arc<dyn Trait + Send + Sync>` for dynamic collaborators, small domain objects with invariants, and explicit lifecycle methods. Avoid Java-style factories/inheritance trees that fight Rust's ownership model. Use `new`/`try_new` constructors with immutable configuration; keep orchestration state machines in dedicated modules. Public getters should not expose mutable internal maps. Domain errors should be enums, with a stable external error-code adapter.

## 54. Interface inventory

| Port/interface | Primary methods | Adapter | Ownership |
|---|---|---|---|
| `AgentCatalog` | list, resolve, publish_version | ADK agent composition | Worker |
| `InvocationRunner` | invoke, cancel, active | ADK Runner adapter | Worker |
| `WorkerTaskIndex` | reserve, bind_handle, get, reconcile | Local lightweight DB | Worker |
| `SessionResolver` | create, resolve, archive | ADK SessionService | Worker |
| `ArtifactAccess` | metadata, fetch, store | ADK ArtifactService | Worker |
| `PeerDirectory` | list_authorized, resolve | Local config or Gateway | Worker |
| `RemoteDelegator` | submit, subscribe, cancel | ADK A2A client | Worker |
| `PolicyEvaluator` | authorize, explain | ADK auth + policy adapter | Both |
| `WorkerDirectory` | register, renew, find | Gateway persistence | Gateway |
| `TeamRepository` | create, version, read, activate | Gateway persistence | Gateway |
| `RouteResolver` | resolve_target | Gateway application | Gateway |
| `EventProjection` | consume, cursor, replay | Gateway DB + relay | Gateway |
| `ApprovalDispatcher` | request, decide, status | Gateway + ADK HITL bridge | Gateway |
| `CredentialSource` | get_scoped_secret | Secret manager/env | Both |
| `Clock` | now, sleep? (test abstraction) | System/test clock | Both |

These port names are **Goldenbee design interfaces**. Only implement one when it creates a real testable separation. When an ADK trait directly satisfies a need, prefer depending on that trait in `worker-adk` instead of wrapping it with another abstraction for ceremony.

## 55. Sample object-oriented Rust service contract

```rust
use std::{future::Future, sync::Arc};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WorkerId(pub String);

#[derive(Clone, Debug)]
pub struct WorkerDescriptor {
    pub id: WorkerId,
    pub endpoint: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug)]
pub enum DirectoryError { NotFound, Unauthorized, Unavailable }

pub trait WorkerDirectoryReader: Send + Sync {
    fn get(&self, id: &WorkerId)
        -> impl Future<Output = Result<WorkerDescriptor, DirectoryError>> + Send;
}

pub struct ResolveWorkerUseCase<D> { directory: Arc<D> }
impl<D: WorkerDirectoryReader> ResolveWorkerUseCase<D> {
    pub fn new(directory: Arc<D>) -> Self { Self { directory } }
    pub async fn execute(&self, id: &WorkerId)
        -> Result<WorkerDescriptor, DirectoryError> {
        self.directory.get(id).await
    }
}
```

This example is a **self-contained design illustration**; it does not claim to compile against ADK APIs. `impl Future` trait methods are static/generic and not object-safe for heterogeneous trait objects; use `async_trait`, boxed futures, or an object-safe adapter when storing `dyn` values. The `WorkerId(String)` sample is deliberately minimal; production code needs constructor validation and typed serde. Avoid `unwrap()` and silent default in request handlers.

## 56. Error architecture

Separate: `DomainError` (invalid state, permission, version conflict), `TransportError` (timeout, TLS, connection), `ProviderError` (model quota, outage), `ExecutionError` (tool, agent, checkpoint), `StorageError` (disk full, conflict, corruption), `UnknownOutcome` (side effect dispatched but result unknown). The public adapter maps these to stable codes such as `WORKER_UNAVAILABLE`, `TEAM_VERSION_CONFLICT`, `TOOL_APPROVAL_REQUIRED`, `TASK_OUTCOME_UNKNOWN`. Logs carry detailed source errors and trace correlation; user-facing responses explain recovery options without dumping stack traces or transient internal state.

Failure should not become the default for an unhandled legitimate scenario. Before returning an error, enumerate expected alternatives and implement a meaningful state transition: wait, ask for input, try authorized fallback, change resource assignment, resume checkpoint, skip optional work with warning, or escalate. Errors are for genuine unrecoverable or invalid states, not a substitute for business-neutral execution scenarios.

## 57. Configuration contracts

```toml
# worker.example.toml — proposed Goldenbee config, not ADK's native format
[worker]
id = "worker-local-1"
mode = "standalone"  # standalone | managed
listen = "127.0.0.1:7801"
data_dir = "./.goldenbee/worker-1"

[adk]
agent_catalog = "./agents"
session_backend = "sqlite"
checkpoint_backend = "sqlite"
artifact_backend = "filesystem"

[limits]
max_active_invocations = 8
max_pending_tasks = 128
max_remote_delegation_depth = 3
max_child_tasks_per_parent = 16

[security]
default_profile = "workspace_read_only"
allow_remote_management = false

[telemetry]
service_name = "goldenbee-agent-worker"
```

```toml
# gateway.example.toml — proposed config
[gateway]
listen = "127.0.0.1:7800"
database_url = "sqlite://./.goldenbee/gateway.db"
public_base_url = "http://127.0.0.1:7800"

[discovery]
worker_lease_seconds = 30
max_workers = 500

[routing]
default_timeout_seconds = 30
require_idempotency_key = true

[events]
retain_critical_days = 30
max_subscriber_backlog = 1000

[auth]
mode = "local-dev" # production: configure OIDC or trusted service auth
```

Config resolution precedence: explicit CLI flags -> environment references -> selected TOML -> safe compiled defaults. Redact secrets when echoing config; pin `config_hash` in readiness. Validate path existence, URLs, policy contradictions, duplicate identities and resource limits before binding listeners. A running config may be reloaded only for explicitly reloadable fields; agent/model/tool changes use immutable versioning and controlled drain.

## 58. Dependency injection and composition root

Each binary has one bootstrap that selects concrete ports, initializes ADK native services, constructs domain services and starts transports. For example: `WorkerBootstrap::build(config, SecretProvider, Telemetry) -> WorkerApplication`. The bootstrap is allowed to know every adapter. `WorkerApplication` owns `Arc<InvocationService>`, `Arc<AgentCatalogService>`, `Arc<PeerConnectionManager>` and shutdown tokens. Dispose in reverse ownership order. Do not expose a global mutable service locator. In tests, inject deterministic fake WorkerDirectory, memory persistence and fake ADK provider responses to validate error and timing scenarios.

## 59. ADK composition code policy

Only include concrete ADK constructors copied and compiled from the **pinned 2.3.0 rustdoc/examples**. The canonical examples of `RunnerConfig`, `LlmAgentBuilder`, `GraphAgent`, `McpToolset`, `CodingAgent` and A2A components are in the official docs referenced in Part II. This document intentionally avoids inventing new ADK method names. The following is a **bootstrap pseudo-sequence**, not compiling Rust:

```text
validate_worker_config()
initialize_adk_telemetry()
open_adk_session_service()
open_adk_artifact_service()
open_adk_memory_service()
open_adk_graph_checkpointer_if_configured()
resolve_adk_model_providers()
resolve_adk_tools_and_mcp_toolsets()
construct_adk_agents_from_immutable_manifest_versions()
construct_adk_runners_with_real_services()
start_worker_admission_and_recovery()
serve_adk_agent_protocol_and_minimal_control_routes()
```

The verification obligation for every step is explicit in the implementation checklist and CI smoke crate. A copy/paste scaffold from `cargo adk new --template api` or `--template multi-agent` is a better starting point than a speculative API wrapper.

---

# Part VII — Persistence, Security, Deployment and Operational Reliability

## 60. Database ownership and SQLite policy

A Worker may use one filesystem directory but should not collapse unrelated ADK stores into a hand-crafted universal ORM. Keep native ADK session tables and graph checkpoint tables under their documented schemas and migrations. Store Gateway-only resources in its own database. If a worker-level metadata index is necessary, create a separate namespace/schema managed by Worker code. A tool-specific SQLite memory backend and `sqlite-vec` extension are **not interchangeable**: ADK's documented SQLite memory is keyword-based; vector search needs native ADK RAG implementation or an explicit compatible adapter.

For SQLite set WAL mode when suitable, set busy timeout, use short transactions, avoid network-mounted shared writable database, and centralize migrations. `SQLITE_BUSY` should be handled by bounded retry for safe transactional operations, not an unbounded generic catch-and-retry after unknown external work. Use a small write pool or serialized write access where benchmarked necessary; avoid holding DB transactions across model calls, A2A requests, tool subprocesses or SSE sends. Persist enough task mapping before network dispatch to reconcile after a crash. Do not store raw secret values in audit columns.

## 61. Proposed Gateway relational schema

```sql
CREATE TABLE workers (
  worker_id TEXT PRIMARY KEY,
  public_key_fingerprint TEXT NOT NULL,
  endpoint_json TEXT NOT NULL,
  capabilities_json TEXT NOT NULL,
  software_version TEXT NOT NULL,
  incarnation TEXT,
  lease_expires_at TEXT,
  status TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE teams (
  team_id TEXT NOT NULL,
  version INTEGER NOT NULL,
  master_worker_id TEXT NOT NULL,
  manifest_json TEXT NOT NULL,
  state TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY(team_id, version)
);

CREATE TABLE executions (
  execution_id TEXT PRIMARY KEY,
  submitted_by TEXT NOT NULL,
  target_worker_id TEXT NOT NULL,
  team_id TEXT,
  team_version INTEGER,
  worker_task_id TEXT,
  idempotency_key TEXT,
  request_digest TEXT NOT NULL,
  projection_state TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE event_projection (
  source_worker_id TEXT NOT NULL,
  stream_id TEXT NOT NULL,
  sequence INTEGER NOT NULL,
  event_id TEXT NOT NULL,
  event_json TEXT NOT NULL,
  recorded_at TEXT NOT NULL,
  PRIMARY KEY(source_worker_id, stream_id, sequence)
);

CREATE TABLE approvals (
  approval_id TEXT PRIMARY KEY,
  execution_id TEXT NOT NULL,
  operation_digest TEXT NOT NULL,
  requested_by TEXT NOT NULL,
  decision_by TEXT,
  decision TEXT,
  expires_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE dispatch_outbox (
  message_id TEXT PRIMARY KEY,
  execution_id TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  dispatch_state TEXT NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0,
  next_attempt_at TEXT,
  updated_at TEXT NOT NULL
);
```

This is a **logical starting schema**, not a production-ready final migration. Add foreign keys, tenant/application namespace, indexes, unique idempotency key scoped to caller, retention, encryption and appropriate timestamps with UTC normalization. A mature implementation should use consistent typed IDs and a migration tool rather than dynamically hand-written SQL scattered through application code. The `dispatch_outbox` pattern covers Gateway dispatch intents; it does not make downstream tool effects exactly once.

## 62. Proposed Worker metadata schema

Store a minimal `execution_bindings` table mapping public execution IDs to native ADK invocation/graph thread/A2A task IDs; `operation_receipts` for external side-effect operation IDs and reconciliation status; and `peer_assignments` referencing remote task handles. The native ADK session service remains the canonical session database, native graph checkpointer remains the graph database, and ADK artifacts/memory remain independent. If ADK's managed runtime or native A2A store covers these mapping functions in pinned 2.3, remove the duplicate tables. Audit this overlap with a direct source code experiment before implementation.

```sql
CREATE TABLE execution_bindings (
  execution_id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  agent_version INTEGER NOT NULL,
  namespace TEXT NOT NULL,
  principal_ref TEXT NOT NULL,
  session_id TEXT NOT NULL,
  native_run_id TEXT,
  native_graph_thread_id TEXT,
  native_a2a_task_id TEXT,
  attempt INTEGER NOT NULL DEFAULT 1,
  state TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE operation_receipts (
  operation_id TEXT PRIMARY KEY,
  execution_id TEXT NOT NULL,
  tool_name TEXT NOT NULL,
  effect_kind TEXT NOT NULL,
  effect_target_hash TEXT,
  state TEXT NOT NULL,
  external_reference TEXT,
  updated_at TEXT NOT NULL
);
```

The exact mapping of runner/run IDs must follow the pinned ADK public API. Never attempt to reverse engineer ephemeral internal identifiers from human-readable log text. These tables are minimized administrative metadata, not an independent session or A2A task engine.

## 63. Data lifecycle and retention

Define independent retention policies for: conversations; checkpoints; task metadata; event projections; tool logs; file artifacts; semantic memories; audit records; credentials; browser recording. Default retention should be conservative. Data erasure must propagate through the authoritative stores and any indexes, caches or downstream connectors. A retention worker must not delete the latest unfinished graph checkpoint required for recovery. Long-running executions need durable references to their pinned agent version, selected model profile and policy at start. Artifact garbage collection must account for shares to other workers and externally signed links.

## 64. Threat model summary

Trust boundaries: client → Gateway; Gateway → Worker; Worker → peer worker; Worker → model provider; Worker → MCP tool server; Worker → browser/terminal sandbox; Worker → storage; external application → Gateway. Threat actors include unauthorized clients, compromised worker instance, malicious peer cards, hostile retrieved documents, prompt injection in tool output, compromised MCP server, replaying network client, malicious workspace code, and excessive-cost requests. The LLM itself is a probabilistic decision system and is not an authorization enforcement point. Treat all model-produced text and tool arguments as untrusted input.

Assets: API credentials, model keys, application data, session history, source files, browser cookies, SSH keys, financial/user records exposed through external integrations, trace content, worker registration identities and approval decisions. Protect confidentiality (scope + encryption), integrity (signatures/digests + idempotency + validated state), and availability (quotas, leases, deadlines, backpressure). The Gateway must never become a credential relay that blindly passes broad owner tokens to every worker.

## 65. Identity and authorization design

A principal may be a human account, headless SDK client, external business service, worker process or configured peer. Authorize the *principal performing an operation*, the resource and the desired action. For team execution, Gateway creates a delegated, bounded context identifying origin principal, objective, team version, capabilities, maximum depth, audience, expiration and trace. Each Worker validates signature, audience, issuer, expiry and capability intersection before admitting task. Peer-to-peer delegation mints a *narrower* scope, not equivalent system-admin rights.

Tool authorization is independent of task admission; permission to run a research agent does not imply permission to call a destructive shell command. Apply ADK's access-control and tool authorization components where supported. Any additional policy layer belongs at a documented Gateway boundary, not as a duplicate permission engine in every agent. Approvals are bound to a canonical operation digest and cannot grant rights outside policy. Maintain replay protection through unique nonce/idempotency receipt and short TTL.

## 66. Confidentiality and secret handling

Secrets are sourced from an explicit secret provider, loaded only into processes that need them, never included in agent prompt text unless the user specifically authorizes a scoped use and the protocol requires it. Do not store raw tokens in configuration TOML committed to Git. Exclude secret values from panic messages, traces, SSE, artifact metadata and shell command logs. Browser credentials and cookies belong to isolated profiles and scoped sessions. Provider keys should be rotated without rebuilding agents where supported; token revocation must prevent new calls, while already-issued requests may need termination. Encryption at rest is policy-driven; choose vetted libraries rather than inventing custom crypto.

## 67. Prompt injection and trust in external content

Retrieved web pages, code repositories, uploaded PDFs, MCP tool outputs and A2A peer responses are *data*, not higher-priority instructions. The agent's instruction hierarchy and authorization system must prevent retrieved content from changing its tool allowlist or destination permissions. Test malicious documents that request secret exfiltration, hidden tool calls, changed goals, or fake system messages. Sanitize rendered HTML and Markdown before displaying in clients. Domain-neutral trust labels can mark `user_input`, `trusted_config`, `tool_result`, `untrusted_document`, `remote_agent_result`; do not trust arbitrary source-provided metadata that claims higher trust.

## 68. Network and SSRF protection

Worker endpoints, MCP HTTP endpoints, web fetch/browser tools and A2A push targets are all potential SSRF vectors. Apply allowlists/private-network restrictions according to deployment policy, ensure DNS rebinding checks at connect time, validate TLS hostnames/certificates and constrain redirects. ADK's A2A guide documents some push protections; reuse native validation and add only deployment-specific network policy. A team manifest referring to a local loopback address on a remote host does not prove reachability. Cross-worker file transfer should reject `file://` and arbitrary server-side URL fetching unless specifically enabled.

## 69. Sandbox security

Use process- and OS-level boundaries for untrusted tools because multiple local ADK agents inside one worker share Rust process memory. Per-tenant memory isolation is not achievable merely by session IDs when arbitrary native plugins execute in one address space. Execute attacker-controlled code via sandboxed subprocess/container/remote runner; use resource limits, restricted mounts, seccomp/namespaces or platform-equivalent mechanisms and disable unneeded network. If a worker must execute privileged corporate operations, isolate it into a separate process/security domain rather than trusting an internal agent role label. Validate any ADK sandbox/code execution capability on the exact OS and threat model.

## 70. Audit policy

Audit worker identity creation, registration, team modifications, permission changes, task submissions, delegation, approvals, cancellations, tool side-effect requests and final verification. Audit events are append-only from the application perspective, signed or tamper-evident if compliance demands it. Every record includes who, action, resource, resulting decision, timestamp, source, trace and policy version; sensitive payloads are stored separately and access-controlled. Audits are not a substitute for ADK telemetry spans: telemetry is operational and may be sampled, while security decisions should be retained according to dedicated policy.

## 71. Observability and SLOs

ADK telemetry already instruments model, tool and agent spans. Initialize it before constructing the ADK Runner; propagate trace context through Gateway, A2A, HTTP and tool adapters. Additional **harness-specific** metrics: worker ready/unreachable counts, active/queued tasks, admission rejection, Gateway routing latency, A2A retry rate, task recovery count, approval age, event replay lag, SQLite busy retries, artifact transfer failures and plugin health. Keep metric label cardinality bounded: do not use raw session IDs as Prometheus labels. Expose operational dashboards without recording raw user content by default.

Proposed starting service-level indicators: successful request routing when target is available, event relay catch-up latency, task metadata durability, worker reboot recovery success, and bounded resource usage under overload. Set numeric SLOs after benchmark measurements rather than inventing guarantees. A durable, truthful `unknown` state is preferable to an incorrect success or failure. Record estimated monetary cost separately from provider-confirmed billing and token usage.

## 72. Availability and disaster recovery

Single-node mode must recover from `SIGTERM`, `SIGKILL`, disk-full, corrupt event projection, temporary Gateway loss, temporary model outage and A2A transport outage. Back up worker state directory with consistency checks suitable for SQLite WAL and artifacts; test restoration to a clean host. Gateway HA is a later deployment mode: it needs shared durable state, leader/lease coordination and load-balanced transports. Do not claim active-active correctness with two gateways writing unrelated local SQLite databases. Recovery must restore authorization and agent version metadata along with checkpoints; a checkpoint without its originating agent definition may be unusable.

## 73. Deployment options

| Mode | Worker | Gateway | Client | Persistence |
|---|---|---|---|---|
| Standalone laptop | One local process | None | CLI/local app | Local ADK stores |
| Local multi-worker | Several processes | One localhost Gateway | CLI/SDK | Separate worker data dirs + Gateway DB |
| Single VPS | Managed worker pool | Same machine | Remote app | Local worker dirs + Gateway DB |
| Distributed fleet | Workers on multiple hosts | Central Gateway | Many clients | Per-worker state + Gateway backend |
| Enterprise isolation | Dedicated workers per trust domain | Authenticated Gateway | Business server via SDK | Per-domain encrypted stores |
| Offline lab | Local model and local tools | Optional | CLI | Fully local |

For Linux, systemd is a reasonable simple deployment supervisor; for containers, set read-only root filesystem where feasible, persistent data volume, resource limits, health probes and graceful termination. Reverse proxy terminates TLS and supports SSE no-buffering/WebSocket upgrades. Keep worker process configs and state directories distinct. Use deployment manifests that do not contain credentials; follow ADK official deployment instructions for agent-serving components before adding custom hosting wrappers.

## 74. Packaging and distribution

Publish three main artifacts: `agent-worker` binary, `agent-gateway` binary and `goldenbee` CLI binary; optional `sdk-rust` crate and generated OpenAPI client libraries. Use reproducible release builds and signed checksums. Document supported CPU architectures, OS minimums, SQLite extension loading and dynamic libraries required by optional models/browser features. Choose Cargo features such as `browser`, `coding`, `realtime`, `rag`, `mcp`, `a2a`, `local-sqlite`, `otlp` rather than making the minimal worker pull all possible dependencies. The release notes must identify selected ADK-Rust version, protocol compatibility and required migrations.

## 75. Configuration compatibility and migrations

Every persisted configuration has `schema_version`, and every worker/gateway handshake advertises a protocol range. On startup, preflight configuration and database migrations. Migrations must be idempotent when interrupted and verified on a backup copy in CI. Read-only downgrade may be supported where safe; never silently open a newer incompatible database and mutate it. Team config changes use optimistic version checks. Worker startup should fail clearly on invalid secrets, unknown required feature flags, unsupported checkpoint schema or duplicate worker ID; a disabled optional capability should degrade without blocking unrelated operations.

---

# Part VIII — Implementation Work Packages and Acceptance Contracts

## 76. Delivery approach

Use vertical feature slices, not disconnected layers implemented in isolation. A vertical slice contains interface contract, ADK reuse proof, thin adapter, persistence, API, tests, telemetry and documentation. The very first milestone must run a single ADK agent via a standalone Worker CLI; second milestone submits the same prompt through Gateway and verifies identical result semantics. No custom multi-agent control flow is permitted before the corresponding native ADK team/graph/A2A experiment passes. Every work package below must include the upstream evidence link and an explicit `reuse/native` versus `glue/new` classification.

### Definition of Done for every work package

1. A proposal/acceptance section is linked to the issue and tests.
2. Verified ADK official guide, pinned rustdoc and example are listed in the PR.
3. The implementation uses native ADK where available, and any custom replacement has an approved ADR.
4. Public and internal interfaces are typed and documented; structs maintain invariants.
5. Authorization, input validation, limits, cancellation, timeouts and invalid states are tested.
6. The happy path and realistic alternate workflows are implemented end-to-end.
7. Important events/traces are emitted with correlation IDs, with no secrets.
8. Tests cover unit, contract, integration and negative cases for the change.
9. The crate compiles, formatting/linting pass, and scenario evidence is attached.
10. Source tree and README are updated; no accidental business-server dependency is introduced.

## 77. Phases with entry and exit gates

| Phase | Core deliverables | Exit gate |
|---|---|---|
| 0 — Official API survey | 2.3.0 compatibility inventory; runnable ADK smoke crate | Native capabilities evidenced |
| 1 — Standalone Worker | Runner, agent catalog, local sessions, CLI, typed tools | Complete direct task with streaming |
| 2 — Worker durability | ADK SQLite services, GraphAgent, interruption/recovery | Restart-resume proves semantics |
| 3 — Worker API | Native ADK server/A2A, minimal control endpoints | HTTP and A2A conformance |
| 4 — Gateway foundation | Registry, auth, routing, idempotency, event projection | Direct target task survives reconnect |
| 5 — Teams and Master | Versioned rosters, ADK team patterns, remote A2A | Master delegates to two workers |
| 6 — Advanced tools | MCP, CodingAgent, browser, sandbox profiles | End-to-end tool verification |
| 7 — SDK and CLI | OpenAPI SDK, stream resume, operations commands | Equivalent outcomes in CLI/SDK |
| 8 — Reliability | Recovery, approval, quotas, audit, observability | Crash/security/soak suites pass |
| 9 — Release | Packaging, deployment docs, compatibility matrix | Signed binaries and upgrade drill |

**Dependency order:** Phase 0 precedes all; Worker direct path precedes Gateway; native A2A prototype precedes remote team code; persistent checkpoint/resume precedes claims of uninterrupted autonomous operation; auth precedes remote management; signed artifact retrieval precedes cross-host evidence sharing. Parallelize independent documentation, schema and test-fixture tasks only after core contracts stabilize.

---

## 78. Work package: ADK dependency survey

**Objective and design.** Pin the 2.3.0 umbrella or exact component crates; inspect rustdoc, feature flags, licensing, MSRV, native interfaces and examples. Build a compatibility manifest with supported/experimental/unsupported flags. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK docs.rs, GitHub tag, quickstart and 2.x migration guide. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Unknown builder signatures, feature-flag drift, unstable APIs, transitive native deps. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Compile five isolated proofs: Runner, GraphAgent, CodingAgent, MCP, A2A. The review fails if an API signature is guessed. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 79. Work package: Scaffold and bootstrap

**Objective and design.** Generate a native ADK agent via cargo-adk templates; embed into Worker composition root with validated TOML, graceful shutdown and telemetry. Preserve upstream scaffold code until tests pass. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK quickstart, cargo-adk templates, Launcher and telemetry. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Startup ordering, provider credentials, misconfigured listeners, inconsistent shutdown. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** `agent-worker doctor` reports readiness; one real/fake model turn completes and process exits cleanly. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 80. Work package: Agent factory

**Objective and design.** Translate immutable catalog manifest to native LlmAgent/GraphAgent/workflow/CodingAgent constructors. Validate names, model IDs, tool references and local roles. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK LlmAgent, workflows, graph and CodingAgent guides. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Dynamic config mutation, duplicate names, builder errors after admission. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Two agent versions coexist; old session remains pinned; unknown tool is rejected before execution. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 81. Work package: Runner invocation adapter

**Objective and design.** Compose a native Runner and forward its typed stream through a bounded channel; keep precise identity mapping and cancellation token lifetime. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK Runner guide and exact 2.3.0 rustdoc. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Accidental global locks, broad session cancellation, premature stream-drop cleanup. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Two sessions execute concurrently; cancellation of one does not interrupt another. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 82. Work package: Local session service

**Objective and design.** Select ADK session persistence implementation, map namespace/principal/session identity, enforce one writing turn per session. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK Sessions and Runner identity-bound persistence docs. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Cross-user session collision, duplicate messages after retry, expired session retention. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Same session resumes across process restart; foreign principal cannot read it. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 83. Work package: Worker admission

**Objective and design.** Implement lightweight bounded queue, per-agent limits and resource permits around—not inside—the Runner. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK Runner concurrency and tool execution strategy docs; Tokio primitives. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Queue starvation, failure to release permit on cancellation, unbounded memory. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Over-capacity requests wait or return 429; active count returns to zero under all outcomes. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 84. Work package: Standalone CLI

**Objective and design.** Use ADK Launcher/CLI for existing direct agent functions and add only Worker status/config/task management commands that are missing. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK deployment Launcher and coding CLI documentation. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Duplicate CLI implementation, inconsistent output, hidden Gateway dependency. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Worker operates offline with configured local model; JSON mode is scriptable. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 85. Work package: Worker health and drain

**Objective and design.** Expose minimal status/start-stop/drain control routes alongside native ADK agent server. Differentiate liveness/readiness and degraded providers. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK Server guide; generic HTTP health semantics. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** False readiness, abrupt exit with unflushed events, unsafe remote admin. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** SIGTERM stops admissions and honors timeout; readiness false during drain. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 86. Work package: Task mapping

**Objective and design.** Persist only execution-to-native ADK IDs, request digests and attempt links; use native A2A task store when in A2A mode. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK A2A task lifecycle, Runner and graph checkpoint docs. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Duplicate competing task databases, ambiguous accepted state, inconsistent recovery. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Idempotent retransmission returns original handle; bad payload reuse is conflict. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 87. Work package: Graph checkpoints

**Objective and design.** Use GraphAgent and SqliteCheckpointer for a branched, human-interrupted, restart-resumable workflow. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK Graph Agents guide, checkpoint examples. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Node re-execution after interrupted frontier, checkpoint incompatibility. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Kill after node completion, restart, and verify finished node not repeated. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 88. Work package: Graph approval

**Objective and design.** Use ADK human-in-loop interruption and native tool confirmation where supported; approval digest bound to operation. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK Graph Agents and Security/Guardrails documentation. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Changed operation after approval, expired decision, wrong approver. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Changing tool arguments invalidates previous approval and blocks execution. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 89. Work package: Portable local team

**Objective and design.** Construct ADK team supervisor/router with explicit roster and policy budgets; capture built-in execution receipts. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK multi-agent guide, CompiledTeam APIs. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Unbounded fanout, mutated roster during run, ambiguous transfer vs delegation. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Coordinator routes to two local specialists and receipt records causal edges. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 90. Work package: Graph and team recovery

**Objective and design.** Restore native ADK snapshots/checkpoints before re-entering a workflow; treat unresolved delegates as unknown until reconciled. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK multi-agent restore documentation and graph resume. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Automatic unsafe replay of remote writes, missing child results. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Recovery retains completed peer result; unresolved delegated call is not silently duplicated. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 91. Work package: MCP local integration

**Objective and design.** Use McpToolset to adapt MCP tools and McpServerManager to monitor dynamic stdio services. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK MCP overview/client/dynamic manager docs. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Stdio process leakage, manager autoApprove misinterpreted as authorization. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Enable/disable local MCP tool without restarting worker; disallowed tool remains inaccessible. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 92. Work package: MCP remote integration

**Objective and design.** Use ADK remote MCP HTTP client, negotiated capabilities and auth injection. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK MCP Streamable HTTP and security docs. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** SSRF, expired token, reconnect ambiguity, exposed remote secrets. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Remote tool call emits ADK tool event; unauthorized endpoint and missing credentials rejected. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 93. Work package: Native coding worker

**Objective and design.** Configure ADK CodingAgent + workspace; add only workspace allocation, isolation and CI acceptance policy. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK CodingAgent harness, devtools and code execution docs. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Two coding tasks modifying same checkout, shell escape, binary output explosion. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Separate worktrees implement independent changes; tests run; artifacts link to diff. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 94. Work package: Browser capability

**Objective and design.** Prefer ADK WebDriver/browser tools for navigation, interaction and extraction; custom lifecycle only if documented missing. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK browser tools documentation. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Browser-process cleanup, cookies crossing sessions, unsafe navigation. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Two sessions use isolated profiles; one disconnect does not expose other's cookies. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 95. Work package: Terminal capability

**Objective and design.** Use ADK existing devtools bash/code execution; isolate subprocess and apply execution policy, not a parallel tool loop. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK CodingAgent devtools and sandbox/code docs. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Unbounded subprocess, path escape, inherited secrets, uncontrolled network. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Timeout terminates child process group; constrained workspace cannot access forbidden path. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 96. Work package: Model provider setup

**Objective and design.** Configure provider-neutral ADK model instances with capability metadata and logical aliases; avoid model client rewrite. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK model provider / Ollama documentation. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Unsupported function schema, missing usage, fallback residency violation. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Provider swap retains task contract; unsupported feature returns preflight error. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 97. Work package: Token and cost accounting

**Objective and design.** Read native ADK telemetry/provider usage; aggregate per task and policy budget at worker boundary. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK Telemetry guide, provider integration usage fields. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Missing usage treated as zero, high-cardinality metrics, incorrect cached token accounting. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Known counts are recorded; unknown values remain explicitly unknown, budget triggers policy event. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 98. Work package: Session context compaction

**Objective and design.** Use documented Runner context-compaction support instead of custom summarization loop. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK Runner context compaction section. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Summary losing source references, unsupported provider caching interactions. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Long history stays within configured window; compaction event trace remains inspectable. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 99. Work package: Semantic memory

**Objective and design.** Construct ADK MemoryService backend; verify exact retrieval semantics per backend, retention and namespace filters. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK Memory overview and backends docs. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** SQLite keyword search mistaken for vector semantic search, cross-tenant data leak. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Identical prompt with scoped principals returns only permitted memories; erasure removes retrieval. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 100. Work package: Knowledge graph

**Objective and design.** Use native GraphMemoryService for entity/relation observation handling; avoid standalone graph ORM unless proven gap. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK Memory Knowledge Graph guide. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Provenance loss, incorrect time semantics, uncontrolled automated memory updates. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Entity changes retain temporal context and source references through retrieval. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 101. Work package: RAG pipeline

**Objective and design.** Use ADK RAG facilities; any sqlite-vec adapter must attach to existing ADK retrieval extension and have ADR. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK RAG tools and memory backends docs. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Provider/tokenizer mismatch, silent truncation, incorrect citations. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Oversized input is chunked without loss; retrieved answer cites accessible source. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 102. Work package: Artifact access

**Objective and design.** Use ADK artifact service; expose secure metadata and download adapters, not duplicate artifact persistence. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK Artifacts guide. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Large event payloads, shared path assumptions, unauthorized artifact ref. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Cross-worker artifact fetched with digest verification and scoped grant. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 103. Work package: A2A worker server

**Objective and design.** Enable native ADK A2A v1 server/agent cards, task store, streaming and validation. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK A2A Deployment guide. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** In-memory production task store, misleading card capabilities, unsupported operations. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Official A2A conformance checks pass; card advertises only implemented abilities. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 104. Work package: A2A client delegation

**Objective and design.** Use ADK remote A2A client for submit, status, subscribe, cancel as documented; preserve native task ID. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK A2A and RemoteA2aAgent docs. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Duplicate task after stream drop, lost parent correlation, unauthenticated peer. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Master can reconnect and fetch child result without resubmitting objective. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 105. Work package: Peer directory

**Objective and design.** Expose authorized peer records to Worker master; direct static config in standalone mode, Gateway discovery when managed. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK A2A agent card semantics; Gateway-owned interface. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Stale agent cards, disallowed peer, peer capability lying. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Unavailable peer is skipped; caller sees clear authorized candidate list. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 106. Work package: Gateway registry

**Objective and design.** Register worker identity, incarnation, endpoint, capability snapshot and lease renewal. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK agent cards for capabilities; custom Gateway boundary. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Registration spoofing, stale lease presented as healthy, duplicate IDs. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Expired lease marks unreachable; new incarnation reconciles without losing history. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 107. Work package: Gateway team manager

**Objective and design.** Store versioned master/subworker rosters and delegation limits; freeze version for executions. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK team model for internal patterns; Gateway custom external roster. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Cycle, privilege escalation, membership race, cross-tenant link. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** A running execution retains old roster snapshot while new team revision activates. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 108. Work package: Gateway route resolver

**Objective and design.** Route explicitly addressed worker or selected team Master according to approved capabilities and quotas. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** Native A2A for remote calls; Gateway-specific policy adapter. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Silent wrong-worker routing, weak fallback, unreachable target. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Correct worker receives task; unauthorized fallback is never invoked. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 109. Work package: Gateway event relay

**Objective and design.** Normalize event envelope, retain critical events, relay ADK/A2A streams and replay on reconnect. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK Runner events, A2A SSE and telemetry docs. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Global ordering assumption, slow subscriber, retained cursor expiration. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Client reconnects after Gateway reboot with no missing durable terminal state. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 110. Work package: Gateway WebSocket

**Objective and design.** Implement duplex control only if native ADK server transport lacks the Gateway-specific session routing feature. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK Server/realtime deployment guides. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Unbounded frames, token leaks in URLs, replayed commands. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** WS command ACK and replay work under disconnect and token refresh. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 111. Work package: Gateway REST/OpenAPI

**Objective and design.** Publish generated OpenAPI, typed errors, route permissions and SDK conformance fixtures; proxy native ADK where possible. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK Server native routes; Gateway contract section. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Duplicated sessions/tasks endpoints, schema drift, undocumented errors. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** OpenAPI snapshot and SDK serialization round-trip tests pass. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 112. Work package: Approval router

**Objective and design.** Route authorized human decisions to correct native ADK graph/task context without becoming an independent AI workflow. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK Graph interrupts and tool confirmation. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Stale approval, wrong user, duplicate decision, expired token. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Decision replay is idempotent; unauthorized decision rejected and audited. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 113. Work package: Client SDK

**Objective and design.** Implement Rust SDK with typed DTOs, retry/idempotency, pagination and stream resumption; generate other languages from OpenAPI. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** Goldenbee Gateway contract; native A2A where practical. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Unbounded automatic retries, ambiguous task creation, incompatible API versions. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** SDK can submit/watch/cancel without duplicating task after injected timeout. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 114. Work package: CLI Gateway interface

**Objective and design.** Use SDK, not handwritten parallel HTTP client; format status/event tables and JSON output. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** Goldenbee Client SDK; ADK CLI for native agent scaffolding. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Output coupling, credentials printed, stdin/stdout misuse. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Shell pipeline can submit and monitor with machine-readable results. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 115. Work package: Authorization adapter

**Objective and design.** Map authenticated principal and delegated claims to ADK tool policies and Gateway routes. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK Access Control / Guardrails docs. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Prompt-based authorization, escalation across delegation, token replay. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Unauthorized tool call cannot run even when malicious model requests it. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 116. Work package: Audit and redaction

**Objective and design.** Log sensitive state transitions separately from sampled telemetry, with structured redacted identifiers and policy version. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK Telemetry and Security docs. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Secrets in trace payload, missing approval audit, unbounded log retention. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Secret canary never appears in telemetry, SSE, CLI or diagnostic bundle. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 117. Work package: Crash and resume

**Objective and design.** Recover worker metadata and native ADK graph checkpoints; reconcile A2A peers; do not blindly replay effects. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK Graph checkpoint and A2A task lifecycle. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Double execution after crash, false completion, orphaned child process. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Kill process at multiple boundaries; record truthful state and no duplicated side effect. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 118. Work package: Release compatibility

**Objective and design.** Pin ADK dependencies, emit protocol feature versions, test upgrades/downgrades, sign distribution artifacts. The implementation is a vertical slice: public contract or config, domain/service interface, native ADK composition where relevant, one persistence boundary, security enforcement at ingress and effect points, telemetry, and scenario tests. Document why each component belongs in Worker, Gateway, or Client, and prevent accidental higher-layer imports.

**Official framework reuse.** ADK docs.rs pinned rustdoc, stability and changelog. Before coding, verify exact symbols and feature flags against the pinned published ADK-Rust 2.3.0 API. Reuse ADK rather than building a parallel implementation. If an upstream component is absent, experimental, or insufficient for this contract, record the precise source proof and proposed thin adapter in an ADR. Business-specific behavior remains out of scope.

**Design risks and alternatives.** Breaking API changes, incompatible checkpoints, half-applied migration. At least one test must cover an alternate valid scenario and one test a genuine invalid scenario. Ensure resource limits, cancellation and authorization do not produce unrelated application errors. Where asynchronous behavior has an uncertain result, expose `unknown/reconciling` with an actionable next step rather than retrying a side effect automatically.

**Acceptance proof.** Previous client works on v1 contract; rejected incompatible binary fails predictably. Attach trace IDs, event samples, resulting artifact or state change, the exact command that reproduces the test, and a negative security case. Report verified evidence, not unsupported claims about what the agent probably did. For this package the PR must include a small interface diagram or table showing inputs, outputs, ownership, durability, failure responses and version compatibility.

## 119. Integration checkpoint before system-level testing

Before proceeding to end-to-end scenarios, perform an integration review: compile the pinned ADK examples, run direct Worker tasks without Gateway, verify that Gateway routing preserves native A2A task handles, reconcile event IDs after restart, and confirm that no business-specific dependencies were introduced. Record limitations that require upstream changes and avoid building provisional duplicate functionality. Security and architecture reviewers must approve changes to trust boundaries, persistent storage ownership or transport semantics.

---

# Part IX — End-to-End Scenarios and Contract Test Matrix

## 120. Scenario A — Single standalone worker

**Given** no Gateway configured and a local model provider reachable, start `agent-worker` with a read-only agent definition and an authenticated local CLI client. **When** the user opens a new session and submits an informational request, the Worker resolves the agent from its versioned catalog, creates or retrieves the ADK session, executes through native Runner, streams typed events and writes canonical history through ADK SessionService. **Then** a follow-up turn uses the same session with correct identity; no business server, Gateway, custom LLM loop or duplicated session store is required.

Alternate cases: local model missing, provider refuses a tool schema, client stream drops mid-turn, session already has active turn, disk full during persistence, process receives SIGTERM. A disconnected client does not imply cancellation unless an explicit policy says so. Evidence: process logs, session row in native service, event sequence, model result and precise use of the upstream Runner. Security: another principal cannot use the same human-readable session identifier to read the first session.

## 121. Scenario B — Direct worker through Gateway

**Given** one Gateway and two registered healthy workers, a caller requests an explicit target worker ID and supplies an idempotency key. **When** Gateway authorizes and routes the request, it records the route decision and externally stable execution ID, invokes the target Worker through existing ADK server/A2A semantics, and binds the returned native handle. **Then** the caller streams progress and final result through Gateway SSE. Sending the same request again returns the same external ID; sending a different payload with the same key returns conflict.

Alternate cases: worker lease expires after discovery but before dispatch, routing timeout occurs after remote acceptance, auth expires during streaming, SSE cursor lost, Gateway reboots mid-task. The Gateway reconciles unknown dispatch by querying the target; it never declares failure solely from connection loss. Evidence: route audit, native worker task ID, duplicated-submit count zero, event replay and task ownership map.

## 122. Scenario C — Master with two remote subworkers

**Given** a versioned team with one Master and two authorized subworkers, user submits a goal that has two independent research/implementation subtasks and a dependent verification step. **When** Master uses ADK team's supervisor/graph capabilities to plan, it selects remote peers from authorized discovery and delegates over native A2A. Subworkers each execute within their own Worker process and return structured evidence. **Then** Master aggregates results, verifies dependency acceptance and publishes final report, while Gateway shows a read-only task graph projection.

Alternate cases: one worker is saturated; one child is unavailable; results conflict; an approval is required; a subworker tries to delegate beyond permitted depth; the Master crashes after submitting a child but before receiving acknowledgment. The Master uses stored remote task handle/correlation to reconcile; it does not create a duplicate child. Evidence: ADK team receipt, A2A transcript, worker-level task handles, artifact digests, final acceptance verdict.

## 123. Scenario D — Durable human approval

**Given** a GraphAgent workflow about to deploy or modify protected resources, the tool policy requires a human decision. **When** the graph reaches the appropriate ADK interrupt/confirmation gate, native checkpoint state is stored and a signed operation-digest approval request reaches Gateway. **Then** an authorized user approves exact arguments and the same graph thread resumes; the sensitive action executes once under the approved policy.

Alternate cases: rejection, expiry, unknown approver, approval request spoofing, changed arguments, repeated decision, process crash while waiting, Gateway offline after decision. Verify idempotent approval handling and explicit denial when digest differs. Native graph checkpoint semantics are the recovery foundation; approval routing metadata in Gateway is not a competing graph engine.

## 124. Scenario E — Coding across separate workers

**Given** one Master, frontend and backend worker instances, and a repository, Master creates independent worktree assignments with shared acceptance criteria. Each CodingAgent uses ADK's native devtools inside its isolated Workspace; tasks run in parallel but do not write the same checkout. A testing worker merges or checks the approved branch state and runs full tests. Master publishes test evidence, diff references and remaining limitations. No manual completion is claimed solely because a `bash` tool returned exit 0 once.

Alternate cases: Rust cargo lock contention, conflicting patches, non-compiling intermediate state, flaky tests, untrusted repository instructions, excessive build memory, sandbox failure, remote artifact digest mismatch. Use checked worktrees and bounded build concurrency. The Worker terminal tool remains an ADK tool, not a custom out-of-band agent executor.

## 125. Scenario F — Restart under active distributed execution

**Given** Master and two subworkers have acknowledged tasks, force-kill the Master while one child is active and one has completed. Restart the Master with the same persistent data and agent version. It loads ADK checkpoint/team snapshot, reconciles child A2A handles, observes the completed child without replay, and waits for active child status. If an external tool side effect was dispatched but the outcome cannot be established, mark it unknown and require verification rather than triggering a second write.

Measure duplicate side effects (must be zero for protected idempotent operations), recovered task count, missing events, graph correctness and resource cleanup. Repeat with Gateway down, one subworker down and storage out of space. Do not report autonomous durability complete until these crash tests pass.

## 126. Scenario G — Business application integration without coupling

**Given** a separately developed web/mobile application server with its own user domain, that server obtains a scoped service credential to Agent Gateway and creates a session on behalf of its authorized principal. It can submit a direct worker task or team objective, stream events into its own client UI, and apply its independent business workflow policy. The Goldenbee harness neither queries its business database directly nor requires business-specific schema migrations. A second unrelated application should integrate through the same SDK and protocol without adding conditional business branches to Worker or Gateway.

Negative tests: client for App A requests App B's worker session; a business app spoofs an admin subject in JSON; an expired delegated token still attempts tool use; a worker tries to directly connect to an unauthorized business database. All must be rejected at a proper boundary.

## 127. Scenario H — Long-lived agent and budgeted loops

**Given** a Master running a bounded goal-oriented graph, one step requires multiple model calls and tool operations. Graph/time budget and ADK team policy limit recursive turns, tokens, tool calls, children and deadline. The run periodically emits durable progress and can pause for dependency availability. If budget expires, it completes with a truthful partial report and an explicit escalation/resume option rather than an infinite loop. Worker process remains healthy and other sessions continue processing.

Test model provider rate limits, cancellation while a tool is executing, agent stalls, compaction after many turns and incomplete source citations. Distinguish provider-confirmed usage from estimated cost. The policy interface should permit administrative reduction of future budget without increasing privileges of a currently running child.

## 128. Contract and fault injection matrix

| Fault / test | Worker | Gateway | Client | Required result |
|---|---|---|---|---|
| Duplicate submit | Stable native handle | Idempotency mapping | Retry same key | No duplicate task |
| Reused key, changed payload | Reject/conflict | Conflict | Error shown | No silent overwrite |
| SSE disconnect | Continue eligible work | Replay cursor | Reconnect | No duplicated prompt |
| Worker crash | Recover ADK state | Mark unreachable/reconcile | Show uncertain state | Truthful status |
| Gateway crash | Execute as allowed | Restore projections | Reconnect | Surviving tasks |
| Master crash | Query child A2A handles | Keep team mapping | Show recovery | No duplicate child |
| Peer unauthorized | Reject A2A call | Exclude discovery | Explain permissions | No access |
| Wrong approval digest | Block action | Reject decision | Explain invalidation | No protected side effect |
| Expired approval | Remain interrupted/deny | Audit expiration | Prompt fresh review | No accidental approval |
| Saturated model | Queue/backoff | Avoid overloaded route | Retry-after | Bounded memory |
| SQLite busy | Bounded safe retry | Retry atomic metadata only | Stable status | No live-lock |
| SQLite disk full | Stop durable admit | Surface storage degraded | Error/actionable | No false accepted tasks |
| MCP server outage | Fail affected tool safely | Not implicated | Task status clear | Other tasks continue |
| Browser takeover | Release interactive lease | Route authorized control | Human takes control | No competing clicks |
| Unauthorized artifact | Reject fetch | Reject proxy | 403 | No information leak |
| Prompt injection | Tool policy blocks | Preserves auth | Visible warning if useful | No authority escalation |
| Budget exceeded | Stop new bounded work | Report limit | Partial report | No runaway costs |
| Tool success with bad result | Evaluation fails | Project status | Show failed acceptance | No false completion |
| App isolation | Namespace isolation | Scoped auth | Separate client | No cross-app data |
| Version mismatch | Advertise support | Negotiate/reject | Helpful error | No corruption |

## 129. Performance and load testing methodology

Benchmark three distinct overhead sources: **framework agent execution overhead**, **Gateway routing/streaming overhead**, and **external model/tool latency**. Do not use a single end-to-end token latency number to infer all three. A deterministic mock LLM and mock tools allow latency/throughput tests without external provider variability. Repeat with local Ollama/remote provider and record hardware, model, prompt tokens, concurrency, p50/p95/p99, memory RSS, open sockets, DB write latency, A2A error rate and event lag. ADK has an official `cargo adk bench` mechanism; use that for framework comparisons before writing harness-specific load drivers. Claims about throughput must be measured on the target deployment rather than extrapolated from unrelated published benchmarks.

Test increasing workloads (1, 2, 4, 8, 16... concurrent sessions), high event burst, slow streaming consumer, 100+ idle registered workers, worker churn and A2A fanout. Establish sustainable worker limits and backpressure behavior, not only peak success at unsafe resource usage. Compare platform-wide operation with direct Worker to quantify Gateway cost. Track CPU/disk/memory per feature flag; browser and embedded model engines may dominate the runtime footprint.

## 130. Evaluation strategy

Use ADK evaluation where available: expected trajectory, tool-use correctness, rubric-based outputs, agent comparisons and CI reporting. Keep deterministic contract tests separate from stochastic quality evaluations. Build a gold dataset of tasks covering research, code editing, multi-agent planning, RAG citations, refusal of unauthorized tools, ambiguous external action result and follow-up interactions. Scoring should include task success, acceptance criteria satisfied, evidence validity, tool errors, recovery correctness, unnecessary user questions, token and wall-time consumption. A generated report that merely says 'done' fails unless verifiable artifacts support completion.

Set release gates on both safety and quality. Safety invariants are hard-fail with zero tolerated privilege bypass; probabilistic task quality gets measured thresholds based on a fixed baseline and confidence intervals. Retain sampled trace evidence with secret redaction. Evaluate on offline deterministic fixtures before expensive live provider evaluation.

---

# Part X — Architecture Decision Records, Release Gates, and References

## 131. ADR inventory

| ADR | Decision | Status | Rationale |
|---|---|---|---|
| ADR-001 | Three-tier harness: Client/Gateway/Worker | Accepted proposed baseline | Business agnostic |
| ADR-002 | Worker is independent process with many ADK agents | Accepted proposed baseline | Flexible concurrency and deployment |
| ADR-003 | Master/subworker is a role/relationship | Accepted proposed baseline | One Worker implementation |
| ADR-004 | ADK native Runner replaces custom model/tool loop | Mandatory | Eliminate duplicated framework code |
| ADR-005 | ADK GraphAgent for durable branching | Mandatory where needed | Checkpoint and HITL are upstream |
| ADR-006 | A2A for remote agent task communication | Proposed | Interoperable standard |
| ADR-007 | MCP for external tool systems | Proposed | Standardized tool discovery |
| ADR-008 | Worker owns execution; Gateway owns routing/projection | Accepted proposed baseline | Single source of truth |
| ADR-009 | SQLite initial single-node metadata | Proposed | Simplicity, bounded scope |
| ADR-010 | Separate OS sandbox for untrusted code | Mandatory | In-process agents are not isolation |
| ADR-011 | No compulsory business application server | Accepted proposed baseline | Reusability |
| ADR-012 | Public API v1 stable independently of ADK version | Proposed | External client longevity |
| ADR-013 | Built-in ADK coding and browser tools first | Mandatory | Avoid parallel tool harness |
| ADR-014 | Native ADK memory, graph and artifact services | Mandatory first choice | Provenance and scope |
| ADR-015 | Gateway optional for standalone Worker | Accepted proposed baseline | Offline/local functionality |
| ADR-016 | Approved slim adapters only after gap proof | Mandatory | Minimize maintenance |
| ADR-017 | Worker process termination is not task failure | Mandatory | Correct distributed semantics |
| ADR-018 | Sensitive approval bound to operation digest | Mandatory | Prevent stale approval |
| ADR-019 | Stable external ID maps to native ADK task/run IDs | Proposed | Provider/version isolation |
| ADR-020 | Explicit release compatibility matrix | Mandatory | Safe upgrades |

All proposed ADRs must state context, options considered, consequences, owner, upstream API proof, security impact, migration path, and acceptance evidence. A custom component that duplicates ADK must carry an ADR with a pinned API/source comparison and a plan to remove it if upstream support becomes available.

## 132. Coding-agent implementation instructions

You are implementing **Goldenbee**, an application-agnostic Rust AI agent harness. The Worker runs ADK-Rust 2.3.0 agents; Gateway coordinates independent Worker processes; SDK/CLI are protocol clients. **Do not implement business applications or a mandatory Command Center UI.** For every assigned package:

1. Read the linked proposal section, its official ADK docs, the pinned rustdoc and official example source.
2. Inspect the actual repository and existing modules before planning code changes.
3. Write a concise execution plan with dependencies, interfaces, existing ADK capability and acceptance tests.
4. Prefer existing ADK native types over custom abstractions, particularly Runner, agent types, GraphAgent, team patterns, MCP, A2A, CodingAgent, memory, guardrails and telemetry.
5. Use encapsulated Rust structs with small traits/ports and dependency injection, abiding by SOLID; avoid giant service objects and unnecessary layers.
6. Implement meaningful alternate scenarios, not only the happy path plus dozens of fatal errors. Use wait/recover/replan/ask/skip-with-warning where appropriate.
7. Keep I/O, state transitions and authorization separate; no unchecked LLM output should mutate privileged state.
8. Implement one complete vertical slice at a time, with tests and actual end-to-end proof. Parallel agents may work on independent modules with non-overlapping files and build outputs.
9. Use `cargo fmt`, clippy, typecheck and applicable targeted tests in a resource-aware sequence. Avoid running competing Cargo builds against the same `target` directory; follow project instruction on when cargo commands are permitted.
10. Never claim a feature complete merely because code compiles; verify actual API behavior, durable state, tooling and user-visible outcome.
11. Review logs/trace for secrets, cancellation correctness, memory leaks, SQL busy handling and resource cleanup.
12. Update module README, API schema, compatibility matrix and decision records. Report tests run, changes made, evidence, remaining risks and blocked items.

**Special rule:** no bespoke ADK replacement implementation (model loop, multi-agent graph, workflow engine, tool registry, MCP/A2A implementation, graph memory, provider client, agent evaluation) without explicit user approval and a documented upstream gap. For missing functionality, prefer composition or native extension traits.

## 133. Required design review questions

Before approving any feature, ask: Is this already in ADK? Is the selected interface stable in 2.3? Does the Worker require Gateway for an otherwise standalone feature? Can two callers with identical IDs cross namespaces? Are side effects idempotent or reconciled? What happens when the process dies between acceptance and first tool call? Which service owns truth and which stores a projection? What is the failure mode for a disconnected peer? What are the memory and concurrency limits? Are secrets and personally identifiable data excluded from logs? Can a client resume the event stream without duplicating the request? How do we prove completion against acceptance criteria?

## 134. Release checklist

- [ ] Official ADK capability map reviewed against pinned 2.3.0 source and rustdoc.
- [ ] No duplicate Agent/Runner/Graph/MCP/A2A implementation introduced.
- [ ] Standalone Worker works without Gateway and uses native ADK sessions.
- [ ] Gateway can register/discover/reconcile workers securely.
- [ ] Direct worker and team Master tasks complete with verifiable evidence.
- [ ] At least two remote subworkers participate in a live A2A task.
- [ ] Native GraphAgent checkpoint survives forced restart.
- [ ] Unknown side effects are reconciled rather than blindly repeated.
- [ ] Gateway SSE and WS satisfy replay and backpressure tests.
- [ ] Authentication, authorization, approval digest, quotas and sandbox tests pass.
- [ ] OpenAPI schema, Rust SDK and CLI examples are consistent.
- [ ] No business-server hard dependencies or domain-specific tables exist.
- [ ] Telemetry, audit, redaction, retention and migrations are documented.
- [ ] Package signing, deployment manifests, backups and restore drills pass.
- [ ] Known limitations and experimental ADK features are disclosed.

## 135. Open questions that do not block starting Phase 0

- Which concrete ADK 2.3.0 SessionService/ArtifactService backends best satisfy Worker persistence and encryption requirements?
- Does native ADK server provide every desired standalone worker HTTP route, or only an overlapping subset?
- Which A2A task-store persistent implementations are stable in the pinned release?
- Can the existing ADK Team receipt/resume mechanism serve remote worker orchestration directly, or does it need only task-handle binding?
- Should a Worker advertise several agent cards or one multiplexed card per endpoint?
- Which provider profiles are mandatory for the first release: local Ollama, OpenAI-compatible remote, or both?
- Does ADK's built-in browser feature suffice for the interactive browser takeover experience, or is an external session controller required?
- Are vector semantic retrieval and graph memory both mandatory for v1, or can native ADK memory be introduced incrementally?
- Is Gateway v1 strictly single-instance, or is shared-store multi-instance coordination a release requirement?
- What security isolation is required between different external application namespaces within the same Worker process?

Resolve these through official API proof and executable experiments, not speculation. The architectural boundaries and user-facing protocol can be designed independently of specific answers.

## 136. Primary official references

**Official publisher and published API**

1. ADK-Rust documentation index — https://adk-rust.com/en/docs
2. Published adk-rust crate (2.3.0 verified) — https://docs.rs/crate/adk-rust/2.3.0
3. ADK-Rust source and official examples — https://github.com/zavora-ai/adk-rust
4. ADK-Rust framework capability map — https://adk-rust.com/en/framework
5. Official quickstart — https://adk-rust.com/en/docs/quickstart
6. Runner and identity-correct cancellation — https://adk-rust.com/en/docs/core/runner
7. Graph agents, SQLite checkpoints and resume — https://adk-rust.com/en/docs/agents/graph-agents
8. Multi-agent teams, policies and execution receipts — https://adk-rust.com/en/docs/agents/multi-agent
9. A2A v1 server/client and task protocol — https://adk-rust.com/en/docs/deployment/a2a
10. A2A architecture overview — https://adk-rust.com/en/a2a
11. MCP overview and capability map — https://adk-rust.com/en/docs/mcp
12. ADK MCP toolset/client and dynamic manager — https://adk-rust.com/en/docs/tools/mcp-tools
13. CodingAgent built-in harness — https://adk-rust.com/en/docs/coding-agent/harness
14. Coding agent components — https://adk-rust.com/en/docs/coding-agent/index
15. ADK long-term memory backends — https://adk-rust.com/en/docs/memory/backends
16. Guardrails — https://adk-rust.com/en/docs/security/guardrails
17. Telemetry — https://adk-rust.com/en/docs/observability/telemetry
18. Full Rust API docs — https://docs.rs/adk-rust/2.3.0/adk_rust/

**Important version note:** the official guides may contain `2.1.0` dependency examples. This proposal uses the verified *published* 2.3.0 crate as the intended baseline but does **not** assert that every guide's sample compiles unchanged against it. Exact 2.3 symbols/feature gates must be checked during Phase 0. Some detailed guide slugs are research pointers; any missing route should be resolved through the current official index and pinned rustdoc.

## 137. Original baseline recommendation

Begin with `cargo adk new` and prove a **standalone Worker → native Runner → real/fake model → event stream → ADK SessionService** vertical slice. Next prove an ADK-native **GraphAgent + SqliteCheckpointer** restart, then **ADK A2A** between two Worker processes, and only then introduce Gateway routing/team APIs. This sequence makes it hard to accidentally build a second ADK framework under a different name. The architectural outcome is a simple, durable, strongly typed Rust harness with one reusable agent runtime, one optional distributed coordination plane and any number of independent API/CLI/business clients.

---


---

# Part XVI — Worker SDK, Native Implementation Packages, Runtime Profiles and Marketplace (Revision 1.1)

> **Design correction / normative override.** The `agents/`, `skills/`, `workflows/`, `mcp/`, `connections/`, `prompts/` and `policies/` directory tree previously proposed for a *developer Worker Package* is **not** the developer deliverable. It describes a **Runtime Profile**. A developer package is primarily Rust source code plus compiled executable capabilities, using the Goldenbee Worker SDK. Whenever earlier sections suggest that a Worker Package consists mainly of YAML agents/skills/prompts, this Part XVI supersedes that interpretation. Existing Worker/Gateway/client separation still applies. Diagrams throughout this revision use plain ASCII, not Mermaid.

## 138. Product taxonomy and ownership

| Entity | Author | Immutable? | Primary content | Runtime ownership |
|---|---|---|---|---|
| Worker SDK | Goldenbee maintainers | Versioned release | Rust API, ADK adapters, manifest & registration contracts | Linked into worker binary |
| Worker Implementation Package | Worker developer | Versioned and content-addressed release | Source, real Rust capabilities, tests, dependency lockfile, released executable | Publisher / registry |
| Worker Instance | Installer/operator | Mutable metadata, immutable binary revision | Instance ID, package version, local DB, capacity, sandbox, secrets references | Running worker |
| Runtime Profile | User/operator/admin | Versioned revisions | Agents, instructions, models, skills, workflows, MCPs, connections, policies | Instance configuration store |
| Worker Team | Operator/Gateway | Versioned roster | Master and authorized subworker instance references | Gateway or standalone master |
| Marketplace | Registry publisher | Immutable releases | Discovery, signed artifacts, compatibility, update metadata | Distribution only |

**Mandatory invariants.** A profile may *select, compose, narrow and configure* existing capabilities but cannot generate native executable features that the installed package lacks. A package may bundle optional default profiles as examples, but those defaults remain replaceable user settings and must not be confused with the package's implementation. The marketplace must not be required at worker invocation time. One published package may be installed more than once; each instance has distinct identity, state, workspace, credentials and resource limits. A master/subworker relationship is a team assignment between instances, not a package property.

### 138.1 Relationship diagram

```text
+-------------------------------+
| Goldenbee Worker SDK          |
| Rust traits, lifecycle, ADK   |
| integration, stable contracts |
+---------------+---------------+
                |
                v
+-------------------------------+       +-------------------------+
| Developer Worker Package      |------>| Publisher / Marketplace |
| Cargo source + real features  |       | verify, sign, distribute|
| build/test/release artifact   |       +------------+------------+
+-------------------------------+                    |
                                                     v
                                        +-------------------------+
                                        | Installed Worker Binary |
                                        | Instance A / Instance B |
                                        +------------+------------+
                                                     ^
                                                     |
                                        +------------+------------+
                                        | User Runtime Profile    |
                                        | agents, models, skills, |
                                        | flows, MCPs, connections|
                                        +------------+------------+
                                                     |
                                                     v
                                        +-------------------------+
                                        | Agent Gateway (optional)|
                                        | discover / teams / route|
                                        +-------------------------+
```

## 139. Developer package = actual feature implementation

**Developer's job:** create an independently buildable Rust worker using `goldenbee-worker-sdk` to register real, typed services, integrations and ADK-supported tools; write tests and package metadata; compile and publish a supported binary/container artifact. This is a source-code SDK ecosystem, not a marketplace of prompts.

**Installer's job:** obtain and verify package artifacts, configure permission grants, provision a worker instance, supply model/credential references, start it in an appropriate sandbox and optionally register it with Agent Gateway.

**User's job:** create/edit Runtime Profiles, choose built-in agents and features, provide instructions and skills, connect integrations, select the model, set workflows, and decide which capabilities each agent can use.

### 139.1 Coding Worker source tree

```text
workers/coding-worker/
|-- Cargo.toml
|-- Cargo.lock
|-- worker.toml                  # package metadata and capability declarations
|-- README.md
|-- LICENSE
|-- src/
|   |-- main.rs                  # generic worker binary bootstrapping
|   |-- lib.rs
|   |-- package.rs               # WorkerPackage implementation
|   |-- capabilities.rs          # registrations & descriptors
|   |-- repository/
|   |   |-- mod.rs
|   |   |-- git_service.rs       # only behavior not supplied by ADK
|   |   `-- worktree_service.rs
|   |-- builds/
|   |   |-- mod.rs
|   |   |-- build_profiles.rs
|   |   `-- diagnostics.rs
|   |-- environments/
|   |   |-- mod.rs
|   |   `-- resource_policy.rs
|   `-- integrations/
|       `-- mod.rs
|-- tests/
|   |-- capability_contract.rs
|   |-- workspace_isolation.rs
|   |-- git_worktree.rs
|   `-- end_to_end.rs
`-- examples/
    `-- run_local.rs
```

**Native reuse rule:** `adk-agent`'s `CodingAgent` (the documented `coding` feature) already wires `adk-devtools` workspace-bound `read_file`, `write_file`, `edit_file`, `glob`, `grep`, and `bash`, plus todo planning. The Coding Worker MUST use these APIs where requirements are satisfied. It must NOT duplicate filesystem edit/search, Bash execution, an inner model loop, or the coding todo loop just to provide a different worker identity. Add repository provisioning, worktree coordination, build metadata, diagnostics, CI integrations, or enhanced execution boundaries only where official ADK components do not meet the exact requirement. File access confinement must still be OS-enforced where untrusted code is involved; a path-scoped workspace API is not a full OS sandbox.

**Official references:** [CodingAgent harness](https://adk-rust.com/en/docs/coding-agent/harness), [Coding Agent overview](https://adk-rust.com/en/docs/coding-agent/index), [ADK quickstart/templates](https://adk-rust.com/en/docs/quickstart), [published crate API](https://docs.rs/adk-rust/2.3.0/adk_rust/). Guide code may show a different version; prove signatures in pinned rustdoc before compiling SDK examples.

### 139.2 Knowledge Worker source tree

```text
workers/knowledge-worker/
|-- Cargo.toml
|-- Cargo.lock
|-- worker.toml
|-- src/
|   |-- main.rs
|   |-- lib.rs
|   |-- package.rs
|   |-- capabilities.rs
|   |-- ingestion/
|   |   |-- mod.rs
|   |   |-- document_source.rs
|   |   |-- ingest_jobs.rs
|   |   `-- extractors.rs
|   |-- retrieval/
|   |   |-- mod.rs
|   |   `-- citation_mapper.rs
|   |-- knowledge_graph/
|   |   |-- mod.rs
|   |   `-- provenance_edges.rs
|   `-- storage/
|       `-- adapters.rs
`-- tests/
    |-- ingestion_contract.rs
    |-- tenant_separation.rs
    |-- citation_provenance.rs
    `-- restart_recovery.rs
```

**Native reuse rule:** build on ADK's `adk-rag` pipeline, embeddings, chunking, vector retrieval, memory service and existing artifacts where supported. The Knowledge Worker implements *new document source adapters, extractors, provenance logic, indexing schedules or graph-specific capabilities* only after official interface comparison. Do not ship a parallel generic RAG engine or agent memory framework. Keep business data ingestion behind authorized connectors and preserve source citation provenance.

**Official references:** [RAG documentation entry](https://adk-rust.com/en/docs), [ADK framework capabilities](https://adk-rust.com/en/framework), [published API](https://docs.rs/adk-rust/2.3.0/adk_rust/). Exact concrete RAG signatures must be checked against the pinned crate version.

### 139.3 Other reference worker families

| Worker | Real developer-implemented feature families | ADK starting point | Not a runtime profile |
|---|---|---|---|
| General Worker | optional domain-neutral connector utilities and safe operations | `LlmAgent`, Runner, tools | Yes, real adapters only |
| Research Worker | crawler connector adapters, source capture, reproducible research archives | ADK tools/MCP, graph, artifacts | Yes |
| Production Worker | media processing, audio/video transcoding, job/render control, media artifact lifecycle | ADK tool integration, artifacts, workflows | Yes |
| Designer Worker | design-file adapters, component extraction, preview/build export | ADK tool/MCP, multimodal model interfaces | Yes |
| Marketing Worker | authorized campaign-provider adapters, publishing jobs, analytics import | ADK tools/MCP + policy | Yes |

Do not create separate binaries when a meaningful variant can be implemented as a Runtime Profile of an existing worker. Publish specialized native packages only where they contribute actual executable functionality.

## 140. Goldenbee Worker SDK: exact responsibility boundary

**Worker SDK owns only harness-level integration:** registration of native capabilities, package identity and metadata, runtime context injection, runtime configuration readers, worker health/lifecycle integration, permission-scoped tool exposure, and access to existing ADK interfaces. ADK owns the agent/tool/Runner/graph/RAG/MCP/A2A implementation whenever supported. Package authors may directly use ADK traits inside the package; the SDK must not hide official capabilities behind duplicated custom engines.

| SDK concern | Intended interface | SOLID rationale | ADK boundary |
|---|---|---|---|
| Package registration | `WorkerPackage` | Single responsibility for package composition | produces ADK-native agents/tools |
| Capability publication | `CapabilityContributor` + descriptor | Open/closed extension point | registers `Tool` or services; never an alternate Tool runtime |
| Runtime context | `WorkerContext` | Interface segregation; only scoped handles | wraps ADK SessionService/artifact/model where useful |
| Capability authorization | `PermissionGrant` resolver | Dependency inversion | additional ingress and tool-boundary policy |
| Lifecycle notifications | `WorkerLifecycle` hooks | Small per-purpose interface | ADK plugins used for agent-level hooks |
| Profile resolution | `ProfileResolver` | Separate responsibilities | builds native ADK agents, workflows |
| Health & readiness | `WorkerHealthContributor` | Avoid giant package interface | not a second ADK Runner |

The following code is a **Goldenbee SDK design contract**, not published ADK-Rust or already-existing Goldenbee implementation. Preserve object safety if using `Arc<dyn WorkerPackage>`; prefer small immutable structs, explicit Result types and trait objects only where runtime substitution is necessary.

```rust
use std::sync::Arc;

pub trait WorkerPackage: Send + Sync {
    fn descriptor(&self) -> WorkerPackageDescriptor;
    fn register(&self, registry: &mut WorkerRegistryBuilder,
                context: &WorkerContext) -> Result<(), WorkerError>;
}

pub trait CapabilityContributor: Send + Sync {
    fn descriptor(&self) -> CapabilityDescriptor;
    fn register(&self, registry: &mut WorkerRegistryBuilder,
                context: &WorkerContext) -> Result<(), WorkerError>;
}

pub struct WorkerContext {
    // Holds scoped service interfaces, not global application state.
    // Exact field types decided after ADK pinned-version compilation.
}

pub struct WorkerRegistryBuilder {
    // Collects registrations of native ADK Tool/Agent and package services.
    // Validates duplicate IDs, permissions and declared dependencies.
}
```

**Example composition:**

```rust
pub struct CodingWorkerPackage {
    repository: std::sync::Arc<dyn RepositoryOperations>,
}

impl WorkerPackage for CodingWorkerPackage {
    fn descriptor(&self) -> WorkerPackageDescriptor {
        WorkerPackageDescriptor::new("goldenbee/coding-worker", "1.0.0")
    }

    fn register(&self, registry: &mut WorkerRegistryBuilder,
                context: &WorkerContext) -> Result<(), WorkerError> {
        // SDK delegates to verified, official ADK CodingAgent + devtools.
        registry.add_native_coding_agent(context)?;
        // Package-specific capability not already provided by ADK.
        registry.add_repository_service(self.repository.clone())?;
        Ok(())
    }
}
```

`add_native_coding_agent` and `add_repository_service` are **proposed SDK ergonomics**, not official ADK symbols. The implementation must delegate to the *actual* ADK API discovered in the version-pinned integration spike, using the smallest adapter possible. The SDK should contain no blanket `unwrap` in untrusted manifest/config I/O; expose typed errors without turning recoverable conditions into fatal user errors.

### 140.1 Dependency rule

```text
+---------------------------+
| Worker Package (Rust)     |
+------------+--------------+
             |
             v
+---------------------------+
| goldenbee-worker-sdk      |----->+----------------------+
| package/runtime contracts |      | ADK-Rust interfaces  |
+------------+--------------+      +----------------------+
             |
             v
+---------------------------+
| goldenbee-worker-runtime  |
| bootstrap and supervision |
+------------+--------------+
             |
             v
+---------------------------+
| standalone worker process |
+---------------------------+

Forbidden: package -> Gateway business records; SDK -> industry-specific logic;
           custom Agent/Runner/RAG/MCP engine duplicating native ADK.
```

## 141. Native package metadata versus runtime profile metadata

Native package metadata should describe **software distribution**, not user-agent customization. A minimal example:

```toml
# worker.toml - Goldenbee proposal, not upstream ADK syntax
[package]
id = "community/coding-worker"
version = "1.0.0"
name = "Coding Worker"
sdk_compatibility = ">=0.1.0 <0.2.0"

[build]
artifact_kind = "native-executable"
supported_targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]

[capabilities]
provided = ["code.read", "code.edit", "code.execute", "repo.worktree"]

[permissions]
required = ["workspace.read"]
optional = ["workspace.write", "terminal.execute", "network.connect"]

[distribution]
provenance_required = true
signature_required = true
```

Declared capabilities must be checked against those actually registered by the compiled package at startup. A declaration alone does not create the capability. Signing metadata and the release manifest bind the source revision, target triple, binary digest, SDK/ADK versions, feature flags, supported protocol revisions and verified build evidence.

### 141.1 Runtime Profile directory — user-owned configuration

```text
worker-data/
`-- instances/
    `-- instance-01/
        |-- instance.json             # identity, pinned package digest, revision
        |-- profiles/
        |   `-- my-rust-developer/
        |       |-- profile.yaml
        |       |-- agents/
        |       |   |-- coordinator.yaml
        |       |   |-- developer.yaml
        |       |   `-- reviewer.yaml
        |       |-- skills/
        |       |   `-- rust-development/SKILL.md
        |       |-- workflows/
        |       |   `-- develop.yaml
        |       |-- mcp/
        |       |   `-- servers.yaml
        |       |-- connections/
        |       |   `-- requirements.yaml
        |       |-- prompts/
        |       |   `-- defaults.md
        |       `-- policies/
        |           `-- permissions.yaml
        |-- state/
        |   `-- runtime.db
        `-- artifacts/
```

**Secrets never belong in package/profile YAML.** Profile connections store opaque references to a protected credential store. Profile files may be materialized for backup/export with secrets redacted. Runtime profiles are stored as revisioned configuration; an invocation records the immutable profile revision used at the start.

### 141.2 Profile customization and effective configuration

Apply configuration in layers: package-supported defaults -> selected Runtime Profile -> authorized instance overrides -> task-scoped restrictions -> non-overridable security ceilings. Security is **intersection-based**, not a normal last-write-wins merge. A task cannot add capabilities or permissions missing from the package, the instance grant, or the principal's authorization.

```text
Native Worker Binary ----------------------> Available Capabilities
        |                                           |
        +--> Optional default profile               |
                    |                               |
                    v                               v
          User Runtime Profile --------------> Capability Validation
                    |                               |
                    v                               v
           Instance & Task Overrides -------> Effective Revision
                                                    |
                                                    v
                                             ADK native agents
                                             + authorized tools
```

Examples of mutable profile fields: agent names/roles, instructions, skills, model-provider references, workflows, tool enablement, context budgets, MCP endpoints, connection bindings, and permitted concurrent tasks. Changes to compiled operations or native trust boundaries require a package release or separately isolated extension mechanism.

## 142. Instance lifecycle and profile revision behavior

**Install != start != configure != activate.** Installation resolves and verifies the binary package; instance creation assigns an immutable instance ID and data directory; profile activation validates the complete effective configuration; startup initializes native ADK services; Gateway registration is optional. Deactivation stops admission of new work while existing invocations retain their original effective configuration unless a supported explicit migration is performed.

```text
Developer -> Build/Test -> Sign/Publish -> Marketplace
                                         |
                                         v
User/Operator -> Select package -> Verify artifact & permission metadata
                                         |
                                         v
                            Install immutable package release
                                         |
                                         v
                            Create independent Worker Instance
                                         |
                                         v
                            Configure / validate Runtime Profile
                                         |
                                         v
                            Start worker -> Ready -> Tasks
                                         |
                                         +--> Optional Agent Gateway registration
```

Allowed instance states: `installed`, `configuring`, `ready`, `running`, `draining`, `stopped`, `faulted`, `upgrading`, and `quarantined`. Use separate lifecycle fields for process health and task state. Explicitly define what `running` means for an idle process (process alive versus task executing). On any upgrade, resolve profile compatibility in a dry-run and preserve state unless a documented migration says otherwise.

## 143. Marketplace service is a distribution plane, not a fourth execution layer

**Marketplace responsibilities:** searchable discovery, publisher verification, package metadata, immutable version listing, checksums and signatures, build provenance, known permissions and dependencies, compatibility information, artifact download, releases/revocation notices, ratings/reviews (optional), and internal/private channels. It does not need to intercept user-to-worker messages or run any ADK agent loop.

**Marketplace release artifacts:** `{package_id, version, target, binary_digest, source_digest, sdk_version, adk_version, protocol_min/max, capabilities, permission_declarations, dependency_lock, signature, provenance, evaluation_summary, downloadable_artifact}`. Use a content-addressed artifact store; never execute a registry-supplied binary merely to inspect metadata. Validate metadata against the running package's registered descriptors after installation.

**Install security gates:** trusted publisher policy, signature and digest verification, supported OS/architecture and CPU requirements, malware/dependency scan policy, explicit user/admin grant for sensitive access, sandbox/environment provisioning, credential scoping, filesystem/network policy and a local acceptance smoke test. A signed native binary may still be malicious; signatures establish provenance, not safety. Marketplace packages must never receive broad host privileges by default.

**Distribution channels:** official curated, community and private organizational registries. Support local `.gbworker` archive or OCI artifact as an implementation option **only after format ADR**; do not falsely claim this extension is already supported by ADK. Avoid creating a custom container registry if standard OCI tooling can satisfy distribution requirements.

### 143.1 Suggested registry APIs (Goldenbee-defined)

| Method | Endpoint | Purpose | Security |
|---|---|---|---|
| `GET` | `/v1/packages` | Discover by capability/category/compatibility | Public filtered metadata |
| `GET` | `/v1/packages/{publisher}/{name}` | Package details and permission declaration | Public/private scope |
| `GET` | `/v1/packages/{publisher}/{name}/versions` | Version history | Auth for private |
| `GET` | `/v1/packages/{publisher}/{name}/versions/{version}` | Immutable version manifest | Signed metadata |
| `POST` | `/v1/publish/versions` | Request publication | Verified publisher |
| `POST` | `/v1/publish/artifacts` | Finalize uploaded artifact digest | Verified publisher |
| `GET` | `/v1/artifacts/{digest}` | Download artifact | Content hash verified |
| `GET` | `/v1/revocations` | Revoked/hazardous release notices | Signed advisory feed |

Registry API should be independent of Gateway REST and Worker Control APIs. Installation APIs may live in Gateway's fleet manager for managed deployments, or in the local worker installer for standalone deployments. Do not require the marketplace for continued task execution.

## 144. Worker SDK and profile management APIs

### 144.1 Package introspection in the worker

| Method | Endpoint | Response/purpose |
|---|---|---|
| `GET` | `/v1/package` | Pinned package identity, digest, compatibility, source provenance |
| `GET` | `/v1/capabilities` | Actual registered capabilities, versions, permissions, readiness |
| `GET` | `/v1/profiles` | List local profiles permitted to caller |
| `POST` | `/v1/profiles` | Create profile revision |
| `GET` | `/v1/profiles/{id}` | Retrieve redacted effective profile metadata |
| `PUT` | `/v1/profiles/{id}` | Submit new revision with optimistic concurrency |
| `POST` | `/v1/profiles/{id}/validate` | Dry-run ADK-native composition, policy checks |
| `POST` | `/v1/profiles/{id}/activate` | Activate revision for *future* invocations |

Profile endpoints are **proposed** and must align with existing Worker protocol versioning. Any high-impact profile change requires appropriate admin rights. Do not expose raw secret values or unsanitized provider keys in responses.

### 144.2 Gateway management API additions

| Method | Endpoint | Purpose |
|---|---|---|
| `POST` | `/v1/instances` | Provision installed package instance (managed environments) |
| `GET` | `/v1/instances/{id}` | Inspect worker package/version/health |
| `POST` | `/v1/instances/{id}/upgrade` | Trigger verified managed upgrade |
| `POST` | `/v1/instances/{id}/rollback` | Restore prior compatible package revision |
| `POST` | `/v1/teams/{id}/members` | Assign instances to Master/subworker teams |

Gateway may *orchestrate* provisioning via deployment adapters but must never assume all worker hosts expose a privileged package installation mechanism. Local on-host administration remains available without Gateway.

### 144.3 CLI UX

```text
agent worker new coding-worker --sdk rust
agent worker build ./coding-worker
agent worker test ./coding-worker
agent worker publish ./coding-worker --registry <registry-url>
agent worker install community/coding-worker@1.0.0
agent worker instance create --package community/coding-worker@1.0.0 --name my-rust-developer
agent worker profile create --instance <id> --from default
agent worker profile validate --instance <id> --profile <profile-id>
agent worker start --instance <id>
agent worker instance upgrade <id> --version 1.1.0 --dry-run
```

These commands are **proposed Goldenbee CLI commands**. Reuse `cargo adk new` and upstream `cargo adk` commands for native ADK agent scaffolding, testing and building where compatible. Do not implement a parallel ADK CLI toolchain without a justified gap.

## 145. Runtime artifact compatibility and dependency policy

A published native Worker Package must state supported target triples, ABI/runtime requirements, ADK version, SDK version, protocol compatibility, and feature flags. **Two separate compatibility contracts** are necessary: a compile-time Rust SDK contract for developer packages, and a wire-level Gateway/Worker protocol contract for already-installed binaries. An API-compatible newer Gateway cannot assume an older worker binary supports every new capability.

Profile schema changes and native package changes must be independently versioned. Resolve dependencies deterministically and record digests in a lockfile or release manifest. If a package publishes default profiles/skills as assets, treat them as **seed data**, never as the authoritative mutable user profile.

### 145.1 Upgrade strategy

1. Verify target package version, artifact signature/digest, platform compatibility and revoked versions.
2. Snapshot worker metadata and export a redacted profile compatibility report.
3. Compare *actual registered capabilities* against activated profiles and team assignments.
4. Identify additional permissions; obtain explicit approval before activating a release that needs them.
5. Drain tasks or use a proven live-side-by-side deployment strategy; never swap executable code within an active task.
6. Run schema migrations with rollback prerequisites and backups. Identify irreversible migrations beforehand.
7. Activate new binary; healthcheck and verify key capabilities and ADK session compatibility.
8. If verification fails, restore compatible binary and state backup; if data migration is irreversible, quarantine and report rather than falsely claiming rollback succeeded.
9. Preserve instance ID, user name, profile revisions, workspace authorization, history and credentials references unless a documented migration requires an explicit change.

**Do not conflate rollback of the binary with rollback of persisted data.** Both need compatibility evidence.

## 146. Security and trust model for developer code

Native worker packages are full executable code. They can attempt filesystem access, process spawning and network activity independently of the model. Therefore worker-level permission prompts alone are insufficient. The installer must run third-party binaries with appropriate OS identities, containers, Linux sandboxing or comparable platform security boundaries, constrained mounts, network policies and narrowly scoped credentials.

- **Package capabilities** describe possible actions; **instance grants** define authorized actions; **agent/profile policy** narrows them; **task delegation** narrows them further.
- **Capability declarations** are not trustworthy evidence of behavior. Treat the executing binary as a separate trust domain.
- **Publisher trust tiers:** official, verified organization, community/unverified, revoked/quarantined.
- **Connections:** short-lived scoped tokens where possible, secrets stored outside package and profile files, explicit connection ownership and revocation.
- **Filesystem and terminal:** use ADK-native tools for semantics, OS sandboxing for strong process isolation.
- **RAG/documents:** validate source permissions, enforce tenant segregation, prevent accidental data indexing across principals, and preserve citation provenance.
- **Supply chain:** verify signatures, digests, reproducible or attested builds when practical, dependency advisories and SBOM.
- **Runtime profiles:** reject tool enablement outside package + instance grants; treat instructions as untrusted for authorization purposes.
- **Master/subworker:** remote delegation must carry inherited limits, not install new software or authorize connections implicitly.

## 147. Developer workflow and SOLID/OOP-style engineering guide

**S — Single Responsibility:** one package-registration object; per-capability services for repository management, document ingestion, rendering etc.; independent profile resolver and installer.

**O — Open/Closed:** developers contribute new capabilities through traits, ADK tools and adapters without modifying core scheduler/Runner code. Avoid `match package_name` inside shared runtime for product-specific behavior.

**L — Liskov Substitution:** any compliant Worker Package can register and run through the same SDK lifecycle; every advertised capability must satisfy shared request/response and error semantics. Do not advertise unsupported methods and then return permanent 'not implemented' errors.

**I — Interface Segregation:** split package identity, capability registration, configuration reads and lifecycle hooks into small contracts. Do not force a Knowledge Worker to implement Coding Worker interfaces or vice versa.

**D — Dependency Inversion:** source modules depend on small ports for repository, document store, artifact access and permission checks, with native ADK or production adapters injected at composition root. Domain modules must not import Gateway REST controller types.

**Rust adaptation:** use structs + traits + `Arc`-based dependencies with explicit ownership; OOP-like encapsulation/composition, not Java-style inheritance. Avoid supertraits and boxed futures where static dispatch is sufficient; keep object-safe interfaces only where dynamic dispatch is needed. Use typed errors for programmer/provider faults and task-level recoverable states for expected conditions. Keep blocking process/file operations off async reactor threads when appropriate.

### 147.1 Feature development contract

For each developer capability PR include: `CapabilityDescriptor`, real working implementation, API/tool registration, runtime permission map, fallback/alternate scenarios, unit tests, integration tests against actual ADK Runner, sandbox negative test, performance budget and documentation. Fake/demo tools are acceptable in tests but not as substitutes for production feature completion.

**Official ADK gate:** for any new tool, ask first whether `adk-devtools`, `adk-tool`, built-in MCP, `adk-rag`, `adk-skill`, `adk-artifact`, `adk-browser`, or the official Runner already provides it. Cite the exact upstream docs/source and write an ADR only if a custom implementation is required.

## 148. Implementation roadmap extensions

| Phase | Deliverable | Mandatory acceptance evidence |
|---|---|---|
| W0 — SDK contract audit | Pinned ADK feature proof, SDK minimal types, package/instance/profile vocabulary | Compile against pinned ADK, gap register reviewed |
| W1 — Native package prototype | `general-worker` via SDK and Worker Runtime | One compiled package, standalone task, no Gateway |
| W2 — Coding Worker | Actual ADK `CodingAgent` + coding-specific services | Read/edit/test real repository within sandbox, evidence retained |
| W3 — Profile engine | Revise agents, skills, workflows, MCP and model per instance | Two instances from same binary behave distinctly, permissions enforced |
| W4 — Knowledge Worker | ADK RAG plus real ingestion adapter | Ingest, retrieve with citations, isolation and restart test |
| W5 — Local installer | Signed artifact verification, instance lifecycle, package provenance | Reject bad digest, wrong target, revoked signer |
| W6 — Gateway integration | Register worker package identity + capabilities; route teams | Master delegates to two different packages concurrently |
| W7 — Private registry | Versioned publish/install/update/rollback | Upgrade preserves profiles; policy escalation blocked |
| W8 — Public Marketplace | Discovery, publisher identity, supply-chain control | Malicious/revoked package security simulation |
| W9 — Remaining workers | Research/production/designer/marketing real capability modules | Use-case E2E per published worker; no prompt-only stand-ins |

**Backlog granularity:** write tasks as vertical slices: source capability + ADK integration + SDK registration + profile selection + permission enforcement + API/CLI smoke test + evidence. Do not create dozens of shell services or manifest schemas before proving a real worker end to end.

## 149. End-to-end acceptance scenarios

**Scenario A — Package vs profile distinction.** Install `coding-worker@1.0.0` twice. Name the instances “Rust Developer” and “Read-only Reviewer.” Give them distinct instructions, model settings and workspace permissions. Verify both execute *the same compiled package version*, yet one may edit within granted workspace and the other cannot. Removing a profile does not uninstall the package.

**Scenario B — Native capability cannot be invented by prompt.** In a Knowledge Worker profile, add instructions to execute shell commands where no terminal capability exists. The runtime refuses execution through capability validation; the AI's words never extend the permission or feature set.

**Scenario C — ADK-native coding reuse.** Inspect SDK/Coding Worker dependencies to confirm the worker uses documented `CodingAgent`/`adk-devtools` rather than a duplicate filesystem/Bash tool engine. Run actual edit/test tasks and capture evidence.

**Scenario D — Knowledge ingestion.** Ingest a document through a real connector, index with the ADK-supported RAG pipeline, query and return traceable source references. Test a document the principal cannot access and ensure retrieval does not leak content.

**Scenario E — Registry tampering.** Attempt to install an artifact with mismatched digest or invalid signature. The installer refuses execution and does not provision credentials; audit retains the rejection reason.

**Scenario F — Upgrade.** Upgrade a Coding Worker while a queued task exists and another invocation is active. Drain/pin tasks, verify new binary, preserve configuration and history, then activate. Test an incompatible profile and confirm upgrade is stopped with a compatibility report.

**Scenario G — Mixed Master Team.** Install Research, Coding and Knowledge Workers; assign a Coding Worker instance the Master role with authorized subworkers. Submit a task requiring source research, implementation and retrieval. Verify plan, remote delegation, artifact transfer, and final evidence from every worker.

**Scenario H — Marketplace offline.** Stop registry access after installing packages. Confirm standalone workers still execute, existing teams route, and authorized active tasks do not require an external marketplace to continue.

## 150. Additional ADRs and open questions

| ADR | Decision | Status |
|---|---|---|
| ADR-021 | Developer Worker Package means compiled source capabilities, not profile YAML | Accepted architectural baseline |
| ADR-022 | Runtime Profile is independent, user-editable configuration | Accepted architectural baseline |
| ADR-023 | Worker SDK composes native ADK functions; never duplicates underlying ADK execution | Mandatory |
| ADR-024 | Native binaries are initial developer distribution unit | Proposed |
| ADR-025 | Registry is optional/offline at execution time | Accepted architectural baseline |
| ADR-026 | OS sandboxing required for untrusted executable packages | Mandatory |
| ADR-027 | Package release and runtime profile versioning are independent | Mandatory |
| ADR-028 | OCI-based artifact distribution preferred if requirements permit | Proposed, requires experiment |
| ADR-029 | Config override never escalates capability grants | Mandatory |
| ADR-030 | Default profiles may ship as optional seed assets, not developer implementation | Accepted architectural baseline |

Open choices before implementation: exact Worker SDK crate version policy; whether Marketplace ships binaries, OCI images, or both; allowed signer roots and registry trust; Linux/macOS/Windows isolation support; which specific `adk-rag` backend is stable under pinned ADK; whether to use ADK-native plugin hooks directly versus a minimal compatibility wrapper; signed update revocation and offline policy; profile schema migrations and rollback constraints; package artifact SBOM requirements; separation of package and runtime secrets stores.

## 151. Source-to-implementation matrix (new worker platform features)

| Requirement | Official ADK starting point | What Goldenbee adds | Required proof |
|---|---|---|---|
| Compiled agent execution | [Runner](https://adk-rust.com/en/docs/core/runner) | Worker Package bootstrap and stable external task ID | Actual Runner invocation |
| Coding execution | [CodingAgent harness](https://adk-rust.com/en/docs/coding-agent/harness), [Coding overview](https://adk-rust.com/en/docs/coding-agent/index) | Domain-specific repo/worktree/build services | No duplicate read/edit/Bash engine |
| RAG/knowledge | [ADK framework](https://adk-rust.com/en/framework), [docs index](https://adk-rust.com/en/docs) | Source connectors, provenance, additional retrieval modes | Query returns grounded citations |
| Custom native features | [Typed tools/framework](https://adk-rust.com/en/framework) | Worker SDK capability registration | Tool calling and permission integration |
| Profiles/skills | [Skills/agent docs](https://adk-rust.com/en/docs) | Revisioned profile persistence, user UX | Effective agent configuration verified |
| Workflows | [Graph Agents](https://adk-rust.com/en/docs/agents/graph-agents) | Declarative binding to registered graph templates | Native graph/checkpoint run |
| MCP connections | [ADK MCP tools](https://adk-rust.com/en/docs/tools/mcp-tools) | Connection grant/secrets refs | Reuse upstream MCP client |
| Remote workers | [A2A deployment](https://adk-rust.com/en/docs/deployment/a2a) | Worker capability registry, team authority | Cross-process task result |
| Artifacts and history | [ADK docs index](https://adk-rust.com/en/docs) | Package/instance metadata and provenance | Durable artifact references |
| Marketplace | No ADK marketplace contract asserted | Signed releases, registry, publisher identity, installer | Offline install and security tests |

**Official reference policy:** the general ADK documentation landing pages describe capabilities, but are not substitutes for proof of a particular 2.3.0 Rust symbol or feature gate. Every implementation PR must cite the exact upstream doc section, pinned rustdoc symbol and relevant official example or source line. If unsupported or experimental, document the gap and request an ADR rather than inventing a framework replacement.

## 152. Revised coding-agent instructions for Worker Package projects

1. Treat Worker Package Rust source as the feature deliverable; `agent.yaml`, `SKILL.md`, workflow YAML and prompts are Runtime Profile assets, not a substitute for implementation.
2. Before writing a tool, inspect ADK-Rust's official implementation. Favor native `CodingAgent`, `adk-devtools`, ADK RAG, graph, tools, sessions, skills and MCP.
3. Create worker functionality with small structs and segregated traits, using constructor injection and explicit interfaces; follow SOLID adapted to Rust composition.
4. Every capability has a registration descriptor and a real implementation; package manifests describe it but do not implement it.
5. Store custom runtime profiles outside the installed package directory, using versioned revisions; never overwrite user changes during package upgrade.
6. Treat native third-party packages as untrusted software, not merely untrusted prompts; verify artifacts and enforce OS boundaries.
7. Reuse Worker/Agent Gateway protocol contracts and events; do not put business application logic in package or SDK.
8. Prove two instances of one binary can run with different profiles; prove a profile cannot enable absent code or escalate privilege.
9. Use ASCII art in fenced `text` blocks for architecture graphs and flowcharts. **Do not use Mermaid syntax** in project Markdown.
10. A coding worker's completion must be verified by actual code changes and applicable tests; a knowledge worker's by real ingestion and grounded citations. Passing compilation alone is not completion.

## 153. Final architecture (revision 1.1)

```text
                +-----------------------------+
                | Client (CLI / SDK / API)    |
                +--------------+--------------+
                               |
                               v
                +-----------------------------+
                | Agent Gateway (optional)    |
                | worker/team/session routing |
                +--------------+--------------+
                               |
                   +-----------+-----------+
                   |                       |
                   v                       v
        +---------------------+   +---------------------+
        | Worker Instance A   |   | Worker Instance B   |
        | coding package      |   | knowledge package   |
        | profile: Rust dev   |   | profile: researcher |
        +----------+----------+   +----------+----------+
                   |                         |
                   +-------------+-----------+
                                 |
                                 v
                 +-----------------------------+
                 | ADK-Rust native interfaces  |
                 | agent / tools / graph / RAG |
                 +-----------------------------+

    Developer SDK --> Native Worker Package --> Marketplace (optional)
                                              --> Install/Verify
                                              --> Worker Instance + Runtime Profile
```

**Architecture outcome:** Goldenbee remains a three-layer, application-agnostic AI harness. The **Worker SDK** is a development extension point; the **Worker Package** is compiled executable functionality; the **Runtime Profile** is independent mutable configuration; the **Marketplace** is an optional distribution channel; the **Gateway** coordinates independent Worker Instances; the **Clients** consume stable APIs. Business servers may integrate as clients but are not mandatory layers. No Mermaid diagrams are used anywhere in this manuscript.

---

**End of revised proposal (Revision 1.1).**

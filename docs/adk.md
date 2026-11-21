# ADK compatibility status

**State: standalone execution verified.** The proof links the
pinned ADK release and runs an actual native `Runner`. It is not a claim that
every optional ADK capability is compatible or enabled.

## Verified baseline

| Concern | Verified value | Evidence |
| --- | --- | --- |
| Published umbrella crate | `adk-rust` 2.3.0 | <https://crates.io/api/v1/crates/adk-rust> and <https://docs.rs/crate/adk-rust/2.3.0> |
| Component crates | `adk-core`, `adk-agent`, `adk-runner`, `adk-session` 2.3.0 | `Cargo.lock` and the facade's pinned dependencies |
| License | Apache-2.0 | `adk-rust` package metadata at the pinned release |
| Crate toolchain | Rust 1.95 minimum, edition 2024 | Pinned crate `Cargo.toml` |
| Workspace toolchain | Rust 1.100.0, edition 2021 | `rust-toolchain.toml`; the consumer edition does not change the framework baseline |
| Feature flags used by the proof | `default-features = false`, `agents`, `runner`, `sessions` | `crates/runtime/Cargo.toml` |
| Verification date | 2026-11-18 | This record and the recorded verification run |

The official guide site currently advertises v2.1.0. Its examples predate
fields added to 2.3.0 (for example, `RunnerConfig` gained optional service and
compaction fields). The pinned 2.3.0 docs.rs page and downloaded crate source
are authoritative when the guide and the pinned release disagree.

## Executable proof

The proof is `crates/runtime/examples/adk-proof.rs`, built on
`crates/runtime/src/native/proof.rs`.

The proof moved with the ticket 02 package migration. The accepted
[repository layout](layout.md) reserves that name for the Gateway client
library and places native execution in Worker-owned runtime modules. The
commands below describe the existing proof location, not the completed target
layout. Keep the recorded evidence intact when moving the implementation.

```sh
cargo run -p runtime --example adk-proof -- --input hello --input second
cargo run -p runtime --example adk-proof -- --input hello --input second --json
cargo run -p runtime --example adk-proof -- --endpoint http://127.0.0.1:8787 --input hello
```

Focused checks:

```sh
cargo test -p runtime --test adk_proof
```

The tests prove:

- two turns in one session receive the same native session context;
- streamed model output is visible as a partial event followed by the final
  aggregate;
- canonical session history grows from two to four events;
- provider failure surfaces as `model.internal` with the provider message;
- an unknown model reference fails before execution;
- a deterministic HTTP model endpoint drives the same native Runner.

The recorded event stream from the verification run is summarized by the test
assertions above.

## Native symbols used

| Behavior | Pinned symbol | Official reference |
| --- | --- | --- |
| Execution loop | `adk_runner::Runner`, `Runner::builder()` | [Runner guide](https://adk-rust.com/en/docs/core/runner) |
| Agent | `adk_agent::LlmAgentBuilder` | [LlmAgent guide](https://adk-rust.com/en/docs/agents/llm-agent) |
| Model boundary | `adk_core::Llm::generate_content` | `adk-core-2.3.0/src/model.rs` |
| Session service | `adk_session::InMemorySessionService`, `SessionService` | [Sessions guide](https://adk-rust.com/en/docs/sessions/sessions) |
| Streaming events | `adk_core::EventStream`, `Event`, `Part::Text` | `adk-core-2.3.0/src/event.rs`, `types.rs` |
| Artifact storage | `adk_artifact::ArtifactService`, `FileArtifactService` | [Artifacts guide](https://adk-rust.com/en/docs/artifacts/artifacts), `adk-artifact-2.3.0/src/service.rs`, `file.rs` |
| Artifact runner wiring | `adk_runner::Runner::builder().artifact_service` | [Runner guide](https://adk-rust.com/en/docs/core/runner), `adk-runner-2.3.0/src/builder.rs` |
| Artifact provenance | `adk_core::EventActions::artifact_delta` | [Events guide](https://adk-rust.com/en/docs/events/events), `adk-core-2.3.0/src/event.rs` |
| Identity | `adk_core::{UserId, SessionId}` | `adk-core-2.3.0/src/identity.rs` |
| Planning semantics | `adk_agent::coding::TodoTool`, `adk_agent::coding::TodoItem` | `adk-agent-2.3.0/src/coding/todo.rs` |
| File/search tools | `adk_devtools::{ReadFileTool, WriteFileTool, EditFileTool, GlobTool, GrepTool, Workspace}` | `adk-devtools-2.3.0/src/*` |
| Tool contract | `adk_core::{Tool, Toolset, ToolContext, ReadonlyContext}` | `adk-core-2.3.0/src/tool.rs` |

The model adapter is a test boundary, not a replacement engine. It implements
the native `Llm` trait; the native `Runner`, `LlmAgent`, session service, event
stream, and turn loop remain ADK's.

## Worker development launcher

The accepted design requires native ADK-Rust Launcher CLI and web-server modes
for every Worker Package, independently of the product SDK and Gateway path.

The pinned 2.3.0 facade source distinguishes two launchers: `runner` without
`gb-client` re-exports the lightweight console launcher; `gb-client` re-exports
`adk_cli::Launcher` with console and `--serve` modes. The pinned manifest's
`gb-client` feature enables the runner, server, sessions, and telemetry dependencies.
The [pinned facade source](https://docs.rs/adk-rust/2.3.0/src/adk_rust/lib.rs.html)
is the source reference for that distinction.

Ticket 01 exercised native Runner behavior, not both full launcher modes. Its
selected flags do not enable the full `gb-client` feature. Package implementation
must compile and exercise the required launcher modes before claiming they
work; source inspection alone is not executable launcher acceptance.

## Native context compaction

Ticket 28 verifies the pinned `context-compaction` feature of `adk-runner`
2.3.0. A Worker profile revision may select native truncation with a token
budget. The runtime does not implement a summarizer or a second history engine.

| Concern | Pinned 2.3.0 symbol/source |
| --- | --- |
| Runner field | `adk_runner::RunnerConfig::context_compaction` (feature `context-compaction`) |
| Builder | `adk_runner::RunnerConfigBuilder::context_compaction` |
| Configuration | `adk_runner::compaction::CompactionConfig { strategy, context_budget, max_retries }` |
| Native strategy | `adk_runner::compaction::TruncationCompaction { preserve_recent }` |
| Proactive trigger | `estimate_event_tokens`, `apply_compaction_with_retry` before the agent call |
| Overflow taxonomy | `runner.context_overflow`, `is_token_limit_error` after a provider token-limit error |

The Runner estimates the session's token count before the model request and
applies the strategy until the context is under budget. If compaction cannot
reach the budget after `max_retries`, the pinned Runner logs the failure and
continues with the remaining context; the harness therefore requires that the
budget stay strictly below every provider context window that can serve the
invocation.

A profile's compaction setting is validated against the provider that actually
runs, including declared fallbacks. An undeclared window fails with
`provider.compaction_context_unknown`; a window at or below the budget fails
with `provider.compaction_context_unsupported`. A successful or attempted
native compaction records a durable `context.compacted` task event and a
`context.compaction` trace sample. The event carries strategy, budget, retained
event counts, the pinned profile revision, and the provider identity. It does
not carry conversation text.

Canonical history remains in `FileSessionService`: the pinned proactive path
replaces only the in-memory `MutableSession` view for the current invocation.
The original native events stay addressable, and the next turn reloads them
before deciding whether to compact again. Only the native `truncation`
strategy is exposed. Persisted LLM summary compaction
(`EventsCompactionConfig`/`LlmEventSummarizer`) and intra-invocation
summarization (`IntraCompactionConfig`) are not part of this ticket's
configuration surface.

## Limitations and next gates

- The proof uses `InMemorySessionService`. Ticket 05 replaced that in the
  production Worker path with `runtime::native::session::FileSessionService`,
  an adapter against the pinned native `SessionService`; canonical session
  events remain ADK events, not a second session engine.
- Official reference for the extension point:
  <https://adk-rust.com/en/docs/sessions/sessions>. Pinned source:
  `adk-session-2.3.0/src/service.rs`.
- Streaming exposes both a partial chunk and the final aggregate event. Clients
  must use `partial`/`turn_complete`; concatenating every event duplicates text.
- The deterministic model is intentionally credential-free. It is not a
  production provider and is not used to claim provider compatibility.
- Optional capabilities — graph checkpoints, A2A persistence, MCP, memory, RAG,
  coding tools, browser tools, sandboxing, telemetry — are not verified by
  ticket 01. Each owning ticket must re-check its pinned symbols and features.
  Ticket 12 verifies the optional `graph-sqlite` feature: native `GraphAgent`,
  `SqliteCheckpointer`, definition-bound checkpoints, restart resume, and the
  native recursion bound are covered by the graph tests.
- Ticket 13 verifies the pinned native `Tool` boundary for protected external
  effects: a Worker-owned adapter persists an operation identity before
  dispatch, records a redacted receipt after a confirmed response, and
  reconciles reserved or unknown operations through a package status or
  idempotency interface. It adds no second tool loop. See the effect tests.
- Ticket 14 verifies the pinned native confirmation boundary for protected
  operations: `LlmAgent::require_tool_confirmation`,
  `ToolConfirmationRequest`, `ToolConfirmationHandler`, and the interrupted
  event carrying the exact call. The Worker persists a digest-bound approval,
  resumes the same native thread, and uses the ticket 12 graph checkpoint for
  restart. See the approval tests.
- Ticket 25 verifies local stdio MCP through the native `McpToolset` and
  Worker-owned child lifecycle. See the local MCP tests.
- Ticket 26 verifies remote MCP Streamable HTTP through the native `McpToolset`
  and `Runner`, plus a Worker-owned destination/credential adapter. See the
  remote MCP tests.
- Ticket 24 verifies native `SKILL.md` parsing and selection through
  `adk_skill::SkillIndex`, `LlmAgentBuilder::with_skills`, and
  `adk_agent::team::WorkflowArchitectureTemplate` binding for profile workflow
  references. It adds operator-configured asset path containment, revision
  pinning for queued work, and validation that skill text cannot add
  capabilities or hooks. See the skill tests.
- Ticket 30 verifies the pinned `GraphMemoryService` valid-time graph backend,
  its SQLite persistence, and a runtime provenance adapter. It does not claim
  transaction-time history or vector retrieval. See the knowledge and memory
  tests.
- Ticket 34 verifies pinned `adk-browser` 2.3.0 (`BrowserSession`,
  `BrowserToolset`, thirtyfour 0.35 WebDriver) through Firefox/geckodriver for
  isolated sessions. It adds Worker-owned destination policy, a per-session
  egress proxy and WebDriver facade, and bounded lifecycle. Ticket 35 adds the
  Worker-owned fenced human-control adapter because the pinned crate has no
  lease, takeover, fencing, expiry, or audit facility; see `docs/control.md`
  and the control tests. Live streaming and terminal control remain
  outside the advertised capability.
- Ticket 21 verifies the pinned file-backed artifact service and runner
  `artifact_delta` provenance. The pinned `GcsArtifactService` feature, remote
  artifact fetching, and a Gateway/SDK download journey are not verified here.
- Ticket 23 verifies native model provider adapters behind logical profiles:
  `deterministic` and `http-json` against loopback fixtures,
  `openai-compatible` against a loopback OpenAI-protocol fixture, `openrouter`
  construction through its pinned client, and `ollama` against a live local
  server. It also verifies admission preflight, constrained fallback,
  credential revocation, and secret-safe inspection. These checks do not claim
  hosted provider availability or a full Coding/Knowledge package surface; see
  the model provider tests.
- Ticket 01 verifies `adk_devtools` 2.3.0 file/search tools through the runtime
  tool kernel. Pinned ADK has no multi-edit or `apply_patch` tool, so those two
  are runtime adapters over the same `Workspace` containment.
- Ticket 02 verifies `adk_agent::coding::TodoTool` item semantics through a
  durable per-task wrapper. Pinned ADK 2.3.0 supplies no PTY, background
  process, wait, kill, generic web fetch/search, image-view, or ask-user tool.
  Those are Goldenbee adapters over the existing sandbox process vector,
  approval record, task event stream, and store. The OS boundary remains
  `prlimit`/`bubblewrap`; the PTY uses `portable_pty` only as the terminal
  carrier and omits bubblewrap `--new-session` because the PTY already creates
  the session.
- Shared wire types in `common`, Client applications, and the client SDK must
  not link native ADK execution. Worker Packages and shared Worker runtime
  modules may carry it. Gateway may use verified native A2A transport without
  importing Worker execution.

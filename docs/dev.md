# Develop Goldenbee

## Check the workspace

Run from the workspace root:

```sh
make check
```

This runs formatting, Clippy with warnings denied, Rust tests, process and dependency checks, rustdoc with warnings denied, and backlog validation. GNU Make serializes these steps, including when invoked with parallel flags. Cargo defaults to two build jobs; `CARGO_BUILD_JOBS` can override that limit for the host.

Individual targets are `build`, `format`, `format-check`, `lint`, `test`, `docs`, and `backlog`. The CI workflow invokes the same `make check` entry point. Run checks after a coherent implementation change, not concurrently with another agent's Cargo build in the same checkout.

Cargo build output goes to the normal workspace target directory, or `CARGO_TARGET_DIR` when set. Do not run competing Cargo commands against one target directory. Independent worktrees should use independent output directories.

## Local provider vault

`scripts/dev.sh setup` and `make start all` enable the Gateway provider vault by
default. The script generates `.goldenbee/dev/vault-master-key` with mode `0600`,
passes it to the Gateway as `GOLDENBEE_VAULT_MASTER_KEY`, and never writes it to
Gateway JSON or a Worker config.

When `.env` contains `OPENROUTER_API_KEY` and `OPENROUTER_MODEL_KEY`:

- `scripts/dev.sh` creates the `openrouter` provider catalog entry, loads the API
  key into the Gateway through `gb-gateway provider credential set` on stdin, and
  runs model discovery against OpenRouter before the provider is marked ready.
  `OPENROUTER_MODEL_KEY` remains the preferred default; without it the dev
  script explicitly enables auto-default and selects the deterministic first
  discovered model. Discovery failure stops the script with a redacted error.
- The provider is assigned to `general-a`, `coding-a`, and `knowledge-a`; no
  key is written to Worker configuration.
- Each Worker config uses `[models] source = "gateway"` with a control token and
  no `[providers]` block or provider API key.
- The script mints a one-time pair code per Worker and runs
  `general enroll --config ... --code ...`, then starts the Workers.
- Workers fetch their assigned encrypted bundle and decrypt locally with
  `state/provider-key` (mode `0600`). Only the encrypted bundle is cached.

Without a provider key the Worker still runs in `source = "gateway"` mode with
`fallback = "deterministic"`; the missing enrollment is explicit in the Worker
log and no placeholder model is invented. Do not put a provider key in a Worker
config or on any command line. See
[Gateway API](gateway-api.md#provider-vault) and
[Worker configuration](worker-config.md#gateway-managed-model-providers).

## Read native ADK sources first

Before adding or changing Worker execution, agent, tool, toolset, session, model, graph, A2A, MCP, memory, RAG, coding, browser, sandbox, callback, or plugin behavior, read the official documentation at <https://adk-rust.com/en/docs> and the relevant pages for that capability. Then read `docs/adk.md` and the matching pinned 2.3.0 source and examples under `~/.cargo/registry/src/*/adk-*/`. The site currently documents v2.1.0; the pinned source is authoritative when they disagree. Confirm the exact symbols and feature flags against that source. Record unsupported requirements as gaps instead of inventing APIs or substituting a replacement engine, and cite the pinned references in ticket evidence.

## Follow the accepted layout

Read the [glossary](../GLOSSARY.md), [repository layout](layout.md), and [architecture decision](adr/0001-layout.md) before choosing module ownership.

Worker Packages belong under `workers/` and share `crates/runtime` with native ADK integration. Gateway and separate Client applications belong under `apps/`. Every Client uses an SDK to reach Gateway. Build the CLI journey first; web follows independently. Do not build one mandatory Client application or UI framework.

For local Worker development, use native ADK-Rust Launcher with its CLI and web-server modes. Each package exposes its real agent composition through those tools without SDK or Gateway. These are not the product Client apps. Verify the required launcher features and both modes; do not replace upstream development tools with custom implementations.

`crates/sdk` is the target Rust Gateway client library. Existing native code under that name is legacy Worker authoring code awaiting relocation. Do not link it into Clients as if it already satisfied the client SDK role. Any future Worker authoring library remains a separate concern.

`crates/common` holds small shared types and helpers. Protocol representations stay independent of ADK and storage implementations. Keep state and authorization decisions with their authoritative owners rather than expanding `common` into a universal runtime.

Use short, generic file, directory, and crate names. Match crate names to their directories and keep standard tool filenames unchanged.

## Interpret process and dependency checks

The original Python smoke checks describe scaffold-only executable behavior. Replace relevant assertions when an owning ticket adds genuine execution or management. Keep tests for unsupported input, secret-safe diagnostics, and dependency separation. Empty passing library suites do not establish feature acceptance.

Dependency checks must follow module roles as names migrate: Client apps, the client SDK, and `common` cannot transitively link native ADK execution. Worker Packages and their runtime may. Gateway can use a verified native A2A adapter but must not import the Worker execution implementation.

Use Cargo metadata to discover actual workspace members. Update workspace membership for implemented Worker Packages instead of assuming that one `apps/worker` executable represents all packages. Documentation of the target structure is not proof that the in-progress manifests and checks have already migrated.

## Verify each implementation slice

Ticket 01 records a native standalone proof. Build subsequent Worker package composition and Gateway behavior on verified native interfaces. Follow each ticket's official-source and executable-evidence requirements instead of treating the initial proof as verification of every optional feature.

Keep new public surfaces smaller than the capabilities they hide. Add concrete types after checking native interfaces. Avoid unimplemented trait forests, unused crate edges, success-returning service stubs, and copied session or task engines.

For feature acceptance, drive real processes through CLI, HTTP, SSE, or native A2A. Substitute deterministic external model and tool endpoints, not the native Runner, graph engine, persistence, or security boundary being tested. Compile-and-run fixtures separately verify Worker package integration and client SDK consumption.

Unresolved product license, publishing policy, package format, profile schema, and production configuration choices remain with their owning tickets. Do not enable publishing or advertise unverified support as part of a naming migration. Never put secrets in committed examples or Runtime Profiles.

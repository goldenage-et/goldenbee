# Goldenbee

[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

A Rust agent harness with three building blocks: **Worker, Gateway, and Client**. Different Worker Packages provide real executable capabilities. Clients are separate applications: CLI first, web later, with desktop and other applications independently addable.

**Every Client connects through an SDK, then Gateway.** SDK means a reusable client library, not a unified application. Language-specific SDKs support independent apps without including Worker execution. Workers share native ADK execution through a common Worker runtime. Runtime Profiles configure installed capabilities rather than create new executable features.

Each Worker also uses **ADK-Rust Launcher for its development CLI and web server**, without SDK or Gateway. These native development tools are separate from the product CLI and web Client applications.

The harness runs without a separate infrastructure project. The package registry is optional; if it is unavailable, registry commands report `registry server is not available` and installed Workers continue to run.

The project is pre-1.0 and under active development. Native ADK Runner execution has been verified standalone; that is not proof that every optional capability or the complete Worker-to-Gateway-to-CLI journey is implemented. Current code still uses some earlier scaffold names. See [target layout and migration status](docs/layout.md).

## Documentation

- [Documentation index](docs/README.md) — start here for the full map.
- [Architecture](docs/architecture.md) — Worker, Gateway, Client, SDK, operator tools, trust boundaries, failure behavior, and limits.
- [Deployment and usage](docs/deployment.md) — local development, standalone Worker, Gateway + Client, verified install, registry, local host lifecycle, Postgres replicas, systemd, and use cases.
- [Operations](docs/operations.md) — boot, drain, restart, upgrade, backup, recovery, and shared Gateway store details.

Detailed protocol and configuration references are linked from the index: [Gateway](docs/gateway-api.md), [Worker](docs/worker-api.md), [Worker configuration](docs/worker-config.md), [Install Goldenbee](docs/goldenbee-cli.md), [Client CLI](docs/gb-client.md), [Install](docs/install.md), [Instances](docs/instance.md), [Registry](docs/registry.md), [Local host lifecycle](docs/goldenbee-composer.md), and [Trust](docs/trust.md).

## Build and check

Install Rust through rustup, Python 3.11 or newer, and GNU Make. `rust-toolchain.toml` pins the workspace toolchain and required components. Framework compatibility evidence is recorded separately in [ADK compatibility](docs/adk.md).

```sh
make build
make check
cargo run --locked -p goldenbee-gateway -- --help
cargo run --locked -p goldenbee-client-cli -- --version
```

Rust dependencies are pinned by `Cargo.lock`. Offline builds require their sources to be cached. Validation commands describe the checks to run; they do not assert that another agent's in-progress migration already passes.

### Web workspace

`apps/goldenbee-web` and `packages/goldenbee-sdk` are a Bun workspace. The web
app imports the reusable `@goldenbee/sdk`; the SDK has no React, UI state, token
storage, or hard-coded platform runtime. All authored source and configuration
is TypeScript/TSX. `bun.lock` is committed at the repository root.

```sh
bun install --frozen-lockfile
bun run check:workspace
bun run check:ts-only
bun run typecheck
bun run lint
bun test
bun run build
```

The browser acceptance suite is the real-Gateway seam. It needs built
`gb-gateway`, `general`, and `effect-fixture` binaries and Chromium:

```sh
cargo build --bin gb-gateway --bin general --bin effect-fixture
bun run --filter goldenbee-web e2e:install
bun run --filter goldenbee-web e2e
```

CI runs the same commands in a separate `web-e2e` job. The browser bundle
remains static; Bun is the package manager and JavaScript runtime for build,
test, and development only, not a deployed Node server. `bun test` runs the
`@goldenbee/sdk` contract suite on Bun's native runner. The web app's React DOM
suite needs jsdom and runs through Vitest invoked by Bun.


Install the Gateway directly from GitHub:

```sh
curl -fsSL https://raw.githubusercontent.com/goldenage-et/goldenbee/main/install.sh | sh
```

Install the Client too:

```sh
curl -fsSL https://raw.githubusercontent.com/goldenage-et/goldenbee/main/install.sh | sh -s -- all
```

The script verifies release checksums and writes a local `gateway.json` and
`trust.json` on first install. The Gateway owns local Worker package installation
and process lifecycle; there is no separate composer process. See
[Install Goldenbee](docs/goldenbee-cli.md).

## Run the General Worker

The Worker host reads configuration at startup. Nothing is compiled in.

```sh
make worker:general
```

This builds `general` and serves `config/general.toml` on `127.0.0.1:8080`.
Override the instance configuration or address:

```sh
CONFIG=/tmp/my-worker/worker.toml WORKER_ADDR=127.0.0.1:8099 make worker:general
```

The `general` executable loads `.env` from the working directory or a parent at
startup, so `OPENROUTER_API_KEY` and `OPENROUTER_MODEL_KEY` work for direct runs
too. Existing process variables win. `GOLDENBEE_DEV_UI=1` is set by the make
target for the native ADK web UI.

```sh
make worker:general:dev                 # native console launcher
make worker:general:run INPUT="hello"   # one task, JSON result
make worker:general:stop                # stop the serve process
```

The development token comes from the configuration file. The example uses
`local-dev-token`. Read [Worker configuration](docs/worker-config.md) for the
schema and asset directories.

## Local development

Prepare isolated development state and the General, Coding, and Knowledge
Workers once:

```sh
make setup
```

Then start services independently:

```sh
make start all              # General + Coding + Knowledge + Gateway
make start gateway          # auto-starts General if needed
make start worker general
make start worker coding
make start worker knowledge
make start gb-client              # full-screen chat through Gateway + SDK
make dev                    # start all services, then open the chat
```

`make start all` prints one `goldenbee://pair/dev` link containing the Gateway,
caller token, and all Worker identities. `make start gateway` also configures
the CLI profile with the same caller token.

Inspect or stop local processes:

```sh
make dev-status
make stop gateway
make stop worker coding
make stop all
```

State, logs, generated tokens, and Worker assets live under `.goldenbee/dev`.
These are development instances using debug binaries and `GOLDENBEE_DEV_UI=1`;
they are not production deployment commands. The native ADK console remains
`make worker:general:dev`, while the Worker web UI is available at the Worker
`/ui/` route during local development.

`make setup` resets the local development state under `.goldenbee/dev`,
including the Gateway registry/dispatch projection, generated Worker state, and
CLI profiles. It does not install system services. The commands use the current
checkout only.

## Use the CLI

```sh
gb-client gw add local http://127.0.0.1:8088 gw-cli-token
gb-client gw switch local
gb-client
```

Bare `gb-client` opens the active Gateway in a full-screen chat: the context pane
shows the profile, Gateway, Worker, agent, session id, and task status. `/worker`
switches Worker, `/agent` switches agent, and `/new` starts a session. Redirected
stdin or stdout falls back to a line-oriented session. See [CLI](docs/gb-client.md).

Caller discovery and Worker administration are separate. `gb-client workers` lists
the Workers a caller may use, while `gb-client worker` commands manage Gateway
approvals and leases:

```sh
gb-client worker list --all
gb-client worker add general-a http://127.0.0.1:8099 --no-preflight
gb-client worker show general-a
gb-client worker disable general-a
gb-client worker enable general-a
gb-client worker probe general-a
gb-client worker remove general-a
```

These commands require a Gateway caller token with the `admin` scope. The local
dev stack generated by `scripts/dev.sh` uses one unrestricted `local` caller
(`scopes = ["*"]`), so no profile switch is required. When `worker add`
generates a control token it prints the token exactly once; store it on the
Worker and never expect to retrieve it again. A Worker with an
optional `[gateway]` configuration registers itself and renews a lease; host
process start, stop, install, and upgrade remain outside the Gateway. See
[Gateway protocol](docs/gateway-api.md) and [Worker configuration](docs/worker-config.md).

## Run as a service

Durability is an application-store responsibility: Worker and Gateway writes are
synced to disk before the temporary file is renamed, and the containing
directory is synced on Unix. Autostart and process supervision remain the host
supervisor's responsibility. Example Linux units are in
[`deploy/systemd`](deploy/systemd):

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now goldenbee.target
sudo systemctl enable --now goldenbee-worker@general-a.service
```

Gateway and Worker units start independently. Restarting never silently
re-submits a task: accepted work may be re-admitted, uncertain running work is
kept `recovering`, and terminal work is unchanged. Container deployments use
the container restart and readiness policies instead of systemd. Multiple
Gateway replicas require the Postgres store (`store.kind = "postgres"`); the
JSON stores are single-writer. Read
[Operations](docs/operations.md#shared-gateway-store) for the shared-store
configuration, migration, backup, rollback, boot, drain, upgrade, recovery,
journald, and container guidance. Host package install, start, stop, and
upgrade are Gateway-owned when local host roots are configured.

## Target structure

| Location | Responsibility |
| --- | --- |
| `workers/*` | Independently built Worker Packages, such as General, Coding, and Knowledge Workers |
| `apps/goldenbee-gateway` | Standalone HTTP adapter, local package cache, and Worker instance lifecycle |
| `crates/gateway-core` | Reusable Worker registry, routing, dispatch, administration, and event forwarding |
| `apps/goldenbee-client-cli` | First user-facing Client application |
| `apps/web`, `apps/desktop` | Later independent Clients, not parts of one mandatory UI |
| `crates/runtime` | Shared Worker harness and native ADK integration |
| `crates/common` | Small shared wire types and helpers |
| `crates/sdk` | Rust client library for Gateway; current legacy Worker code still needs relocation |
| `packages/sdk` | Future TypeScript client library for the web milestone |
| `tests` | Public-client, protocol, security, and recovery verification |
| `config`, `schema`, `examples` | Documented locations for verified feature artifacts |

Future directories above are planned locations, not claims that every package exists. Add real capabilities and independently useful applications rather than empty projects. The [glossary](GLOSSARY.md) distinguishes packages, instances, profiles, Clients, and SDKs.

## Project documents

- [Accepted architecture decision](docs/adr/0001-layout.md)
- [Package format decision](docs/adr/0002-package-format.md)
- [Architecture guide](docs/architecture.md)
- [Deployment and usage guide](docs/deployment.md)
- [Repository layout](docs/layout.md)
- [Development checks and testing](docs/dev.md)
- [Operations, durability, and systemd](docs/operations.md)
- [ADK compatibility status](docs/adk.md)
- [Source proposal](proposal.md)

Each feature requires its own observable acceptance evidence. Verified standalone Runner behavior does not establish optional graph, A2A persistence, MCP, memory, RAG, browser, or coding capability support.

## Contributing

The project is not accepting external contributions yet. Issues and pull
requests may be closed until the contribution workflow is published.

Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md).

## License

Licensed under the [Apache License, Version 2.0](LICENSE). Third-party crate
licenses remain with their respective authors; see `Cargo.lock` and each
package's metadata.

# Worker authoring and hosted configuration

This is the agreed target for how Worker Packages are built and run. It
supersedes ad-hoc per-package startup, environment parsing, and agent lists.
It is a design instruction, not a claim that the code already exists.

Read this before changing `crates/runtime`, `workers/*`, or any Worker startup
or configuration behavior. Read `GLOSSARY.md`, `docs/layout.md`,
`docs/worker-api.md`, and the pinned ADK source first.

## Status

- The target host API is **not implemented yet**.
- Existing code (`workers/general`) still builds agents from process
  environment and a handwritten manifest. That is the code being replaced.
- Do not mark work complete because the target shape exists in this
  document. Completion still requires executable evidence.

## Ownership

| Owner | Responsibility |
| --- | --- |
| Worker Package (`workers/*`) | Executable capabilities, native agent/tool composition, package-specific services, compiled hooks |
| Shared runtime (`crates/runtime`) | Hosting, configuration loading, asset loading, profile/authority checks, task admission, native Runner/session integration, serving, shutdown |
| Operator | The runtime config file, asset directories, instance storage, credentials, grants |
| Native ADK | Agent loop, Runner, sessions, tools, callbacks, plugins, serving |

A Worker Package is a domain application. Do not add runtime methods for
Coding, Research, or Knowledge behavior. Do not introduce an abstraction named
`domain`; it was discussion shorthand, not an API.

## Configuration is runtime input

Configuration is selected when the packaged executable runs, never compiled in.

```sh
<package> --config /path/to/worker.toml
```

- Do not hardcode `./worker.toml` or repository paths in package code.
- Resolve relative paths against the configuration file's directory, not the
  current working directory or executable location.
- The config may live anywhere the process can read.
- Config changes must not require rebuilding or repackaging.
- Config cannot create capabilities missing from the compiled package or widen
  instance/authority limits.

Target schema (TOML):

```toml
instance = "general-a"
namespace = "local"
default_agent = "general"
token = "local-dev-token"          # development only; never commit a real secret
addr = "127.0.0.1:8080"

[paths]
agents = "./agents"
prompts = "./prompts"
skills = "./skills"

[storage]
directory = "./state"

[settings]
# Package-defined values, validated by the package.
```

`token` is a development convenience. Production credential references remain
a separate issue. Do not put secret values in examples.

## Automatic asset loading

The runtime loads assets from configured directories before serving:

- `agents/*.toml`: agent definitions.
- `prompts/*.md`: prompt text referenced by agent definitions.
- `skills/*/SKILL.md`: skill catalog.

Agent definition (initial schema):

```toml
name = "general"
description = "General purpose native ADK agent"
model = "deterministic"
prompt = "general"
skills = []
tools = ["exec_command"]
```

Rules:

- Missing files, duplicate names, unknown models, unknown tools, and unknown
  skills fail startup with a stable error.
- Loaded assets form one immutable startup revision.
- Manual registration may add or override nothing silently: duplicates fail.
- Files may configure installed capabilities only. They do not load Rust code.
- Skill catalog entries are discoverable and passed through context. Native ADK
  skill binding remains pending; do not claim it before the tests verify it.

## Host API

Keep the public surface small. Target shape, not implemented:

```rust
#[tokio::main]
async fn main() -> runtime::Result<()> {
    runtime::worker::Worker::new("general", env!("CARGO_PKG_VERSION"))
        .tool("exec_command", sandbox_tool(...))
        .before_task(check_task)
        .after_task(record_outcome)
        .on_start(start)
        .on_shutdown(stop)
        .adk_plugin(audit_plugin())
        .run()
        .await
}
```

Required methods:

- `new(name, version)`.
- `tool(id, Arc<dyn adk_rust::Tool>)` for compiled tools available to agents.
- `before_task`, `after_task`, `on_start`, `on_shutdown` hooks.
- `adk_plugin(adk_plugin::Plugin)`.
- `run()`: parse CLI, load config/assets, build agents, host tasks/serving, and
  shut down.

Do **not** add:

- An `operation(...)` registration.
- A `Worker`/`Agent`/`Tool` framework that duplicates ADK.
- A custom workflow interpreter.
- A `setup`/services abstraction until a real package needs it and the design
  is agreed separately.
- A second development CLI or web server.

The host owns CLI commands:

```text
--config PATH              required for run/dev/serve/capabilities
--help, --version
capabilities [--json]
run --input TEXT [--agent ID] [--session ID] [--json]
dev [--agent ID]
serve [--addr ADDR] [--agent ID]
```

Keep the existing security behavior: loopback by default, bearer
authentication on management/native invocation routes, A2A discovery public,
delegated capability narrowing enforced.

## Agent construction

Build native agents from `LlmAgentBuilder` using the definition's model,
instruction, description, and bound tools:

```rust
let builder = LlmAgentBuilder::new(definition.name)
    .description(definition.description)
    .instruction(prompt)
    .model(model_for(&definition.model)?);

for tool in bound_tools {
    builder = builder.tool(tool);
}
```

- Agent IDs are definition names and must match compiled/registered
  capabilities.
- `default_agent` must exist; never infer from map order.
- Internal workflow children are not automatically public task targets.
- Native composition (sequential/parallel/loop/graph/custom `Agent`) stays
  package code. Do not add a runtime composition enum.

## Tools

- A registered tool means installed, not automatically authorized for every
  agent.
- An agent definition's `tools` list selects exposures.
- Effective authority still intersects instance grants, profile restrictions,
  caller/delegation authority, and task limits.
- General must keep its `exec_command` sandbox tool bound to the
  `general` agent through this mechanism. Do not drop that capability.
- Dynamic toolsets are a later, separately proved slice.

## Hooks

Two distinct categories; do not merge them.

### Worker hooks

- `on_start`: after configuration and services are ready, before accepting
  work.
- `before_task`: after durable admission, before native execution. An error
  rejects execution with a stable error.
- `after_task`: after the terminal outcome is recorded. It cannot undo effects
  or rewrite the outcome.
- `on_shutdown`: stop admission, cancel/drain bounded work, release resources.

Hooks are compiled package code. They receive read-only task/config context.
They cannot widen authority.

### Native ADK hooks

Use pinned native ADK callbacks/plugins rather than substituting a custom
interception layer.

- Per-agent: `before_callback`, `after_callback`, `before_model_callback`,
  `after_model_callback`, `before_tool_callback`, `after_tool_callback`,
  `after_tool_callback_full`, `on_tool_error` (see `adk-agent` 2.3.0).
- Global: `adk_plugin::Plugin` / `PluginManager` (see `adk-plugin` 2.3.0).
- `adk-rust` does not expose the plugin feature through its facade. Add the
  direct pinned dependencies `adk-plugin = "=2.3.0"` and
  `adk-runner = { version = "=2.3.0", default-features = false, features =
  ["plugins"] }`, and pass `PluginManager` through `Runner::builder()`.
- Mandatory authorization checks run outside hooks and cannot be disabled by a
  package or configuration hook.
- Hook ordering must be explicit in code and tests.
- Native plugin callbacks are not a sandbox and do not replace OS isolation.

## Package change: General

Replace the current environment-driven composition in
`workers/general/src` with the host:

- `main.rs`: construct `Worker`, register `exec_command`, register hooks,
  `run().await`.
- `lib.rs`: keep only package-specific helpers such as `sandbox_tool`,
  `sandbox_policy`, and workspace selection. Remove the handwritten
  `manifest()` and `build_agents()` as the source of truth; derive capabilities
  from the host registration and loaded definitions.
- Add `workers/general/example/worker.toml`, `agents/general.toml`,
  `agents/echo.toml`, `prompts/general.md`, and `prompts/echo.md`.
- Tests must supply a temporary config and asset directory. Keep coverage for
  registration, config rejection, durable sessions, sandbox behavior, native
  serving/auth, and A2A.
- Preserve the ADK proof and sandbox acceptance tests.
- Record rerun results in the pull request or commit description.

## Acceptance for this change

1. `general --config <tmp>/worker.toml capabilities` lists capabilities derived
   from assets/tools.
2. The same executable starts two instances with different config directories
   and separate storage; agents/prompts differ; no shared state.
3. `run`, `dev`, and `serve` work with the config; no env-based agent lists.
4. `exec_command` remains bound to `general` and the sandbox tests still pass.
5. Missing prompt, unknown tool, unknown skill, duplicate agent, and missing
   default agent fail startup.
6. A native `adk_plugin` callback is observed during a task; a worker
   `before_task` rejection prevents execution; `on_shutdown` runs.
7. Bearer authentication, delegation audience/capability narrowing, retained
   events, and A2A still pass their existing tests.
8. `cargo fmt`, `cargo clippy --workspace --all-targets --all-features`,
   `cargo test --workspace --all-features`, and `make check` pass.

## Coordination

Work from the default branch, keep changes focused, and do not stage, revert,
or overwrite unrelated in-progress work. If a stated acceptance criterion
conflicts with this instruction, stop and record the conflict instead of
silently changing the design.

# Worker configuration and assets

A packaged Worker is configured when it starts. The executable has no compiled
configuration path and does not consult its installation directory or current
working directory for assets.

```sh
general --config /any/readable/path/worker.toml capabilities --json
general --config /any/readable/path/worker.toml run --input hello --json
general --config /any/readable/path/worker.toml dev
general --config /any/readable/path/worker.toml serve --addr 127.0.0.1:8080
```

`--config` is required for every command. The host accepts flags before or after
the command. `--help` and `--version` do not require a configuration file.

## Environment

The `general` executable loads `.env` from the working directory or a parent at
startup. Existing process variables are not overridden. Provider credentials are
resolved only through `credential = "env:NAME"` references in `[providers]` and
are never selected by package code or copied onto tasks and events.

## Deployment paths and autostart

For a host service, keep configuration under `/etc/goldenbee/workers/` and
mutable instance state under `/var/lib/goldenbee/workers/<instance>/`. The
systemd template in [`deploy/systemd`](../deploy/systemd) passes
`--config /etc/goldenbee/workers/%i.toml`, reads secrets from a root-owned
`%i.env` file with mode `0600`, and limits writes to that instance's data
directory. `GOLDENBEE_DATA_DIR` and `GOLDENBEE_SANDBOX_DIR` should point inside
the same directory so the sandbox workspace survives a restart.

Autostart, restart, and stop are the host supervisor's responsibility. A Worker
can run standalone or register with a Gateway; neither process requires the
other to start first. Do not put tokens in `ExecStart`. See
[Operations](operations.md) for boot, drain, upgrade, backup, and container
guidance.

## TOML schema

```toml
instance = "general-a"
namespace = "local"
default_agent = "general"
token = "local-dev-token"          # development only; never commit a real secret
token_scopes = ["*"]               # scopes bound to the development token
addr = "127.0.0.1:8080"
config_revision = 1                # optional label for admitted tasks
max_streams = 64                    # optional concurrent task SSE ceiling
stream_buffer = 512                 # optional live event broadcast capacity
stream_keepalive_ms = 15000         # optional SSE keepalive interval

[paths]
agents = "./agents"
prompts = "./prompts"
skills = "./skills"
commands = "./commands"

[storage]
directory = "./state"

# Optional per-owner retention windows. Zero disables deletion for that class.
[retention]
tasks_days = 30
sessions_days = 90
checkpoints_days = 30
events_days = 30
artifacts_days = 30
memory_days = 30
documents_days = 30
audit_days = 180
traces_days = 7

[settings]
# Package-defined values, exposed through Worker context.
mode = "demo"

# Optional additional local token bindings. Production credential references
# remain a later slice; this exists for local multi-caller checks.
[[tokens]]
token = "reader-token"
namespace = "local"
principal = "reader"
scopes = ["task:read"]

[gateway]                          # optional; omit for standalone operation
url = "http://127.0.0.1:8088"
control_token = "replace-with-the-gateway-issued-token"
renew_interval_ms = 15000
advertise_url = "http://127.0.0.1:8099"  # optional; defaults to the bound listener

# Optional explicitly authorized static peers. Presence is the authorization
# decision; no Gateway discovery is required. The effective capability set is
# the intersection of this list and the peer's advertised A2A agent skills.
[peers.research-b]
identity = "research-b"             # exact remote instance id from /v1/health
endpoint = "http://127.0.0.1:8081"  # base origin; no path, query, or credentials
capabilities = ["research"]         # authorized remote agent capabilities
token_ttl_ms = 30000                # child delegation lifetime
max_depth = 1                       # child may not delegate further
max_fanout = 2                      # concurrent children under one parent
limits = { max_duration_ms = 60000, max_tokens = 20000, max_depth = 1, max_fanout = 2 }
```

Unknown fields fail startup so a typo cannot silently select another directory
or drop a policy setting.

- Relative paths resolve against the configuration file's directory. Absolute
  paths are accepted within host permissions and isolation policy.
- `token` binds principal `local` in `namespace`. Without any token binding the
  Worker starts with no accepted bearer token; management and invocation routes
  return `401`.
- Capacity fields `max_active`, `max_queue`, `per_principal`, and
- Capacity fields `max_active`, `max_queue`, `per_principal`, and
  `event_retention` are optional and default to the runtime limits. Event
  streams add optional `max_streams` (default 64), `stream_buffer` (default
  512), and `stream_keepalive_ms` (default 15000). They bound viewers only;
  execution admission is unchanged.
- `drain_timeout_ms` bounds how long admitted work may finish after admission
  closes. The default is 10000 ms. At expiry remaining execution is cancelled
  and unfinished attempts are left recoverable rather than marked failed or
  succeeded. Keep the systemd `TimeoutStopSec` comfortably above this value.
- `[retention]` is optional and applies only when an authorized operator calls
  `POST /v1/retention`. Each window belongs to its authoritative store:
  conversations, checkpoints, task metadata, durable events, artifacts,
  long-term memory, ingested documents/derived facts, audit, and telemetry.
  Zero disables deletion for that class. The newest checkpoint and all
  recovery state for an unfinished task are preserved regardless of a window.
- `default_agent` must resolve to a configured or manually registered agent.
- `[gateway]` is optional. `url` and `control_token` are required when the
  section exists. `renew_interval_ms` defaults to 15000. The heartbeat registers
  the bound listener unless `advertise_url` is set. A missing or unreachable
  Gateway never blocks local `run`, `dev`, or `serve`. The control token is a
  secret: use an operator-issued value and never commit it.
- `[peers.<id>]` entries are the complete authorization set for remote
  delegation. A peer that is not listed is never contacted. `identity` must
  match the peer's `/v1/health` instance, and `endpoint` must be a base origin
  without credentials, query, or path. Loopback may use HTTP; non-loopback
  peers require `allow_remote = true` and HTTPS. Child delegation tokens are
  signed with `GOLDENBEE_DELEGATION_SECRET`, the same deployment secret a
  Gateway uses; both Worker processes must share it. Effective capabilities are
  the intersection of `capabilities` and the peer's advertised A2A agent skills.


## Optional native browser capability

A package declares the capability in code (`.browser("browser")`); an operator
decides whether this instance may use it and where it may navigate. A profile
cannot create the toolset, and configuration that enables a browser the package
did not declare fails startup with `browser.not_installed`.

```toml
[browser]
enabled = true
webdriver_url = "http://127.0.0.1:4444"  # external driver endpoint
browser = "firefox"                      # firefox or chrome
driver = "managed"                       # managed, external, or auto
driver_binary = "/snap/bin/geckodriver"  # optional; managed/auto only
headless = true
profile = "minimal"                      # minimal, form_filling, scraping, full
allowed_hosts = ["127.0.0.1:8099"]       # explicit host or host:port allowlist
require_https = true                     # HTTPS outside loopback
max_sessions = 2
session_timeout_ms = 300000
idle_timeout_ms = 120000
max_output_bytes = 262144
downloads_dir = "browser-downloads"
```

- Enabling the section requires at least one explicit `allowed_hosts` entry.
  Empty or wildcard allowlists are rejected (`browser.policy_empty`,
  `browser.policy_invalid`). The list accepts a host (`example.com`) or an exact
  endpoint (`127.0.0.1:8099`).
- `driver = "managed"` starts one geckodriver process per native browser
  session, because geckodriver serves a single session at a time. `external`
  connects to a multi-session WebDriver endpoint. `auto` manages loopback
  Firefox endpoints when geckodriver is discoverable and otherwise uses the
  supplied endpoint. Managed mode looks at `driver_binary`,
  `GOLDENBEE_GECKODRIVER`, then standard installation paths.
- Each authenticated native session gets its own WebDriver session, Firefox
  profile, cookie jar, download directory, egress proxy, and (managed mode)
  driver process. Different principals or session ids never share a browser.
  A lost session is replaced on the next call, up to `max_sessions`.
- `session_timeout_ms` and `idle_timeout_ms` are hard ceilings. Worker shutdown,
  task-bound session release, idle expiry, and hard expiry quit the browser,
  close the adapters, and remove the session download directory.
- Navigation policy is deterministic and outside the prompt. `file:`,
  `javascript:`, `data:`, `chrome:`, and `about:` destinations are refused;
  destinations must match the allowlist; HTTPS is required outside loopback.
  DNS results are checked so an allowlisted public name cannot resolve to a
  private, loopback, link-local, or unspecified address unless the operator
  listed that address literally. A per-session loopback egress proxy enforces
  the same policy on redirects and subresources before a connection is made.
- `browser_evaluate_js` is never exposed. `browser_file_upload` is also
  withheld because model-selected uploads would read arbitrary host files. No
  human takeover or live streaming is advertised. A missing driver or failed session reports `browser.unavailable`
  and a `browser` subsystem that is not ready; other tasks on the Worker keep
  running.
- Download isolation uses one directory per session. Snap-packaged Firefox only
  writes inside the real home directory, so keep `downloads_dir` on a
  home-accessible filesystem for that packaging.
- Credentials and tokens are never placed in browser assets, cookies, prompts,
  or events. The browser capability does not accept application credentials;
  profile `secret_refs` remain Worker-owned references and are not exposed to
  the page.

## Generic file and search tools

A package opts in with `.file_tools()` (all seven) or `.file_tool(...)` /
`.builtin_tools([...])` for a narrower set. Configuration cannot create a tool
the package did not declare. A package that declares nothing exposes no generic
file or search tool.

| Tool | Source | Permission | Effect |
| --- | --- | --- | --- |
| `read_file` | pinned `adk_devtools::ReadFileTool` | `workspace:read` | none |
| `glob` | pinned `adk_devtools::GlobTool` | `workspace:read` | none |
| `grep` | pinned `adk_devtools::GrepTool` | `workspace:read` | none |
| `write_file` | pinned `adk_devtools::WriteFileTool` | `workspace:write` | `file_write` |
| `edit_file` | pinned `adk_devtools::EditFileTool` | `workspace:write` | `file_write` |
| `multi_edit` | runtime adapter over the same workspace writer | `workspace:write` | `file_write` |
| `apply_patch` | runtime adapter for the Codex `*** Begin Patch` format | `workspace:write` | `file_write` |

`multi_edit` applies `{ "path", "edits": [{ "old_string", "new_string",
"replace_all"? }] }` sequentially in memory and writes once. `apply_patch`
accepts `{ "patch" }` with `*** Add File:`, `*** Update File:` (optionally
`*** Move to:`), and `*** Delete File:` operations. Both reject paths outside
the configured workspace.

```toml
[tools]
workspace = "workspace"      # default: <storage>/workspace
timeout_ms = 120000          # per-call wall-clock deadline
max_output_bytes = 1048576   # serialized result ceiling
max_parallel = 4             # concurrent wrapped tool calls
```

Every declared and enabled tool appears in `capabilities --config <path>` as a
`tool` capability with its workspace permission. A disabled tool is absent from
the inventory, absent from agent binding, and denied by the runtime gate if a
model requests its name directly.

All tool sources share one runtime kernel: package registration,
`Worker::toolset` discovery, MCP toolset views, profile-built agents, and package
agent factories that use `settings.tool_kernel()`. The kernel applies the
timeout, cancellation, output ceiling, path lock, bounded concurrency, evidence,
and audit described below.

## Process, web, image, plan, and question tools

These capabilities are opt-in like the file tools. A package that declares
nothing exposes none of them. Configuration cannot create a tool the package did
not declare, and a profile can only narrow the selection.

### Process and PTY tools

`.process_tools(policy)` enables all five process tools;
`.process_tool(tool, policy)` enables one. The policy carries the same explicit
`SandboxPolicy` the package already supplies for `exec_command`:

```rust
Worker::new("general", env!("CARGO_PKG_VERSION")).process_tools(
    runtime::native::ProcessPolicy::new(
        runtime::native::SandboxPolicy::new(workspace).with_timeout(Duration::from_secs(30)),
    )
    .max_processes(4)
    .max_buffer_bytes(256 * 1024)
    .max_lifetime(Duration::from_secs(300)),
)
```

| Tool | Permission | Behavior |
| --- | --- | --- |
| `shell` | `process:execute` | Runs one command to completion and returns exit code plus bounded stdout/stderr. |
| `background_process` | `process:execute` | Starts one command and returns a stable `process_id` while it runs. |
| `pty` | `process:execute` | Starts an interactive terminal, or writes `input` and optionally resizes an existing `process_id`. |
| `process_wait` | `process:read` | Waits up to `timeout_ms` and drains output captured since the last read. |
| `process_kill` | `process:execute` | Sends the sandbox process-group signal and reports the observed final state. |

A process tool cannot start without an explicit policy: startup fails with
`config.process_policy`. Missing sandbox mechanisms fail closed per call with
`sandbox_unavailable`; there is no host fallback. `working_dir` must stay inside
the sandbox workspace. Every mode executes the same validated
`prlimit`/`bubblewrap` vector; only the terminal differs, and the PTY omits
bubblewrap `--new-session` because the PTY layer already creates the controlling
session.

Process ids are stable for the life of the Worker process. They are not durable
across restart; a wait or kill after restart returns `process.not_found` because
the old process no longer exists. Output is bounded per stream by
`max_buffer_bytes`; dropped bytes and truncation are reported, never silently
discarded. At most `max_processes` commands may run concurrently, and
`max_lifetime` bounds a background command that never exits.

Task cancellation cascades: the runtime binds every process to its owning task,
kills the group when the task token is cancelled, and records
`process.terminated` with `reason: "cancelled"`. Worker shutdown kills anything
still running. A kill or wait can only address a process owned by the calling
task; another task's id is denied with `process.denied`.

Process calls emit `process.started`, `tool.output` (bounded redacted chunks),
`process.exited`, and `process.terminated` before the terminal task event. The
kernel still emits `tool.started` and `tool.completed` with the normal timing,
sandbox, audit, and redaction fields.

### Web tools

`.web_tools(policy)` enables `web_fetch` and `web_search`;
`.web_tool(tool, policy)` enables one. An empty allowlist denies every
destination, so there is no implicit public network.

```rust
Worker::new("research", env!("CARGO_PKG_VERSION")).web_tools(
    runtime::native::WebPolicy::new()
        .allow_domain("docs.example.com")
        .allow_redirects(2)
        .max_bytes(512 * 1024)
        .search_url("https://search.example.com/v1?q={query}"),
)
```

| Tool | Permission | Behavior |
| --- | --- | --- |
| `web_fetch` | `web:fetch` | Fetches one URL and returns the final URL, status, content type, and bounded text. |
| `web_search` | `web:search` | Calls the configured `{query}` endpoint and returns typed results. |

The policy enforces the scheme (HTTPS unless `allow_http(true)`), an exact or
wildcard destination allowlist, and manual redirect handling with every hop
re-checked. Redirects are off unless `allow_redirects(n)` is set. The request
client ignores proxy environment variables, pins the validated DNS answer for
the request, and refuses loopback, private, link-local, documentation, and
unspecified addresses unless `allow_private_network(true)` is set explicitly
(for an operator-controlled local search service). Response bodies are capped
at `max_bytes` and marked `truncated`; non-text content types return
`web.unsupported_content` instead of a binary body. Search results whose URLs
fall outside the policy are dropped and counted as `filtered`. A Worker with no
search endpoint returns `web.not_configured`.

A policy with a search URL but no `{query}` placeholder fails startup with
`config.web_policy`. Stable fetch errors include `web.invalid_url`,
`web.destination_denied`, `web.private_network_denied`, `web.redirect_denied`,
`web.timeout`, `web.status`, `web.unsupported_content`, and
`web.search_invalid_response`.

### Image, plan, and question tools

Enable with `.builtin_tools([BuiltinTool::ViewImage, BuiltinTool::WriteTodos,
BuiltinTool::AskUser])` or `.builtin_tools(BuiltinTool::INTERACTION)`.

| Tool | Permission | Behavior |
| --- | --- | --- |
| `view_image` | `workspace:read` | Returns `{kind, path, media_type, bytes, digest, encoding, data_url}` for a PNG, JPEG, GIF, or WebP inside the workspace. |
| `write_todos` | `plan:write` | Replaces the durable plan for this task and emits `plan.updated`. |
| `ask_user` | `user:ask` | Records a question through the existing approval channel and returns a typed answer or a truthful pending status. |

`view_image` rejects non-image content with `image.unsupported` and an image
above its inline ceiling with `image.too_large`; it never returns a truncated
data URL. `write_todos` delegates item validation to the pinned
`adk_agent::coding::TodoTool`, then persists the list per task in the runtime
store. The plan survives a Worker restart and is removed with the task by
retention or erasure. The `plan.updated` payload carries the full item list,
`total`, and `remaining`; a Client renders it without runtime-owned
presentation.

`ask_user` accepts `question`, optional `options` (at most eight), and
`ttl_ms`. It records operation `user.ask` in the existing approval store,
appends a `user.question` event, and waits a bounded time. A Client decides the
approval with `ApprovalDecision { decision, digest, answer }`; the runtime stores
the answer only for `user.ask` and only when approved. The result is
`{status: "answered", answer}`, `{status: "declined"}`, or
`{status: "pending", error: "user.pending"}` after the deadline. An answer that
is not one of the offered options returns `user.invalid_answer`.

ADK reuse and gaps for this group are recorded in
[`adk.md`](adk.md): `TodoTool` supplies plan item semantics; pinned ADK 2.3.0
has no PTY, background, wait, kill, generic web, image, or ask-user tool, so
these are Goldenbee adapters over the existing sandbox, approval, event, and
store mechanisms. The process shape follows the Codex `unified_exec` contract
(start returns an id, wait drains output, kill is explicit) and the OpenCode
shell/PTY session model.

### Tool events and evidence

Mutating calls append durable `tool.started`, `tool.completed`, and `tool.effect`
events to the task stream before the terminal task event. Non-mutating calls
append `tool.started` and `tool.completed`. The stable payload fields are:

- identity: `tool`, `call_id`, `invocation`, `agent`;
- policy: `capability`, `risk`, `permission`, `sandbox`, `redaction`;
- bounds: `timeout_ms`, `max_output_bytes`, `queue_wait_ms`, `duration_ms`,
  `output_bytes`, `truncated`;
- evidence: `schema_digest`, `arguments_digest`, `result_digest`, `post_digest`,
  `diff`, and `effect_evidence` (`kind`, `paths`, `digest`, `post_digest`).

Paths are workspace-relative. Argument and result content is represented by a
digest, not copied into events. Each mutating call also writes a redacted
`tool.effect` audit entry with outcome `committed` or `failed` before the task
outcome is committed. A tool deadline returns `tool.timeout`; cancellation
returns `tool.cancelled`; neither is reported as success.

## Configured memory files

`[context]` assembles operator-selected memory files into the instruction of
every loaded agent and profile composition. The runtime hardcodes no vendor
filename; the operator names the files, globs, and precedence.

```toml
[context]
max_memory_bytes = 262144        # per-file ceiling
max_context_bytes = 1048576      # assembled ceiling
max_memory_files = 128
max_import_depth = 8

[[context.memory]]
id = "project"
paths = ["AGENTS.md", "docs/conventions.md"]
globs = ["notes/*.md"]
precedence = 100                 # higher is assembled first
imports = true                   # follow `@path` lines
optional = false                 # a missing file fails startup
```

- Sources are ordered by precedence descending, then by id. Declared `paths`
  keep their order and glob matches are sorted, so the same configuration
  produces the same context across restarts and profile revisions.
- `@relative/path` lines import another file relative to the importing file.
  Cycles, excessive depth, duplicate files, and the configured file/byte
  ceilings are reported with `context.memory_*` errors or diagnostics.
- A required source fails startup. An `optional = true` source records a typed
  diagnostic in the task `context.manifest` event instead of being silently
  omitted.
- Every admitted task appends a durable `context.manifest` event containing the
  ordered source identities, content digests, memory configuration digest,
  selected skills/tools, profile and asset revisions, and the effective
  instruction digest. Prompt and memory text are not copied into the event.
- Memory files are context input, not native long-term memory. The native
  `NativeMemory` service and `remember_memory` tool remain separate.
- A manual `session compact` or `POST /v1/sessions/{id}/compact` schedules the
  native compaction path for the next model turn. The pinned ADK Runner exposes
  no public immediate-compaction method, so the response reports
  `applies_on = "next_model_turn"`; canonical session events are retained.

## Permission modes and rules

The operator selects one mode and may add ordered rules. The policy is data in
the Worker configuration; it is evaluated before every tool dispatch in the
shared kernel, for package tools, built-in tools, MCP toolset views, and tools
bound by profile or package agents.

```toml
[permissions]
mode = "default"          # default | accept-edits | plan | full-auto
approval_wait_ms = 30000  # bounded in-tool wait before a truthful pending result
approval_ttl_ms = 900000  # how long a decision can authorize a retry

[[permissions.rules]]
id = "allow-git-status"
decision = "allow"        # allow | deny | ask
tools = ["exec_command"]
command_prefix = ["git", "status"]
priority = 10

[[permissions.rules]]
id = "deny-secrets"
decision = "deny"
paths = ["secrets/*"]
priority = 100

[[permissions.rules]]
id = "ask-remote"
decision = "ask"
domains = ["*.example.com"]
```

| Mode | Reads | File writes/edits | Deletes, commands, network, high risk |
| --- | --- | --- | --- |
| `default` | allow | ask | ask |
| `accept-edits` | allow | allow | ask |
| `plan` | allow | deny | deny |
| `full-auto` | allow | allow | allow |

A rule matches by tool-name glob (`*` and `?`), command argv prefix, resolved
workspace-relative path glob, and network host glob. Empty predicates do not
constrain that dimension. A `paths` rule matches only when at least one resolved
path matches; a `command_prefix` rule matches only when the call actually
carries a command; a `domains` rule matches only when the call actually carries
a host. Matching never trusts a model-supplied label of the tool, path, command,
or domain.

Rules are ordered data. The highest-priority matching rule group decides; at
equal priority `deny` outranks `ask`, which outranks `allow`, and the first rule
in configuration order wins an exact tie. A mode is a ceiling: an `allow` rule
can upgrade a default ask, but it cannot widen `plan`. An explicit `deny` always
wins, including in `full-auto`; `full-auto` auto-approves `ask` rules because the
mode exists to remove prompts.

### Approval identity

An ask creates one durable approval bound to the exact tool, canonical argument
digest, resolved paths, command, network domain, sandbox profile, requesting
principal, and pinned profile revision. The canonical digest of that action is
the approval digest. Repeating the same action finds the recorded decision; any
changed field produces a new digest and therefore a new decision. Argument
content appears as a digest, not as plaintext in events or audit. The
`approval.requested` event and the approval record carry the exact action, the
mode, and the matching rule, so a Client can render why work paused.

An approval can be approved or rejected through the existing approval routes.
Only the task caller or an administrator in the task namespace may decide, and
the decision must carry the digest the approver saw.

### Ask behavior

A decision is requested inside the tool call and waits up to
`approval_wait_ms`. The turn releases its execution permit while it waits, as
with any native approval.

- If a decision arrives in time, an approval allows that exact call and a
  rejection or expiry returns `permission.denied` without executing.
- If no decision arrives, the call returns a typed `approval.pending` result,
  appends an `approval.pending` event, and the native turn continues. The
  approval stays durable for a later decision, but a later retry is a new call
  and must produce the same exact action to reuse it.
- Ordinary, non-graph tasks are not resumable from a pending approval. The
  runtime does not claim that continuing the turn authorizes the action, and it
  does not claim checkpoint-based resume for a non-graph task.

### Evidence and audit

Every decision appends a `permission.decision` event before dispatch with the
final `decision`, the evaluated `evaluated` value, `mode`, `rule`, `reason`,
`paths`, `command`, `domain`, `action_digest`, the sandbox profile, and a
`policy` snapshot (`revision`, `digest`, `mode`, `rule_count`). `tool.started`
and `tool.completed` carry the same decision under `permission`.

Protected effect tools keep their own exact operation approval. When the mode
would ask for an effect tool, the kernel records the handoff through
`tool.started`/`tool.completed` and does not create a second approval; the
effect tool's `approval.requested` remains the decision evidence. An explicit
ask rule is still enforced by the kernel.

Denied calls do not execute. They append the same event and a redacted
`permission.decision` audit record whose detail names the matching rule and
decision without argument or command plaintext. An operator policy change
advances the policy revision and records a `permission.policy` audit entry.
Each tool call snapshots the policy when it starts, so a change applies only to
later calls and earlier evidence keeps the revision that actually decided it.

## Sandbox profiles and network policy

The runtime owns the sandbox policy. The operator selects one profile and may
add network, path, command, proxy, and process ceilings. The policy is evaluated
before authorization and dispatch in the same kernel, so a profile, plugin,
command, subagent, or model request can only narrow it. The package-declared OS
boundary adopts the same rules through `SandboxPolicy::with_rules(...)`.

```toml
[sandbox]
profile = "workspace-write"       # read-only | workspace-write | full-access
writable_roots = ["src", "tests"] # workspace-write only; omitted means all
protected_paths = [".git", ".agents", ".codex", ".aws"]
protected_grants = []             # explicit metadata grants
command_allow = ["git status", "cargo "]  # omitted means every command
command_max_bytes = 16384

[sandbox.network]
enabled = false                   # default: no network
allow_hosts = ["api.example.com"] # omitted means every host while enabled
proxy = "http://127.0.0.1:3128"   # exported as HTTP(S)_PROXY when enabled
proxy_bypass = ["localhost"]

[sandbox.limits]
timeout = 10000                   # milliseconds
max_processes = 4096              # RLIMIT_NPROC and prlimit --nproc
max_open_files = 256
memory_bytes = 536870912
cpu_seconds = 5
max_file_bytes = 16777216
max_output_bytes = 1048576
```

| Profile | Workspace reads | Workspace writes | Protected metadata |
| --- | --- | --- | --- |
| `read-only` | allow | deny (`sandbox.read_only`) | deny |
| `workspace-write` | allow | only authorized roots | explicit grant required |
| `full-access` | allow | allow | allowed and recorded as an exception |

`workspace-write` with `writable_roots` omitted authorizes the whole workspace.
A path outside the workspace, a `..` escape, or a symlink that resolves outside
is denied with `sandbox.path_denied`. `read-only` mounts the workspace
read-only inside bubblewrap; `workspace-write` mounts only the authorized roots
writable and overlays protected paths read-only; `full-access` mounts the
workspace writable. Network is a separate axis: when disabled, the kernel
denies every declared network tool with `sandbox.network_disabled` and bubblewrap
does not share the host network namespace. When enabled, `allow_hosts` limits
declared destinations (`sandbox.network_denied`), and the proxy environment is
exported to contained processes.

Every call appends a durable `sandbox.decision` event with the profile, decision,
stable code, workspace-relative paths, command digest, domain, policy snapshot,
and any protected-path exceptions. A denial also writes a redacted
`sandbox.decision` audit entry with outcome `deny`; `full-access` writes an
`exception` entry for every protected path it allows. A denied call never
executes; a denied mutation produces only a failed `tool.effect` record, never a
committed one.

## Agent definitions

Agent files live in the configured `agents` directory. Each `*.toml` is one
native `LlmAgent`:

```toml
name = "general"
description = "General purpose native ADK agent"
model = "deterministic"
prompt = "general"
skills = ["tone"]
tools = ["exec_command"]
toolsets = []
```

- `name` is the task target and must be unique across file definitions and
  compiled manual registrations.
- `model` is a logical provider reference. It resolves through `[providers]` in
  the operator configuration. `deterministic` and `echo` are built-in local
  diagnostics; any other reference must be declared. The legacy inline
  `http+URL` diagnostic reference remains available and reads no environment.
  Provider endpoints and credentials are never read from package code or from
  ad-hoc environment variables.
- `prompt` resolves to `<prompts>/<id>.md`.
- `skills` resolve to `<skills>/<name>/SKILL.md` catalog entries.
- `tools` and `toolsets` resolve only to capabilities registered by compiled
  package code. A configuration file cannot load Rust code or expose an
  unregistered tool.
- Missing, unreadable, malformed, duplicate, or unresolved references fail
  before the Worker starts serving. Errors use stable `config.*`, `asset.*`, and
  `worker.*` codes and do not include credential values.

Agent definitions are native `LlmAgentBuilder` inputs. Workflow composition
remains package code; this schema does not introduce a runtime workflow
language.

## Providers

`[providers.<id>]` declares a logical provider. A profile revision, agent file,
or package-registered agent names the id; the runtime preflights it before
admission and records the concrete provider, native adapter, model, locality,
residency, sanitized endpoint, credential reference, and effective profile
revision on the task.

```toml
[providers.local-json]
adapter = "http-json"          # deterministic | http-json | ollama | openai-compatible | openrouter
model = "local-model"
endpoint = "http://127.0.0.1:9000/generate"
modalities = ["text"]
tool_calling = true
local = true
residency = "local"
context_window = 32768
cost_rank = 0
input_cost_micros_per_million = 0      # optional estimate basis
candidate_cost_micros_per_million = 0
data_class = "local"
fallbacks = ["deterministic"]

[providers.remote-openai]
adapter = "openai-compatible"
model = "operator-model"
endpoint = "https://provider.example/v1"
modalities = ["text"]
tool_calling = true
local = false
residency = "remote"
context_window = 131072
credential = "env:PROVIDER_API_KEY"
cost_rank = 5
data_class = "shared"
```

- `adapter` selects a native ADK adapter. Unknown adapters fail startup.
- `endpoint` is required for `http-json` and `openai-compatible`. `ollama`
  defaults to `http://127.0.0.1:11434`; `openrouter` defaults to its hosted base
  URL. Endpoints must not embed credentials in userinfo.
- `credential` is a `secret://`, `env:`, `vault:`, or `gateway:` reference,
  never a value. `openai-compatible` and `openrouter` require one. `secret://`
  and `vault:` report `provider.credential_unavailable` until a provider is
  installed. `gateway:<id>` is produced only by Gateway-managed model loading.
- `input_cost_micros_per_million` and `candidate_cost_micros_per_million`
  optionally declare an operator price in millionths of a currency unit per
  million tokens. They only produce a labelled cost estimate; they are never
  reported as provider billing. Without them, monetary cost remains
  unavailable rather than zero.
- `allow_remote = true` is required before a non-local provider is admitted.
  A provider declared `local = true` must have a loopback endpoint.
- Preflight rejects a missing text modality, bound tools when `tool_calling` is
  false, a token budget above the declared context window (or an undeclared one
  when a budget is set), a missing or revoked credential, and an unapproved
  remote deployment.
- `fallbacks` is an explicit allowlist. A candidate must preserve locality,
  residency, credential authority, cost rank, and data-handling class; chains
  and self-fallback are rejected. Only rate-limit, timeout, or unavailable
  primary errors may fall back, and fallback happens only before any output.
  Fallback applies to a profile composition, where the selected provider is
  reported back and replaces the task's recorded provider. An agent-level
  reference resolves the primary only, so its recorded provider is always the
  one that actually ran.
- Revoking a credential blocks new provider calls through already-built models.
  No package rebuild or restart is required.

## Gateway-managed model providers

`[models]` selects where provider metadata and credentials come from. The
default is `local`, which preserves `[providers]` and environment references.

```toml
instance = "general-a"
default_model = "openrouter"       # logical provider id fetched from the Gateway
allow_remote = true                # remote Gateway assignments still need approval

[models]
source = "gateway"
gateway_url = "http://127.0.0.1:8080"
control_token = "worker-control-token"   # or inherit [gateway].control_token
worker_id = "general-a"                   # defaults to instance
private_key_path = "state/provider-key"  # defaults to <storage>/provider-key
cache_path = "state/provider-cache.json" # defaults to <storage>/provider-cache.json
refresh_ttl_ms = 60000
fallback = "deny"                        # deny | deterministic
```

- `gateway_url` and `control_token` fall back to `[gateway].url` and
  `[gateway].control_token`. The Worker id defaults to `instance`.
- The Worker creates one X25519 private key with mode `0600` when absent. The
  key is never logged or transmitted. Public-key enrollment happens once:
  `general enroll --config worker.toml --code <one-time-code>` where the code
  comes from `gb-gateway provider pair`.
- At startup the Worker fetches `GET /v1/model-providers`, decrypts only its
  assigned envelopes, and builds `ProviderRegistry` entries with
  `credential = "gateway:<id>"`. Provider ids are the Gateway catalog ids, so
  `default_model` and generated profiles select them directly.
- Every discovered or explicit model becomes a selectable alias
  `provider_id/model` with the concrete model in its profile. The bare
  `provider_id` remains the provider's preferred default. Agents and profile
  revisions may select either the provider id or a specific alias; one Worker
  process serves all models, with no per-model process.
- A provider that is not `ready` is omitted from the bundle. Empty discovered
  models, a missing default, or a failed discovery therefore fail closed instead
  of selecting a placeholder.
- The cache contains the encrypted bundle only. Plaintext credentials never
  reach disk, logs, task records, or events.
- A cached bundle is reused only while its envelopes are unexpired and its
  `refresh_ttl_ms` age permits. If the Gateway is unavailable and no unexpired
  cache exists, `fallback = "deny"` fails startup. `fallback = "deterministic"`
  explicitly selects the local deterministic adapter under `default_model`.
- Gateway-assigned remote providers are treated as operator-approved for that
  Worker; local `allow_remote = true` is still required for any provider that is
  not declared local in the Gateway catalog.
- Revocation blocks the next bundle fetch. An execution that already pinned an
  adapter keeps it until it finishes. Do not put a provider API key in this
  file when `source = "gateway"`; there is no per-Worker provider secret.



`skills/<name>/SKILL.md` files are parsed by the pinned native
`adk_skill::parse_skill_markdown` parser. The catalog is exposed through Worker
context and resolved by agent definitions and profile revisions. A resolved
agent receives a native `SkillIndex` containing only its selected skills.

Two disclosure modes exist:

```toml
[skills]
disclosure = "inherit"      # default: eager selection/injection
max_instruction_chars = 8000
max_resource_chars = 16000
```

- `inherit` keeps `LlmAgentBuilder::with_skills`: the pinned selector chooses
  the relevant skill for each user turn and injects that prompt block.
- `progressive` is available only when the package declares
  `Worker::progressive_skills()`. The agent receives `list_skills` (names and
  descriptions), `load_skill` (one body), and `load_skill_resource` (one file
  under `references/`, `assets/`, or `scripts/`). Bodies are never injected up
  front.

Pinned gap: `adk_skill::SkillToolset` records activation in session state, but
the Runner's `ToolContext::state()` returns `None` in ADK 2.3.0, so native L3
always rejects an activated skill. The runtime adapter uses the pinned
`adk_skill::build_skill_system_instruction` and `SkillIndex` but reads the
immutable index directly. It never adds a tool: `allowed-tools` is validated
against tools already bound to that agent. Scripts are readable resources only;
they are never executed by the skill surface. To run one, the model must call
the ordinary sandboxed command tool under its normal approval policy.

`commands/*.toml` files define Worker slash commands and are loaded only from
the operator-selected `paths.commands` directory. The file stem is the command
name; subdirectories use Gemini-style namespacing (`review/security.toml` becomes
`review:security`). Unknown fields, duplicate names, malformed placeholders, and
`!{...}` shell interpolation fail startup.

```toml
description = "Summarize a topic"
prompt = "Summarize {{args}} for an operator audience."
allowed_tools = ["read_file"]
model = "fast"                 # must be a model the Worker already declares
agent = "general"              # must be a loaded (file-defined) agent

[[arguments]]
name = "audience"
default = "operator"
```

Declared arguments are parsed as `--name value` or `--name=value`. Missing,
unknown, duplicate, positional, and unclosed-quote inputs fail with
`command.*` errors before task admission. A command may only narrow the target
agent: `allowed_tools` must be a subset of the tools that agent already binds,
and the command model must be a model already declared by an agent or the
default model. A command with `allowed_tools` drops dynamic toolsets, MCP
servers, and skill binding because those surfaces cannot be proven narrower.

Asset files must resolve inside their configured directory. A prompt, agent, or
`SKILL.md` symlink that escapes the configured root fails `asset.path_denied`
before a candidate revision is activated.

## Worker authoring

```rust
let worker = runtime::worker::Worker::new("general", env!("CARGO_PKG_VERSION"))
    .feature("sandbox")
    .required_permissions(["task:submit", "session:write"])
    .agent("manual", manual_agent)
    .tool_with_permissions(
        "exec_command",
        sandbox_tool,
        ["exec:command", "workspace:write"],
    )
    .toolset("dynamic", toolset)
    .callbacks(native_callbacks)
    .adk_plugin(audit_plugin)
    .on_start(start_hook)
    .before_task(before_task_hook)
    .after_task(after_task_hook)
    .on_shutdown(shutdown_hook);
worker.run().await?;
```

- Loaded file agents are the default composition.
- Manual native agents, tools, and toolsets are optional registrations.
- Every hook is compiled package code. Configuration can select package-defined
  settings but cannot execute scripts from an asset directory.
- `Worker::manifest()` derives the installed capability inventory from compiled
  registrations. `capabilities` reports that list separately from configured
  agent names.
- `runtime` in release metadata comes from `runtime::RUNTIME_VERSION`, not the
  package's own version, so independently versioned packages remain compatible.
- `feature(...)` declares a compiled native feature (for example `rag` or
  `sandbox`) in signed metadata. The installer compares the full declared set
  with its trust policy.
- `tool_with_permissions(...)` declares the permissions a tool exercises. The
  installer sees them in `CapabilityDescriptor.permissions` and gates them like
  package-level `required_permissions`.

## Worker-native profile management

Profiles are managed without Gateway or an SDK:

```sh
general --config worker.toml profile list --json
general --config worker.toml profile show research --revision 1
general --config worker.toml profile create research \
  --instruction "Answer as a reviewer." --model deterministic \
  --composition single --member general --capability general \
  --skill tone --grant task:submit --secret-ref secret://reviewer-key --activate
general --config worker.toml profile create review-chain \
  --instruction "CHAIN" --workflow sequential --member general --member echo \
  --capability general --capability echo --grant task:submit --activate
general --config worker.toml profile revise research --expected 1 \
  --instruction "Answer as a strict reviewer." --activate
general --config worker.toml profile activate research --revision 2
general --config worker.toml profile deactivate research
general --config worker.toml profile delete research
general --config worker.toml profile export research
```

- Management uses the principal bound to the configuration's development
  `token` and `token_scopes`; `profile:read` and `profile:write` are enforced.
- Revisions are immutable and appended. `--expected` is the optimistic
  concurrency check; a stale writer receives `profile.revision_conflict`.
- Activation validates the complete revision first. Invalid models, members,
  capabilities, instance grants, skills, compositions, or restriction conflicts
  leave the previous active revision unchanged.
- `profile export` redacts package settings and profile instructions, and
  contains only secret references from `secret_refs`; token values are never
  exported.
- `run --profile NAME` and `dev --profile NAME` exercise a profile through
  native execution. HTTP submission sets `"profile": "NAME"` in
  `SubmitTaskRequest`.
- A profile revision with `members` rebuilds the selected file-defined native
  agents with that revision's instruction, model, tools, toolsets, and skills.
  Skills are bound through the native selection index, so only the matching
  skill is injected into a turn.
- `--workflow` binds members and limits to a registered native template:
  `single`, `sequential`, `parallel`, `loop`, `team`, `fan-out-fan-in`, or
  `review-loop`. Templates lower through pinned `adk-agent`
  `WorkflowArchitectureTemplate`/`TeamArchitectureTemplate` values; the runtime
  validates parameters before activation and does not add a workflow
  interpreter. `fan-out-fan-in` uses the final member as the aggregator;
  `review-loop` requires exactly one producer and one reviewer.
- A revision without members keeps the configured base agent, which preserves
  the seed `default` profile.
- Effective authority intersects compiled registrations, instance grants,
  principal scopes, profile capabilities/grants/restrictions, and task limits.
  A profile that no longer grants `task:submit` blocks new admission.
- In-flight and queued tasks pin the asset revision and profile revision
  selected at admission. Activating a newer revision affects only newly
  admitted work.

### Context compaction

A profile revision may select the pinned native Runner truncation strategy:

```sh
general --config worker.toml profile create long-chat \
  --instruction "Continue the conversation." --model local-json \
  --capability general --grant task:submit \
  --compaction-budget 8192 --compaction-preserve-recent 8 \
  --compaction-max-retries 2 --activate
```

- `--compaction-budget` is an estimated token budget, not a provider billing
  limit. Compaction triggers before a request when the native estimate exceeds
  it.
- `--compaction-strategy` accepts the supported native value `truncation`.
  Unknown strategies fail profile creation or activation; the runtime does not
  install a custom summarizer.
- `--compaction-preserve-recent` keeps the most recent native events verbatim.
  `--compaction-max-retries` bounds native truncation attempts.
- The budget must remain strictly below the selected provider's declared
  `context_window`, and below every declared fallback window for that provider.
  An unknown window is `provider.compaction_context_unknown`; an insufficient
  window is `provider.compaction_context_unsupported`. Both fail before a task
  is recorded.
- A profile with no members resolves the provider declared for the base agent.
  If that agent has no declared provider reference, compaction is rejected with
  `provider.compaction_unverified` rather than guessing.
- Compaction appends a durable `context.compacted` task event and a
  `context.compaction` trace. Canonical native session events are retained, so
  later turns can still access earlier turns, artifact deltas, and source
  references.
- Absent a compaction setting, revisions keep the pinned Runner default and no
  compaction occurs.
- A configured cost budget requires operator-declared input and candidate
  pricing on the selected provider and every declared fallback. Without that
  basis the profile or task is rejected with `budget.cost_unverifiable` before
  admission instead of reporting invented usage.

## Lifecycle and native hooks

Worker hooks run at real runtime points:

| Hook | Point |
| --- | --- |
| `on_start` | after configuration, assets, storage, recovery planning, and native execution are ready; before admission opens or a listener accepts work |
| `before_task` | after durable task admission; an error rejects execution and records a failed task |
| `after_task` | after the terminal outcome is persisted; an error is recorded as redacted evidence and cannot change the outcome |
| `on_shutdown` | after admission stops and admitted work drains; before native plugins close |

Startup order is `initializing → recovering (reconciliation plan) → warming
(on_start) → ready (admission opens) → recovered accepted work dispatched`.
A failed `on_start` leaves the phase `failed`, never reports readiness, and
does not dispatch recovered work.

Shutdown order is admission stop and stream close → bounded drain → `on_shutdown`
→ native plugin/executor close → instance-owned resource cancellation →
`stopped`. A failed hook or expired drain still runs plugin close and owned
resource cleanup, records explicit degradation, and ends in `stopped_degraded`
with a non-zero host exit instead of a clean stop. The sequence runs once;
repeated shutdown calls observe the same report.

`TaskHookContext` carries the durable `task_id` in both admission and
after-task hooks, plus the frozen namespace, caller, agent, session, profile
revision, asset revision, input, settings, and cancellation token. Package code
can attach evidence or audit records to the exact task without re-reading a
shared mutable projection.

Native ADK callbacks are attached per agent with `LlmAgentBuilder`:
`before_callback`, `after_callback`, `before_model_callback`,
`after_model_callback`, `before_tool_callback`, and `after_tool_callback`.
Automatically loaded agents receive the package callback factory; manually
registered agents can attach the same callbacks when package code builds them.

Run-scope native plugins use the pinned `adk_plugin::Plugin` and `Runner` plugin
manager. The Runner dispatches `before_run`, `on_user_message`, `on_event`, and
`after_run`. The classic plugin's `before_agent`, `after_agent`, `before_model`,
`after_model`, `before_tool`, `after_tool`, `on_model_error`, and `on_tool_error`
fields exist in 2.3.0 but are not reached by the Runner dispatch path; use
per-agent callbacks for agent/model/tool interception. Do not claim those
classic plugin hooks.

Mandatory authentication, namespace/capability admission, and profile
validation run outside hook code. A plugin or callback can short-circuit a valid
run but cannot create admission for an unknown agent or bypass bearer
authentication.

## Reload

`SIGHUP` validates a complete candidate configuration and asset set. The
candidate must keep the same configured agent ids. On success, new invocations
use the new executor and a higher `config_revision`/asset revision. Active
invocations keep their previously resolved native agent. A malformed or
unresolved candidate leaves the old revision and executor active. There is no
filesystem watcher.

## Coding package

The Coding Worker uses the same shared runtime and operator-selected
configuration. It adds two compiled native agents and one instance-scoped
service:

- `developer` is a writable native coding agent bound to the instance worktree.
  It composes the pinned `adk_agent::coding::CODING_SYSTEM_PROMPT`,
  `adk_agent::coding::TodoTool`, and `adk_devtools::DevToolset` through
  `LlmAgentBuilder` so the shared runtime tool kernel wraps every developer
  tool. Its native `bash` tool is disabled; commands run through the shared
  runtime sandbox as `exec_command`.
- `reviewer` uses the same composition over a read-only workspace. The native
  devtools toolset omits `write_file`, `edit_file`, and `bash`, and no
  `exec_command` tool is attached.
- `adk_agent::coding::CodingAgent` itself does not accept an externally wrapped
  toolset, so the package composes the same pinned ADK primitives directly
  rather than binding an un-wrapped convenience agent.
- Repository, worktree, build, and evidence services are package-owned and
  constructed once per process. A changed repository setting requires a restart
  rather than silently rebinding a live checkout.

Required `[settings]` keys:

```toml
[settings]
workspace_root = "../.goldenbee/scratch/coding/workspace"  # relative to this file
repo = "repo"                                    # below workspace_root
base_ref = "HEAD"                                # detached checkout
checks = "sh tests/check.sh, sh tests/lint.sh"   # comma or newline separated
build_parallelism = "1"                          # per-instance command slots
max_build_seconds = "120"
max_output_bytes = "262144"
max_memory_bytes = "1073741824"
max_processes = "8192"
developer_model = "deterministic"
reviewer_model = "deterministic"
```

Each instance owns `<workspace_root>/worktrees/<instance>` and an exclusive
`<instance>.owner` file. Two instances from the same repository use distinct
worktrees and `CARGO_TARGET_DIR` values. A second live writer for one instance
is rejected with `coding.workspace_busy`.

`checks` run again after the native model stops. Task evidence is attached to
the task projection and stored as native artifacts with digests. A model turn
cannot mark a coding task accepted by itself: acceptance requires a non-empty
diff, at least one configured check with exit code 0, digest verification, and
no repository change outside the owned worktree. Cancelled tasks skip checks,
record that limitation, and remove their checkout.

Roles are assignments, not executables. The one `coding` binary exposes
`developer` and `reviewer` agents; profiles may select either compiled agent
directly, and a sequential or team composition can connect them for a Master
role. File-defined agents in the configured asset directories remain the normal
customization path and can reference `exec_command`.

## Isolation

One process hosts one Worker Instance. A second process of the same executable
with another configuration file has independent assets, identity, token
bindings, settings, durable task state, and native session storage. Mutable
services are not shared implicitly between instances.

The ticket 32 sandbox command tool remains a compiled registration. Its
mechanism and workspace selection remain deployment environment inputs
(`GOLDENBEE_SANDBOX_*`), not asset-file configuration.

## Compatibility notes

Official documentation read for this slice:

- <https://adk-rust.com/en/docs/agents/llm-agent>
- <https://adk-rust.com/en/docs/core/plugins>
- <https://adk-rust.com/en/docs/callbacks/callbacks>
- <https://adk-rust.com/en/docs/sessions/sessions>
- <https://adk-rust.com/en/docs/deployment/launcher>
- <https://adk-rust.com/en/docs/deployment/server>
- <https://adk-rust.com/en/docs/skills/skill-registry>

The site documents v2.1.0. This package uses pinned `adk-rust` 2.3.0. Pinned
symbols: `adk_agent::LlmAgentBuilder`, `adk_core::{BeforeModelCallback,
AfterModelCallback, BeforeToolCallback, AfterToolCallback}`,
`adk_plugin::{Plugin, PluginManager}`, `adk_runner::Runner::builder`,
`adk_runner::RunnerConfigBuilder::plugin_manager`, `adk_skill::parse_skill_markdown`,
`adk_server::{create_app_with_a2a, ServerConfig}`, and `adk_rust::Launcher`.

Known gaps:

- Classic plugin agent/model/tool callbacks are not dispatched by the pinned
  Runner. Per-agent callbacks cover those points.
- Enhanced plugins (`adk_agent::LlmAgentBuilder::enhanced_plugin`) are not used;
  they overlap the per-agent callback surface and are not needed for this slice.
- Skill binding uses the pinned native `SkillIndex`/`LlmAgentBuilder::with_skills`
  selection path, or `adk_skill::build_skill_system_instruction` with a bounded
  runtime L1/L2/L3 adapter when the package and operator select progressive
  disclosure. `adk_skill::SkillToolset` activation checks cannot be used because
  the Runner's `ToolContext::state()` is `None`; that gap is documented above.
  A filesystem watcher is not installed; operators explicitly validate and
  activate a candidate with `reload`/SIGHUP.
- `before_task` runs after durable admission, so a rejected task has a durable
  `failed` record rather than disappearing.

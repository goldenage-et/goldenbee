# CLI

The CLI is an independent Client. It reaches Workers only through the SDK and
Gateway. Gateway profiles are stored locally so repeated use does not require
flags.

## Gateway profiles

```sh
gb-client gw add local http://127.0.0.1:8088 gw-cli-token
gb-client gw list
gb-client gw detail local
gb-client gw switch local
```

`add` creates or updates a profile. The first profile becomes active. Tokens
are stored in SQLite; list and detail output show only a masked value.

Profiles live in `$GOLDENBEE_CLI_HOME/cli.db`, otherwise
`$XDG_CONFIG_HOME/goldenbee/cli.db`, otherwise `$HOME/.config/goldenbee/cli.db`.
The directory is `0700` and the database is `0600`.

## Interactive session

```sh
gb-client                 # active profile
gb-client chat            # explicit active profile
gb-client gw open local   # named profile
gb-client chat --worker general-a --agent general
gb-client manage          # open Worker, Provider, and Model management
```

`chat` is the explicit session command. Bare `gb-client` remains an alias so
existing scripts and muscle memory keep working. `Ctrl+P` toggles management
from chat, and `manage` opens it directly. The management shell uses dedicated
pages instead of modal lists:

```text
+ goldenbee · local -----------------------------+
| Chat | Workers | Providers | Models            |
+-------------------------------------------------+
| page content                                    |
+-------------------------------------------------+
| status / hints                                  |
+-------------------------------------------------+
```

With a terminal on stdin and stdout the session opens a full-screen chat drawn
with Unicode borders. The top pane shows the active profile and Gateway, the
selected Worker and agent, the conversation session id, and whether a task is
running. Every message uses the full conversation-pane width. User messages
appear as full-width rows with a lightly shaded background that covers the text and both
padding columns, including every wrapped line. Assistant messages are
transparent and keep the same full width, with no block border or background.
Neither role adds a `you>` or `ai>` prefix. Local status and errors appear as
inline rows, and one blank row separates messages. The bordered input box keeps
a visible cursor and scrolls horizontally for long messages.

When stdin or stdout is redirected, the CLI keeps a line-oriented session so
scripts and tests can pipe input and read the streamed transcript.

Typing `/` opens a command popover above the input box. It filters as you type;
`Up` and `Down` move the highlight, `Enter` runs the highlighted command, `Tab`
completes it into the input line, and `Esc` closes the popover without leaving
the chat.

### Keys

| Key | Action |
| --- | --- |
| `Enter` | Submit the current line |
| `Up` / `Down` | Recall history when the input is empty, otherwise scroll |
| `PgUp` / `PgDn` | Scroll one page |
| `Home` / `End` | Scroll to the start or end when the input is empty, otherwise edit the line |
| `Left` / `Right` | Move the cursor |
| `Backspace` / `Delete` | Delete before or under the cursor |
| `Tab` | Complete the highlighted command |
| `Ctrl+U` / `Ctrl+W` | Delete to the start or the previous word |
| `Ctrl+T` | Show or hide model reasoning |
| `Ctrl+O` | Show compact or detailed tool calls |
| `Ctrl+L` | Redraw |
| `Esc` | Close the command popover or picker; otherwise leave the chat |
| `Ctrl+C` | Leave the chat |

Bracketed paste inserts text on one line. While a task runs, model text keeps
arriving; the view follows new text unless you scrolled up, and the context pane
shows a `running` indicator. Escape and `Ctrl+C` restore the terminal, cursor,
and screen on every exit path, including task errors.

### Chat commands

```text
/worker [id]   list or switch Worker
/worker add    open the Worker form
/workers       open the Workers management page
/providers     open the Providers management page
/provider add  open the provider form
/models        open the Models management page
/manage        open management
/agents        list agents on the current Worker
/agent [name]  list or switch agent
/gateway       show the current Gateway
/thinking [on|off]  show or hide model reasoning
/tools [detail|compact]  show tool-call details
/new           start a new session id
/help
/exit
```

`Tab` and `Shift+Tab` cycle management pages. `Esc` returns to chat from
Workers or Providers and returns to Providers from Models. Dialogs are used only
for confirmation, small text, secrets, and selection.

`/worker` and `/agent` without an argument open a selection list. Switching
Worker keeps the current session id; switching agent keeps it too. `/new` starts
a fresh session id. Recovery guidance from Gateway errors is added to the
conversation instead of closing the interface.

Each submitted line goes through the SDK. The CLI follows the Gateway task
stream and renders model text as token deltas arrive. The final task projection
is authoritative: if the live stream is unavailable or drops, the CLI falls
back to polling and appends the complete result. Deltas are non-durable, so a
reconnect can skip token fragments without losing the terminal outcome.

Reasoning deltas render in a separate `Thinking` block. The block is collapsed
by default and shows only its character count; `Ctrl+T` or `/thinking on`
expands it. Tool calls render inline as `→ tool` followed by a pending, ok, or
error result. `Ctrl+O` or `/tools detail` adds the agent, server, error code,
and duration. Thinking text is never merged into the answer, and credentials or
raw tool payloads are not printed.

`gb-client watch <id>` exposes the same SDK journey without the full-screen interface:
it prints retained and live events, resumes once from the last durable sequence
after a transport drop, exits on the terminal event, and never resubmits the
prompt. `--after <seq>` starts from an explicit cursor.

## Provider administration

Provider commands require a caller token with `admin` or `provider:admin`.

```sh
gb-client provider list [--json]
gb-client provider show <id> [--json]
gb-client provider add <id> --adapter <kind> [--model <id>] \
    [--endpoint <url>] [--modality <text>] [--allowed-model <id>] \
    [--auto-default] [--tool-calling] [--context-window <n>] \
    [--local] [--residency <label>] \
    (--worker <id>... | --all-workers) [--json]
gb-client provider update <id> --revision <n> [metadata flags] \
    [--enable|--disable] [--json]
gb-client provider discover <id> [--json]
gb-client provider credential set <id>
gb-client provider credential revoke <id> [--json]
gb-client provider pair --worker <id> --instance <name> [--json]
```

`provider credential set` reads the key from stdin only. The key is never
accepted on argv, printed in JSON, or written to CLI history.

`provider pair` prints a one-time Worker enrollment code. Treat the code and
link as secrets.

## Model management

Models are discovered by and owned by one provider; they are not standalone
Gateway entities.

```sh
gb-client model list [--provider <id>] [--json]
gb-client model allow <provider> --model <id>... [--json]
gb-client model clear <provider> [--json]
gb-client model preferred <provider> --model <id> [--json]
gb-client model discover <provider> [--json]
```

In the Models management page, `space` toggles an allow-list entry, `enter`
sets the preferred model, `c` clears the allow-list, `r` rediscovers, and
`esc` returns to Providers.

## Artifacts

```sh
gb-client artifact list <task-id> [--json]
gb-client artifact show <task-id> <artifact-id> [--json]
gb-client artifact download <task-id> <artifact-id> --output <path> [--force]
```

Downloads use the SDK's digest-verified artifact transfer. The CLI refuses to
overwrite an existing path unless `--force` is present.

## Management pages

- Workers: `enter` inspect, `a` add, `e` edit, `p` probe, `d` disable,
  `E` enable, `x` remove with confirmation, `l` audit, `/` filter, `r` refresh.
- Providers: `a` add, `e` edit metadata, `s` set a masked credential, `R`
  revoke with confirmation, `d` discover, `x` enable/disable, `w` edit Worker
  assignments, `m` manage models, `p` pair a Worker and show a one-time code,
  `r` refresh.
- Models: `space` toggle allowed, `enter` set preferred, `c` clear the
  allow-list, `r` rediscover, `esc` back to Providers.

Every Gateway call made by a management page runs in a background task and
returns through the UI event channel, so the event loop never blocks on the
SDK. The local dev stack grants its single `local` caller `scopes = ["*"]`, so
management pages work without switching profiles. A denied action on a scoped
deployment reports the required scope, for example
`admin/provider:admin scope required; switch to an admin profile`.

Credential input is masked in the dialog. The value is never rendered, written
to status text, or included in logs.

## Worker administration

Worker administration is separate from caller discovery. `gb-client workers` is the
caller-scoped discovery view and hides disabled or decommissioned identities.
`gb-client worker` commands use the Gateway admin API and require an `admin` token.

```sh
gb-client worker list [--all] [--json]
gb-client worker show <id> [--json]
gb-client worker add <id> <endpoint> \
    [--instance <id>] [--package <name>] \
    [--capability <id>]... [--control-token-stdin] \
    [--no-preflight] [--json]
gb-client worker update <id> --revision <n> \
    [--endpoint <url>] [--instance <id>] [--package <name>] \
    [--capability <id>]... [--json]
gb-client worker disable <id>
gb-client worker enable <id>
gb-client worker probe <id>
gb-client worker remove <id>
gb-client worker audit [--worker <id>] [--json]
```

- Human-readable output is a table. `--json` is stable wire JSON.
- `worker list` includes approved, registered, unreachable, disabled, and
  decommissioned identities. `--all` is accepted as an explicit full-inventory
  request; the endpoint always returns the full approved inventory.
- `worker add` prints a generated control token exactly once, with a warning to
  store it securely. A supplied token can be read from stdin so it does not
  appear in shell history. No later command prints a stored token.
- `worker update` requires the revision returned by `show` or the previous
  update.
- `worker probe` verifies the live endpoint and reconciles registration.
- `worker remove` decommissions the identity. Task history remains readable;
  the id is terminal and a replacement uses a new id.
- Host process start, stop, install, and upgrade are not CLI commands here.

## Team management

```sh
gb-client team list [--json]
gb-client team show <name> [--json]
gb-client team create <name> --master <worker> \
    [--member <worker[:capability[,capability]]>]... \
    [--max-depth <n>] [--max-fanout <n>] \
    [--locality local|remote] [--json]
gb-client team revise <name> --version <n> --master <worker> \
    [--member <worker[:capability[,capability]]>]... \
    [--max-depth <n>] [--max-fanout <n>] \
    [--locality local|remote] [--json]
gb-client team validate <name> [--version <n>] [--json]
gb-client team activate <name> [--version <n>] [--json]
gb-client team deactivate <name> [--version <n>] [--json]
```

A team is Gateway-owned configuration, not a Worker process. `create` produces
an inactive version 1 roster; `validate` and `activate` recheck live membership.
`revise` requires the observed `--version`; a stale value fails with
`team.version_conflict` and exit code 3. JSON mode returns the same wire `Team`
shape as the SDK. Human output uses one stable line per roster so it remains
safe in a shell pipeline.

## Runtime Profile management

```sh
gb-client profile list --worker <id> [--json]
gb-client profile show <name> --worker <id> [--json]
gb-client profile create <name> --worker <id> --instruction <text> \
    [--model <id>] [--capability <id>]... [--grant <scope>]... \
    [--restriction <scope>]... [--composition <kind>] \
    [--member <agent>]... [--skill <name>]... [--mcp <connection>]... \
    [--secret-ref <reference>]... [--compaction-budget <tokens>] [--json]
gb-client profile revise <name> --worker <id> --expected <n> --instruction <text> \
    [profile options] [--json]
gb-client profile validate <name> --worker <id> [--revision <n>] [--json]
gb-client profile activate <name> --worker <id> --revision <n> [--json]
gb-client profile deactivate <name> --worker <id> [--json]
```

Every profile command names the owning Worker and calls the SDK's Worker-scoped
profile operations. The Worker remains the authority for immutable revisions,
optimistic concurrency, resolved file-loaded agents and skills, provider
validation, grants, and activation. `--expected` is the observed revision
count. A stale revision fails with `profile.revision_conflict` and exit code 3;
a failed validation leaves the previous active revision unchanged.

Profile output never prints secret values. Secret references such as
`secret://provider-key` are preserved, while instruction text is redacted
before both human and JSON presentation. Use `gb-client submit --profile <name>` to
pin the activated revision for a new task.

## Submit selection

```sh
gb-client submit --worker <id> --agent <name> --input <text> \
    [--session <id>] [--profile <name>] \
    [--team <name>] [--team-version <n>] [--key <key>] [--json]
```

`--profile`, `--team`, and `--team-version` are passed to the SDK unchanged.
The Gateway and owning Worker resolve them; the CLI does not inspect profile
files or roster storage.

## Approvals

```sh
gb-client approvals <task-id> [--pending] [--json]
gb-client approve <task-id> <approval-id> [--digest <sha256:...>] [--json]
gb-client reject <task-id> <approval-id> [--digest <sha256:...>] [--json]
gb-client revoke <task-id> <approval-id> [--json]
```

These commands call the SDK approval methods. The task id is the Gateway
external execution id, not the private Worker handle. `approvals --pending`
shows outstanding requests for that exact routed task. `approve` and `reject`
fetch the Worker-owned record when `--digest` is omitted, so the decision carries
the exact operation digest the approver inspects. A supplied digest is forwarded
unchanged; the Gateway rejects a changed binding. `revoke` only applies while
the approval is still pending. The Worker remains the authority for expiry,
authority checks, resume, and the protected effect.

## Operations

```sh
gb-client health
gb-client workers
gb-client submit --worker general-live --agent general --input "hello" --json
gb-client task <id> --json
gb-client events <id>
gb-client watch <id> [--after <seq>]
gb-client cancel <id>
```

Connection precedence:

1. `--gateway` and `--token`
2. active profile
3. `GOLDENBEE_GATEWAY_URL` and `GOLDENBEE_TOKEN`

Explicit flags win. The CLI never connects to a Worker directly.

## Exit and output behavior

| Exit | Meaning |
| --- | --- |
| `0` | Success. |
| `1` | Ordinary operation error such as not found or an unsupported state. |
| `2` | Usage error: unknown command, missing positional, or malformed option. |
| `3` | Validation or optimistic-concurrency conflict, including `team.version_conflict` and `profile.revision_conflict`. |
| `4` | Authorization failure: missing scope, hidden Worker, or denied profile/team change. |
| `5` | Transport uncertainty, including a lost acknowledgment (`task.unknown_submission`) or unreachable Worker. |

Errors print `cli: <stable.code>: <message>`, optional recovery guidance, and
non-secret details to standard error. Successful commands write only their
result to standard output. JSON mode is wire-shaped and retains resource
identities, versions, active flags, and conflicts; profile instructions are
redacted. Caller tokens, control tokens, and secret values never appear in
output or errors.

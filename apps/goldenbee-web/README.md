# Goldenbee Web Client

An independent browser Client for a Goldenbee Gateway. React, TypeScript, and
Vite build static assets only: there is no Node BFF, server-side rendering, or
embedded Gateway asset.

The Client is a presentation and protocol Client. It never executes agents,
holds provider credentials, or duplicates Gateway policy.

## Routes

Routing is hash-based so the static build needs no server SPA fallback:

| Route | Screen |
| --- | --- |
| `#/` | Connection overview and visible Workers |
| `#/chat` | Command Center workspace: local sessions, multi-panel chat, Worker/agent/profile selection, streaming, and approvals |
| `#/attention` | Honest empty approval-queue statement; approvals appear inline |
| `#/diagnostics` | Gateway and Worker diagnostics |
| `#/audit` | Worker registry and control audit |
| `#/packages` | Installed packages and verified package install |
| `#/instances` | Local instance list and deployment |
| `#/instances/<name>` | Instance detail, lifecycle controls, and logs |
| `#/providers` | Model provider catalog |
| `#/providers/<id>` | Provider detail, credential, and discovery |
| `#/teams` | Team roster list |
| `#/teams/<name>` | Team detail and activation |
| `#/profiles` | Worker-owned Runtime Profiles |
| `#/profiles/<worker>/<profile>` | Profile revisions and activation |
| `#/tasks/<taskId>` | Durable task projection, retained events, and approvals |

Package and instance pages require `instance:read` to inspect and
`instance:admin` for install, deploy, start, stop, restart, upgrade, and
rollback. Provider administration requires `admin`. Denied scopes render as an
explicit access-denied state; the browser cannot widen authority.

## Requirements

- Bun 1.2 or newer
- A reachable Goldenbee Gateway and a caller token from its configuration
- For the browser acceptance suite: built `gb-gateway`, `general`, and
  `effect-fixture` binaries plus Chromium

## Run

From the repository root (Bun workspaces resolve `@goldenbee/sdk`):

```bash
bun install --frozen-lockfile
cp apps/goldenbee-web/.env.example apps/goldenbee-web/.env
bun run --filter goldenbee-web dev
```

`.env` supports:

| Variable | Used by | Purpose |
| --- | --- | --- |
| `GOLDENBEE_GATEWAY_URL` | Vite dev server | Target for the same-origin `/v1` proxy. Never sent to the browser. |
| `VITE_GATEWAY_URL` | Browser | Optional default Gateway address in the connection form. Defaults to the current origin. |

The development proxy is for local work only. Production must serve the static
assets from the same origin as Gateway through a reviewed reverse proxy.

## Deployment and reverse proxy

This release deliberately has no Gateway CORS layer and no `Origin` check on the
WebSocket upgrade. A browser at another origin therefore cannot be trusted to
protect its own bearer token. Deploy the Client and Gateway on the same origin:

1. Build with `bun run build`. The output in `apps/goldenbee-web/dist` is static
   HTML, hashed JS, CSS, and an SVG favicon. There is no Node server entry and
   no runtime Node dependency; `scripts/check-static-build.ts` fails the build
   if a server artifact or `node:` import appears.
2. Serve `dist` from the same host and port that expose `/v1`.
3. Reverse-proxy `/v1` to `gb-gateway`, including the SSE route
   `/v1/tasks/<id>/events/stream`. Disable response buffering for that route so
   streamed deltas arrive immediately.
4. Reverse-proxy the socket ticket and WebSocket routes if a future Client
   surface uses them. Chat uses SSE, not WebSocket.
5. Terminate TLS at the reverse proxy. The caller token travels only in the
   `Authorization` header.

Do not point a separately hosted Client at a Gateway without adding a reviewed
Gateway CORS allowlist and WebSocket `Origin` validation. Both are future work,
not configuration toggles in this release.

Example nginx location (review before use):

```nginx
location /v1/ {
    proxy_pass http://127.0.0.1:8080;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_buffering off;
    proxy_read_timeout 3600s;
}
```

## Scripts

Run from the repository root:

```bash
bun run --filter goldenbee-web dev        # Vite dev server with /v1 proxy
bun run --filter goldenbee-web typecheck  # build SDK, then TypeScript check
bun run --filter goldenbee-web lint       # oxlint
bun run --filter goldenbee-web test       # build SDK, then app Vitest via Bun
bun run --filter goldenbee-web build      # SDK build, typecheck, static bundle, static check
bun run --filter goldenbee-web preview    # Serve the production bundle locally
bun run --filter goldenbee-web e2e        # real Gateway + Workers + Chromium
bun run --filter goldenbee-web e2e:install # Chromium with system dependencies
```

Root helpers:

```bash
bun run check:ts-only    # fail on authored JavaScript
bun run check:workspace  # fail on npm/Yarn/pnpm lockfiles; require bun.lock
bun run check:build      # verify the built bundle is static browser output
bun test                 # @goldenbee/sdk suite on Bun
bun run test             # workspace checks, SDK suite, and app DOM suite
```

`bun.lock` is committed at the repository root. Bun is the package manager,
JavaScript runtime, and script runner. The browser bundle stays static; Bun is
not a deployed Node server. The app's React DOM suite invokes Vitest through Bun
because it needs jsdom; the SDK does not depend on Vitest.

## End-to-end acceptance

`apps/goldenbee-web/e2e/stack.ts` starts a real `general` Worker, a real
`effect-fixture` Worker, and a real `gb-gateway` with temporary generated
configuration. Vite then starts with `GOLDENBEE_GATEWAY_URL` pointing at that
Gateway, so Chromium exercises the same-origin `/v1` proxy used in development.
The stack prints one readiness line and terminates every child process on exit
or signal.

Build the binaries once per checkout:

```bash
cargo build --bin gb-gateway --bin general --bin effect-fixture
bun run --filter goldenbee-web e2e:install
bun run --filter goldenbee-web e2e
```

The suite uses fixed, non-production test tokens only. It never writes a real
secret. Stack configuration and logs live in `e2e/.stack/`, which is ignored.
Playwright traces are retained only on failure; the global teardown rewrites any
retained trace so the fixed caller token is removed from delivered artifacts,
and fails if the token remains anywhere else.

Journeys cover:

- connect with a caller token, select Worker/agent/Runtime Profile, send a task,
  see streamed answer text, call the real native `save_artifact` tool, open task
  evidence, and reload to restore the local chat;
- drop an in-flight stream by reloading the browser, then reconnect the still
  running task from its saved durable cursor and restore the terminal output;
- approval with the real protected-effect Worker: exact digest, approve, one
  external ledger effect, reject, no rejected effect, and task approval state;
- invalid token, read-only scope denial, unavailable Worker, and HTML/script
  injection in task or model output;
- token non-leakage in URLs, `localStorage`, and browser console output.

Reasoning and Master plan rendering are covered by SDK, reducer, and component
tests. The deterministic fixture model does not produce those events, so the
browser journey never fabricates them.

## Capability matrix

See [docs/capabilities.md](docs/capabilities.md). It records what Gateway v1
proves for this Client and the surfaces that stay explicitly unsupported. The
Web Client does not claim parity with the CLI TUI.

Short version:

| Surface | Status |
| --- | --- |
| Connect, health, scoped Worker discovery | Real Gateway |
| Task submit, SSE stream, stop, retry, durable cursor reconnect | Real Gateway |
| Text, tool status, usage, diagnostics, approvals | Real Gateway events; only when reported |
| Reasoning and Master plan | Rendered when reported; live-only or event-dependent |
| Durable task detail and retained events | Real Gateway |
| Teams, Runtime Profiles, providers, packages, instances, diagnostics, audit | Real Gateway routes |
| Workspace file tree, global activity feed, cross-device history, attachments | Unsupported; no Gateway route in this release |
| Concurrent authenticated Gateways, BFF/SSR, provider credentials in the browser | Not implemented by design |

## Boundaries

- `@goldenbee/sdk` (`packages/goldenbee-sdk`) is the single Gateway client
  boundary for every TypeScript Client. UI and state code import from
  `@goldenbee/sdk`, never from a transport or HTTP helper directly. It owns URL
  validation, bearer headers, bounded timeouts, error decoding, retries, tasks,
  approvals, SSE, and sockets. It has no React, UI state, token storage, or
  hard-coded platform runtime; the app injects browser `fetch` and WebSocket.
- `src/features/connection/` owns connection state, token persistence, and the
  connection screen. Zustand holds in-memory connection state; the token store
  is the only module that touches sessionStorage.
- `src/features/chat/` owns the task stream, the reduced chat state, and target
  selection. `chat-selection.ts` resolves the Worker, agent, and Runtime Profile
  for the next task; `chat-selection-store.ts` persists only those three names
  in localStorage and never stores a token.
- `src/features/host/` owns package sources, package install, local instance
  deployment, lifecycle controls, and instance logs. It passes one stable
  operation id through a provision, upgrade, or rollback retry and treats a
  lost response as an uncertain outcome, never as success.
- `src/features/providers/` owns model provider administration and write-only
  credential entry.
- `src/components/ui/` contains vendored shadcn/ui and AI Elements base
  components. `src/components/ai-elements/` holds app-local presentational
  compositions. Sources are recorded in
  [docs/ai-elements.md](docs/ai-elements.md).
- TanStack Query owns package, instance, provider, and Worker Runtime Profile
  collections and detail queries. It is deliberately unused by the connection
  handshake, which is connection state rather than a cached server collection.
- All authored source and configuration in this app is TypeScript/TSX. The
  workspace check `bun run check:ts-only` fails if authored JavaScript appears
  outside generated output.

## Token policy

- Memory is the default. A caller token is sent only as
  `Authorization: Bearer ...` and never placed in a URL.
- "Remember token for this tab" writes to `sessionStorage`. Closing the tab
  clears it.
- The caller token is never read from or written to `localStorage`.
  `localStorage` holds only non-secret preferences, such as the last Worker,
  agent, and Runtime Profile names, window layout, and Client settings.
- A token is written to sessionStorage only after Gateway accepts it. A failed
  or unreachable connection clears the attempted token.
- Error messages come from typed `GatewayError` values and never include the
  token or request headers.

## Connection states

| State | Meaning |
| --- | --- |
| `idle` | No attempt yet. |
| `connecting` | Health and authenticated discovery are in flight. |
| `connected` | Gateway health answered and `/v1/workers` accepted the token. |
| `unauthorized` | Gateway answered, but the caller token was rejected. |
| `forbidden` | Gateway accepted the token but denied discovery. |
| `unreachable` | No Gateway answered at the address. |
| `error` | Gateway answered but the authenticated check failed for another reason. |
| `invalid` | The URL or token is missing or malformed; no request was made. |

## Verification

```bash
bun install --frozen-lockfile
bun run typecheck
bun run lint
bun run check:ts-only
bun run check:workspace
bun test
bun run --filter goldenbee-web test
bun run build
bun run check:build
cargo build --bin gb-gateway --bin general --bin effect-fixture
bun run --filter goldenbee-web e2e
```

Supporting suites cover the SDK against a fake transport and real Gateway
contract fixtures, SSE parser malformed frames and cursor expiry, the chat
reducer (text, reasoning, tools, approvals, plans, terminal states, replay,
malformed events), IndexedDB restore, reconnect and gap handling, stop/retry,
target selection, approvals, task detail, management pages, and token
persistence. The Playwright suite is the one real-Gateway seam.

## Dependencies

No AI SDK, TanStack AI, provider SDK, Worker execution code, or Node server is
present. AI Elements patterns are presentational compositions; they are not
wired to AI SDK transports. `@playwright/test` is a development-only dependency
for the acceptance suite.

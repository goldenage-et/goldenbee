# Goldenbee Web Client

An independent browser Client for a Goldenbee Gateway. React, TypeScript, and
Vite build static assets only: there is no Node BFF, server-side rendering, or
embedded Gateway asset.

Ticket 01 covers the connection shell. Chat, approvals, task detail, Worker
management, and other Gateway routes start in later tickets.

## Requirements

- Bun 1.2 or newer
- A reachable Goldenbee Gateway and a caller token from its configuration

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

The development proxy is for local work only. Production should serve the
static assets from the same origin as Gateway through a reviewed reverse proxy.
Gateway CORS is optional and only needed for an explicitly separate origin.

## Scripts

Run from the repository root:

```bash
bun run --filter goldenbee-web dev        # Vite dev server with /v1 proxy
bun run --filter goldenbee-web typecheck  # build SDK, then TypeScript check
bun run --filter goldenbee-web lint       # oxlint
bun run --filter goldenbee-web test       # build SDK, then app Vitest via Bun
bun run --filter goldenbee-web build      # SDK build, typecheck, static bundle
bun run --filter goldenbee-web preview    # Serve the production bundle locally
```

From the root, `bun test` runs the `@goldenbee/sdk` suite on Bun's native test
runner. `bun run test` runs that suite plus the app DOM tests.

`bun.lock` is committed at the repository root. Bun is the package manager,
JavaScript runtime, and script runner. The browser bundle stays static; Bun is
not a deployed Node server. The app's React DOM suite invokes Vitest through Bun
because it needs jsdom; the SDK does not depend on Vitest.


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
- `src/components/ui/` contains vendored shadcn/ui and AI Elements base
  components. `src/components/ai-elements/` holds app-local presentational
  compositions. Sources are recorded in
  [docs/ai-elements.md](docs/ai-elements.md).
- TanStack Query is the server-state boundary for later collection and detail
  queries. It is deliberately unused by the connection handshake, which is
  connection state rather than a cached server collection.
- All authored source and configuration in this app is TypeScript/TSX. The
  workspace check `bun run check:ts-only` fails if authored JavaScript appears
  outside generated output.

## Token policy

- Memory is the default. A caller token is sent only as
  `Authorization: Bearer ...` and never placed in a URL.
- "Remember token for this tab" writes to `sessionStorage`. Closing the tab
  clears it.
- `localStorage` is never read or written.
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

The `@goldenbee/sdk` suite covers the full typed Gateway surface against a fake
fetch transport. The app suite covers token persistence, connection state
transitions, and connection screen states. Run both with `bun run test` from the
repository root. The repository-level Playwright seam against a real Gateway is
a later ticket.

## Dependencies

No AI SDK, TanStack AI, provider SDK, Worker execution code, or Node server is
present. AI Elements patterns are presentational compositions; they are not
wired to AI SDK transports.

# Goldenbee Web Client

An independent browser Client for a Goldenbee Gateway. React, TypeScript, and
Vite build static assets only: there is no Node BFF, server-side rendering, or
embedded Gateway asset.

Ticket 01 covers the connection shell. Chat, approvals, task detail, Worker
management, and other Gateway routes start in later tickets.

## Requirements

- Node.js 22.12 or newer
- A reachable Goldenbee Gateway and a caller token from its configuration

## Run

```bash
npm install
cp .env.example .env
npm run dev
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

```bash
npm run dev        # Vite development server with /v1 proxy
npm run typecheck  # TypeScript project build, no emit
npm run lint       # oxlint
npm run test       # Vitest unit and component tests
npm run build      # typecheck plus static production bundle in dist/
npm run preview    # Serve the production bundle locally
```

`package-lock.json` is committed. npm is the package manager.

## Boundaries

- `src/sdk/` is the single Gateway client boundary. UI and state code import
  from `@/sdk`, never from a transport or HTTP helper directly. It owns URL
  validation, bearer headers, bounded timeouts, error decoding, health, and
  caller-scoped discovery. Later tickets extend this module with tasks,
  approvals, SSE, and sockets.
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

The unit suite covers the SDK against a fake fetch transport, token
persistence, connection state transitions, and the connection screen states.
The repository-level Playwright seam against a real Gateway is a later ticket.

## Dependencies

No AI SDK, TanStack AI, provider SDK, Worker execution code, or Node server is
present. AI Elements patterns are presentational compositions; they are not
wired to AI SDK transports.

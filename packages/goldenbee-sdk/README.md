# @goldenbee/sdk

`packages/goldenbee-sdk` is the reusable, platform-neutral TypeScript SDK for
the Goldenbee Gateway HTTP/SSE/WebSocket API. It is a root Bun workspace package
consumed by `apps/goldenbee-web`, and is intended for browser, Bun, Node,
desktop, and mobile consumers.

The SDK owns protocol types, HTTP JSON, error decoding, bounded retries, SSE,
socket tickets, and socket frames. It does not own React, UI state, token
storage, or any platform runtime: callers inject `fetch` and, when needed, a
WebSocket factory. All authored source and configuration is TypeScript.

## Install and build

From the repository root:

```bash
bun install --frozen-lockfile
bun run --filter @goldenbee/sdk build
bun run --filter @goldenbee/sdk typecheck
bun run --filter @goldenbee/sdk lint
bun run --filter @goldenbee/sdk test
```

The app's `typecheck`, `build`, `test`, and `dev` scripts rebuild this package
first, so a fresh checkout can run `bun run build` from the root.

```ts
import { createGatewayClient } from "@goldenbee/sdk"

const client = createGatewayClient({
  baseUrl: "https://gateway.example.com",
  token,
  fetch: (input, init) => fetch(input, init),
  // Required only for socketConnect/openSocket:
  webSocket: (url) => new WebSocket(url),
})
```

`fetch` is required. The SDK never reaches for `globalThis.fetch` or
`globalThis.WebSocket`. `openSocket`/`socketConnect` throw
`sdk.platform_missing` when no WebSocket factory was supplied.

## Modules

| Module | Responsibility |
| --- | --- |
| `client.ts` | Typed method surface for every Gateway route used by tickets 03-11. |
| `transport.ts` | Fetch JSON, bearer headers, bounded retries, response decoding, byte responses, stream connect. |
| `errors.ts` | One `GatewayError` with `kind`, `code`, `message`, `retryable`, `recovery`, `status`, and `details`. |
| `sse.ts` | Fetch-based SSE parser, stream cursor, de-duplication, cursor-gap detection. |
| `socket.ts` | One-use ticket URL, client/server frame types, scope validation, socket wrapper. |
| `types.ts` | Curated wire types. `schema/gateway-v1.json` is authoritative where it covers a shape. |
| `validate.ts` | Runtime top-level guards so malformed responses fail as `invalid-response`. |


## Method surface

- Connection/discovery: `health`, `workers`/`listWorkers`, `discover`,
  `checkConnection`.
- Diagnostics/audit: `diagnostics`, `workerAudit`, `workerAuditLegacy`,
  `controlAudit`.
- Tasks: `submit`, `task`, `events`, `watch`, `cancel`.
- Approvals: `approvals`, `approval`, `decideApproval`, `revokeApproval`.
- Artifacts: `artifacts`, `artifact`, `artifactContent`.
- Socket: `socketTicket`, `socketConnect(ticket)`, `openSocket` (fresh ticket
  every call).
- Teams: `teams`, `team`, `createTeam`, `updateTeam`, `validateTeam`,
  `activateTeam`, `deactivateTeam`.
- Profiles: `profiles`, `profile`, `createProfile`, `reviseProfile`,
  `validateProfile`, `activateProfile`, `deactivateProfile`, `deleteProfile`.
- Providers: `providers`, `provider`, `createProvider`, `updateProvider`,
  `discoverProvider`, `setProviderCredential`, `revokeProviderCredential`,
  `providerPairCode`.
- Packages/instances: `packages`, `installPackage`, `instances`,
  `localInstance`, `provisionLocalInstance`, `startInstance`, `stopInstance`,
  `restartInstance`, `upgradeLocalInstance`, `rollbackLocalInstance`,
  `instanceLogs`, `localHost`, `hosts`.

Path IDs are percent-encoded. Every authenticated call sends
`Authorization: Bearer <token>`; `health` is public. The token is never put in a
URL, thrown message, log, or localStorage. SDK-generated transport messages
redact it if a transport error echoes it.

## Errors and uncertainty

```ts
class GatewayError extends Error {
  kind: GatewayErrorKind
  code: string | undefined
  message: string
  retryable: boolean
  recovery: string | undefined
  status: number | undefined
  details: Record<string, string> | undefined
}
```

`stream.cursor_expired`, `stream.cursor_invalid`, and
`task.unknown_submission` map to distinct `kind` values.

HTTP 202 with `task.unknown_submission` is a success-family response carrying an
error body. The SDK rejects it as `kind: "uncertain-submission"` (never success,
never ordinary rejection). Callers must offer inspect/reconcile, not a resubmit
with a new key. `error.isUncertainSubmission` and `error.details` expose the
condition.

## Retries and idempotency

- Bounded attempts (default 3, max 8; initial 100 ms, cap 5 s, jitter).
- Retried: network/timeouts, `sdk.transport`, `storage.unavailable`,
  `route.worker_unavailable`, retryable 429, and retryable 5xx.
- Never retried: validation, auth, scope, not-found, conflict, digest mismatch,
  and idempotency conflicts.
- `submit` captures the idempotency key once per call. Every retry sends the
  exact same body and key; a reconciled 202 keeps the first truthful
  uncertainty even if a later attempt loses the connection.

## SSE

`watch(taskId, after)` opens `/v1/tasks/{id}/events/stream?after=n` with fetch,
not `EventSource`, so the bearer header works and `AbortSignal` closes the body.

The parser handles split chunks, `\n\n` and `\r\n\r\n`, multiline `data:`,
comments/keepalives, empty blocks, `id:`/`event:`/`retry:` fields, malformed
JSON, and a 1 MiB incomplete-block bound (`sdk.stream_overflow`). Unknown event
kinds inside a valid envelope pass through to the reducer. `stream.draining` is
the one non-envelope and is returned as `{ type: "draining", phase }`.

`GatewayTaskStream` exposes `lastSeq` (in-memory de-duplication including
ephemeral deltas) and `lastDurableSeq` (persist this for reconnects). Events are
de-duplicated by `(stream_id, seq)`; a foreign `stream_id` is
`sdk.stream_foreign`. `stream.*` notices do not advance the cursor.

`GET /events` returns `StreamPage`. `pageHasCursorGap(page, after)` is true for
`cursor_expired` and for `after=0` with `oldest_seq > 1`.

## Socket

1. `socketTicket()` mints a 30 s single-use ticket over authenticated HTTP.
2. `openSocket()` mints a fresh ticket and opens
   `ws(s)://{host}{path}?ticket=...`; the bearer token never appears in the URL.
3. `socketConnect(ticket)` uses an existing ticket. Tickets are single-use, so a
   reconnect must call `socketTicket()` again.
4. Client frames carry `schema_version`, `command_id`, `scope`, and one of
   `subscribe`, `submit`, `cancel`, `approval`. `socketCommandFrame` validates
   scope before sending.
5. Server frames are `ack`, `error`, `event`, and `heartbeat`. Event frames are
   de-duplicated by `(stream_id, seq)`.

### Browser idle limitation

The Gateway heartbeat is a JSON `{"type":"heartbeat"}` frame, not a WebSocket
protocol ping, and the Gateway resets its reader idle timer only on **client**
frames. A browser cannot send protocol ping frames, and the protocol has no
no-op/ping command. A browser socket that only listens is therefore closed after
`socket.idle_timeout_ms` (default 60 s). Use SSE for chat streams. For socket
management use, raise the deployment idle timeout or add a Gateway ping command
as a follow-up. There is also no `unsubscribe` command; subscription slots are
freed by closing the connection.

## Tests

`src/*.test.ts` are contract tests against a fake fetch transport: exact
method/path/body/headers for each route, typed error decoding, 202 uncertainty,
retry bounds and idempotency reuse, SSE edge cases, cursor behavior, socket
frames, and token non-leakage. The tests need no running Gateway.

The SDK suite runs on Bun's native test runner (`bun test`). The web app's
React DOM tests need jsdom and run through Vitest invoked by Bun
(`bunx vitest`) from `apps/goldenbee-web`. There is no Node server in the
deployed artifact; Bun is the package manager, test/build runner, and
development runtime. All authored SDK source and configuration is TypeScript.

## Verification (ticket 02)

Run from the repository root:

```sh
bun install --frozen-lockfile
bun run check:ts-only
bun run typecheck
bun run lint
bun test
bun run build
```

Expected evidence: 113 SDK contract tests and 19 web tests pass, both TypeScript
projects typecheck, oxlint is clean, the TypeScript-only authored-source check
passes, and Vite emits a static bundle. The SDK build emits `dist/index.js` plus
per-module `.d.ts` declarations.


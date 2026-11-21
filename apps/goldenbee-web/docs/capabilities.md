# Web Client capability matrix

This page is the release boundary for `apps/goldenbee-web`. It distinguishes
what Gateway v1 proves from what the Client deliberately does not simulate. The
Web Client is not a CLI TUI replacement and does not claim feature parity.

## Supported from real Gateway routes

| Capability | Gateway evidence | Web Client surface |
| --- | --- | --- |
| Health and authenticated discovery | `GET /v1/health`, `GET /v1/workers` | Connection screen, Gateway overview, Worker list |
| Task submission | `POST /v1/tasks` with a stable idempotency key | Composer and submit lifecycle |
| Live task stream | `GET /v1/tasks/{id}/events/stream` over SSE | Answer text, tool status, task state, usage, diagnostics |
| Stop and retry | `POST /v1/tasks/{id}/cancel`, new submit for retry | Stop control and Retry action |
| Durable replay and reconnect | `GET /v1/tasks/{id}/events?after=...` and `next_seq` | Reconnect from the last durable cursor; explicit gap notice |
| Task projection | `GET /v1/tasks/{id}` | Task detail state, output, usage, and evidence facts |
| Approvals | Task approval routes and decision/revoke routes | Inline approval card with exact digest, status, and receipt |
| Worker Runtime Profiles | Worker-owned profile routes | Profile selection, revision view, activation |
| Teams | Team routes | Roster list, detail, validation, and activation |
| Providers | Admin provider and credential routes | Catalog, metadata, discovery, write-only credential entry |
| Packages and local instances | Host routes | Verified install, deploy, lifecycle, logs |
| Diagnostics and audit | Diagnostic, registry audit, and control audit routes | Diagnostics and audit pages |
| Task artifacts | Artifact metadata and content routes | Focused-session Artifacts tab |
| Local chat history | Browser IndexedDB | Reload restore, multiple local chats, backgrounds |
| Local settings and Gateway URL profiles | Browser `localStorage` (non-secret only) | Density, default profile, inspector default, saved URLs |

## Explicitly unsupported

| Surface | Why it is absent | Honest Client behavior |
| --- | --- | --- |
| Workspace file tree | Gateway has no workspace file API in this release | No file explorer. The inspector uses Gateway Artifacts only. |
| Global activity feed | Gateway has no global activity route | No invented feed. Activity is limited to local task history the browser observed. |
| Cross-device conversation history | Gateway has no session-history or task-list route | History is browser-local IndexedDB. A future Gateway history API can replace it. |
| Attachments | The task contract has no attachment transport | Composer states attachments are unsupported. |
| Concurrent multi-Gateway authentication | One bearer session at a time | Local profiles store URLs only; connecting another Gateway replaces the active identity. |
| Rich tool arguments, output, or duration | `tool.call`/`tool.result` carry name, agent, server, and status | Tool timeline shows only those facts. No fabricated arguments or durations. |
| Reasoning replay | `model.thinking` is not durable | Live reasoning renders in a collapsible trace; a reload cannot replay it. |
| Master plan display without plan events | Plans require `master.plan_created`/`master.child_observed` events | Rendering appears only when the Gateway and Worker report a plan. No progress percentages are invented. |
| Gateway or Worker configuration writes | Those routes do not exist in this release | Gateway and Worker settings are read-only facts. Client settings stay local. |
| Approval queue with global pending items | Approvals are task-scoped; no global queue route | Attention states there is no queue. Approvals appear inline and on task detail. |
| BFF, SSR, cookie sessions, CSRF, RBAC | Out of scope by architecture decision | Static browser assets call Gateway directly through the SDK. |
| Provider credential display | Credentials are write-only | A saved credential is reported by state only; the key is never shown. |
| CLI-specific coding, git, PR, or review workflows | The Web Client is not a coding surface | No coding prompts, terminal, embedded editor, or git UI. |

## Transport and deployment limits

- Chat uses SSE through `fetch` so the `Authorization` header works. The SDK can
  speak the WebSocket protocol, but the browser does not send protocol pings and
  would hit the Gateway idle timeout unless a deployment raises it or a ping
  command is added. WebSocket is not the primary chat transport.
- Gateway has no CORS layer and no `Origin` check on the WebSocket upgrade in
  this release. Production requires a same-origin reverse proxy. A CORS
  allowlist and WebSocket `Origin` validation are future Gateway work.
- The caller token is memory-only by default. Current-tab `sessionStorage` is an
  explicit option. The token is never written to `localStorage`, URLs, or logs.
- The production build is static. There is no Node runtime, BFF, SSR, or
  Gateway-embedded asset in the deployment.

## Verification map

| Claim | Primary evidence |
| --- | --- |
| Real browser journey through Gateway and Worker | `apps/goldenbee-web/e2e/*.spec.ts` with `e2e/stack.ts` |
| Reconnect from a durable cursor after a dropped stream | `e2e/journey.spec.ts` active-task reload recovery |
| Exact approval digest and one protected effect | `e2e/approval.spec.ts` plus the effect fixture's external ledger |
| Malformed SSE frame and cursor expiry | `packages/goldenbee-sdk/src/sse.test.ts`, `chat-recovery.test.ts`, `chat-reducer.test.ts` |
| Token non-leakage | E2E URL/localStorage/console assertions and trace teardown redaction |
| Static build with no Node runtime | `scripts/check-static-build.ts` |
| Authored TypeScript only and one lockfile | `scripts/check-typescript-only.ts`, `scripts/check-workspace.ts` |
| Reasoning, tools, plans, approvals reducer behavior | `apps/goldenbee-web/src/features/chat/chat-reducer.test.ts`, `chat-approval.test.ts`, component tests |

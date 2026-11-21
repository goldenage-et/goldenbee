import { describe, expect, it, mock } from "bun:test"

import {
  createGatewayClient,
  type GatewayClient,
} from "./index.js"

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  })
}

const health = { status: "ok", workers: 1, reachable: 1 }
const worker = {
  id: "general-a",
  instance: "default",
  package: "general",
  version: "0.1.0",
  status: "approved",
  reachable: true,
  locality: "local",
  endpoint: "http://worker",
  capabilities: [{ id: "general", kind: "agent", description: "General" }],
  declared_capabilities: [],
  approved_capabilities: ["general"],
  protocol: "worker-v1",
  incarnation: "inc-1",
  incarnations: [],
  capacity: { active: 0, limit: 4, queued: 0, queue_limit: 8 },
  lease_expires_at: 1,
  renewed_at: 1,
}
const submitResponse = {
  task_id: "exec-1",
  worker: "general-a",
  worker_instance: "default",
  worker_incarnation: "inc-1",
  agent: "general",
  profile: "default:1",
  state: "accepted",
  replayed: false,
}
const task = {
  task_id: "exec-1",
  worker: "general-a",
  worker_instance: "default",
  connectivity: "online",
  task: {
    id: "worker-task-1",
    namespace: "local",
    caller: "operator",
    agent: "general",
    session_id: "chat-1",
    state: "running",
  },
}
const event = {
  event_id: "evt-1",
  source: "worker",
  incarnation: "inc-1",
  stream_id: "task:exec-1",
  seq: 1,
  schema_version: 1,
  kind: "task.accepted",
  durable: true,
  payload: {},
  at: 1,
}
const page = {
  stream_id: "task:exec-1",
  events: [event],
  next_seq: 2,
  oldest_seq: 1,
  cursor_expired: false,
}
const approvalRecord = {
  id: "ap-1",
  task_id: "worker-task-1",
  namespace: "local",
  operation: "ledger_write",
  target: "entry",
  digest: "sha256:abc",
  risk: "write",
  requested_by: "general",
  expires_at: 2,
  status: "pending",
  created_at: 1,
}
const approval = { task_id: "exec-1", worker: "general-a", approval: approvalRecord }
const artifact = {
  id: "art-1",
  task_id: "exec-1",
  native_invocation_id: "inv-1",
  attempt: 1,
  version: 1,
  namespace: "local",
  owner: "general",
  name: "report.txt",
  content_type: "text/plain",
  size: 5,
  digest: `sha256:${"a".repeat(64)}`,
  finalized: true,
  created_at: 1,
}
const team = {
  name: "alpha",
  version: 1,
  master: "general-a",
  members: [
    { worker: "general-a", role: "master", capabilities: ["general"] },
    { worker: "echo-a", role: "worker", capabilities: ["echo"] },
  ],
  active: true,
}
const profile = { name: "default", active_revision: 1, revisions: [] }
const provider = {
  id: "openrouter",
  adapter: "openrouter",
  revision: 1,
  active: true,
  modalities: [],
  tool_calling: true,
  local: false,
  residency: "remote",
  allowed_workers: ["*"],
  models: [],
  models_source: "pending",
  allow_auto_default: false,
  ready: false,
  credential_revoked: false,
  model: "",
}
const credential = {
  provider: "openrouter",
  version: 1,
  fingerprint: `sha256:${"b".repeat(64)}`,
  revision: 1,
  ready: true,
  models: ["openai/gpt"],
  created_at: 1,
}
const pairCode = {
  code: "pair-1",
  link: "https://worker/pair?code=pair-1",
  worker_id: "general-a",
  instance: "default",
  expires_at: 1,
}
const packageSummary = {
  package: "general",
  version: "0.1.0",
  artifact_digest: `sha256:${"c".repeat(64)}`,
  signer: "release",
  target: "linux-x86_64",
}
const hostView = {
  host: "local",
  instance: "general-b",
  package: "general",
  version: "0.1.0",
  artifact_digest: `sha256:${"c".repeat(64)}`,
  binary_digest: `sha256:${"d".repeat(64)}`,
  signer: "release",
  target: "linux-x86_64",
  protocol: "worker-v1",
  lifecycle: "running",
  process_health: "ok",
  task_activity: "idle",
  limits: { max_active: 1, max_queue: 1, per_principal: 1 },
  workspace_write: false,
  operator_config: true,
  compatibility: {
    target: "linux-x86_64",
    protocol: "worker-v1",
    runtime: "1",
    adk: "1",
    compatible: true,
    issues: [],
  },
}
const hostDescriptor = { host: "local", protocol: "gateway-v1", adapter_version: "1" }
const operationOutcome = {
  operation_id: "op-1",
  kind: "provision",
  host: "local",
  instance: "general-b",
  status: "succeeded",
}

interface Entry {
  name: string
  method: string
  path: string
  body?: unknown
  auth?: boolean
  response: unknown
  call: (client: GatewayClient) => Promise<unknown>
}

const entries: Entry[] = [
  {
    name: "health",
    method: "GET",
    path: "/v1/health",
    auth: false,
    response: health,
    call: (client) => client.health(),
  },
  {
    name: "workers",
    method: "GET",
    path: "/v1/workers",
    response: [worker],
    call: (client) => client.workers(),
  },
  {
    name: "discover",
    method: "GET",
    path: "/v1/workers?capability=agent&protocol=worker-v1&locality=local&health=healthy",
    response: [worker],
    call: (client) =>
      client.discover({
        capability: "agent",
        protocol: "worker-v1",
        locality: "local",
        health: "healthy",
      }),
  },
  {
    name: "diagnostics",
    method: "GET",
    path: "/v1/workers/general-a/diagnostics?task=exec-1",
    response: {
      generated_at: 1,
      health: {},
      metrics: [],
      storage: { metadata: "ok", artifacts: "ok", degraded: [] },
      findings: [],
      correlation: {},
      audit: [],
      traces: [],
      subsystems: {},
    },
    call: (client) => client.diagnostics("general-a", "exec-1"),
  },
  {
    name: "workerAudit",
    method: "GET",
    path: "/v1/admin/workers/audit?worker=general-a",
    response: [
      {
        at: 1,
        action: "approve",
        worker: "general-a",
        principal: "operator",
        outcome: "accepted",
        detail: "ok",
      },
    ],
    call: (client) => client.workerAudit("general-a"),
  },
  {
    name: "workerAuditLegacy",
    method: "GET",
    path: "/v1/workers/audit?worker=general-a",
    response: [],
    call: (client) => client.workerAuditLegacy("general-a"),
  },
  {
    name: "controlAudit",
    method: "GET",
    path: "/v1/workers/general-a/sessions/s%2F1/control/audit",
    response: [
      {
        at: 1,
        action: "command",
        session_id: "s/1",
        principal: "operator",
        generation: 1,
        outcome: "accepted",
        detail: "click",
      },
    ],
    call: (client) => client.controlAudit("general-a", "s/1"),
  },
  {
    name: "submit",
    method: "POST",
    path: "/v1/tasks",
    body: {
      worker: "general-a",
      agent: "general",
      session_id: "chat-1",
      input: "hello",
      idempotency_key: "idem-1",
      profile: "default",
    },
    response: submitResponse,
    call: (client) =>
      client.submit({
        worker: "general-a",
        agent: "general",
        session_id: "chat-1",
        input: "hello",
        idempotency_key: "idem-1",
        profile: "default",
      }),
  },
  {
    name: "task",
    method: "GET",
    path: "/v1/tasks/exec%2F1",
    response: task,
    call: (client) => client.task("exec/1"),
  },
  {
    name: "events",
    method: "GET",
    path: "/v1/tasks/exec-1/events?after=3&limit=10",
    response: page,
    call: (client) => client.events("exec-1", 3, 10),
  },
  {
    name: "cancel",
    method: "POST",
    path: "/v1/tasks/exec-1/cancel",
    response: task,
    call: (client) => client.cancel("exec-1"),
  },
  {
    name: "approvals",
    method: "GET",
    path: "/v1/tasks/exec-1/approvals?pending=true",
    response: [approval],
    call: (client) => client.approvals("exec-1", true),
  },
  {
    name: "approval",
    method: "GET",
    path: "/v1/tasks/exec-1/approvals/ap%2F1",
    response: approval,
    call: (client) => client.approval("exec-1", "ap/1"),
  },
  {
    name: "decideApproval",
    method: "POST",
    path: "/v1/tasks/exec-1/approvals/ap-1/decision",
    body: { decision: "approved", digest: "sha256:abc" },
    response: approval,
    call: (client) =>
      client.decideApproval("exec-1", "ap-1", {
        decision: "approved",
        digest: "sha256:abc",
      }),
  },
  {
    name: "revokeApproval",
    method: "POST",
    path: "/v1/tasks/exec-1/approvals/ap-1/revoke",
    body: {},
    response: approval,
    call: (client) => client.revokeApproval("exec-1", "ap-1"),
  },
  {
    name: "artifacts",
    method: "GET",
    path: "/v1/tasks/exec-1/artifacts",
    response: [artifact],
    call: (client) => client.artifacts("exec-1"),
  },
  {
    name: "artifact",
    method: "GET",
    path: "/v1/tasks/exec-1/artifacts/art%2F1",
    response: artifact,
    call: (client) => client.artifact("exec-1", "art/1"),
  },
  {
    name: "socketTicket",
    method: "POST",
    path: "/v1/socket/tickets",
    response: { ticket: "one-use", expires_at_ms: 1, path: "/v1/socket" },
    call: (client) => client.socketTicket(),
  },
  {
    name: "teams",
    method: "GET",
    path: "/v1/teams",
    response: [team],
    call: (client) => client.teams(),
  },
  {
    name: "team",
    method: "GET",
    path: "/v1/teams/alpha",
    response: team,
    call: (client) => client.team("alpha"),
  },
  {
    name: "createTeam",
    method: "POST",
    path: "/v1/teams",
    body: { name: "alpha", master: "general-a", members: [] },
    response: team,
    call: (client) =>
      client.createTeam({ name: "alpha", master: "general-a", members: [] }),
  },
  {
    name: "updateTeam",
    method: "PUT",
    path: "/v1/teams/alpha",
    body: { expected_version: 1, master: "general-a", members: [] },
    response: team,
    call: (client) =>
      client.updateTeam("alpha", {
        expected_version: 1,
        master: "general-a",
        members: [],
      }),
  },
  {
    name: "validateTeam",
    method: "POST",
    path: "/v1/teams/alpha/validate",
    body: { version: 2 },
    response: team,
    call: (client) => client.validateTeam("alpha", { version: 2 }),
  },
  {
    name: "activateTeam",
    method: "POST",
    path: "/v1/teams/alpha/activate",
    body: {},
    response: team,
    call: (client) => client.activateTeam("alpha"),
  },
  {
    name: "deactivateTeam",
    method: "POST",
    path: "/v1/teams/alpha/deactivate",
    body: { version: 2 },
    response: team,
    call: (client) => client.deactivateTeam("alpha", { version: 2 }),
  },
  {
    name: "profiles",
    method: "GET",
    path: "/v1/workers/general-a/profiles",
    response: [profile],
    call: (client) => client.profiles("general-a"),
  },
  {
    name: "profile",
    method: "GET",
    path: "/v1/workers/general-a/profiles/default",
    response: profile,
    call: (client) => client.profile("general-a", "default"),
  },
  {
    name: "createProfile",
    method: "POST",
    path: "/v1/workers/general-a/profiles",
    body: { name: "default", revision: { revision: 0, instruction: "", model: "m", created_at: 0 } },
    response: profile,
    call: (client) =>
      client.createProfile("general-a", {
        name: "default",
        revision: { revision: 0, instruction: "", model: "m", created_at: 0 },
      }),
  },
  {
    name: "reviseProfile",
    method: "PUT",
    path: "/v1/workers/general-a/profiles/default",
    body: {
      expected_revision: 1,
      revision: { revision: 0, instruction: "", model: "m", created_at: 0 },
    },
    response: profile,
    call: (client) =>
      client.reviseProfile("general-a", "default", {
        expected_revision: 1,
        revision: { revision: 0, instruction: "", model: "m", created_at: 0 },
      }),
  },
  {
    name: "validateProfile",
    method: "POST",
    path: "/v1/workers/general-a/profiles/default/validate",
    body: { revision: 3 },
    response: profile,
    call: (client) => client.validateProfile("general-a", "default", { revision: 3 }),
  },
  {
    name: "activateProfile",
    method: "POST",
    path: "/v1/workers/general-a/profiles/default/activate",
    body: {},
    response: profile,
    call: (client) => client.activateProfile("general-a", "default"),
  },
  {
    name: "deactivateProfile",
    method: "POST",
    path: "/v1/workers/general-a/profiles/default/deactivate",
    body: {},
    response: profile,
    call: (client) => client.deactivateProfile("general-a", "default"),
  },
  {
    name: "deleteProfile",
    method: "DELETE",
    path: "/v1/workers/general-a/profiles/default",
    response: { deleted: "default" },
    call: (client) => client.deleteProfile("general-a", "default"),
  },
  {
    name: "providers",
    method: "GET",
    path: "/v1/admin/model-providers",
    response: [provider],
    call: (client) => client.providers(),
  },
  {
    name: "provider",
    method: "GET",
    path: "/v1/admin/model-providers/openrouter",
    response: provider,
    call: (client) => client.provider("openrouter"),
  },
  {
    name: "createProvider",
    method: "POST",
    path: "/v1/admin/model-providers",
    body: { id: "openrouter", adapter: "openrouter", residency: "remote" },
    response: provider,
    call: (client) =>
      client.createProvider({
        id: "openrouter",
        adapter: "openrouter",
        residency: "remote",
      }),
  },
  {
    name: "updateProvider",
    method: "PATCH",
    path: "/v1/admin/model-providers/openrouter",
    body: { revision: 1, active: false },
    response: provider,
    call: (client) =>
      client.updateProvider("openrouter", { revision: 1, active: false }),
  },
  {
    name: "discoverProvider",
    method: "POST",
    path: "/v1/admin/model-providers/openrouter/discover",
    response: provider,
    call: (client) => client.discoverProvider("openrouter"),
  },
  {
    name: "setProviderCredential",
    method: "PUT",
    path: "/v1/admin/model-providers/openrouter/credential",
    body: { key: "sk-secret" },
    response: credential,
    call: (client) => client.setProviderCredential("openrouter", "sk-secret"),
  },
  {
    name: "revokeProviderCredential",
    method: "DELETE",
    path: "/v1/admin/model-providers/openrouter/credential",
    response: credential,
    call: (client) => client.revokeProviderCredential("openrouter"),
  },
  {
    name: "providerPairCode",
    method: "POST",
    path: "/v1/admin/model-providers/pair-code",
    body: { worker_id: "general-a", instance: "default" },
    response: pairCode,
    call: (client) => client.providerPairCode("general-a", "default"),
  },
  {
    name: "packages",
    method: "GET",
    path: "/v1/packages",
    response: { packages: [packageSummary] },
    call: (client) => client.packages(),
  },
  {
    name: "installPackage",
    method: "POST",
    path: "/v1/packages/install",
    body: { source: { kind: "url", url: "https://example.test/general.tgz" } },
    response: { package: packageSummary, installed_now: true },
    call: (client) =>
      client.installPackage({
        source: { kind: "url", url: "https://example.test/general.tgz" },
      }),
  },
  {
    name: "instances",
    method: "GET",
    path: "/v1/instances",
    response: [hostView],
    call: (client) => client.instances(),
  },
  {
    name: "localInstance",
    method: "GET",
    path: "/v1/instances/general-b",
    response: { host: "local", view: hostView },
    call: (client) => client.localInstance("general-b"),
  },
  {
    name: "provisionLocalInstance",
    method: "POST",
    path: "/v1/instances",
    body: {
      operation_id: "op-1",
      instance: "general-b",
      worker: "general-b",
      release_digest: `sha256:${"c".repeat(64)}`,
    },
    response: operationOutcome,
    call: (client) =>
      client.provisionLocalInstance({
        operation_id: "op-1",
        instance: "general-b",
        worker: "general-b",
        release_digest: `sha256:${"c".repeat(64)}`,
      }),
  },
  {
    name: "startInstance",
    method: "POST",
    path: "/v1/instances/general-b/start",
    response: { host: "local", view: hostView },
    call: (client) => client.startInstance("general-b"),
  },
  {
    name: "stopInstance",
    method: "POST",
    path: "/v1/instances/general-b/stop",
    response: { host: "local", view: hostView },
    call: (client) => client.stopInstance("general-b"),
  },
  {
    name: "restartInstance",
    method: "POST",
    path: "/v1/instances/general-b/restart",
    response: { host: "local", view: hostView },
    call: (client) => client.restartInstance("general-b"),
  },
  {
    name: "upgradeLocalInstance",
    method: "POST",
    path: "/v1/instances/general-b/upgrade",
    body: { operation_id: "op-2", release_digest: `sha256:${"d".repeat(64)}` },
    response: { ...operationOutcome, kind: "upgrade", operation_id: "op-2" },
    call: (client) =>
      client.upgradeLocalInstance("general-b", {
        operation_id: "op-2",
        release_digest: `sha256:${"d".repeat(64)}`,
      }),
  },
  {
    name: "rollbackLocalInstance",
    method: "POST",
    path: "/v1/instances/general-b/rollback",
    body: { operation_id: "op-3" },
    response: { ...operationOutcome, kind: "rollback", operation_id: "op-3" },
    call: (client) =>
      client.rollbackLocalInstance("general-b", { operation_id: "op-3" }),
  },
  {
    name: "instanceLogs",
    method: "GET",
    path: "/v1/instances/general-b/logs?tail=50",
    response: { instance: "general-b", lines: ["line"] },
    call: (client) => client.instanceLogs("general-b", 50),
  },
  {
    name: "localHost",
    method: "GET",
    path: "/v1/host",
    response: hostDescriptor,
    call: (client) => client.localHost(),
  },
  {
    name: "hosts",
    method: "GET",
    path: "/v1/hosts",
    response: [hostDescriptor],
    call: (client) => client.hosts(),
  },
]

describe("Gateway SDK contract", () => {
  it.each(entries)("$name uses $method $path", async (entry) => {
    const seen: Array<{ url: string; init?: RequestInit }> = []
    const fetch = mock(async (url: string, init?: RequestInit) => {
      seen.push({ url, init })
      return json(entry.response)
    })
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      token: "caller-token",
      fetch,
      retry: { maxAttempts: 1 },
    })

    await expect(entry.call(client)).resolves.toBeDefined()

    expect(seen).toHaveLength(1)
    const [{ url, init }] = seen
    expect(url).toBe(`http://gateway.test${entry.path}`)
    expect(init?.method).toBe(entry.method)
    const headers = new Headers(init?.headers)
    expect(headers.get("accept")).toBe("application/json")
    expect(headers.get("authorization")).toBe(
      entry.auth === false ? null : "Bearer caller-token",
    )
    if (entry.body === undefined) {
      expect(init?.body).toBeUndefined()
    } else {
      expect(JSON.parse(String(init?.body))).toEqual(entry.body)
    }
  })

  it("stamps a fresh idempotency key when the caller omits one", async () => {
    const fetch = mock(async (_url: string, _init?: RequestInit) =>
      json(submitResponse),
    )
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      token: "caller-token",
      fetch,
      retry: { maxAttempts: 1 },
    })

    await client.submit({
      worker: "general-a",
      agent: "general",
      session_id: "chat-1",
      input: "hello",
    })

    const init = fetch.mock.calls[0][1]
    const body = JSON.parse(String(init?.body))
    expect(body.idempotency_key).toMatch(/^submit-/)
  })

  it("clamps the event page limit to the published range", async () => {
    const fetch = mock(async (_url: string, _init?: RequestInit) => json(page))
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      token: "caller-token",
      fetch,
      retry: { maxAttempts: 1 },
    })

    await client.events("exec-1", 0, 5_000)
    expect(fetch.mock.calls[0][0]).toContain("limit=1000")
  })

  it("opens a fetch SSE stream with the bearer header", async () => {
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(
          new TextEncoder().encode(
            `data: ${JSON.stringify({ ...event, seq: 3 })}\n\n`,
          ),
        )
        controller.close()
      },
    })
    const fetch = mock(
      async (_url: string, _init?: RequestInit) =>
        new Response(body, {
          status: 200,
          headers: { "content-type": "text/event-stream" },
        }),
    )
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      token: "caller-token",
      fetch,
      retry: { maxAttempts: 1 },
    })

    const stream = await client.watch("exec/1", 2)
    await expect(stream.next()).resolves.toMatchObject({
      type: "event",
      event: { seq: 3 },
    })
    await expect(stream.next()).resolves.toBeNull()

    const [url, init] = fetch.mock.calls[0]
    expect(url).toBe(
      "http://gateway.test/v1/tasks/exec%2F1/events/stream?after=2",
    )
    expect(new Headers(init?.headers).get("accept")).toBe("text/event-stream")
    expect(new Headers(init?.headers).get("authorization")).toBe(
      "Bearer caller-token",
    )
  })

  it("delivers artifact bytes and response headers", async () => {
    const fetch = mock(
      async () =>
        new Response(new Uint8Array([104, 105]), {
          status: 200,
          headers: {
            "content-type": "text/plain",
            "content-length": "2",
            "x-goldenbee-artifact-digest": `sha256:${"e".repeat(64)}`,
            "x-goldenbee-artifact-id": "art-1",
          },
        }),
    )
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      token: "caller-token",
      fetch,
      retry: { maxAttempts: 1 },
    })

    const content = await client.artifactContent("exec-1", "art-1")
    expect(content.contentType).toBe("text/plain")
    expect(content.contentLength).toBe(2)
    expect(content.digest).toBe(`sha256:${"e".repeat(64)}`)
    expect(content.artifactId).toBe("art-1")
    expect(await content.blob.text()).toBe("hi")
  })
})

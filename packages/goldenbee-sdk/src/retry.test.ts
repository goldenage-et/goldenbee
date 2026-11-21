import { describe, expect, it, mock } from "bun:test"

import {
  createGatewayClient,
  type GatewayFetch,
  type GatewaySubmitResponse,
} from "./index.js"

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  })
}

const submitResponse: GatewaySubmitResponse = {
  task_id: "exec-1",
  worker: "general-a",
  worker_instance: "default",
  agent: "general",
  profile: "default:1",
  state: "accepted",
  replayed: false,
}

const unknownSubmission = {
  code: "task.unknown_submission",
  message: "the submission may have reached the Worker",
  retryable: false,
  recovery: "inspect the task",
  details: { execution_id: "exec-9", worker: "general-a" },
}

const submitRequest = {
  worker: "general-a",
  agent: "general",
  session_id: "chat-1",
  input: "hello",
  idempotency_key: "idem-stable",
}

function build(fetch: GatewayFetch, maxAttempts = 3) {
  const sleep = mock(async () => undefined)
  const client = createGatewayClient({
    baseUrl: "http://gateway.test",
    token: "caller-token",
    fetch,
    retry: { maxAttempts, initialDelayMs: 1, maxDelayMs: 2 },
    sleep,
  })
  return { client, sleep }
}

describe("bounded transport retries", () => {
  it("retries a network failure and then succeeds", async () => {
    let attempts = 0
    const fetch = mock(async () => {
      attempts += 1
      if (attempts < 3) {
        throw new TypeError("connection reset")
      }
      return json({ status: "ok", workers: 1, reachable: 1 })
    })
    const { client, sleep } = build(fetch)

    await expect(client.health()).resolves.toEqual({
      status: "ok",
      workers: 1,
      reachable: 1,
    })
    expect(fetch).toHaveBeenCalledTimes(3)
    expect(sleep).toHaveBeenCalledTimes(2)
  })

  it("stops at the configured attempt bound", async () => {
    const fetch = mock(async () => {
      throw new TypeError("connection reset")
    })
    const { client } = build(fetch, 3)

    await expect(client.workers()).rejects.toMatchObject({ kind: "network" })
    expect(fetch).toHaveBeenCalledTimes(3)
  })

  it("does not retry a conflict or an unauthorized response", async () => {
    const conflict = mock(async () =>
      json(
        {
          code: "team.version_conflict",
          message: "stale version",
          retryable: false,
          recovery: "refetch the team",
        },
        409,
      ),
    )
    const unauthorized = mock(async () =>
      json(
        {
          code: "auth.unauthenticated",
          message: "bad token",
          retryable: false,
          recovery: "check the token",
        },
        401,
      ),
    )

    await expect(build(conflict).client.teams()).rejects.toMatchObject({
      kind: "conflict",
    })
    expect(conflict).toHaveBeenCalledTimes(1)
    await expect(build(unauthorized).client.teams()).rejects.toMatchObject({
      kind: "unauthorized",
    })
    expect(unauthorized).toHaveBeenCalledTimes(1)
  })

  it("retries an explicitly retryable rate limit", async () => {
    let attempts = 0
    const fetch = mock(async () => {
      attempts += 1
      if (attempts === 1) {
        return json(
          {
            code: "stream.subscriber_limit",
            message: "too many viewers",
            retryable: true,
            recovery: "back off",
            details: { retry_after_ms: "5" },
          },
          429,
        )
      }
      return json({ status: "ok", workers: 0, reachable: 0 })
    })
    const { client } = build(fetch)

    await expect(client.health()).resolves.toMatchObject({ status: "ok" })
    expect(fetch).toHaveBeenCalledTimes(2)
  })
})

describe("task submit idempotency", () => {
  it("reuses the exact body and idempotency key while reconciling", async () => {
    let attempts = 0
    const bodies: string[] = []
    const fetch = mock(async (_url: string, init?: RequestInit) => {
      attempts += 1
      bodies.push(String(init?.body))
      if (attempts < 3) {
        return json(unknownSubmission, 202)
      }
      return json(submitResponse)
    })
    const { client } = build(fetch)

    await expect(client.submit(submitRequest)).resolves.toEqual(submitResponse)
    expect(fetch).toHaveBeenCalledTimes(3)
    expect(new Set(bodies).size).toBe(1)
    expect(JSON.parse(bodies[0]).idempotency_key).toBe("idem-stable")
  })

  it("surfaces uncertainty after the bound without inventing success", async () => {
    const fetch = mock(async () => json(unknownSubmission, 202))
    const { client } = build(fetch, 3)

    await expect(client.submit(submitRequest)).rejects.toMatchObject({
      kind: "uncertain-submission",
      status: 202,
      code: "task.unknown_submission",
      details: { execution_id: "exec-9", worker: "general-a" },
    })
    expect(fetch).toHaveBeenCalledTimes(3)
  })

  it("keeps the first uncertainty when a later retry loses the connection", async () => {
    let attempts = 0
    const fetch = mock(async () => {
      attempts += 1
      if (attempts === 1) {
        return json(unknownSubmission, 202)
      }
      throw new TypeError("connection reset")
    })
    const { client } = build(fetch, 3)

    await expect(client.submit(submitRequest)).rejects.toMatchObject({
      code: "task.unknown_submission",
      details: { execution_id: "exec-9" },
    })
    expect(fetch).toHaveBeenCalledTimes(3)
  })
})

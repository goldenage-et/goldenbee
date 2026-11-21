import { describe, expect, it, mock } from "bun:test"

import {
  GatewayError,
  createGatewayClient,
  normalizeGatewayUrl,
  type GatewayWorker,
} from "./index.js"

const healthBody = { status: "ok", workers: 3, reachable: 2 }
const workerBody: GatewayWorker[] = [
  {
    id: "worker-1",
    instance: "default",
    package: "general",
    version: "0.1.0",
    status: "approved",
    reachable: true,
    locality: "local",
  },
]

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  })
}

function fakeFetch(
  handler: (url: string, init?: RequestInit) => Response | Promise<Response>,
) {
  return mock(async (url: string, init?: RequestInit) => handler(url, init))
}

describe("normalizeGatewayUrl", () => {
  it("keeps an http origin and removes trailing slashes", () => {
    expect(normalizeGatewayUrl("http://127.0.0.1:8080/")).toBe(
      "http://127.0.0.1:8080",
    )
    expect(normalizeGatewayUrl(" https://gateway.example.com/base/ ")).toBe(
      "https://gateway.example.com/base",
    )
  })

  it("rejects a URL without an http scheme", () => {
    expect(() => normalizeGatewayUrl("127.0.0.1:8080")).toThrow(GatewayError)
    expect(() => normalizeGatewayUrl("ftp://gateway.example.com")).toThrow(
      /http/i,
    )
  })

  it("rejects embedded credentials, query strings, and fragments", () => {
    expect(() =>
      normalizeGatewayUrl("https://user:secret@gateway.example.com"),
    ).toThrow(/credentials/i)
    expect(() => normalizeGatewayUrl("https://gateway.example.com?token=x")).toThrow(
      /query/i,
    )
    expect(() => normalizeGatewayUrl("https://gateway.example.com#x")).toThrow(
      /fragment/i,
    )
  })
})

describe("gateway health", () => {
  it("fetches public health without a bearer token", async () => {
    const fetch = fakeFetch(() => json(healthBody))
    const client = createGatewayClient({
      baseUrl: "http://gateway.test/",
      token: "secret-token",
      fetch,
    })

    await expect(client.health()).resolves.toEqual(healthBody)

    const [url, init] = fetch.mock.calls[0]
    expect(url).toBe("http://gateway.test/v1/health")
    expect(init?.headers).not.toHaveProperty("Authorization")
    expect(url).not.toContain("secret-token")
  })

  it("maps a rejected fetch to an unreachable Gateway error", async () => {
    const fetch = fakeFetch(() => {
      throw new TypeError("fetch failed")
    })
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      fetch,
    })

    await expect(client.health()).rejects.toMatchObject({
      name: "GatewayError",
      kind: "network",
      retryable: true,
    })
  })
})

describe("authenticated discovery", () => {
  it("sends the caller token only in the Authorization header", async () => {
    const fetch = fakeFetch((url) =>
      url.endsWith("/v1/workers")
        ? json(workerBody)
        : json(healthBody),
    )
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      token: "caller-token",
      fetch,
    })

    await expect(client.listWorkers()).resolves.toEqual(workerBody)

    const [url, init] = fetch.mock.calls[0]
    expect(url).toBe("http://gateway.test/v1/workers")
    expect(url).not.toContain("caller-token")
    expect(new Headers(init?.headers).get("authorization")).toBe(
      "Bearer caller-token",
    )
  })

  it("decodes an unauthorized error body without exposing the token", async () => {
    const fetch = fakeFetch(() =>
      json(
        {
          code: "auth.unauthenticated",
          message: "token is not recognized by this Gateway",
          retryable: false,
          recovery: "check the caller token",
        },
        401,
      ),
    )
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      token: "wrong-token",
      fetch,
    })

    const error = await client.listWorkers().catch((cause: unknown) => cause)

    expect(error).toBeInstanceOf(GatewayError)
    expect(error).toMatchObject({
      kind: "unauthorized",
      status: 401,
      code: "auth.unauthenticated",
      recovery: "check the caller token",
    })
    expect(JSON.stringify(error)).not.toContain("wrong-token")
    expect(String(error)).not.toContain("wrong-token")
  })

  it("rejects a successful response that is not the documented shape", async () => {
    const fetch = fakeFetch(() => new Response("not-json", { status: 200 }))
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      token: "caller-token",
      fetch,
    })

    await expect(client.listWorkers()).rejects.toMatchObject({
      kind: "invalid-response",
    })
  })

  it("rejects a Worker with a status outside the Gateway schema", async () => {
    const fetch = fakeFetch(() =>
      json([{ ...workerBody[0], status: "mystery" }]),
    )
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      token: "caller-token",
      fetch,
    })

    await expect(client.listWorkers()).rejects.toMatchObject({
      kind: "invalid-response",
    })
  })
})

describe("checkConnection", () => {
  it("reports connected with health and visible Workers", async () => {
    const fetch = fakeFetch((url) =>
      url.endsWith("/v1/health") ? json(healthBody) : json(workerBody),
    )
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      token: "caller-token",
      fetch,
    })

    await expect(client.checkConnection()).resolves.toEqual({
      status: "connected",
      health: healthBody,
      workers: workerBody,
    })
  })

  it("reports unauthorized only when health succeeded", async () => {
    const fetch = fakeFetch((url) =>
      url.endsWith("/v1/health")
        ? json(healthBody)
        : json(
            {
              code: "auth.unauthenticated",
              message: "token is not recognized by this Gateway",
              retryable: false,
              recovery: "check the caller token",
            },
            401,
          ),
    )
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      token: "wrong-token",
      fetch,
    })

    await expect(client.checkConnection()).resolves.toMatchObject({
      status: "unauthorized",
      health: healthBody,
      error: { kind: "unauthorized" },
    })
  })

  it("reports unreachable when the Gateway cannot be reached", async () => {
    const fetch = fakeFetch(() => {
      throw new TypeError("fetch failed")
    })
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      token: "caller-token",
      fetch,
    })

    await expect(client.checkConnection()).resolves.toMatchObject({
      status: "unreachable",
      error: { kind: "network" },
    })
  })

  it("keeps a reachable Gateway distinct from a failed authenticated call", async () => {
    const fetch = fakeFetch((url) =>
      url.endsWith("/v1/health")
        ? json(healthBody)
        : json(
            {
              code: "gateway.internal",
              message: "unexpected failure",
              retryable: true,
              recovery: "retry later",
            },
            500,
          ),
    )
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      token: "caller-token",
      fetch,
    })

    await expect(client.checkConnection()).resolves.toMatchObject({
      status: "error",
      health: healthBody,
      error: { kind: "server" },
    })
  })
})

describe("timeouts", () => {
  it("aborts a request that exceeds the configured deadline", async () => {
    const fetch = mock((_url: string, init?: RequestInit) =>
      new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener("abort", () => {
          reject(new DOMException("aborted", "AbortError"))
        })
      }),
    )
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      fetch,
      timeoutMs: 10,
      retry: { maxAttempts: 3, initialDelayMs: 1, maxDelayMs: 1 },
    })

    await expect(client.health()).rejects.toMatchObject({ kind: "timeout" })
    expect(fetch).toHaveBeenCalledTimes(3)
  })
})

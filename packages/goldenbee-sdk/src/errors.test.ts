import { describe, expect, it, mock } from "bun:test"

import {
  GatewayError,
  createGatewayClient,
  isGatewayError,
  kindForCode,
} from "./index.js"

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  })
}

const envelope = {
  code: "scope.task_submit",
  message: "the caller token does not have task:submit",
  retryable: false,
  recovery: "ask the operator for task:submit",
  details: { scope: "task:submit" },
}

describe("typed Gateway errors", () => {
  it("decodes one code/message/retryable/recovery/status/details shape", async () => {
    const fetch = mock(async () => json(envelope, 403))
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      token: "caller-token",
      fetch,
      retry: { maxAttempts: 1 },
    })

    const error = await client.workers().catch((cause: unknown) => cause)

    expect(error).toBeInstanceOf(GatewayError)
    expect(isGatewayError(error)).toBe(true)
    expect(error).toMatchObject({
      kind: "forbidden",
      status: 403,
      code: "scope.task_submit",
      message: envelope.message,
      retryable: false,
      recovery: envelope.recovery,
      details: { scope: "task:submit" },
    })
  })

  it("treats HTTP 202 task.unknown_submission as uncertainty, not success or rejection", async () => {
    const fetch = mock(async () =>
      json(
        {
          code: "task.unknown_submission",
          message: "the submission may have reached the Worker",
          retryable: false,
          recovery: "inspect the task before retrying with a new key",
          details: { execution_id: "exec-9", worker: "general-a" },
        },
        202,
      ),
    )
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      token: "caller-token",
      fetch,
      retry: { maxAttempts: 1 },
    })

    const error = await client
      .submit({
        worker: "general-a",
        agent: "general",
        session_id: "chat-1",
        input: "hello",
        idempotency_key: "idem-1",
      })
      .catch((cause: unknown) => cause)

    expect(error).toBeInstanceOf(GatewayError)
    expect(error).toMatchObject({
      kind: "uncertain-submission",
      status: 202,
      code: "task.unknown_submission",
      retryable: false,
      details: { execution_id: "exec-9", worker: "general-a" },
    })
    expect((error as GatewayError).isUncertainSubmission).toBe(true)
  })

  it("rejects a success status that carries a typed error envelope", async () => {
    const fetch = mock(async () =>
      json({
        code: "task.unknown_submission",
        message: "outcome unknown",
        retryable: false,
        recovery: "inspect",
      }),
    )
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      token: "caller-token",
      fetch,
      retry: { maxAttempts: 1 },
    })

    await expect(
      client.submit({
        worker: "general-a",
        agent: "general",
        session_id: "chat-1",
        input: "hello",
        idempotency_key: "idem-1",
      }),
    ).rejects.toMatchObject({ code: "task.unknown_submission" })
  })

  it("falls back to the status when the error body is malformed", async () => {
    const fetch = mock(
      async () => new Response("<html>bad gateway</html>", { status: 502 }),
    )
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      token: "caller-token",
      fetch,
      retry: { maxAttempts: 1 },
    })

    await expect(client.workers()).rejects.toMatchObject({
      kind: "invalid-response",
    })
  })

  it("rejects a malformed success body as an invalid response", async () => {
    const fetch = mock(async () => json({ unexpected: true }))
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      token: "caller-token",
      fetch,
      retry: { maxAttempts: 1 },
    })

    await expect(client.workers()).rejects.toMatchObject({
      kind: "invalid-response",
    })
  })

  it("never puts the caller token in a thrown message", async () => {
    const fetch = mock(async (url: string) => {
      throw new TypeError(`failed to fetch ${url} with token super-secret-token`)
    })
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      token: "super-secret-token",
      fetch,
      retry: { maxAttempts: 1 },
    })

    const error = await client.workers().catch((cause: unknown) => cause)
    expect(String(error)).not.toContain("super-secret-token")
    expect(JSON.stringify(error)).not.toContain("super-secret-token")
    expect((error as GatewayError).message).toContain("[redacted]")
  })

  it("maps cursor and uncertainty codes to distinct kinds", () => {
    expect(kindForCode("stream.cursor_expired", 400)).toBe("cursor-expired")
    expect(kindForCode("stream.cursor_invalid", 400)).toBe("cursor-invalid")
    expect(kindForCode("task.unknown_submission", 202)).toBe(
      "uncertain-submission",
    )
    expect(kindForCode("scope.task_read", 403)).toBe("forbidden")
  })
})

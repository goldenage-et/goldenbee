import { describe, expect, it, mock } from "bun:test"

import {
  GatewaySocket,
  approvalCommand,
  cancelCommand,
  createGatewayClient,
  gatewayErrorFromFrame,
  parseServerFrame,
  socketCommandFrame,
  socketUrl,
  submitCommand,
  subscribeCommand,
  type GatewaySocketLike,
  type SocketTicket,
  type StreamEvent,
} from "./index.js"

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  })
}

class FakeSocket implements GatewaySocketLike {
  readonly sent: string[] = []
  closed = false
  onSend: ((data: string) => void) | undefined
  private readonly listeners = new Map<string, Array<(event: unknown) => void>>()

  send(data: string): void {
    this.sent.push(data)
    this.onSend?.(data)
  }

  close(): void {
    this.closed = true
    this.emit("close", {})
  }

  addEventListener(type: string, listener: (event: unknown) => void): void {
    const list = this.listeners.get(type) ?? []
    list.push(listener)
    this.listeners.set(type, list)
  }

  removeEventListener(type: string, listener: (event: unknown) => void): void {
    this.listeners.set(
      type,
      (this.listeners.get(type) ?? []).filter((entry) => entry !== listener),
    )
  }

  emit(type: string, event: unknown): void {
    for (const listener of this.listeners.get(type) ?? []) {
      listener(event)
    }
  }

  message(frame: unknown): void {
    this.emit("message", { data: JSON.stringify(frame) })
  }
}

function event(seq: number, streamId = "task:exec-1"): StreamEvent {
  return {
    event_id: `evt-${seq}`,
    source: "worker",
    incarnation: "inc-1",
    stream_id: streamId,
    seq,
    schema_version: 1,
    kind: "tool.call",
    durable: true,
    payload: {},
    at: seq,
  }
}

const ticket: SocketTicket = {
  ticket: "one/use+token",
  expires_at_ms: 1,
  path: "/v1/socket",
}

describe("socket tickets and URL", () => {
  it("builds a ws URL without the bearer token", () => {
    expect(socketUrl("http://gateway.test/base", ticket)).toBe(
      "ws://gateway.test/v1/socket?ticket=one%2Fuse%2Btoken",
    )
    expect(socketUrl("https://gateway.test", ticket)).toBe(
      "wss://gateway.test/v1/socket?ticket=one%2Fuse%2Btoken",
    )
    expect(socketUrl("https://gateway.test", ticket)).not.toContain("caller-token")
  })

  it("requires an injected WebSocket factory", async () => {
    const fetch = mock(async (_url: string, _init?: RequestInit) =>
      json({}),
    )
    const client = createGatewayClient({
      baseUrl: "https://gateway.test",
      token: "caller-token",
      fetch,
      retry: { maxAttempts: 1 },
    })

    await expect(client.openSocket()).rejects.toMatchObject({
      code: "sdk.platform_missing",
    })
    expect(fetch).not.toHaveBeenCalled()
  })

  it("mints a fresh ticket for every openSocket call", async () => {
    const fetch = mock(
      async (_url: string, _init?: RequestInit) =>
        new Response(JSON.stringify(ticket), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
    )
    const urls: string[] = []
    const client = createGatewayClient({
      baseUrl: "https://gateway.test",
      token: "caller-token",
      fetch,
      retry: { maxAttempts: 1 },
      webSocket: (url) => {
        urls.push(url)
        return new FakeSocket()
      },
    })

    await client.openSocket()
    await client.openSocket()

    expect(fetch).toHaveBeenCalledTimes(2)
    expect(urls).toHaveLength(2)
    expect(urls[0]).toBe("wss://gateway.test/v1/socket?ticket=one%2Fuse%2Btoken")
    const init = fetch.mock.calls[0][1]
    expect(new Headers(init?.headers).get("authorization")).toBe(
      "Bearer caller-token",
    )
  })
})

describe("socket frames", () => {
  it("validates command scope before sending", () => {
    expect(() =>
      socketCommandFrame("c1", { task_id: "exec-1" }, cancelCommand("exec-2")),
    ).toThrow(/scope does not match/)

    expect(() =>
      socketCommandFrame(
        "c2",
        { worker: "general-b" },
        submitCommand({
          worker: "general-a",
          agent: "general",
          session_id: "chat-1",
          input: "hello",
        }),
      ),
    ).toThrow(/submission target/)

    expect(
      socketCommandFrame("c3", { task_id: "exec-1" }, subscribeCommand("exec-1", 4)),
    ).toMatchObject({
      schema_version: 1,
      command_id: "c3",
      scope: { task_id: "exec-1" },
      command: { kind: "subscribe", task_id: "exec-1", after: 4 },
    })
  })

  it("keeps approval decisions on the task scope", () => {
    const frame = socketCommandFrame(
      "c4",
      { task_id: "exec-1" },
      approvalCommand("exec-1", "ap-1", {
        decision: "approved",
        digest: "sha256:abc",
      }),
    )
    expect(frame.command).toEqual({
      kind: "approval",
      task_id: "exec-1",
      approval_id: "ap-1",
      decision: { decision: "approved", digest: "sha256:abc" },
    })
  })

  it("parses server frames and converts error frames", () => {
    const ack = parseServerFrame(
      JSON.stringify({
        type: "ack",
        command_id: "c1",
        outcome: {
          kind: "subscribed",
          task_id: "exec-1",
          after: 0,
        },
      }),
    )
    expect(ack).toMatchObject({ type: "ack", command_id: "c1" })

    const error = parseServerFrame(
      JSON.stringify({
        type: "error",
        command_id: "c1",
        error: {
          code: "socket.scope_mismatch",
          message: "scope does not match",
          retryable: false,
          recovery: "fix the frame scope",
        },
      }),
    )
    expect(error.type).toBe("error")
    if (error.type === "error") {
      expect(gatewayErrorFromFrame(error)).toMatchObject({
        kind: "socket",
        code: "socket.scope_mismatch",
        retryable: false,
        recovery: "fix the frame scope",
      })
    }
  })

  it("de-duplicates event frames by stream and sequence", async () => {
    const fake = new FakeSocket()
    const socket = new GatewaySocket(fake)
    fake.message({ type: "event", command_id: "c1", task_id: "exec-1", event: event(1) })
    fake.message({ type: "event", command_id: "c1", task_id: "exec-1", event: event(1) })
    fake.message({ type: "event", command_id: "c1", task_id: "exec-1", event: event(2) })

    await expect(socket.next()).resolves.toMatchObject({ event: { seq: 1 } })
    await expect(socket.next()).resolves.toMatchObject({ event: { seq: 2 } })
  })

  it("rejects a malformed frame with a typed protocol error", async () => {
    const fake = new FakeSocket()
    const socket = new GatewaySocket(fake)
    const pending = socket.next().catch((cause: unknown) => cause)
    fake.emit("message", { data: "not-json" })
    await expect(pending).resolves.toMatchObject({
      kind: "socket",
      code: "sdk.socket_protocol",
    })
  })

  it("matches a command ack by command_id and skips events", async () => {
    const fake = new FakeSocket()
    const socket = new GatewaySocket(fake)
    fake.onSend = () => {
      fake.message({
        type: "event",
        command_id: "c1",
        task_id: "exec-1",
        event: event(1),
      })
      fake.message({
        type: "ack",
        command_id: "c1",
        outcome: { kind: "cancelled", task: { task_id: "exec-1" } },
      })
    }

    const frame = socketCommandFrame("c1", { task_id: "exec-1" }, cancelCommand("exec-1"))
    await expect(socket.command(frame)).resolves.toMatchObject({
      type: "ack",
      command_id: "c1",
    })
    // The event observed while waiting for the ack is not lost.
    await expect(socket.next()).resolves.toMatchObject({ event: { seq: 1 } })
    expect(fake.sent).toHaveLength(1)
    expect(JSON.parse(fake.sent[0])).toMatchObject({
      schema_version: 1,
      command_id: "c1",
      command: { kind: "cancel", task_id: "exec-1" },
    })
  })

  it("closes into a typed socket-closed condition", async () => {
    const fake = new FakeSocket()
    const socket = new GatewaySocket(fake)
    socket.close()
    await expect(socket.next()).rejects.toMatchObject({
      kind: "socket-closed",
      code: "sdk.socket_closed",
    })
  })
})

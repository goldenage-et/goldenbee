import { describe, expect, it, mock } from "bun:test"

import {
  GatewayTaskStream,
  MAX_STREAM_BLOCK_BYTES,
  SseBlockParser,
  createGatewayClient,
  pageHasCursorGap,
  type StreamEvent,
} from "./index.js"

function event(seq: number, kind = "model.delta", durable = false): StreamEvent {
  return {
    event_id: `evt-${seq}`,
    source: "worker",
    incarnation: "inc-1",
    stream_id: "task:exec-1",
    seq,
    schema_version: 1,
    kind,
    durable,
    payload: { text: `t${seq}` },
    at: seq,
  }
}

function sseBody(chunks: string[]): Response {
  const encoder = new TextEncoder()
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      for (const chunk of chunks) {
        controller.enqueue(encoder.encode(chunk))
      }
      controller.close()
    },
  })
  return new Response(body, {
    status: 200,
    headers: { "content-type": "text/event-stream" },
  })
}

function streamOf(chunks: string[], after = 0): GatewayTaskStream {
  return new GatewayTaskStream(sseBody(chunks), { after })
}

describe("SSE block parser", () => {
  it("splits chunks and joins multiline data", () => {
    const first = JSON.stringify(event(1))
    const split = first.indexOf(",") + 1
    const parser = new SseBlockParser()
    const blocks = parser.push(
      `data: ${first.slice(0, split)}\ndata: ${first.slice(split)}\n\n`,
    )
    expect(blocks).toHaveLength(1)
    expect(blocks[0].data).toBe(`${first.slice(0, split)}\n${first.slice(split)}`)
  })

  it("handles a CRLF delimiter split across chunks", async () => {
    const stream = streamOf([
      `id: 1\r\nevent: model.delta\r\ndata: ${JSON.stringify(event(1))}\r`,
      "\n\r\n",
    ])
    await expect(stream.next()).resolves.toMatchObject({
      type: "event",
      event: { seq: 1, kind: "model.delta" },
    })
    await expect(stream.next()).resolves.toBeNull()
  })

  it("ignores comments and other fields", () => {
    const parser = new SseBlockParser()
    const blocks = parser.push(
      `: goldenbee keepalive\n\nretry: 1000\nid: 4\nevent: model.delta\ndata: ${JSON.stringify(
        event(4),
      )}\n\n`,
    )
    expect(blocks).toHaveLength(1)
    expect(blocks[0]).toMatchObject({ event: "model.delta", id: "4" })
  })

  it("ignores an empty data block", () => {
    const parser = new SseBlockParser()
    expect(parser.push("data:\n\n")).toEqual([])
    expect(parser.push(": comment\n\n")).toEqual([])
  })

  it("rejects a malformed data frame", async () => {
    const stream = streamOf(["data: not-json\n\n"])
    await expect(stream.next()).rejects.toMatchObject({
      code: "sdk.stream_invalid",
    })
  })

  it("caps an incomplete block instead of growing without bound", () => {
    const parser = new SseBlockParser()
    expect(() =>
      parser.push(`data: ${"x".repeat(MAX_STREAM_BLOCK_BYTES)}`),
    ).toThrow(/bounded client buffer/)
  })
})

describe("task stream frames", () => {
  it("parses stream.draining as a non-envelope notice", async () => {
    const stream = streamOf([
      `event: stream.draining\ndata: {"phase":"draining"}\n\n`,
    ])
    await expect(stream.next()).resolves.toEqual({
      type: "draining",
      phase: "draining",
    })
  })

  it("passes unknown valid event kinds through to the reducer", async () => {
    const stream = streamOf([
      `data: ${JSON.stringify(event(1, "future.vendor_kind", true))}\n\n`,
    ])
    await expect(stream.next()).resolves.toMatchObject({
      type: "event",
      event: { kind: "future.vendor_kind" },
    })
  })

  it("de-duplicates by stream and sequence", async () => {
    const stream = streamOf([
      `data: ${JSON.stringify(event(1))}\n\n`,
      `data: ${JSON.stringify(event(1))}\n\n`,
      `data: ${JSON.stringify(event(2))}\n\n`,
    ])
    await expect(stream.next()).resolves.toMatchObject({ event: { seq: 1 } })
    await expect(stream.next()).resolves.toMatchObject({ event: { seq: 2 } })
    await expect(stream.next()).resolves.toBeNull()
    expect(stream.lastSeq).toBe(2)
  })

  it("refuses to mix a foreign stream identity", async () => {
    const foreign = { ...event(2), stream_id: "task:other" }
    const stream = streamOf([
      `data: ${JSON.stringify(event(1))}\n\n`,
      `data: ${JSON.stringify(foreign)}\n\n`,
    ])
    await expect(stream.next()).resolves.toBeDefined()
    await expect(stream.next()).rejects.toMatchObject({
      code: "sdk.stream_foreign",
    })
  })

  it("tracks the durable cursor separately from ephemeral sequences", async () => {
    const stream = streamOf([
      `data: ${JSON.stringify(event(1, "model.delta", false))}\n\n`,
      `data: ${JSON.stringify(event(2, "tool.call", true))}\n\n`,
      `data: ${JSON.stringify({
        ...event(2, "stream.lagged", false),
        payload: { from_seq: 2 },
      })}\n\n`,
    ])
    await stream.next()
    await stream.next()
    await stream.next()
    expect(stream.lastSeq).toBe(2)
    expect(stream.lastDurableSeq).toBe(2)
  })

  it("resumes after a durable cursor", async () => {
    const stream = streamOf(
      [
        `data: ${JSON.stringify(event(1, "task.accepted", true))}\n\n`,
        `data: ${JSON.stringify(event(2, "tool.call", true))}\n\n`,
      ],
      1,
    )
    await expect(stream.next()).resolves.toMatchObject({ event: { seq: 2 } })
    await expect(stream.next()).resolves.toBeNull()
  })

  it("aborts cleanly through an AbortSignal", async () => {
    const controller = new AbortController()
    const body = new ReadableStream<Uint8Array>({
      start(streamController) {
        controller.signal.addEventListener("abort", () => {
          streamController.error(new DOMException("aborted", "AbortError"))
        })
      },
    })
    const stream = new GatewayTaskStream(
      new Response(body, { status: 200 }),
      { signal: controller.signal },
    )
    const pending = stream.next().catch((cause: unknown) => cause)
    controller.abort()
    await expect(pending).resolves.toMatchObject({ kind: "aborted" })
  })

  it("is async-iterable", async () => {
    const stream = streamOf([
      `data: ${JSON.stringify(event(1))}\n\n`,
      `data: ${JSON.stringify(event(2))}\n\n`,
    ])
    const seen: number[] = []
    for await (const frame of stream) {
      if (frame.type === "event") {
        seen.push(frame.event.seq)
      }
    }
    expect(seen).toEqual([1, 2])
  })
})

describe("cursor failure conditions", () => {
  const errorBody = (code: string, message: string) => ({
    code,
    message,
    retryable: false,
    recovery: "reload the task and oldest retained events",
  })

  it("reports cursor expiry as a distinct typed condition", async () => {
    const fetch = mock(
      async () => new Response(JSON.stringify(errorBody("stream.cursor_expired", "too old")), {
        status: 400,
        headers: { "content-type": "application/json" },
      }),
    )
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      token: "caller-token",
      fetch,
      retry: { maxAttempts: 1 },
    })

    await expect(client.watch("exec-1", 2)).rejects.toMatchObject({
      kind: "cursor-expired",
      code: "stream.cursor_expired",
      status: 400,
    })
  })

  it("reports an invalid cursor distinctly", async () => {
    const fetch = mock(
      async () => new Response(JSON.stringify(errorBody("stream.cursor_invalid", "bad cursor")), {
        status: 400,
        headers: { "content-type": "application/json" },
      }),
    )
    const client = createGatewayClient({
      baseUrl: "http://gateway.test",
      token: "caller-token",
      fetch,
      retry: { maxAttempts: 1 },
    })

    await expect(client.watch("exec-1", 2)).rejects.toMatchObject({
      kind: "cursor-invalid",
      code: "stream.cursor_invalid",
    })
  })

  it("detects a trimmed history page with after=0", () => {
    const page = {
      stream_id: "task:exec-1",
      events: [],
      next_seq: 10,
      oldest_seq: 5,
      cursor_expired: false,
    }
    expect(pageHasCursorGap(page, 0)).toBe(true)
    expect(pageHasCursorGap({ ...page, oldest_seq: 1 }, 0)).toBe(false)
    expect(
      pageHasCursorGap({ ...page, cursor_expired: true, oldest_seq: 1 }, 4),
    ).toBe(true)
  })
})

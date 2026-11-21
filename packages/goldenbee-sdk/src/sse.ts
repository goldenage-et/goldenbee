import { GatewayError, isGatewayError, sdkError } from "./errors.js"
import { encodeSegment, queryString, type GatewayTransport } from "./transport.js"
import type {
  GatewayStreamFrame,
  StreamEvent,
  StreamPage,
} from "./types.js"

/** Maximum buffered bytes for one incomplete Server-Sent Event block. */
export const MAX_STREAM_BLOCK_BYTES = 1024 * 1024

export interface ParsedSseBlock {
  event?: string
  id?: string
  data: string
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function isStreamEventEnvelope(value: unknown): value is StreamEvent {
  return (
    isRecord(value) &&
    typeof value.event_id === "string" &&
    typeof value.source === "string" &&
    typeof value.incarnation === "string" &&
    typeof value.stream_id === "string" &&
    typeof value.seq === "number" &&
    Number.isFinite(value.seq) &&
    typeof value.schema_version === "number" &&
    typeof value.kind === "string" &&
    typeof value.durable === "boolean" &&
    "payload" in value &&
    typeof value.at === "number"
  )
}

/** Parse one SSE block into its `data`, `event`, and `id` fields. */
export function parseSseBlock(block: string): ParsedSseBlock | null {
  const data: string[] = []
  let event: string | undefined
  let id: string | undefined
  for (const line of block.split("\n")) {
    if (!line || line.startsWith(":")) {
      // Empty lines end a block; comments are keepalives.
      continue
    }
    const colon = line.indexOf(":")
    const field = colon === -1 ? line : line.slice(0, colon)
    let value = colon === -1 ? "" : line.slice(colon + 1)
    if (value.startsWith(" ")) {
      value = value.slice(1)
    }
    if (field === "data") {
      data.push(value)
    } else if (field === "event") {
      event = value
    } else if (field === "id") {
      id = value
    }
    // `retry:` and unknown fields are informational.
  }
  if (data.length === 0) {
    return null
  }
  const joined = data.join("\n")
  if (!joined.trim()) {
    return null
  }
  return { event, id, data: joined }
}

/**
 * Decode one SSE block into a typed frame.
 *
 * `stream.draining` is emitted with an `event:` name and a `{"phase":...}`
 * payload; it is the one non-envelope that must not fail the stream. Unknown
 * event kinds inside a valid envelope are passed through to the reducer.
 */
export function frameFromBlock(block: ParsedSseBlock): GatewayStreamFrame {
  let value: unknown
  try {
    value = JSON.parse(block.data) as unknown
  } catch {
    throw sdkError("stream", "Gateway stream data is not valid JSON.", {
      code: "sdk.stream_invalid",
    })
  }
  if (block.event === "stream.draining") {
    const phase =
      isRecord(value) && typeof value.phase === "string" ? value.phase : "draining"
    return { type: "draining", phase }
  }
  if (!isStreamEventEnvelope(value)) {
    throw sdkError("stream", "Gateway stream event does not match the protocol envelope.", {
      code: "sdk.stream_invalid",
    })
  }
  return { type: "event", event: value }
}

/**
 * Incremental fetch-body SSE parser.
 *
 * Handles arbitrary chunk boundaries, `\n\n` and `\r\n\r\n` separators,
 * multiline `data:`, comments, and a bounded incomplete block. A block larger
 * than {@link MAX_STREAM_BLOCK_BYTES} is a typed protocol failure instead of an
 * unbounded allocation.
 */
export class SseBlockParser {
  private buffer = ""
  private readonly decoder = new TextDecoder()
  private bytes = 0

  push(chunk: string | Uint8Array): ParsedSseBlock[] {
    const text = typeof chunk === "string" ? chunk : this.decoder.decode(chunk, { stream: true })
    if (text) {
      this.append(text)
    }
    return this.drain(false)
  }

  flush(): ParsedSseBlock[] {
    const tail = this.decoder.decode()
    if (tail) {
      this.append(tail)
    }
    return this.drain(true)
  }

  private append(text: string): void {
    this.buffer += text
    // Normalize CRLF but keep a trailing CR for the next chunk.
    const trailingCr = this.buffer.endsWith("\r")
    const body = trailingCr ? this.buffer.slice(0, -1) : this.buffer
    this.buffer = body.replace(/\r\n/g, "\n") + (trailingCr ? "\r" : "")
    this.bytes = new TextEncoder().encode(this.buffer).length
  }

  private drain(flush: boolean): ParsedSseBlock[] {
    const blocks: ParsedSseBlock[] = []
    for (;;) {
      const index = this.buffer.indexOf("\n\n")
      if (index === -1) {
        break
      }
      const block = this.buffer.slice(0, index)
      this.buffer = this.buffer.slice(index + 2)
      this.bytes = new TextEncoder().encode(this.buffer).length
      this.acceptBlock(block, blocks)
    }
    if (!flush && this.bytes > MAX_STREAM_BLOCK_BYTES) {
      throw sdkError(
        "stream",
        "Gateway stream block exceeded the bounded client buffer.",
        { code: "sdk.stream_overflow", retryable: false },
      )
    }
    if (flush && this.buffer.trim()) {
      this.acceptBlock(this.buffer, blocks)
      this.buffer = ""
      this.bytes = 0
    }
    return blocks
  }

  private acceptBlock(block: string, blocks: ParsedSseBlock[]): void {
    if (new TextEncoder().encode(block).length > MAX_STREAM_BLOCK_BYTES) {
      throw sdkError(
        "stream",
        "Gateway stream block exceeded the bounded client buffer.",
        { code: "sdk.stream_overflow", retryable: false },
      )
    }
    const parsed = parseSseBlock(block)
    if (parsed) {
      blocks.push(parsed)
    }
  }
}

/**
 * Per-stream de-duplication and resumable cursor state.
 *
 * Durable and ephemeral events share one sequence. `stream.*` control notices
 * (for example `stream.lagged`) carry the last observed sequence and do not
 * advance the cursor. A foreign stream identity is a protocol failure rather
 * than a silently mixed stream.
 */
export class StreamCursor {
  private streamId: string | undefined
  private seq: number
  private durableSeq: number

  constructor(after = 0) {
    this.seq = after
    this.durableSeq = after
  }

  get lastSeq(): number {
    return this.seq
  }

  get lastDurableSeq(): number {
    return this.durableSeq
  }

  accept(event: StreamEvent): StreamEvent | null {
    if (this.streamId === undefined) {
      this.streamId = event.stream_id
    } else if (this.streamId !== event.stream_id) {
      throw sdkError(
        "protocol",
        "The stream changed identity; refusing to mix another task's events.",
        { code: "sdk.stream_foreign" },
      )
    }
    if (event.kind.startsWith("stream.")) {
      return event
    }
    if (event.seq <= this.seq) {
      return null
    }
    this.seq = event.seq
    if (event.durable) {
      this.durableSeq = event.seq
    }
    return event
  }
}

/**
 * A fetch-based replay-then-live task stream.
 *
 * Fetch is used instead of `EventSource` so the bearer token can be sent as an
 * Authorization header and the stream can be aborted by signal.
 */
export class GatewayTaskStream implements AsyncIterable<GatewayStreamFrame> {
  private readonly reader: ReadableStreamDefaultReader<Uint8Array>
  private readonly parser = new SseBlockParser()
  private readonly cursor: StreamCursor
  private readonly signal: AbortSignal | undefined
  private queue: GatewayStreamFrame[] = []
  private done = false

  constructor(response: Response, options: { after?: number; signal?: AbortSignal } = {}) {
    if (!response.body) {
      throw sdkError("stream", "Gateway stream response has no body.")
    }
    this.reader = response.body.getReader()
    this.cursor = new StreamCursor(options.after ?? 0)
    this.signal = options.signal
  }

  get lastSeq(): number {
    return this.cursor.lastSeq
  }

  /** Highest durable sequence; persist this cursor for reconnects. */
  get lastDurableSeq(): number {
    return this.cursor.lastDurableSeq
  }

  async next(): Promise<GatewayStreamFrame | null> {
    for (;;) {
      const queued = this.queue.shift()
      if (queued) {
        return queued
      }
      if (this.done) {
        return null
      }
      let chunk
      try {
        chunk = await this.reader.read()
      } catch (cause) {
        if (this.signal?.aborted) {
          throw sdkError("aborted", "The Gateway stream was cancelled.")
        }
        throw this.streamFailure(cause)
      }
      if (chunk.done) {
        this.acceptBlocks(this.parser.flush())
        this.done = true
        continue
      }
      this.acceptBlocks(this.parser.push(chunk.value))
    }
  }

  /** Stop reading and release the underlying fetch body. */
  async close(): Promise<void> {
    this.done = true
    try {
      await this.reader.cancel()
    } catch {
      // A stream that already closed needs no cleanup.
    }
  }

  async *[Symbol.asyncIterator](): AsyncIterator<GatewayStreamFrame> {
    try {
      for (;;) {
        const frame = await this.next()
        if (!frame) {
          return
        }
        yield frame
      }
    } finally {
      await this.close()
    }
  }

  private acceptBlocks(blocks: ParsedSseBlock[]): void {
    for (const block of blocks) {
      const frame = frameFromBlock(block)
      if (frame.type === "event") {
        const accepted = this.cursor.accept(frame.event)
        if (!accepted) {
          continue
        }
        this.queue.push({ type: "event", event: accepted })
      } else {
        this.queue.push(frame)
      }
    }
  }

  private streamFailure(cause: unknown): GatewayError {
    if (isGatewayError(cause)) {
      return cause
    }
    const detail = cause instanceof Error && cause.message ? `: ${cause.message}` : "."
    return sdkError(
      "stream",
      `Gateway stream failed${detail}`,
      { code: "sdk.stream", retryable: true },
    )
  }
}

export async function openTaskStream(options: {
  transport: GatewayTransport
  taskId: string
  after?: number
  signal?: AbortSignal
}): Promise<GatewayTaskStream> {
  const after = options.after ?? 0
  const response = await options.transport.openStream(
    `/v1/tasks/${encodeSegment(options.taskId)}/events/stream${queryString({ after })}`,
    options.signal,
  )
  return new GatewayTaskStream(response, { after, signal: options.signal })
}

/**
 * Open the read-only workspace change stream for one Worker.
 *
 * The Worker emits the same event envelope as task streams, so the existing
 * bearer-auth fetch transport, parser, and abort behavior are reused.
 */
export async function openWorkspaceStream(options: {
  transport: GatewayTransport
  worker: string
  signal?: AbortSignal
}): Promise<GatewayTaskStream> {
  const response = await options.transport.openStream(
    `/v1/workers/${encodeSegment(options.worker)}/workspace/events/stream`,
    options.signal,
  )
  return new GatewayTaskStream(response, { signal: options.signal })
}

/**
 * Detect a history gap in a retained event page.
 *
 * `cursor_expired` covers a cursor older than retention. With `after=0` the
 * page API does not set that flag, but `oldest_seq > 1` still means the front
 * of history was trimmed.
 */
export function pageHasCursorGap(page: StreamPage, after: number): boolean {
  return page.cursor_expired || (after === 0 && page.oldest_seq > 1)
}

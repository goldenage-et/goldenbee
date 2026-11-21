import { GatewayError, isGatewayError, sdkError } from "./errors.js"
import { StreamCursor } from "./sse.js"
import type {
  ApprovalDecision,
  GatewaySubmitRequest,
  SocketClientFrame,
  SocketCommand,
  SocketScope,
  SocketServerFrame,
  SocketTicket,
  StreamEvent,
} from "./types.js"

/**
 * Browser WebSocket idle limitation
 * ---------------------------------
 * The Gateway sends JSON `{"type":"heartbeat"}` frames, not WebSocket protocol
 * pings, and resets its reader idle timer only on *client* frames. A browser
 * cannot send protocol ping frames. A browser socket that only listens is
 * therefore closed after `socket.idle_timeout_ms` (default 60 s), and the
 * protocol has no no-op/ping command. Use SSE for chat streams. If the socket
 * is used for management, raise the deployment idle timeout or add a Gateway
 * ping command as a follow-up.
 */

export interface GatewaySocketLike {
  send(data: string): void
  close(code?: number, reason?: string): void
  addEventListener(type: string, listener: (event: unknown) => void): void
  removeEventListener(type: string, listener: (event: unknown) => void): void
}

export type GatewayWebSocketFactory = (url: string) => GatewaySocketLike

/** Build the ticket-authenticated upgrade URL from the Gateway base URL. */
export function socketUrl(baseUrl: string, ticket: SocketTicket): string {
  const url = new URL(baseUrl)
  const scheme = url.protocol === "https:" ? "wss:" : "ws:"
  const path = ticket.path.startsWith("/") ? ticket.path : `/${ticket.path}`
  return `${scheme}//${url.host}${path}?ticket=${encodeURIComponent(ticket.ticket)}`
}

export function subscribeCommand(taskId: string, after = 0): SocketCommand {
  return { kind: "subscribe", task_id: taskId, after }
}

export function submitCommand(request: GatewaySubmitRequest): SocketCommand {
  return { kind: "submit", request }
}

export function cancelCommand(taskId: string): SocketCommand {
  return { kind: "cancel", task_id: taskId }
}

export function approvalCommand(
  taskId: string,
  approvalId: string,
  decision: ApprovalDecision,
): SocketCommand {
  return {
    kind: "approval",
    task_id: taskId,
    approval_id: approvalId,
    decision,
  }
}

export function socketCommandFrame(
  commandId: string,
  scope: SocketScope,
  command: SocketCommand,
): SocketClientFrame {
  const frame: SocketClientFrame = {
    schema_version: 1,
    command_id: commandId,
    scope,
    command,
  }
  validateSocketFrame(frame)
  return frame
}

/**
 * Reject a frame whose scope lies about its command target. The Gateway does
 * the authoritative check; this keeps UI bugs from turning into a 403.
 */
export function validateSocketFrame(frame: SocketClientFrame): void {
  if (frame.schema_version !== 1) {
    throw sdkError("protocol", "The socket frame schema version is not supported.", {
      code: "socket.unsupported_protocol",
    })
  }
  if (!frame.command_id.trim() || frame.command_id.length > 128) {
    throw sdkError("protocol", "Socket command_id must be 1-128 characters.", {
      code: "socket.invalid_frame",
    })
  }
  if (frame.command.kind === "submit") {
    if (frame.scope.task_id !== undefined) {
      throw sdkError(
        "protocol",
        "A submission command must not carry a task scope.",
        { code: "socket.scope_mismatch", status: 403 },
      )
    }
    const expected = frame.command.request.worker
    if (frame.scope.worker !== undefined && frame.scope.worker !== expected) {
      throw sdkError(
        "protocol",
        "The socket frame scope does not match the submission target.",
        { code: "socket.scope_mismatch", status: 403 },
      )
    }
    return
  }
  if (frame.scope.task_id !== frame.command.task_id) {
    throw sdkError(
      "protocol",
      "The socket frame scope does not match the command task.",
      { code: "socket.scope_mismatch", status: 403 },
    )
  }
  if (frame.scope.worker !== undefined) {
    throw sdkError(
      "protocol",
      "Only a submission command may carry a Worker scope.",
      { code: "socket.scope_mismatch", status: 403 },
    )
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function isEnvelope(value: unknown): value is StreamEvent {
  return (
    isRecord(value) &&
    typeof value.event_id === "string" &&
    typeof value.stream_id === "string" &&
    typeof value.seq === "number" &&
    typeof value.kind === "string" &&
    typeof value.durable === "boolean" &&
    typeof value.at === "number"
  )
}

/** Decode and validate one server frame. */
export function parseServerFrame(raw: string): SocketServerFrame {
  let value: unknown
  try {
    value = JSON.parse(raw) as unknown
  } catch {
    throw sdkError("socket", "Gateway socket frame is not valid JSON.", {
      code: "sdk.socket_protocol",
    })
  }
  if (!isRecord(value) || typeof value.type !== "string") {
    throw sdkError("socket", "Gateway socket frame does not match the protocol.", {
      code: "sdk.socket_protocol",
    })
  }
  if (value.type === "heartbeat") {
    if (typeof value.at !== "number") {
      throw sdkError("socket", "Gateway heartbeat frame is malformed.", {
        code: "sdk.socket_protocol",
      })
    }
    return { type: "heartbeat", at: value.at }
  }
  if (value.type === "ack") {
    if (typeof value.command_id !== "string" || !isRecord(value.outcome)) {
      throw sdkError("socket", "Gateway ack frame is malformed.", {
        code: "sdk.socket_protocol",
      })
    }
    return {
      type: "ack",
      command_id: value.command_id,
      outcome: value.outcome as unknown as Extract<
        SocketServerFrame,
        { type: "ack" }
      >["outcome"],
    }
  }
  if (value.type === "error") {
    if (!isRecord(value.error) || typeof value.error.code !== "string") {
      throw sdkError("socket", "Gateway error frame is malformed.", {
        code: "sdk.socket_protocol",
      })
    }
    return {
      type: "error",
      command_id:
        typeof value.command_id === "string" ? value.command_id : undefined,
      error: value.error as unknown as Extract<SocketServerFrame, { type: "error" }>["error"],
    }
  }
  if (value.type === "event") {
    if (
      typeof value.command_id !== "string" ||
      typeof value.task_id !== "string" ||
      !isEnvelope(value.event)
    ) {
      throw sdkError("socket", "Gateway event frame is malformed.", {
        code: "sdk.socket_protocol",
      })
    }
    return {
      type: "event",
      command_id: value.command_id,
      task_id: value.task_id,
      event: value.event,
    }
  }
  throw sdkError("socket", "Gateway sent an unknown socket frame type.", {
    code: "sdk.socket_protocol",
  })
}

/** Convert a socket error frame into the shared typed error. */
export function gatewayErrorFromFrame(
  frame: Extract<SocketServerFrame, { type: "error" }>,
): GatewayError {
  return new GatewayError({
    kind: "socket",
    message: frame.error.message,
    code: frame.error.code,
    retryable: frame.error.retryable,
    recovery: frame.error.recovery,
    details: frame.error.details,
  })
}

interface Waiter {
  resolve: (frame: SocketServerFrame) => void
  reject: (error: GatewayError) => void
}

/**
 * Typed wrapper around one open WebSocket.
 *
 * Server `event` frames are de-duplicated by `(stream_id, seq)` through the
 * same cursor as the HTTP SSE stream. Heartbeats and acknowledgments are
 * returned to the caller so command flows can match `command_id`.
 */
export class GatewaySocket implements AsyncIterable<SocketServerFrame> {
  private readonly socket: GatewaySocketLike
  private readonly queue: SocketServerFrame[] = []
  private readonly deferred: SocketServerFrame[] = []
  private readonly waiters: Waiter[] = []
  private readonly cursors = new Map<string, StreamCursor>()
  private failure: GatewayError | undefined
  private closed = false

  constructor(socket: GatewaySocketLike) {
    this.socket = socket
    socket.addEventListener("message", (event) => {
      const data = isRecord(event) ? event.data : undefined
      this.onMessage(data)
    })
    socket.addEventListener("error", () => {
      this.fail(sdkError("socket", "Gateway socket failed.", { retryable: true }))
    })
    socket.addEventListener("close", () => {
      this.closed = true
      this.fail(
        sdkError(
          "socket-closed",
          "The Gateway socket closed; resume with a fresh ticket and durable cursor.",
          { code: "sdk.socket_closed" },
        ),
      )
    })
  }

  send(frame: SocketClientFrame): void {
    validateSocketFrame(frame)
    if (this.closed) {
      throw sdkError("socket-closed", "The Gateway socket is closed.", {
        code: "sdk.socket_closed",
      })
    }
    this.socket.send(JSON.stringify(frame))
  }

  next(): Promise<SocketServerFrame> {
    const deferred = this.deferred.shift()
    if (deferred) {
      return Promise.resolve(deferred)
    }
    return this.readFrame()
  }

  private readFrame(): Promise<SocketServerFrame> {
    const queued = this.queue.shift()
    if (queued) {
      return Promise.resolve(queued)
    }
    if (this.failure) {
      return Promise.reject(this.failure)
    }
    if (this.closed) {
      return Promise.reject(
        sdkError("socket-closed", "The Gateway socket is closed.", {
          code: "sdk.socket_closed",
        }),
      )
    }
    return new Promise<SocketServerFrame>((resolve, reject) => {
      this.waiters.push({ resolve, reject })
    })
  }

  /** Send a command frame and wait for its matching ack or typed error. */
  async command(frame: SocketClientFrame): Promise<SocketServerFrame> {
    this.send(frame)
    for (;;) {
      const received = await this.readFrame()
      if (received.type === "ack" && received.command_id === frame.command_id) {
        return received
      }
      if (received.type === "error") {
        if (received.command_id === undefined || received.command_id === frame.command_id) {
          throw gatewayErrorFromFrame(received)
        }
      }
      // Preserve subscription events and other frames for the next `next()`.
      if (this.deferred.length >= 256) {
        throw sdkError(
          "socket",
          "The bounded socket frame buffer is full; drain subscription events before issuing another command.",
          { code: "sdk.socket_overflow" },
        )
      }
      this.deferred.push(received)
    }
  }

  async *[Symbol.asyncIterator](): AsyncIterator<SocketServerFrame> {
    for (;;) {
      yield await this.next()
    }
  }

  close(): void {
    if (!this.closed) {
      this.closed = true
      this.socket.close()
    }
  }

  private onMessage(data: unknown): void {
    if (typeof data !== "string") {
      this.fail(
        sdkError("socket", "Gateway sent an unexpected binary socket frame.", {
          code: "sdk.socket_protocol",
        }),
      )
      return
    }
    let frame: SocketServerFrame
    try {
      frame = parseServerFrame(data)
    } catch (cause) {
      this.fail(
        isGatewayError(cause)
          ? cause
          : sdkError("socket", "Gateway socket frame is malformed.", {
              code: "sdk.socket_protocol",
            }),
      )
      return
    }
    if (frame.type === "event") {
      let cursor = this.cursors.get(frame.event.stream_id)
      if (!cursor) {
        cursor = new StreamCursor()
        this.cursors.set(frame.event.stream_id, cursor)
      }
      let accepted: StreamEvent | null
      try {
        accepted = cursor.accept(frame.event)
      } catch (cause) {
        this.fail(
          isGatewayError(cause)
            ? cause
            : sdkError("protocol", "Gateway mixed stream identities.", {
                code: "sdk.stream_foreign",
              }),
        )
        return
      }
      if (!accepted) {
        return
      }
      frame = { ...frame, event: accepted }
    }
    this.push(frame)
  }

  private push(frame: SocketServerFrame): void {
    const waiter = this.waiters.shift()
    if (waiter) {
      waiter.resolve(frame)
      return
    }
    this.queue.push(frame)
  }

  private fail(error: GatewayError): void {
    this.failure = error
    while (this.waiters.length > 0) {
      this.waiters.shift()?.reject(error)
    }
  }
}

export function connectSocket(
  factory: GatewayWebSocketFactory,
  baseUrl: string,
  ticket: SocketTicket,
): GatewaySocket {
  return new GatewaySocket(factory(socketUrl(baseUrl, ticket)))
}

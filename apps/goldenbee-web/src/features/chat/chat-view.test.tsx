import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { render, screen, waitFor } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, describe, expect, it } from "vitest"
import type { ReactNode } from "react"

import { App } from "@/App"
import { ChatView } from "@/features/chat/chat-view"
import type { ChatGateway, ChatStream } from "@/features/chat/chat-runner"
import {
  chatSelectionStorageKey,
  createChatSelectionStore,
  type ChatSelectionStore,
} from "@/features/chat/chat-selection-store"
import type { SelectionGateway } from "@/features/chat/chat-profiles"
import {
  createConnectionStore,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import { createTokenStore } from "@/features/connection/token-store"
import {
  GatewayError,
  type GatewayRequestOptions,
  type GatewayStreamFrame,
  type GatewaySubmitRequest,
  type GatewaySubmitResponse,
  type GatewayTask,
  type Profile,
  type ProfileVersionRequest,
  type StreamEvent,
  type WorkerSummary,
} from "@goldenbee/sdk"

function memoryStorage(): Storage {
  const values = new Map<string, string>()
  return {
    get length() {
      return values.size
    },
    clear: () => values.clear(),
    getItem: (key) => values.get(key) ?? null,
    key: (index) => Array.from(values.keys())[index] ?? null,
    removeItem: (key) => {
      values.delete(key)
    },
    setItem: (key, value) => {
      values.set(key, value)
    },
  }
}

const worker: WorkerSummary = {
  id: "general-a",
  instance: "general-a",
  package: "general",
  version: "0.1.0",
  status: "approved",
  reachable: true,
  locality: "local",
  capabilities: [
    { id: "general", kind: "agent", description: "General agent" },
    { id: "echo", kind: "agent", description: "Echo agent" },
  ],
}

function activeProfile(revision = 1): Profile {
  return {
    name: "default",
    active_revision: revision,
    revisions: [
      {
        revision,
        instruction: "You are helpful.",
        model: `provider/model-${revision}`,
        composition: "single",
        created_at: revision,
      },
    ],
  }
}

function connectedStore(workers: WorkerSummary[] = [worker]): ConnectionStore {
  const store = createConnectionStore({
    tokenStore: createTokenStore({ sessionStorage: memoryStorage() }),
    defaultUrl: "http://gateway.test",
    createClient: () => ({
      checkConnection: async () => ({
        status: "connected",
        health: { status: "ok", workers: workers.length, reachable: 1 },
        workers,
      }),
    }),
  })
  store.setState({
    status: "connected",
    gatewayUrl: "http://gateway.test",
    token: "caller-token",
    health: { status: "ok", workers: workers.length, reachable: 1 },
    workers,
  })
  return store
}

function selectionWith(
  values: Partial<{ worker: string; agent: string; profile: string }>,
): ChatSelectionStore {
  const storage = memoryStorage()
  const store = createChatSelectionStore(storage)
  store.write({ worker: "", agent: "", ...values })
  return store
}

function event(
  seq: number,
  kind: string,
  payload: unknown,
  durable = true,
): StreamEvent {
  return {
    event_id: `evt-${seq}`,
    source: "worker",
    incarnation: "inc-1",
    stream_id: "task:exec",
    seq,
    schema_version: 1,
    kind,
    durable,
    payload,
    at: seq,
  }
}

class TestStream implements ChatStream {
  lastDurableSeq = 0
  private readonly events: StreamEvent[]
  private readonly signal: AbortSignal | undefined

  constructor(events: StreamEvent[], signal?: AbortSignal) {
    this.events = events
    this.signal = signal
  }

  async *[Symbol.asyncIterator](): AsyncIterator<GatewayStreamFrame> {
    for (const next of this.events) {
      yield { type: "event", event: next }
      if (next.durable) {
        this.lastDurableSeq = Math.max(this.lastDurableSeq, next.seq)
      }
    }
    if (this.events.length === 0 && this.signal) {
      await new Promise<void>((resolve) => {
        if (this.signal?.aborted) {
          resolve()
          return
        }
        this.signal?.addEventListener("abort", () => resolve(), { once: true })
      })
    }
  }
}

class GatedStream implements ChatStream {
  lastDurableSeq = 0
  private readonly before: StreamEvent[]
  private readonly gate: Promise<void>
  private readonly after: StreamEvent[]

  constructor(
    before: StreamEvent[],
    gate: Promise<void>,
    after: StreamEvent[],
  ) {
    this.before = before
    this.gate = gate
    this.after = after
  }

  async *[Symbol.asyncIterator](): AsyncIterator<GatewayStreamFrame> {
    for (const next of this.before) {
      yield { type: "event", event: next }
      if (next.durable) {
        this.lastDurableSeq = Math.max(this.lastDurableSeq, next.seq)
      }
    }
    await this.gate
    for (const next of this.after) {
      yield { type: "event", event: next }
      if (next.durable) {
        this.lastDurableSeq = Math.max(this.lastDurableSeq, next.seq)
      }
    }
  }
}

class ChatGatewayDouble implements ChatGateway {
  submits: GatewaySubmitRequest[] = []
  watches: Array<{ taskId: string; after: number }> = []
  cancels: string[] = []
  streams: StreamEvent[][] = []
  streamFactories: Array<() => ChatStream> = []
  submitError: GatewayError | null = null
  cancelTask: GatewayTask | null = null

  submit(
    request: GatewaySubmitRequest,
    _options?: GatewayRequestOptions,
  ): Promise<GatewaySubmitResponse> {
    this.submits.push(request)
    if (this.submitError) {
      return Promise.reject(this.submitError)
    }
    const attempt = this.submits.length
    return Promise.resolve({
      task_id: `exec-${attempt}`,
      worker: request.worker ?? "general-a",
      worker_instance: "general-a",
      agent: request.agent,
      profile: "default:1",
      state: "accepted",
      replayed: false,
    })
  }

  watch(
    taskId: string,
    after = 0,
    options?: GatewayRequestOptions,
  ): Promise<ChatStream> {
    this.watches.push({ taskId, after })
    const factory = this.streamFactories.shift()
    if (factory) {
      return Promise.resolve(factory())
    }
    const events = this.streams.shift() ?? []
    return Promise.resolve(new TestStream(events, options?.signal))
  }

  cancel(taskId: string): Promise<GatewayTask> {
    this.cancels.push(taskId)
    return Promise.resolve(
      this.cancelTask ?? {
        task_id: taskId,
        worker: "general-a",
        worker_instance: "general-a",
        connectivity: "online",
        task: {
          id: "worker-task",
          namespace: "local",
          caller: "operator",
          agent: "general",
          session_id: "chat-test",
          state: "cancelled",
        },
      },
    )
  }
}

class ProfileGatewayDouble implements SelectionGateway {
  activations: Array<{
    worker: string
    name: string
    revision: number | undefined
  }> = []
  activationError: GatewayError | null = null
  private readonly fallback: Profile[]
  private readonly lists = new Map<string, Profile[]>()

  constructor(fallback: Profile[] = [activeProfile()]) {
    this.fallback = fallback
  }

  set(workerId: string, profiles: Profile[]): void {
    this.lists.set(workerId, profiles)
  }

  profiles(workerId: string): Promise<Profile[]> {
    return Promise.resolve(this.lists.get(workerId) ?? this.fallback)
  }

  activateProfile(
    workerId: string,
    name: string,
    request?: ProfileVersionRequest,
  ): Promise<Profile> {
    this.activations.push({ worker: workerId, name, revision: request?.revision })
    if (this.activationError) {
      return Promise.reject(this.activationError)
    }
    const list = this.lists.get(workerId) ?? this.fallback
    const index = list.findIndex((candidate) => candidate.name === name)
    if (index < 0) {
      return Promise.reject(new Error("profile not found"))
    }
    const current = list[index]!
    const next: Profile = {
      ...current,
      active_revision: request?.revision ?? current.active_revision,
    }
    this.lists.set(
      workerId,
      list.map((candidate, position) =>
        position === index ? next : candidate,
      ),
    )
    return Promise.resolve(next)
  }
}

interface RenderChatOptions {
  store?: ConnectionStore
  profiles?: ProfileGatewayDouble
  selection?: ChatSelectionStore
}

function renderChat(
  gateway: ChatGateway,
  options: RenderChatOptions = {},
): ReturnType<typeof render> {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  function wrapper({ children }: { children: ReactNode }) {
    return (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    )
  }
  return render(
    <ChatView
      store={options.store ?? connectedStore()}
      client={gateway}
      profileClient={options.profiles ?? new ProfileGatewayDouble()}
      selectionStore={options.selection ?? createChatSelectionStore(memoryStorage())}
    />,
    { wrapper },
  )
}

async function sendPrompt(user: ReturnType<typeof userEvent.setup>, text: string) {
  await user.type(screen.getByLabelText("Message"), text)
  const send = screen.getByRole("button", { name: "Send message" })
  await waitFor(() => expect(send).toBeEnabled())
  await user.click(send)
}

afterEach(() => {
  window.location.hash = ""
  window.localStorage.clear()
})

describe("chat view", () => {
  it("submits the selected target and streams the answer into the reply", async () => {
    const gateway = new ChatGatewayDouble()
    gateway.streams = [
      [
        event(1, "task.accepted", { state: "accepted" }),
        event(2, "model.delta", { text: "Hello " }, false),
        event(3, "model.delta", { text: "world" }, false),
        event(4, "task.succeeded", {
          state: "succeeded",
          output: "Hello world",
        }),
      ],
    ]
    const user = userEvent.setup()
    renderChat(gateway)

    await sendPrompt(user, "say hello")

    expect(await screen.findByText("Hello world")).toBeInTheDocument()
    expect(screen.getByText("Task succeeded")).toBeInTheDocument()
    expect(gateway.submits[0]).toMatchObject({
      worker: "general-a",
      agent: "general",
      profile: "default",
      input: "say hello",
      session_id: expect.stringMatching(/^chat-/),
      idempotency_key: expect.stringMatching(/^chat-/),
    })
    expect(gateway.watches[0]).toMatchObject({ taskId: "exec-1", after: 0 })
  })

  it("shows failure guidance and retries without duplicating the user turn", async () => {
    const gateway = new ChatGatewayDouble()
    gateway.streams = [
      [
        event(1, "task.accepted", { state: "accepted" }),
        event(2, "task.failed", {
          state: "failed",
          error: {
            code: "worker.crashed",
            message: "The Worker stopped before answering.",
            retryable: true,
            recovery: "Retry the task.",
          },
        }),
      ],
      [
        event(3, "task.accepted", { state: "accepted" }),
        event(4, "task.succeeded", { state: "succeeded", output: "second try" }),
      ],
    ]
    const user = userEvent.setup()
    renderChat(gateway)

    await sendPrompt(user, "retry me")

    expect(await screen.findByText("Task failed")).toBeInTheDocument()
    expect(
      screen.getByText(/The Worker stopped before answering\./),
    ).toBeInTheDocument()

    await user.click(screen.getByRole("button", { name: "Retry" }))

    expect(await screen.findByText("Task succeeded")).toBeInTheDocument()
    expect(screen.getAllByText("retry me")).toHaveLength(1)
    expect(gateway.submits).toHaveLength(2)
    expect(gateway.submits[0]?.idempotency_key).not.toBe(
      gateway.submits[1]?.idempotency_key,
    )
    expect(gateway.submits[1]?.input).toBe("retry me")
  })

  it("renders reasoning, tools, plan, and reported usage around the answer", async () => {
    const gateway = new ChatGatewayDouble()
    gateway.streams = [
      [
        event(1, "task.accepted", { state: "accepted" }),
        event(
          2,
          "model.thinking",
          { text: "Check ", author: "general" },
          false,
        ),
        event(
          3,
          "model.thinking.complete",
          { text: "Check the log.", author: "general" },
          false,
        ),
        event(4, "tool.call", {
          id: "call-1",
          name: "exec_command",
          agent: "general",
          server: "mcp",
        }),
        event(5, "tool.result", {
          id: "call-1",
          name: "exec_command",
          agent: "general",
          server: "mcp",
          ok: false,
          error_code: "tool.failed",
        }),
        event(6, "master.plan_created", {
          plan_id: "plan-1",
          children: [
            { id: "c1", target: "general-a", capability: "general" },
            { id: "c2", target: "echo-a", capability: "echo" },
          ],
          criteria: [],
        }),
        event(7, "master.child_observed", {
          plan_id: "plan-1",
          child: "c1",
          binding: "b1",
          remote_task_id: "exec-2",
          state: "succeeded",
          artifact_digests: ["sha256:abc"],
        }),
        event(8, "task.succeeded", {
          state: "succeeded",
          output: "Done",
          usage: {
            prompt_tokens: 10,
            candidate_tokens: 5,
            estimated_cost_micros: 42,
            model_calls: 1,
            tool_calls: 0,
          },
        }),
      ],
    ]
    const user = userEvent.setup()
    renderChat(gateway)

    await sendPrompt(user, "inspect the trace")

    expect(await screen.findByText("Done")).toBeInTheDocument()
    expect(screen.getByRole("button", { name: /Thought/ })).toBeInTheDocument()
    expect(
      screen.getByText(/Reasoning is live-only; reconnecting will not replay it\./),
    ).toBeInTheDocument()

    await user.click(screen.getByRole("button", { name: /Thought/ }))
    expect(await screen.findByText("Check the log.")).toBeInTheDocument()

    expect(screen.getByText("Failed to run exec_command")).toBeInTheDocument()
    expect(screen.getByText("general · mcp")).toBeInTheDocument()
    expect(screen.getByText("Error code: tool.failed")).toBeInTheDocument()
    expect(screen.getByText("1 of 2 done")).toBeInTheDocument()
    expect(screen.getByText("Prompt tokens")).toBeInTheDocument()
    expect(screen.getByText("10")).toBeInTheDocument()
    expect(
      screen.getByText("42 micro-units (operator estimate)"),
    ).toBeInTheDocument()
  })

  it("shows a 202 unknown submission as uncertainty with no retry action", async () => {
    const gateway = new ChatGatewayDouble()
    gateway.submitError = new GatewayError({
      kind: "uncertain-submission",
      status: 202,
      code: "task.unknown_submission",
      message: "The Gateway could not confirm whether the task was accepted.",
      recovery: "Inspect the task before sending it again.",
      retryable: false,
      details: { execution_id: "exec-unknown", worker: "general-a" },
    })
    const user = userEvent.setup()
    renderChat(gateway)

    await sendPrompt(user, "did it start?")

    expect(await screen.findByText("Submission uncertain")).toBeInTheDocument()
    expect(
      screen.getByText("Inspect the task before sending it again."),
    ).toBeInTheDocument()
    expect(screen.queryByText("Task failed")).not.toBeInTheDocument()
    expect(screen.queryByRole("button", { name: "Retry" })).not.toBeInTheDocument()
    expect(gateway.watches).toEqual([])
  })

  it("stops an active task through the Gateway cancellation route", async () => {
    const gateway = new ChatGatewayDouble()
    gateway.streams = [[]]
    const user = userEvent.setup()
    renderChat(gateway)

    await sendPrompt(user, "long task")
    await waitFor(() => {
      expect(gateway.watches).toHaveLength(1)
    })

    await user.click(screen.getByRole("button", { name: "Stop task" }))

    expect(await screen.findByText("Task cancelled")).toBeInTheDocument()
    expect(gateway.cancels).toEqual(["exec-1"])
  })
})

describe("target selection", () => {
  it("persists only the non-secret target names", async () => {
    const gateway = new ChatGatewayDouble()
    const storage = memoryStorage()
    const selection = createChatSelectionStore(storage)
    renderChat(gateway, { selection })

    await waitFor(() =>
      expect(selection.read()).toEqual({
        worker: "general-a",
        agent: "general",
        profile: "default",
      }),
    )

    const raw = storage.getItem(chatSelectionStorageKey) ?? ""
    expect(raw).not.toContain("caller-token")
    expect(raw).not.toContain("Authorization")
    expect(Object.keys(JSON.parse(raw)).sort()).toEqual([
      "agent",
      "profile",
      "worker",
    ])
  })

  it("blocks sending when no reachable Worker is visible", async () => {
    const gateway = new ChatGatewayDouble()
    const user = userEvent.setup()
    renderChat(gateway, { store: connectedStore([]) })

    await user.type(screen.getByLabelText("Message"), "hello?")

    expect(
      await screen.findByText(/No reachable Worker is visible/),
    ).toBeInTheDocument()
    expect(screen.getByRole("button", { name: "Send message" })).toBeDisabled()
    expect(gateway.submits).toEqual([])
  })

  it("blocks sending when the selected Worker advertises no agent", async () => {
    const toolsOnly: WorkerSummary = {
      ...worker,
      id: "tools-only",
      instance: "tools-only",
      capabilities: [
        { id: "exec_command", kind: "tool", description: "Shell tool" },
      ],
    }
    const gateway = new ChatGatewayDouble()
    const user = userEvent.setup()
    renderChat(gateway, {
      store: connectedStore([toolsOnly]),
      selection: selectionWith({
        worker: "tools-only",
        agent: "exec_command",
        profile: "default",
      }),
    })

    await user.type(screen.getByLabelText("Message"), "hello?")

    expect(
      await screen.findByText(/advertises no agent capability/),
    ).toBeInTheDocument()
    expect(screen.getByRole("button", { name: "Send message" })).toBeDisabled()
    expect(gateway.submits).toEqual([])
  })

  it("keeps an unreachable persisted Worker selected and disables Send", async () => {
    const down: WorkerSummary = {
      ...worker,
      id: "down-a",
      instance: "down-a",
      reachable: false,
    }
    const fallback: WorkerSummary = { ...worker, id: "general-b" }
    const gateway = new ChatGatewayDouble()
    const user = userEvent.setup()
    renderChat(gateway, {
      store: connectedStore([down, fallback]),
      selection: selectionWith({
        worker: "down-a",
        agent: "general",
        profile: "default",
      }),
    })

    await user.type(screen.getByLabelText("Message"), "hello?")

    expect(
      await screen.findByText(/unreachable or not accepting tasks/),
    ).toBeInTheDocument()
    expect(screen.getByLabelText("Worker")).toHaveValue("down-a")
    expect(screen.getByRole("button", { name: "Send message" })).toBeDisabled()
    expect(gateway.submits).toEqual([])
  })

  it("blocks sending when a persisted profile disappeared", async () => {
    const gateway = new ChatGatewayDouble()
    const user = userEvent.setup()
    renderChat(gateway, {
      selection: selectionWith({
        worker: "general-a",
        agent: "general",
        profile: "retired",
      }),
    })

    await user.type(screen.getByLabelText("Message"), "hello?")

    expect(
      await screen.findByText(/Runtime Profile retired is not available/),
    ).toBeInTheDocument()
    expect(screen.getByRole("button", { name: "Send message" })).toBeDisabled()
    expect(gateway.submits).toEqual([])
  })

  it("shows the active revision and activates the latest revision", async () => {
    const profiles = new ProfileGatewayDouble()
    const inactive: Profile = {
      name: "default",
      active_revision: 0,
      revisions: [
        {
          revision: 1,
          instruction: "",
          model: "provider/model-1",
          composition: "single",
          created_at: 1,
        },
        {
          revision: 3,
          instruction: "",
          model: "provider/model-3",
          composition: "single",
          created_at: 3,
        },
      ],
    }
    profiles.set("general-a", [inactive])
    const gateway = new ChatGatewayDouble()
    gateway.streams = [
      [
        event(1, "task.accepted", { state: "accepted" }),
        event(2, "task.succeeded", { state: "succeeded", output: "done" }),
      ],
    ]
    const user = userEvent.setup()
    renderChat(gateway, {
      profiles,
      selection: selectionWith({
        worker: "general-a",
        agent: "general",
        profile: "default",
      }),
    })

    expect(
      await screen.findByText(/No active revision · latest r3 · provider\/model-3/),
    ).toBeInTheDocument()
    await user.type(screen.getByLabelText("Message"), "after activation")
    expect(screen.getByRole("button", { name: "Send message" })).toBeDisabled()

    await user.click(screen.getByRole("button", { name: "Activate r3" }))

    expect(await screen.findByText("Activated default revision 3.")).toBeInTheDocument()
    expect(profiles.activations).toEqual([
      { worker: "general-a", name: "default", revision: 3 },
    ])
    expect(
      await screen.findByText(/Active revision 3 · provider\/model-3/),
    ).toBeInTheDocument()

    const send = screen.getByRole("button", { name: "Send message" })
    await waitFor(() => expect(send).toBeEnabled())
    await user.click(send)

    expect(await screen.findByText("done")).toBeInTheDocument()
    expect(gateway.submits[0]).toMatchObject({
      worker: "general-a",
      agent: "general",
      profile: "default",
    })
  })

  it("shows a denied activation without changing the active revision", async () => {
    const profiles = new ProfileGatewayDouble()
    profiles.set("general-a", [
      {
        name: "default",
        active_revision: 1,
        revisions: [
          {
            revision: 1,
            instruction: "",
            model: "provider/model-1",
            composition: "single",
            created_at: 1,
          },
          {
            revision: 2,
            instruction: "",
            model: "provider/model-2",
            composition: "single",
            created_at: 2,
          },
        ],
      },
    ])
    profiles.activationError = new GatewayError({
      kind: "forbidden",
      status: 403,
      code: "scope.profile.write",
      message: "The caller token lacks the profile:write scope.",
      retryable: false,
    })
    const user = userEvent.setup()
    renderChat(new ChatGatewayDouble(), { profiles })

    const activate = await screen.findByRole("button", { name: "Activate r2" })
    await user.click(activate)

    expect(
      await screen.findByText(/The caller token lacks the profile:write scope\./),
    ).toBeInTheDocument()
    expect(
      screen.getByText(/cannot activate profiles; scope cannot be widened/),
    ).toBeInTheDocument()
    expect(screen.getByText(/Active revision 1 · provider\/model-1/)).toBeInTheDocument()
    expect(screen.queryByText(/Activated default/)).not.toBeInTheDocument()
  })

  it("applies selection changes to the next task without rewriting the active one", async () => {
    const gateway = new ChatGatewayDouble()
    let release: () => void = () => {}
    const gate = new Promise<void>((resolve) => {
      release = resolve
    })
    gateway.streamFactories = [
      () =>
        new GatedStream(
          [
            event(1, "task.accepted", { state: "accepted" }),
            event(2, "model.delta", { text: "working" }, false),
          ],
          gate,
          [
            event(3, "task.succeeded", {
              state: "succeeded",
              output: "working done",
            }),
          ],
        ),
      () =>
        new TestStream([
          event(4, "task.accepted", { state: "accepted" }),
          event(5, "task.succeeded", { state: "succeeded", output: "echo done" }),
        ]),
    ]
    const user = userEvent.setup()
    renderChat(gateway)

    await sendPrompt(user, "first task")
    expect(await screen.findByText("working")).toBeInTheDocument()

    await user.selectOptions(screen.getByLabelText("Agent"), "echo")

    expect(gateway.submits).toHaveLength(1)
    expect(gateway.submits[0]?.agent).toBe("general")

    release()
    expect(await screen.findByText("working done")).toBeInTheDocument()

    await sendPrompt(user, "second task")

    expect(await screen.findByText("echo done")).toBeInTheDocument()
    expect(gateway.submits[1]).toMatchObject({
      worker: "general-a",
      agent: "echo",
      profile: "default",
      input: "second task",
    })
  })
})

describe("app shell chat navigation", () => {
  it("routes to chat from the primary navigation", async () => {
    window.location.hash = "#/overview"
    const store = connectedStore()
    const user = userEvent.setup()
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    })
    render(
      <QueryClientProvider client={queryClient}>
        <App store={store} />
      </QueryClientProvider>,
    )

    expect(screen.getByText("Gateway connected")).toBeInTheDocument()

    await user.click(screen.getByRole("link", { name: "Chat" }))

    expect(await screen.findByRole("heading", { name: "Chat" })).toBeInTheDocument()
    expect(screen.getByLabelText("Message")).toBeInTheDocument()
    expect(screen.getByText("Start a task")).toBeInTheDocument()
  })
})

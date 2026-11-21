import type { GatewayWorker, Profile } from "@goldenbee/sdk"
import { describe, expect, it } from "vitest"

import {
  activeRevision,
  agentCapabilities,
  latestRevisionNumber,
  resolveSelection,
  selectableWorkers,
} from "./chat-selection"
import type { ChatSelection } from "./chat-types"

const empty: ChatSelection = { worker: "", agent: "", profile: "" }

function worker(overrides: Partial<GatewayWorker> = {}): GatewayWorker {
  return {
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
      { id: "exec_command", kind: "tool", description: "Shell tool" },
    ],
    ...overrides,
  }
}

function profile(
  name: string,
  activeRevision: number,
  revisions = [1, 2, 3],
): Profile {
  return {
    name,
    active_revision: activeRevision,
    revisions: revisions.map((revision) => ({
      revision,
      instruction: "",
      model: `provider/model-${revision}`,
      composition: "single",
      created_at: revision,
    })),
  }
}

describe("selectable workers", () => {
  it("lists only reachable Workers that accept work", () => {
    const workers = [
      worker({ id: "ready-a" }),
      worker({ id: "down-a", reachable: false }),
      worker({ id: "disabled-a", status: "disabled" }),
      worker({ id: "registered-b", status: "registered" }),
    ]

    expect(selectableWorkers(workers).map((candidate) => candidate.id)).toEqual([
      "ready-a",
      "registered-b",
    ])
  })
})

describe("agent capabilities", () => {
  it("offers only agent capabilities and never tool capabilities", () => {
    expect(agentCapabilities(worker()).map((capability) => capability.id)).toEqual([
      "general",
      "echo",
    ])
    expect(agentCapabilities(undefined)).toEqual([])
  })
})

describe("profile revisions", () => {
  it("reports the active revision and the latest revision", () => {
    const candidate = profile("default", 2)

    expect(activeRevision(candidate)?.model).toBe("provider/model-2")
    expect(latestRevisionNumber(candidate)).toBe(3)
    expect(latestRevisionNumber(profile("empty", 0, []))).toBe(0)
  })
})

describe("selection resolution", () => {
  it("reports an actionable error when no Worker is visible", () => {
    const resolved = resolveSelection({ workers: [], selection: empty })

    expect(resolved.ready).toBe(false)
    expect(resolved.issues).toEqual([
      expect.objectContaining({ field: "worker", code: "worker.none" }),
    ])
  })

  it("does not silently replace an unreachable selected Worker", () => {
    const resolved = resolveSelection({
      workers: [
        worker({ id: "down-a", reachable: false }),
        worker({ id: "general-b" }),
      ],
      selection: { worker: "down-a", agent: "general" },
    })

    expect(resolved.ready).toBe(false)
    expect(resolved.selection.worker).toBe("down-a")
    expect(resolved.worker).toBeNull()
    expect(resolved.issues).toEqual([
      expect.objectContaining({
        field: "worker",
        code: "worker.unavailable",
      }),
    ])
  })

  it("reports a Worker that disappeared from the caller's allow-list", () => {
    const resolved = resolveSelection({
      workers: [worker({ id: "general-b" })],
      selection: { worker: "general-a", agent: "general" },
    })

    expect(resolved.ready).toBe(false)
    expect(resolved.issues[0]).toMatchObject({
      code: "worker.unavailable",
      message: expect.stringContaining("no longer visible"),
    })
  })

  it("reports a Worker with no agent capability", () => {
    const resolved = resolveSelection({
      workers: [
        worker({
          id: "tools-only",
          capabilities: [
            { id: "exec_command", kind: "tool", description: "Shell tool" },
          ],
        }),
      ],
      selection: { worker: "tools-only", agent: "" },
    })

    expect(resolved.ready).toBe(false)
    expect(resolved.issues).toEqual([
      expect.objectContaining({ field: "agent", code: "agent.none" }),
    ])
  })

  it("rejects an agent the selected Worker does not advertise", () => {
    const resolved = resolveSelection({
      workers: [worker()],
      selection: { worker: "general-a", agent: "exec_command" },
    })

    expect(resolved.ready).toBe(false)
    expect(resolved.issues).toEqual([
      expect.objectContaining({ field: "agent", code: "agent.unavailable" }),
    ])
  })

  it("defaults a blank selection to the first reachable Worker, agent, and active profile", () => {
    const resolved = resolveSelection({
      workers: [worker({ id: "general-a" }), worker({ id: "general-b" })],
      selection: empty,
      profiles: [profile("default", 2)],
    })

    expect(resolved.ready).toBe(true)
    expect(resolved.issues).toEqual([])
    expect(resolved.selection).toEqual({
      worker: "general-a",
      agent: "general",
      profile: "default",
    })
  })

  it("keeps a valid agent and profile when the Worker still offers them", () => {
    const resolved = resolveSelection({
      workers: [worker()],
      selection: { worker: "general-a", agent: "echo", profile: "default" },
      profiles: [profile("default", 1)],
    })

    expect(resolved.ready).toBe(true)
    expect(resolved.selection).toEqual({
      worker: "general-a",
      agent: "echo",
      profile: "default",
    })
  })

  it("reports a profile that disappeared from the Worker", () => {
    const resolved = resolveSelection({
      workers: [worker()],
      selection: { worker: "general-a", agent: "general", profile: "gone" },
      profiles: [profile("default", 1)],
    })

    expect(resolved.ready).toBe(false)
    expect(resolved.issues).toEqual([
      expect.objectContaining({ field: "profile", code: "profile.unavailable" }),
    ])
  })

  it("reports an inactive profile and the revision that would activate it", () => {
    const resolved = resolveSelection({
      workers: [worker()],
      selection: { worker: "general-a", agent: "general", profile: "default" },
      profiles: [profile("default", 0)],
    })

    expect(resolved.ready).toBe(false)
    expect(resolved.issues[0]).toMatchObject({
      code: "profile.inactive",
      message: expect.stringContaining("revision 3"),
    })
  })

  it("waits for the profile catalog before accepting a named profile", () => {
    const resolved = resolveSelection({
      workers: [worker()],
      selection: { worker: "general-a", agent: "general", profile: "default" },
      profiles: null,
    })

    expect(resolved.ready).toBe(false)
    expect(resolved.issues).toEqual([
      expect.objectContaining({ field: "profile", code: "profile.loading" }),
    ])
  })

  it("allows the Worker default when the profile catalog cannot be read", () => {
    const resolved = resolveSelection({
      workers: [worker()],
      selection: { worker: "general-a", agent: "general", profile: "" },
      profiles: null,
      profileError: "The token lacks profile:read.",
    })

    expect(resolved.ready).toBe(true)
    expect(resolved.issues).toEqual([])
    expect(resolved.selection.profile).toBeUndefined()
  })

  it("refuses to send a profile it cannot validate", () => {
    const resolved = resolveSelection({
      workers: [worker()],
      selection: { worker: "general-a", agent: "general", profile: "default" },
      profiles: null,
      profileError: "The token lacks profile:read.",
    })

    expect(resolved.ready).toBe(false)
    expect(resolved.issues).toEqual([
      expect.objectContaining({
        field: "profile",
        code: "profile.catalog_unavailable",
      }),
    ])
  })

  it("requires an active profile when the Worker has no active revision", () => {
    const resolved = resolveSelection({
      workers: [worker()],
      selection: empty,
      profiles: [profile("idle", 0)],
    })

    expect(resolved.ready).toBe(false)
    expect(resolved.issues).toEqual([
      expect.objectContaining({ field: "profile", code: "profile.none_active" }),
    ])
  })
})

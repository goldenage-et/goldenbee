import { describe, expect, it } from "vitest"

import {
  buildCreateTeamRequest,
  buildUpdateTeamRequest,
  emptyTeamMemberDraft,
  validateTeamForm,
  type TeamFormValues,
} from "./team-form"

function values(overrides: Partial<TeamFormValues> = {}): TeamFormValues {
  return {
    name: "research",
    master: "general-a",
    locality: "any",
    members: [
      {
        ...emptyTeamMemberDraft(),
        worker: "general-b",
        capabilities: "general, research",
      },
    ],
    maxDepth: "",
    maxFanout: "",
    ...overrides,
  }
}

describe("team form validation", () => {
  it("builds a typed create request without sending blank optional values", () => {
    const result = buildCreateTeamRequest(
      values({ locality: "local", maxDepth: "3", maxFanout: "2" }),
    )

    expect(result.issues).toEqual([])
    expect(result.request).toEqual({
      name: "research",
      master: "general-a",
      locality: "local",
      limits: { max_depth: 3, max_fanout: 2 },
      members: [
        {
          worker: "general-b",
          capabilities: ["general", "research"],
        },
      ],
    })
  })

  it("requires a name, a master, and a subworker distinct from the master", () => {
    const issues = validateTeamForm(
      values({
        name: " ",
        master: "",
        members: [{ ...emptyTeamMemberDraft(), capabilities: "general" }],
      }),
    )

    expect(issues.map((issue) => issue.field)).toEqual([
      "name",
      "master",
      "member-0-worker",
    ])
  })

  it("rejects a roster with no member distinct from the master", () => {
    const result = buildCreateTeamRequest(
      values({
        members: [
          { ...emptyTeamMemberDraft(), worker: "general-a", capabilities: "general" },
        ],
      }),
    )

    expect(result.request).toBeUndefined()
    expect(result.issues).toContainEqual({
      field: "members",
      message: "Add at least one member that is not the Master.",
    })
  })

  it("requires a capability for every member and rejects duplicates", () => {
    const result = buildCreateTeamRequest(
      values({
        members: [
          { ...emptyTeamMemberDraft(), worker: "general-b", capabilities: " " },
          { ...emptyTeamMemberDraft(), worker: "general-b", capabilities: "general" },
        ],
      }),
    )

    expect(result.request).toBeUndefined()
    expect(result.issues.map((issue) => issue.field)).toContain(
      "member-0-capabilities",
    )
    expect(result.issues.map((issue) => issue.field)).toContain("member-1-worker")
  })

  it("keeps team and member depth inside the Gateway ceilings", () => {
    const result = buildCreateTeamRequest(
      values({
        maxDepth: "17",
        members: [
          { ...emptyTeamMemberDraft(), worker: "general-b", capabilities: "general" },
        ],
      }),
    )

    expect(result.request).toBeUndefined()
    expect(result.issues.map((issue) => issue.field)).toContain("max_depth")
  })

  it("carries the observed version on a revision", () => {
    const result = buildUpdateTeamRequest(values(), 4)

    expect(result.issues).toEqual([])
    expect(result.request).toMatchObject({
      expected_version: 4,
      master: "general-a",
    })
  })

  it("rejects a fanout below the number of subworkers before the request", () => {
    const result = buildUpdateTeamRequest(
      values({
        maxFanout: "0",
        members: [
          { ...emptyTeamMemberDraft(), worker: "general-b", capabilities: "general" },
          { ...emptyTeamMemberDraft(), worker: "general-c", capabilities: "general" },
        ],
      }),
      2,
    )

    expect(result.request).toBeUndefined()
    expect(result.issues.map((issue) => issue.field)).toContain("max_fanout")
  })
})

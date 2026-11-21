import { describe, expect, it } from "vitest"

import {
  buildCreateProfileRequest,
  buildReviseProfileRequest,
  emptyProfileFormValues,
  profileFormValues,
  type ProfileFormValues,
} from "./profile-form"
import { profileFixture } from "./test-fixtures"

function values(overrides: Partial<ProfileFormValues> = {}): ProfileFormValues {
  return {
    ...emptyProfileFormValues(),
    name: "research",
    instruction: "Research the question and cite evidence.",
    model: "openai/gpt-4.1",
    ...overrides,
  }
}

describe("runtime profile form validation", () => {
  it("builds a typed create request with lists, limits, and compaction", () => {
    const result = buildCreateProfileRequest(
      values({
        capabilities: "general, research",
        grants: "fs.read, net.read",
        restrictions: "fs.write",
        composition: "sequential",
        members: "planner, writer",
        skills: "citation",
        mcp: "search",
        workflow: "review",
        secretRefs: "env:SEARCH_KEY, vault:search",
        maxModelCalls: "4",
        compactionEnabled: true,
        contextBudget: "64000",
        preserveRecent: "8",
        maxRetries: "2",
      }),
    )

    expect(result.issues).toEqual([])
    expect(result.request).toEqual({
      name: "research",
      revision: {
        revision: 0,
        created_at: 0,
        instruction: "Research the question and cite evidence.",
        model: "openai/gpt-4.1",
        capabilities: ["general", "research"],
        grants: ["fs.read", "net.read"],
        restrictions: ["fs.write"],
        composition: "sequential",
        members: ["planner", "writer"],
        skills: ["citation"],
        mcp: ["search"],
        workflow: "review",
        secret_refs: ["env:SEARCH_KEY", "vault:search"],
        limits: { max_model_calls: 4 },
        compaction: {
          strategy: "truncation",
          context_budget: 64000,
          preserve_recent: 8,
          max_retries: 2,
        },
      },
    })
  })

  it("requires an instruction, a model, and a name before sending", () => {
    const result = buildCreateProfileRequest(
      values({ name: " ", instruction: " ", model: "" }),
    )

    expect(result.request).toBeUndefined()
    expect(result.issues.map((issue) => issue.field)).toEqual([
      "name",
      "instruction",
      "model",
    ])
  })

  it("rejects an unknown composition instead of sending it", () => {
    const result = buildCreateProfileRequest(
      values({ composition: "swarm" as ProfileFormValues["composition"] }),
    )

    expect(result.request).toBeUndefined()
    expect(result.issues).toContainEqual({
      field: "composition",
      message: "Choose a supported composition.",
    })
  })

  it("validates compaction numbers before activation can fail", () => {
    const result = buildCreateProfileRequest(
      values({
        compactionEnabled: true,
        contextBudget: "0",
        preserveRecent: "-1",
        maxRetries: "two",
      }),
    )

    expect(result.request).toBeUndefined()
    expect(result.issues.map((issue) => issue.field)).toEqual([
      "context_budget",
      "preserve_recent",
      "max_retries",
    ])
  })

  it("treats secret_refs as names only and rejects a pasted value", () => {
    const result = buildCreateProfileRequest(
      values({ secretRefs: "env:SEARCH_KEY, sk-live-123" }),
    )

    expect(result.request).toBeUndefined()
    expect(result.issues).toContainEqual({
      field: "secret_refs",
      message:
        "Secret references must use secret://, env:, or vault: names, never credential values.",
    })
  })

  it("carries the observed revision count on a revision", () => {
    const result = buildReviseProfileRequest(values(), 3)

    expect(result.issues).toEqual([])
    expect(result.request).toMatchObject({
      expected_revision: 3,
      revision: {
        revision: 0,
        instruction: "Research the question and cite evidence.",
      },
    })
    expect(result.request).not.toHaveProperty("name")
  })

  it("seeds a form from the latest revision and keeps secret refs as names", () => {
    const profile = profileFixture()
    const latest = profile.revisions[profile.revisions.length - 1]
    const form = profileFormValues(profile, latest)

    expect(form.name).toBe("research")
    expect(form.model).toBe("openai/gpt-4.1")
    expect(form.skills).toBe("citation")
    expect(form.secretRefs).toBe("env:SEARCH_KEY")
    expect(form.compactionEnabled).toBe(true)
    expect(form.contextBudget).toBe("64000")
  })
})

import { describe, expect, it } from "vitest"

import { parseRoute, routeHref } from "./router"

describe("app hash routes", () => {
  it("keeps the overview as the default route", () => {
    expect(parseRoute("")).toEqual({ name: "overview" })
    expect(parseRoute("#/")).toEqual({ name: "overview" })
    expect(parseRoute("#/unknown")).toEqual({ name: "overview" })
  })

  it("round-trips catalog, chat, and provider routes", () => {
    expect(parseRoute("#/providers")).toEqual({ name: "providers" })
    expect(parseRoute("#/providers/openrouter")).toEqual({
      name: "provider",
      id: "openrouter",
    })
    expect(parseRoute("#/chat")).toEqual({ name: "chat" })
    expect(routeHref({ name: "chat" })).toBe("#/chat")
    expect(parseRoute("#/attention")).toEqual({ name: "attention" })
    expect(routeHref({ name: "attention" })).toBe("#/attention")
    expect(parseRoute(routeHref({ name: "provider", id: "team alpha" }))).toEqual({
      name: "provider",
      id: "team alpha",
    })
  })

  it("round-trips package and instance routes", () => {
    expect(parseRoute("#/packages")).toEqual({ name: "packages" })
    expect(parseRoute("#/instances")).toEqual({ name: "instances" })
    expect(parseRoute("#/instances/general-a")).toEqual({
      name: "instance",
      instance: "general-a",
    })
    expect(
      parseRoute(routeHref({ name: "instance", instance: "general b" })),
    ).toEqual({ name: "instance", instance: "general b" })
  })

  it("round-trips team routes", () => {
    expect(parseRoute("#/teams")).toEqual({ name: "teams" })
    expect(routeHref({ name: "teams" })).toBe("#/teams")
    expect(parseRoute("#/teams/research")).toEqual({
      name: "team",
      team: "research",
    })
    expect(parseRoute(routeHref({ name: "team", team: "team alpha" }))).toEqual({
      name: "team",
      team: "team alpha",
    })
  })

  it("round-trips Worker-scoped profile routes", () => {
    expect(parseRoute("#/profiles")).toEqual({ name: "profiles" })
    expect(routeHref({ name: "profiles" })).toBe("#/profiles")
    expect(parseRoute("#/profiles/general-a")).toEqual({
      name: "profiles",
      worker: "general-a",
    })
    expect(
      parseRoute(routeHref({ name: "profiles", worker: "general a" })),
    ).toEqual({ name: "profiles", worker: "general a" })
    expect(parseRoute("#/profiles/general-a/research")).toEqual({
      name: "profile",
      worker: "general-a",
      profile: "research",
    })
    expect(
      parseRoute(
        routeHref({ name: "profile", worker: "general a", profile: "deep/dive" }),
      ),
    ).toEqual({ name: "profile", worker: "general a", profile: "deep/dive" })
  })

  it("round-trips the task detail route", () => {
    expect(parseRoute("#/tasks/exec-1")).toEqual({
      name: "task",
      taskId: "exec-1",
    })
    expect(routeHref({ name: "task", taskId: "exec 1" })).toBe("#/tasks/exec%201")
    expect(
      parseRoute(routeHref({ name: "task", taskId: "exec 1" })),
    ).toEqual({ name: "task", taskId: "exec 1" })
  })

  it("round-trips the diagnostics and audit routes", () => {
    expect(parseRoute("#/diagnostics")).toEqual({ name: "diagnostics" })
    expect(routeHref({ name: "diagnostics" })).toBe("#/diagnostics")
    expect(parseRoute("#/audit")).toEqual({ name: "audit" })
    expect(routeHref({ name: "audit" })).toBe("#/audit")
    // Existing routes are untouched by the operator additions.
    expect(parseRoute("#/chat")).toEqual({ name: "chat" })
    expect(parseRoute("#/attention")).toEqual({ name: "attention" })
  })
})

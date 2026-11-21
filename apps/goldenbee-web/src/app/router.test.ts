import { describe, expect, it } from "vitest"

import { parseRoute, routeHref } from "./router"

describe("app hash routes", () => {
  it("keeps the overview as the default route", () => {
    expect(parseRoute("")).toEqual({ name: "overview" })
    expect(parseRoute("#/")).toEqual({ name: "overview" })
    expect(parseRoute("#/unknown")).toEqual({ name: "overview" })
  })

  it("round-trips catalog and provider routes", () => {
    expect(parseRoute("#/providers")).toEqual({ name: "providers" })
    expect(parseRoute("#/providers/openrouter")).toEqual({
      name: "provider",
      id: "openrouter",
    })
    expect(parseRoute(routeHref({ name: "provider", id: "team alpha" }))).toEqual({
      name: "provider",
      id: "team alpha",
    })
  })
})

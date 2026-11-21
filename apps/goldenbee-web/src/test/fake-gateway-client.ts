import type { GatewayClient } from "@goldenbee/sdk"

/**
 * Test seam for management pages. Every Gateway method starts as a typed
 * failure so a test only has to name the calls the page is allowed to make.
 */
export function fakeGatewayClient(
  overrides: Partial<GatewayClient> = {},
): GatewayClient {
  const target = {
    baseUrl: "http://gateway.test",
    hasToken: true,
    ...overrides,
  } as GatewayClient

  return new Proxy(target, {
    get(object, property, receiver) {
      const value = Reflect.get(object, property, receiver)
      if (value !== undefined) {
        return value
      }
      return () =>
        Promise.reject(
          new Error(`Unexpected Gateway call: ${String(property)}`),
        )
    },
  })
}

/**
 * One stable operation identity per user action.
 *
 * Retrying a provision, upgrade, or rollback must resend the same id. A new
 * user action gets a new id. The Gateway rejects an id reused with a different
 * body as `host.operation_conflict`.
 */
export function newOperationId(prefix = "host"): string {
  const cryptoApi = globalThis.crypto
  if (cryptoApi && typeof cryptoApi.randomUUID === "function") {
    return `${prefix}-${cryptoApi.randomUUID()}`
  }
  return `${prefix}-${Date.now().toString(36)}-${Math.random()
    .toString(36)
    .slice(2)}`
}

/** Split the comma or newline separated grants field into scope entries. */
export function parseGrants(value: string): string[] {
  return value
    .split(/[\n,]/)
    .map((entry) => entry.trim())
    .filter((entry) => entry.length > 0)
}

/**
 * Fixed, non-production credentials for the real cross-process E2E stack.
 *
 * These values only exist in temporary test configuration. They are not
 * deployment defaults, are never generated from a real secret, and are not
 * accepted by any production configuration in this repository.
 */
export const E2E_BASE_URL = "http://127.0.0.1:5173"

export const E2E_CALLER_TOKEN = "e2e-caller-token-not-a-secret"
export const E2E_READONLY_TOKEN = "e2e-readonly-token-not-a-secret"
export const E2E_DOWN_TOKEN = "e2e-down-token-not-a-secret"
export const E2E_INVALID_TOKEN = "e2e-invalid-token-not-a-secret"
export const E2E_WORKER_TOKEN = "e2e-worker-token-not-a-secret"
export const E2E_DELEGATION_SECRET = "e2e-delegation-secret-not-a-secret"

export const E2E_GENERAL_WORKER = "worker-general"
export const E2E_EFFECT_WORKER = "worker-effect"
export const E2E_DOWN_WORKER = "worker-down"

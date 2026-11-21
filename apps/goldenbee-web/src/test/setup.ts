import "@testing-library/jest-dom/vitest"

import { cleanup } from "@testing-library/react"
import { afterEach } from "vitest"

afterEach(() => {
  cleanup()
  // Hash routes are window-global; keep an App test from leaking its route into
  // the next test file or case.
  window.history.replaceState(null, "", window.location.pathname)
})

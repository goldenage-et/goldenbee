import "@testing-library/jest-dom/vitest"

import { cleanup } from "@testing-library/react"
import { afterEach } from "vitest"

// jsdom does not implement matchMedia; the shadcn Sidebar and the Client
// viewport hooks use it during layout.
if (typeof window.matchMedia !== "function") {
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    writable: true,
    value: (query: string): MediaQueryList => ({
      matches: false,
      media: query,
      onchange: null,
      addListener: () => undefined,
      removeListener: () => undefined,
      addEventListener: () => undefined,
      removeEventListener: () => undefined,
      dispatchEvent: () => false,
    }),
  })
}

// jsdom does not implement the Web Animations API; Base UI ScrollArea probes it.
if (typeof Element.prototype.getAnimations !== "function") {
  Object.defineProperty(Element.prototype, "getAnimations", {
    configurable: true,
    writable: true,
    value: () => [],
  })
}

// jsdom does not implement ResizeObserver; react-resizable-panels mounts an
// observer for every group even when no layout change is possible.
if (typeof globalThis.ResizeObserver !== "function") {
  class ResizeObserverMock {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
  Object.defineProperty(globalThis, "ResizeObserver", {
    configurable: true,
    writable: true,
    value: ResizeObserverMock,
  })
}

// jsdom does not implement the Web Animations API; Base UI scroll areas check
// finite animations when their viewport settles.
if (typeof Element.prototype.getAnimations !== "function") {
  Object.defineProperty(Element.prototype, "getAnimations", {
    configurable: true,
    writable: true,
    value: () => [],
  })
}

afterEach(() => {
  cleanup()
  // Hash routes are window-global; keep an App test from leaking its route into
  // the next test file or case.
  window.history.replaceState(null, "", window.location.pathname)
})

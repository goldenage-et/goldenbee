/// <reference types="vitest/config" />
import path from "node:path"

import tailwindcss from "@tailwindcss/vite"
import react from "@vitejs/plugin-react"
import { defineConfig, loadEnv } from "vite"

// https://vite.dev/config/
export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "")
  // Server-side only: the browser never receives the Gateway address through
  // this variable. Development traffic uses the same-origin /v1 proxy. Bun's
  // process environment wins so a spawned E2E stack can target its own Gateway
  // without writing a `.env` file.
  const proxyTarget =
    process.env.GOLDENBEE_GATEWAY_URL ||
    env.GOLDENBEE_GATEWAY_URL ||
    "http://127.0.0.1:8080"

  return {
    plugins: [react(), tailwindcss()],
    resolve: {
      alias: {
        "@": path.resolve(import.meta.dirname, "./src"),
      },
    },
    server: {
      proxy: {
        "/v1": {
          target: proxyTarget,
          changeOrigin: true,
          ws: true,
        },
      },
    },
    test: {
      environment: "jsdom",
      setupFiles: ["./src/test/setup.ts"],
      include: ["src/**/*.test.{ts,tsx}"],
      restoreMocks: true,
      // The jsdom suites start one environment per file; under a fully loaded
      // machine a single interaction test can exceed the 5s default without
      // being slow on its own.
      testTimeout: 20_000,
    },
  }
})

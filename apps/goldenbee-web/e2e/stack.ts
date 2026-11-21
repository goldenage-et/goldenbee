/**
 * Real cross-process E2E stack for the Goldenbee Web Client.
 *
 * Starts one General Worker, one protected-effect fixture Worker, and one
 * `gb-gateway` instance with generated temporary configuration. Vite then runs
 * with `GOLDENBEE_GATEWAY_URL` pointed at the real Gateway so the browser uses
 * the same-origin `/v1` proxy exactly as it does in development.
 *
 * This file is not a test. Playwright's `webServer` runs it, waits for the Vite
 * URL, and stops it with SIGTERM. Every child process is terminated on exit.
 */
import { spawn, type ChildProcess } from "node:child_process"
import { once } from "node:events"
import { createWriteStream, existsSync } from "node:fs"
import { mkdir, rm, writeFile } from "node:fs/promises"
import { dirname, join, resolve } from "node:path"
import { createInterface } from "node:readline"
import { fileURLToPath } from "node:url"
import { randomUUID } from "node:crypto"

import {
  E2E_CALLER_TOKEN,
  E2E_DELEGATION_SECRET,
  E2E_DOWN_TOKEN,
  E2E_DOWN_WORKER,
  E2E_EFFECT_WORKER,
  E2E_GENERAL_WORKER,
  E2E_READONLY_TOKEN,
  E2E_WORKER_TOKEN,
} from "./tokens"

const here = dirname(fileURLToPath(import.meta.url))
const appRoot = resolve(here, "..")
const repoRoot = resolve(appRoot, "../..")
const stackRoot = join(here, ".stack")
const viteUrl = "http://127.0.0.1:5173"
const children = new Set<ChildProcess>()
let shuttingDown = false

const buildCommand =
  "cargo build --bin gb-gateway --bin general --bin effect-fixture"

interface ProcessResult {
  child: ChildProcess
  stop(): Promise<void>
}

function logPath(name: string): string {
  return join(stackRoot, `${name}.log`)
}

function requireBinaries(): string {
  const directory = process.env.GOLDENBEE_E2E_BIN_DIR
    ? resolve(process.env.GOLDENBEE_E2E_BIN_DIR)
    : join(repoRoot, "target", "debug")
  const missing = ["gb-gateway", "general", "effect-fixture"].filter(
    (name) => !existsSync(join(directory, name)),
  )
  if (missing.length > 0) {
    console.error(
      [
        "[e2e] missing Rust binaries: " + missing.join(", "),
        `[e2e] looked in ${directory}`,
        "[e2e] build them from the repository root with:",
        "",
        `  ${buildCommand}`,
        "",
      ].join("\n"),
    )
    process.exit(1)
  }
  return directory
}

async function stopChild(child: ChildProcess | undefined): Promise<void> {
  if (!child || child.exitCode !== null || child.signalCode !== null) {
    return
  }
  child.kill("SIGTERM")
  await Promise.race([
    once(child, "exit").then(() => undefined),
    new Promise<void>((resolvePromise) => setTimeout(resolvePromise, 3_000)),
  ])
  if (child.exitCode === null && child.signalCode === null) {
    child.kill("SIGKILL")
    await Promise.race([
      once(child, "exit").then(() => undefined),
      new Promise<void>((resolvePromise) => setTimeout(resolvePromise, 1_000)),
    ])
  }
}

async function shutdown(code: number): Promise<void> {
  if (shuttingDown) {
    return
  }
  shuttingDown = true
  await Promise.all([...children].map((child) => stopChild(child)))
  process.exit(code)
}

function track(child: ChildProcess): void {
  children.add(child)
  child.once("exit", () => children.delete(child))
  child.once("error", () => children.delete(child))
}

function attachLog(child: ChildProcess, name: string): void {
  const stream = createWriteStream(logPath(name), { flags: "a" })
  child.stdout?.pipe(stream, { end: false })
  child.stderr?.pipe(stream, { end: false })
  child.once("exit", () => stream.end())
}

/** Spawn a child and resolve once its first stdout line matches the pattern. */
async function spawnAnnounced(
  name: string,
  command: string,
  args: string[],
  env: NodeJS.ProcessEnv,
  pattern: RegExp,
): Promise<{ process: ProcessResult; announcement: string }> {
  const child = spawn(command, args, {
    cwd: repoRoot,
    env,
    stdio: ["ignore", "pipe", "pipe"],
  })
  track(child)
  if (!child.stdout) {
    throw new Error(`${name} did not expose stdout`)
  }
  const reader = createInterface({ input: child.stdout })
  attachLog(child, name)

  const announcement = await new Promise<string>((resolvePromise, reject) => {
    const timer = setTimeout(() => {
      reject(new Error(`${name} did not announce readiness within 30s`))
    }, 30_000)
    reader.on("line", (line) => {
      if (pattern.test(line)) {
        clearTimeout(timer)
        resolvePromise(line)
      }
    })
    child.once("exit", (code, signal) => {
      clearTimeout(timer)
      reject(
        new Error(
          `${name} exited before readiness (code=${code ?? "null"}, signal=${signal ?? "none"})`,
        ),
      )
    })
  })

  return {
    process: {
      child,
      stop: () => stopChild(child),
    },
    announcement,
  }
}

async function waitForHealth(
  name: string,
  base: string,
  ready?: (value: Record<string, unknown>) => boolean,
): Promise<Record<string, unknown>> {
  const deadline = Date.now() + 30_000
  let lastError = "no response"
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`${base}/v1/health`)
      if (response.ok) {
        const value = (await response.json()) as Record<string, unknown>
        if (!ready || ready(value)) {
          return value
        }
        lastError = `not ready: ${JSON.stringify(value.phase ?? value.status)}`
      } else {
        lastError = `HTTP ${response.status}`
      }
    } catch (error) {
      lastError = error instanceof Error ? error.message : String(error)
    }
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 50))
  }
  throw new Error(`${name} did not become healthy at ${base}: ${lastError}`)
}

function baseFromAnnouncement(line: string): string {
  const index = line.indexOf("listening on ")
  if (index === -1) {
    throw new Error(`unexpected listener announcement: ${line}`)
  }
  return line.slice(index + "listening on ".length).trim()
}

async function writeGeneralWorker(dataRoot: string, instance: string): Promise<string> {
  const assets = join(dataRoot, "assets")
  const agents = join(assets, "agents")
  const prompts = join(assets, "prompts")
  await mkdir(agents, { recursive: true })
  await mkdir(prompts, { recursive: true })
  await mkdir(join(dataRoot, "state"), { recursive: true })
  await mkdir(join(dataRoot, "workspace"), { recursive: true })
  await writeFile(
    join(agents, "general.toml"),
    [
      'name = "general"',
      'description = "General Worker"',
      'model = "deterministic"',
      'prompt = "general"',
      'tools = ["exec_command", "save_artifact"]',
      "",
    ].join("\n"),
  )
  await writeFile(
    join(prompts, "general.md"),
    "You are the Goldenbee General Worker. Answer usefully and concisely.",
  )
  const config = join(dataRoot, "worker.toml")
  await writeFile(
    config,
    [
      `instance = "${instance}"`,
      'namespace = "local"',
      'default_agent = "general"',
      `token = "${E2E_WORKER_TOKEN}"`,
      'token_scopes = ["*"]',
      'grants = ["*"]',
      "",
      "[paths]",
      `agents = "${agents}"`,
      `prompts = "${prompts}"`,
      "",
      "[storage]",
      `directory = "${join(dataRoot, "state")}"`,
      "",
    ].join("\n"),
  )
  return config
}

async function writeEffectWorker(dataRoot: string, instance: string): Promise<string> {
  const assets = join(dataRoot, "assets")
  const agents = join(assets, "agents")
  const prompts = join(assets, "prompts")
  await mkdir(agents, { recursive: true })
  await mkdir(prompts, { recursive: true })
  await mkdir(join(dataRoot, "state"), { recursive: true })
  await mkdir(join(dataRoot, "external"), { recursive: true })
  await mkdir(join(dataRoot, "faults"), { recursive: true })
  await writeFile(
    join(agents, "effect.toml"),
    [
      'name = "effect"',
      'description = "Protected effect agent"',
      'model = "deterministic"',
      'prompt = "effect"',
      'tools = ["ledger_write"]',
      "",
    ].join("\n"),
  )
  await writeFile(
    join(prompts, "effect.md"),
    "Write the requested ledger entry.",
  )
  const config = join(dataRoot, "worker.toml")
  await writeFile(
    config,
    [
      `instance = "${instance}"`,
      'namespace = "local"',
      'default_agent = "effect"',
      `token = "${E2E_WORKER_TOKEN}"`,
      'token_scopes = ["*"]',
      'grants = ["*"]',
      "",
      "[paths]",
      `agents = "${agents}"`,
      `prompts = "${prompts}"`,
      "",
      "[storage]",
      `directory = "${join(dataRoot, "state")}"`,
      "",
    ].join("\n"),
  )
  return config
}

async function main(): Promise<void> {
  await rm(stackRoot, { recursive: true, force: true })
  await mkdir(stackRoot, { recursive: true })
  const binaries = requireBinaries()
  const runId = randomUUID().slice(0, 8)

  const delegationEnv = {
    GOLDENBEE_DELEGATION_SECRET: E2E_DELEGATION_SECRET,
  }

  const generalData = join(stackRoot, "general")
  const generalConfig = await writeGeneralWorker(
    generalData,
    `e2e-general-${runId}`,
  )
  const general = await spawnAnnounced(
    "general",
    join(binaries, "general"),
    [
      "serve",
      "--config",
      generalConfig,
      "--addr",
      "127.0.0.1:0",
    ],
    {
      ...process.env,
      ...delegationEnv,
      GOLDENBEE_DATA_DIR: generalData,
      GOLDENBEE_SANDBOX_DIR: join(generalData, "workspace"),
    },
    /listening on /,
  )
  const generalBase = baseFromAnnouncement(general.announcement)
  const generalHealth = await waitForHealth("general worker", generalBase)

  const effectData = join(stackRoot, "effect")
  const effectConfig = await writeEffectWorker(effectData, `e2e-effect-${runId}`)
  const effect = await spawnAnnounced(
    "effect-fixture",
    join(binaries, "effect-fixture"),
    [
      "serve",
      "--config",
      effectConfig,
      "--addr",
      "127.0.0.1:0",
    ],
    {
      ...process.env,
      ...delegationEnv,
      GOLDENBEE_DATA_DIR: effectData,
      GOLDENBEE_EFFECT_DIR: join(effectData, "external"),
      GOLDENBEE_FAULT_DIR: join(effectData, "faults"),
      GOLDENBEE_EFFECT_APPROVAL: "1",
      GOLDENBEE_EFFECT_APPROVAL_TTL_MS: "600000",
    },
    /listening on /,
  )
  const effectBase = baseFromAnnouncement(effect.announcement)
  const effectHealth = await waitForHealth("effect fixture worker", effectBase)

  const gatewayData = join(stackRoot, "gateway")
  await mkdir(gatewayData, { recursive: true })
  const gatewayConfig = join(gatewayData, "gateway.json")
  await writeFile(
    gatewayConfig,
    JSON.stringify(
      {
        listen: "127.0.0.1:0",
        data_dir: join(gatewayData, "state"),
        registry: join(gatewayData, "registry.json"),
        max_streams: 256,
        callers: [
          {
            token: E2E_CALLER_TOKEN,
            id: "e2e-caller",
            namespace: "local",
            scopes: [
              "task:submit",
              "task:read",
              "task:cancel",
              "artifact:read",
              "approval:read",
              "approval:decide",
              "team:read",
              "team:write",
              "profile:read",
              "profile:write",
            ],
            workers: [E2E_GENERAL_WORKER, E2E_EFFECT_WORKER, E2E_DOWN_WORKER],
          },
          {
            token: E2E_READONLY_TOKEN,
            id: "e2e-readonly",
            namespace: "local",
            scopes: ["task:read", "team:read", "profile:read"],
            workers: [E2E_GENERAL_WORKER],
          },
          {
            token: E2E_DOWN_TOKEN,
            id: "e2e-down",
            namespace: "local",
            scopes: ["task:submit", "task:read"],
            workers: [E2E_DOWN_WORKER],
          },
        ],
        workers: [
          {
            id: E2E_GENERAL_WORKER,
            endpoint: generalBase,
            instance: generalHealth.instance,
          },
          {
            id: E2E_EFFECT_WORKER,
            endpoint: effectBase,
            instance: effectHealth.instance,
          },
          {
            // Port 1 is never served in test or CI environments, so this
            // Worker is approved but deterministically unreachable.
            id: E2E_DOWN_WORKER,
            endpoint: "http://127.0.0.1:1",
          },
        ],
      },
      null,
      2,
    ),
  )

  const gateway = await spawnAnnounced(
    "gb-gateway",
    join(binaries, "gb-gateway"),
    ["serve", "--addr", "127.0.0.1:0", "--config", gatewayConfig],
    {
      ...process.env,
      ...delegationEnv,
    },
    /listening on /,
  )
  const gatewayBase = baseFromAnnouncement(gateway.announcement)
  await waitForHealth("gateway", gatewayBase)

  const vite = spawn(process.execPath, [
    "x",
    "vite",
    "--host",
    "127.0.0.1",
    "--port",
    "5173",
    "--strictPort",
  ], {
    cwd: appRoot,
    env: {
      ...process.env,
      GOLDENBEE_GATEWAY_URL: gatewayBase,
    },
    stdio: ["ignore", "pipe", "pipe"],
  })
  track(vite)
  attachLog(vite, "vite")

  const viteDeadline = Date.now() + 60_000
  while (Date.now() < viteDeadline) {
    try {
      const response = await fetch(viteUrl)
      if (response.ok) {
        break
      }
    } catch {
      // Vite is still binding the listener.
    }
    if (vite.exitCode !== null) {
      throw new Error(`vite exited before readiness (code=${vite.exitCode})`)
    }
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 100))
  }

  const response = await fetch(viteUrl)
  if (!response.ok) {
    throw new Error(`vite did not serve ${viteUrl}`)
  }

  // Exactly one readiness line. It contains no caller token.
  console.log(
    `[e2e] stack ready vite=${viteUrl} gateway=${gatewayBase} workers=${E2E_GENERAL_WORKER},${E2E_EFFECT_WORKER} stack=${stackRoot}`,
  )

  // The webServer stays alive until Playwright sends SIGTERM.
  await Promise.race([
    once(gateway.process.child, "exit"),
    once(effect.process.child, "exit"),
    once(general.process.child, "exit"),
    once(vite, "exit"),
  ])
  if (!shuttingDown) {
    throw new Error("an E2E stack process exited unexpectedly")
  }
}

process.on("SIGINT", () => {
  void shutdown(0)
})
process.on("SIGTERM", () => {
  void shutdown(0)
})
process.on("uncaughtException", (error) => {
  console.error("[e2e] stack failure:", error)
  void shutdown(1)
})
process.on("unhandledRejection", (error) => {
  console.error("[e2e] stack failure:", error)
  void shutdown(1)
})

void main().catch((error: unknown) => {
  console.error("[e2e] stack failure:", error)
  void shutdown(1)
})

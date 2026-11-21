/**
 * Playwright trace sanitation.
 *
 * The browser must send `Authorization: Bearer <token>`, and Playwright's trace
 * recorder stores request headers. On a failing test the retained trace would
 * therefore contain the fixed, non-production E2E token. Nothing here changes
 * the app; after the run every trace archive is rewritten so no token remains in
 * a delivered artifact. A residual occurrence fails the teardown instead of
 * leaving a secret-bearing artifact behind.
 */
import { spawnSync } from "node:child_process"
import { mkdtemp, readdir, readFile, rm, stat, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { dirname, join, relative, resolve } from "node:path"
import { fileURLToPath } from "node:url"

import {
  E2E_CALLER_TOKEN,
  E2E_DELEGATION_SECRET,
  E2E_DOWN_TOKEN,
  E2E_INVALID_TOKEN,
  E2E_READONLY_TOKEN,
  E2E_WORKER_TOKEN,
} from "./tokens"

const here = dirname(fileURLToPath(import.meta.url))
const appRoot = resolve(here, "..")
const redaction = "[REDACTED-E2E-TOKEN]"
const secrets = [
  E2E_CALLER_TOKEN,
  E2E_READONLY_TOKEN,
  E2E_DOWN_TOKEN,
  E2E_INVALID_TOKEN,
  E2E_WORKER_TOKEN,
  E2E_DELEGATION_SECRET,
]

async function entries(directory: string): Promise<string[]> {
  const result: string[] = []
  let list
  try {
    list = await readdir(directory, { withFileTypes: true })
  } catch {
    return result
  }
  for (const entry of list) {
    const path = join(directory, entry.name)
    if (entry.isDirectory()) {
      result.push(...(await entries(path)))
    } else if (entry.isFile()) {
      result.push(path)
    }
  }
  return result
}

function run(command: string, args: string[], cwd: string) {
  return spawnSync(command, args, {
    cwd,
    encoding: "utf8",
    maxBuffer: 32 * 1024 * 1024,
  })
}

function containsSecret(value: Buffer): boolean {
  return secrets.some((secret) => value.includes(Buffer.from(secret)))
}

function redact(value: Buffer): Buffer {
  let next = value
  for (const secret of secrets) {
    next = Buffer.from(next.toString("latin1").split(secret).join(redaction), "latin1")
  }
  return next
}

async function zipContainsSecret(path: string): Promise<boolean> {
  const listing = run("unzip", ["-Z1", path], dirname(path))
  if (listing.error) {
    throw new Error(
      `[e2e] unzip is required to verify retained traces: ${listing.error.message}`,
    )
  }
  if (listing.status !== 0) {
    return containsSecret(await readFile(path))
  }
  const names = listing.stdout.split(/\r?\n/).filter(Boolean)
  for (const name of names) {
    const result = run("unzip", ["-p", path, name], dirname(path))
    if (result.error) {
      throw new Error(`[e2e] unzip is unavailable: ${result.error.message}`)
    }
    if (containsSecret(Buffer.from(result.stdout, "latin1"))) {
      return true
    }
  }
  return false
}

async function sanitizeZip(path: string): Promise<boolean> {
  if (!(await zipContainsSecret(path))) {
    return false
  }

  const directory = await mkdtemp(join(tmpdir(), "goldenbee-trace-"))
  const extract = run("unzip", ["-q", path, "-d", directory], dirname(path))
  if (extract.error || extract.status !== 0) {
    await rm(directory, { recursive: true, force: true })
    throw new Error(`[e2e] could not extract ${relative(appRoot, path)}`)
  }
  for (const file of await entries(directory)) {
    const bytes = await readFile(file)
    if (containsSecret(bytes)) {
      await writeFile(file, redact(bytes))
    }
  }
  await rm(path)
  const zip = run("zip", ["-qr", path, "."], directory)
  await rm(directory, { recursive: true, force: true })
  if (zip.error || zip.status !== 0) {
    throw new Error(`[e2e] could not rewrite ${relative(appRoot, path)}`)
  }
  if (await zipContainsSecret(path)) {
    throw new Error(
      `[e2e] retained trace still contains a token after redaction: ${relative(appRoot, path)}`,
    )
  }
  return true
}

export default async function globalTeardown(): Promise<void> {
  const roots = [
    join(appRoot, "test-results"),
    join(appRoot, "playwright-report"),
    join(appRoot, "blob-report"),
  ]
  let rewritten = 0
  for (const root of roots) {
    let rootStat
    try {
      rootStat = await stat(root)
    } catch {
      continue
    }
    if (!rootStat.isDirectory()) {
      continue
    }
    for (const file of await entries(root)) {
      if (file.endsWith(".zip")) {
        if (await sanitizeZip(file)) {
          rewritten += 1
        }
      } else {
        const bytes = await readFile(file)
        if (containsSecret(bytes)) {
          await writeFile(file, redact(bytes))
          rewritten += 1
        }
      }
    }
  }

  const residual: string[] = []
  for (const root of roots) {
    for (const file of await entries(root)) {
      const secret =
        file.endsWith(".zip")
          ? await zipContainsSecret(file)
          : containsSecret(await readFile(file))
      if (secret) {
        residual.push(relative(appRoot, file))
      }
    }
  }
  if (residual.length > 0) {
    throw new Error(
      `[e2e] caller token remained in test artifacts: ${residual.join(", ")}`,
    )
  }
  if (rewritten > 0) {
    console.log(
      `[e2e] redacted the caller token from ${rewritten} retained test artifact(s)`,
    )
  }
}

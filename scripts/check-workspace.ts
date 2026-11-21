/**
 * Workspace lock and package-manager check.
 *
 * Bun is the only package manager for the TypeScript workspace. A committed
 * npm, Yarn, or pnpm lockfile is a second, divergent dependency graph, so the
 * check fails on one. `bun.lock` must exist for the frozen CI install.
 */
import { readdir, stat } from "node:fs/promises"
import { dirname, join, relative, resolve } from "node:path"
import { fileURLToPath } from "node:url"

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..")
const ignored = new Set([
  ".git",
  "node_modules",
  "target",
  "dist",
  "coverage",
  ".stack",
  "test-results",
  "playwright-report",
])
const forbidden = new Set(["package-lock.json", "yarn.lock", "pnpm-lock.yaml"])
const found: string[] = []

async function walk(directory: string): Promise<void> {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    if (ignored.has(entry.name)) {
      continue
    }
    const path = join(directory, entry.name)
    if (entry.isDirectory()) {
      await walk(path)
    } else if (entry.isFile() && forbidden.has(entry.name)) {
      found.push(relative(root, path))
    }
  }
}

await walk(root)

if (found.length > 0) {
  console.error(
    "Bun is the only package manager for this workspace. Remove:\n",
  )
  for (const path of found) {
    console.error(`  ${path}`)
  }
  process.exit(1)
}

const lock = await stat(join(root, "bun.lock")).then(() => true).catch(() => false)
if (!lock) {
  console.error("bun.lock is missing; CI installs with --frozen-lockfile.")
  process.exit(1)
}

console.log("Workspace package manager is Bun and bun.lock is present.")

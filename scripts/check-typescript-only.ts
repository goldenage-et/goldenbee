import { readdir } from "node:fs/promises"
import { join, relative } from "node:path"

const roots = ["apps/goldenbee-web", "packages/goldenbee-sdk"]
const generated = new Set(["node_modules", "dist", "coverage", ".vite", ".tmp"])
const forbidden = /\.(js|jsx|mjs|cjs)$/

const violations: string[] = []

async function walk(directory: string): Promise<void> {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    if (generated.has(entry.name)) {
      continue
    }
    const path = join(directory, entry.name)
    if (entry.isDirectory()) {
      await walk(path)
      continue
    }
    if (entry.isFile() && forbidden.test(entry.name)) {
      violations.push(relative(process.cwd(), path))
    }
  }
}

for (const root of roots) {
  await walk(root)
}

if (violations.length > 0) {
  console.error(
    "Authored JavaScript is not allowed outside generated output. Use TypeScript instead:\n",
  )
  for (const violation of violations) {
    console.error(`  ${violation}`)
  }
  process.exit(1)
}

console.log("TypeScript-only authored source check passed.")

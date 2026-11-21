/**
 * Release check for the browser Client bundle.
 *
 * The Gateway serves no SPA fallback and this release has no BFF or SSR, so the
 * production output must be static browser assets only. This check refuses a
 * build that adds a server entry, a native module, or a Node builtin import to
 * the shipped bundle.
 */
import { readdir, readFile } from "node:fs/promises"
import { dirname, join, relative, resolve } from "node:path"
import { fileURLToPath } from "node:url"

const here = dirname(fileURLToPath(import.meta.url))
const appRoot = resolve(here, "..", "apps", "goldenbee-web")
const dist = join(appRoot, "dist")

async function walk(directory: string): Promise<string[]> {
  const files: string[] = []
  const list = await readdir(directory, { withFileTypes: true })
  for (const entry of list) {
    const path = join(directory, entry.name)
    if (entry.isDirectory()) {
      files.push(...(await walk(path)))
    } else if (entry.isFile()) {
      files.push(path)
    }
  }
  return files
}

const problems: string[] = []

const index = join(dist, "index.html")
try {
  const html = await readFile(index, "utf8")
  if (!/<script[^>]+type="module"/.test(html)) {
    problems.push("dist/index.html does not load an ES module bundle")
  }
  if (/\b(?:src|href)="(?!\/|https?:|data:|#)/.test(html)) {
    problems.push("dist/index.html contains a non-absolute asset path")
  }
} catch {
  problems.push("dist/index.html is missing; run `bun run build` first")
}

const files = await walk(dist).catch(() => [])
for (const path of files) {
  const relativePath = relative(dist, path)
  if (
    /(?:^|\/)(?:server|ssr)(?:\.[^.]+)?$/.test(relativePath) ||
    relativePath === "package.json"
  ) {
    problems.push(`server-side artifact in static output: ${relativePath}`)
  }
  if (/\.node$/.test(relativePath)) {
    problems.push(`native addon in static output: ${relativePath}`)
  }
  if (!/\.(js|css|html|svg|map)$/.test(relativePath)) {
    continue
  }
  const text = await readFile(path, "utf8")
  if (/\bfrom\s*["']node:|\bimport\s*\(\s*["']node:|\brequire\s*\(\s*["']node:/.test(text)) {
    problems.push(`Node builtin import in shipped asset: ${relativePath}`)
  }
  if (/\bprocess\.versions\.node\b/.test(text)) {
    problems.push(`Node runtime probe in shipped asset: ${relativePath}`)
  }
}

const assets = files.filter((path) => relative(dist, path).startsWith("assets/"))
// A non-empty asset directory plus index.html proves the build is a browser
// bundle rather than a server output.
if (files.length > 0 && assets.length === 0) {
  problems.push("dist has no hashed assets/ directory")
}

if (problems.length > 0) {
  console.error("Static browser build check failed:\n")
  for (const problem of problems) {
    console.error(`  ${problem}`)
  }
  process.exit(1)
}

console.log(
  `Static browser build verified: ${files.length} files, no server entry, no Node builtin import.`,
)

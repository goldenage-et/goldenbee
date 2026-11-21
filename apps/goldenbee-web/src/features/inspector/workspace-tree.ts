import type {
  StreamPage,
  WorkspaceEntry,
  WorkspaceTreePage,
} from "@goldenbee/sdk"

import type { FileTreeNodeData } from "@/components/ai-elements/file-tree"

export interface ChangedFile {
  path: string
  change: string
  additions: number
  deletions: number
}

export interface WorkspaceGateway {
  workspaceTree(
    worker: string,
    path?: string,
    limit?: number,
    cursor?: string,
  ): Promise<WorkspaceTreePage>
}

export interface EventsGateway {
  events(taskId: string, after: number, limit: number): Promise<StreamPage>
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function textField(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined
}

function numberField(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) ? value : 0
}

/**
 * Load every bounded directory page for one folder.
 *
 * The caller still bounds the total page count; a workspace cannot turn one
 * folder expansion into an unbounded request chain.
 */
export async function loadWorkspaceDirectory(
  client: WorkspaceGateway,
  worker: string,
  path: string,
): Promise<{ entries: WorkspaceEntry[]; truncated: boolean }> {
  const entries: WorkspaceEntry[] = []
  let cursor: string | undefined
  let truncated = false
  for (let page = 0; page < 10; page += 1) {
    const result = await client.workspaceTree(worker, path, 500, cursor)
    entries.push(...result.entries)
    truncated = result.truncated
    if (!result.next_cursor) {
      break
    }
    cursor = result.next_cursor
  }
  return { entries, truncated }
}

/**
 * Read the focused task's durable events and merge the latest change for each
 * workspace-relative path.
 */
export async function loadChangedFiles(
  client: EventsGateway,
  taskId: string,
): Promise<Map<string, ChangedFile>> {
  const changes = new Map<string, ChangedFile>()
  let after = 0
  for (let page = 0; page < 20; page += 1) {
    const result = await client.events(taskId, after, 500)
    for (const event of result.events) {
      if (event.kind !== "diff.recorded") {
        continue
      }
      const payload = isRecord(event.payload) ? event.payload : {}
      const files = Array.isArray(payload.files) ? payload.files : []
      for (const file of files) {
        if (!isRecord(file)) {
          continue
        }
        const path = textField(file.path)
        if (!path) {
          continue
        }
        changes.set(path, {
          path,
          change: textField(file.change) ?? "M",
          additions: numberField(file.additions),
          deletions: numberField(file.deletions),
        })
      }
    }
    if (result.next_seq <= after || result.cursor_expired) {
      break
    }
    after = result.next_seq
  }
  return changes
}

/** Merge one directory page into the recursive File Tree projection. */
export function buildFileTreeNodes(
  entries: WorkspaceEntry[],
  children: Readonly<Record<string, WorkspaceEntry[] | undefined>>,
  loading: ReadonlySet<string>,
  changes: ReadonlyMap<string, ChangedFile>,
): FileTreeNodeData[] {
  return entries.map((entry) => {
    const change = changes.get(entry.path)
    if (entry.kind === "directory") {
      const nested = children[entry.path]
      return {
        name: entry.name,
        path: entry.path,
        kind: "directory" as const,
        loading: loading.has(entry.path),
        children: nested
          ? buildFileTreeNodes(nested, children, loading, changes)
          : undefined,
      }
    }
    return {
      name: entry.name,
      path: entry.path,
      kind: "file" as const,
      change: change?.change,
      additions: change?.additions,
      deletions: change?.deletions,
    }
  })
}

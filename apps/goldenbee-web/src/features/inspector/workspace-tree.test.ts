import { describe, expect, it } from "vitest"
import type { StreamPage, WorkspaceEntry } from "@goldenbee/sdk"

import {
  buildFileTreeNodes,
  loadChangedFiles,
  loadWorkspaceDirectory,
} from "@/features/inspector/workspace-tree"

function page(entries: WorkspaceEntry[]) {
  return {
    path: "",
    entries,
    truncated: false,
  }
}

describe("workspace tree helpers", () => {
  it("builds nested nodes and merges change metadata", () => {
    const entries: WorkspaceEntry[] = [
      {
        name: "index.html",
        path: "index.html",
        kind: "file",
        size: 10,
        modified_at: 1,
        hidden: false,
      },
    ]
    const nodes = buildFileTreeNodes(
      entries,
      {},
      new Set(),
      new Map([
        [
          "index.html",
          {
            path: "index.html",
            change: "modify",
            additions: 2,
            deletions: 1,
          },
        ],
      ]),
    )
    expect(nodes).toMatchObject([
      {
        name: "index.html",
        kind: "file",
        change: "modify",
        additions: 2,
        deletions: 1,
      },
    ])
  })

  it("loads all directory pages and change events", async () => {
    const client = {
      async workspaceTree() {
        return page([
          {
            name: "notes",
            path: "notes",
            kind: "directory" as const,
            size: 0,
            modified_at: 1,
            hidden: false,
          },
        ])
      },
    }
    const directory = await loadWorkspaceDirectory(client, "general-a", "")
    expect(directory.truncated).toBe(false)
    expect(directory.entries[0]?.path).toBe("notes")

    const events = {
      async events(): Promise<StreamPage> {
        return {
          stream_id: "task:exec-1",
          events: [
            {
              event_id: "evt-1",
              source: "worker",
              incarnation: "inc-1",
              stream_id: "task:exec-1",
              seq: 1,
              schema_version: 1,
              kind: "diff.recorded",
              durable: true,
              at: 1,
              payload: {
                files: [
                  {
                    path: "index.html",
                    change: "modify",
                    additions: 2,
                    deletions: 1,
                  },
                ],
              },
            },
          ],
          next_seq: 1,
          oldest_seq: 0,
          cursor_expired: false,
        }
      },
    }
    const changes = await loadChangedFiles(events, "exec-1")
    expect(changes.get("index.html")).toMatchObject({
      change: "modify",
      additions: 2,
      deletions: 1,
    })
  })
})

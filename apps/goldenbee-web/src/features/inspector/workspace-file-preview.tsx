import { useQuery } from "@tanstack/react-query"
import { useEffect, useState } from "react"
import type { WorkspaceFile } from "@goldenbee/sdk"

import { Markdown } from "@/components/ai-elements/markdown"
import { Badge } from "@/components/ui/badge"
import type { WorkspaceLiveStatus } from "@/features/inspector/use-workspace-live"

export interface WorkspacePreviewGateway {
  workspaceFile(worker: string, path: string): Promise<WorkspaceFile>
}

export interface WorkspaceFilePreviewProps {
  client: WorkspacePreviewGateway | null
  worker: string | null
  path: string
  liveVersion: number
  liveStatus: WorkspaceLiveStatus
  onClose?(): void
}

function formatSize(size: number): string {
  if (size < 1024) {
    return `${size} B`
  }
  if (size < 1024 * 1024) {
    return `${(size / 1024).toFixed(1)} KiB`
  }
  return `${(size / (1024 * 1024)).toFixed(1)} MiB`
}

function formatTime(value: number): string {
  if (!value) {
    return "Not reported"
  }
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? "Not reported" : date.toISOString()
}

type PreviewKind = "markdown" | "html" | null

function previewKind(path: string): PreviewKind {
  const lower = path.toLowerCase()
  if (lower.endsWith(".md") || lower.endsWith(".markdown") || lower.endsWith(".mdx")) {
    return "markdown"
  }
  if (lower.endsWith(".html") || lower.endsWith(".htm")) {
    return "html"
  }
  return null
}

export function WorkspaceFilePreview({
  client,
  worker,
  path,
  liveVersion,
  liveStatus,
}: WorkspaceFilePreviewProps) {
  const file = useQuery({
    queryKey: ["workspace-preview", worker, path, liveVersion],
    queryFn: () => client!.workspaceFile(worker!, path),
    enabled: worker !== null && client !== null && path.length > 0,
    retry: false,
  })
  const kind = previewKind(path)
  const [view, setView] = useState<"preview" | "source">("preview")

  useEffect(() => {
    // oxlint-disable-next-line react/set-state-in-effect
    setView("preview")
  }, [path])

  return (
    <section
      aria-label={`File preview ${path}`}
      className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-[9px] border border-line bg-surface"
    >
      <header className="flex items-center gap-2 border-b border-line px-2.5 py-1.5">
        <p className="min-w-0 flex-1 truncate font-mono text-micro text-foreground">
          {path}
        </p>
        {kind ? (
          <div
            role="group"
            aria-label="File view"
            className="flex items-center rounded-[5px] border border-line bg-surface-2 p-0.5"
          >
            <button
              type="button"
              aria-pressed={view === "preview"}
              onClick={() => setView("preview")}
              className={
                view === "preview"
                  ? "rounded-[3px] bg-surface-3 px-1.5 py-0.5 text-micro font-medium text-foreground"
                  : "rounded-[3px] px-1.5 py-0.5 text-micro text-muted-foreground hover:text-foreground"
              }
            >
              Preview
            </button>
            <button
              type="button"
              aria-pressed={view === "source"}
              onClick={() => setView("source")}
              className={
                view === "source"
                  ? "rounded-[3px] bg-surface-3 px-1.5 py-0.5 text-micro font-medium text-foreground"
                  : "rounded-[3px] px-1.5 py-0.5 text-micro text-muted-foreground hover:text-foreground"
              }
            >
              Source
            </button>
          </div>
        ) : null}
        <Badge variant="outline" className="h-4 rounded px-1 text-micro">
          {liveStatus === "live"
            ? "Live"
            : liveStatus === "reconnecting"
              ? "Reconnecting"
              : "Read-only"}
        </Badge>
      </header>

      <div className="min-h-0 flex-1 overflow-auto p-2.5">
        {!worker ? (
          <p className="text-meta text-muted-foreground">
            The focused session has no Worker yet.
          </p>
        ) : client === null ? (
          <p className="text-meta text-muted-foreground">
            Workspace browsing is unavailable for this connection.
          </p>
        ) : file.isLoading ? (
          <p role="status" className="text-meta text-muted-foreground">
            Loading file…
          </p>
        ) : file.isError ? (
          <div role="alert" className="grid gap-1">
            <p className="text-meta text-destructive">
              The file could not be read.
            </p>
            <p className="text-micro text-muted-foreground">
              {file.error instanceof Error
                ? file.error.message
                : "The Worker rejected the preview request."}
            </p>
          </div>
        ) : file.data ? (
          <div className="grid gap-1.5">
            <p className="flex flex-wrap gap-x-3 gap-y-0.5 text-micro text-dim">
              <span>{formatSize(file.data.size)}</span>
              <span>{formatTime(file.data.modified_at)}</span>
              {file.data.digest ? (
                <span className="max-w-full truncate font-mono">
                  {file.data.digest}
                </span>
              ) : null}
              {file.data.truncated ? (
                <span className="text-gold">truncated</span>
              ) : null}
              {file.data.binary ? (
                <span className="text-gold">binary</span>
              ) : null}
            </p>
            {file.data.binary ? (
              <p className="text-micro text-muted-foreground">
                Binary content is not previewed.
              </p>
            ) : kind === "markdown" && view === "preview" ? (
              <div className="min-h-0 overflow-auto rounded-[6px] border border-line bg-surface-2 p-3">
                <Markdown>{file.data.text ?? ""}</Markdown>
              </div>
            ) : kind === "html" && view === "preview" ? (
              <iframe
                title={`Preview ${path}`}
                sandbox=""
                srcDoc={file.data.text ?? ""}
                className="h-[60vh] w-full rounded-[6px] border border-line bg-white"
              />
            ) : (
              <pre className="min-h-0 overflow-auto whitespace-pre-wrap break-words rounded-[6px] border border-line bg-surface-2 p-2.5 text-micro text-foreground">
                {file.data.text ?? ""}
              </pre>
            )}
          </div>
        ) : null}
      </div>
    </section>
  )
}

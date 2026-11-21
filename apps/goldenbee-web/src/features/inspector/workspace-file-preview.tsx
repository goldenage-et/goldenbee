import { useQuery } from "@tanstack/react-query"
import type { WorkspaceFile } from "@goldenbee/sdk"

import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
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
  onClose(): void
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

export function WorkspaceFilePreview({
  client,
  worker,
  path,
  liveVersion,
  liveStatus,
  onClose,
}: WorkspaceFilePreviewProps) {
  const file = useQuery({
    queryKey: ["workspace-preview", worker, path, liveVersion],
    queryFn: () => client!.workspaceFile(worker!, path),
    enabled: worker !== null && client !== null && path.length > 0,
    retry: false,
  })

  const name = path.split("/").at(-1) ?? path

  return (
    <section
      aria-label={`File preview ${path}`}
      className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-[9px] border border-line bg-surface"
    >
      <header className="flex flex-wrap items-center gap-2 border-b border-line px-3 py-2">
        <div className="min-w-0 flex-1">
          <p className="text-micro font-bold tracking-[0.14em] text-dim uppercase">
            File preview
          </p>
          <h2 className="truncate text-meta font-semibold text-foreground">
            {name}
          </h2>
          <p className="break-all font-mono text-micro text-dim">{path}</p>
        </div>
        <Badge variant="outline" className="h-5 rounded px-1.5 text-micro">
          {liveStatus === "live"
            ? "Live"
            : liveStatus === "reconnecting"
              ? "Reconnecting"
              : "Read-only"}
        </Badge>
        <Button type="button" size="sm" variant="ghost" onClick={onClose}>
          Close
        </Button>
      </header>

      <div className="min-h-0 flex-1 overflow-auto p-3">
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
          <div className="grid gap-2">
            <dl className="flex flex-wrap gap-x-4 gap-y-1 text-micro text-muted-foreground">
              <div className="flex gap-1">
                <dt className="text-dim">Size</dt>
                <dd>{formatSize(file.data.size)}</dd>
              </div>
              <div className="flex gap-1">
                <dt className="text-dim">Modified</dt>
                <dd>{formatTime(file.data.modified_at)}</dd>
              </div>
              {file.data.digest ? (
                <div className="flex min-w-0 gap-1">
                  <dt className="text-dim">Digest</dt>
                  <dd className="truncate font-mono">{file.data.digest}</dd>
                </div>
              ) : null}
              {file.data.truncated ? (
                <div className="text-gold">Preview truncated</div>
              ) : null}
              {file.data.binary ? <div className="text-gold">Binary</div> : null}
            </dl>
            {file.data.binary ? (
              <p className="text-meta text-muted-foreground">
                Binary content is not previewed.
              </p>
            ) : (
              <pre className="min-h-0 overflow-auto whitespace-pre-wrap break-words rounded-[7px] border border-line bg-surface-2 p-3 text-micro text-foreground">
                {file.data.text ?? ""}
              </pre>
            )}
          </div>
        ) : null}
      </div>
    </section>
  )
}

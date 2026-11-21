import { useQuery } from "@tanstack/react-query"
import { useEffect, useMemo, useState } from "react"
import type { WorkspaceContent, WorkspaceFile } from "@goldenbee/sdk"

import { Markdown } from "@/components/ai-elements/markdown"
import { Badge } from "@/components/ui/badge"
import type { WorkspaceLiveStatus } from "@/features/inspector/use-workspace-live"

export interface WorkspacePreviewGateway {
  workspaceFile(worker: string, path: string): Promise<WorkspaceFile>
  workspaceContent(worker: string, path: string): Promise<WorkspaceContent>
}

export interface WorkspaceFilePreviewProps {
  client: WorkspacePreviewGateway | null
  worker: string | null
  path: string
  liveVersion: number
  liveStatus: WorkspaceLiveStatus
  onClose?(): void
}

type PreviewKind =
  | "markdown"
  | "html"
  | "json"
  | "csv"
  | "image"
  | "pdf"
  | "audio"
  | "video"
  | "binary"
  | null

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

function previewKind(path: string, binary = false): PreviewKind {
  const lower = path.toLowerCase()
  if (lower.endsWith(".md") || lower.endsWith(".markdown") || lower.endsWith(".mdx")) {
    return "markdown"
  }
  if (lower.endsWith(".html") || lower.endsWith(".htm")) {
    return "html"
  }
  if (lower.endsWith(".json") || lower.endsWith(".jsonc")) {
    return "json"
  }
  if (lower.endsWith(".csv") || lower.endsWith(".tsv")) {
    return "csv"
  }
  if (/\.(png|jpe?g|gif|webp|avif|bmp|ico|svg)$/i.test(path)) {
    return "image"
  }
  if (lower.endsWith(".pdf")) {
    return "pdf"
  }
  if (/\.(mp3|wav|ogg|m4a|flac)$/i.test(path)) {
    return "audio"
  }
  if (/\.(mp4|webm|mov|m4v)$/i.test(path)) {
    return "video"
  }
  return binary ? "binary" : null
}

function mediaKind(kind: PreviewKind): boolean {
  return kind === "image" || kind === "pdf" || kind === "audio" || kind === "video"
}

function hexDump(bytes: Uint8Array, limit: number): string {
  const lines: string[] = []
  const max = Math.min(bytes.length, limit)
  for (let offset = 0; offset < max; offset += 16) {
    const row = bytes.slice(offset, offset + 16)
    const hex = Array.from(row, (byte) => byte.toString(16).padStart(2, "0"))
      .join(" ")
      .padEnd(47, " ")
    const ascii = Array.from(row, (byte) =>
      byte >= 32 && byte <= 126 ? String.fromCharCode(byte) : ".",
    ).join("")
    lines.push(`${offset.toString(16).padStart(8, "0")}  ${hex}  ${ascii}`)
  }
  if (bytes.length > limit) {
    lines.push(`… ${bytes.length - limit} more bytes`)
  }
  return lines.join("\n")
}

function csvRows(text: string, maxRows = 200, maxColumns = 16): string[][] {
  return text
    .split(/\r?\n/)
    .filter((line) => line.length > 0)
    .slice(0, maxRows)
    .map((line) => line.split(",").slice(0, maxColumns))
}

function prettyJson(text: string): string | null {
  try {
    return JSON.stringify(JSON.parse(text), null, 2)
  } catch {
    return null
  }
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
  const kind = previewKind(path, file.data?.binary)
  const needsContent = mediaKind(kind) || kind === "binary"
  const content = useQuery({
    queryKey: ["workspace-content", worker, path, liveVersion],
    queryFn: () => client!.workspaceContent(worker!, path),
    enabled:
      worker !== null &&
      client !== null &&
      path.length > 0 &&
      (needsContent || (kind === "markdown" && file.data?.truncated === true)),
    retry: false,
  })
  const [view, setView] = useState<"preview" | "source">("preview")
  const [objectUrl, setObjectUrl] = useState<string | null>(null)
  const [hex, setHex] = useState("")

  useEffect(() => {
    // oxlint-disable-next-line react/set-state-in-effect
    setView("preview")
  }, [path])

  useEffect(() => {
    if (!content.data || !mediaKind(kind)) {
      // oxlint-disable-next-line react/set-state-in-effect
      setObjectUrl(null)
      return
    }
    const url = URL.createObjectURL(content.data.blob)
    // oxlint-disable-next-line react/set-state-in-effect
    setObjectUrl(url)
    return () => {
      URL.revokeObjectURL(url)
    }
  }, [content.data, kind])

  useEffect(() => {
    if (!content.data || !needsContent) {
      // oxlint-disable-next-line react/set-state-in-effect
      setHex("")
      return
    }
    let cancelled = false
    void content.data.blob
      .arrayBuffer()
      .then((buffer) => {
        if (!cancelled) {
          setHex(hexDump(new Uint8Array(buffer), 4096))
        }
      })
      .catch(() => {
        if (!cancelled) {
          setHex("")
        }
      })
    return () => {
      cancelled = true
    }
  }, [content.data, needsContent])

  const json = useMemo(
    () => (kind === "json" && file.data?.text ? prettyJson(file.data.text) : null),
    [kind, file.data?.text],
  )
  const rows = useMemo(
    () => (kind === "csv" && file.data?.text ? csvRows(file.data.text) : []),
    [kind, file.data?.text],
  )

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
          <p className="text-micro text-muted-foreground">
            The focused session has no Worker yet.
          </p>
        ) : client === null ? (
          <p className="text-micro text-muted-foreground">
            Workspace browsing is unavailable for this connection.
          </p>
        ) : file.isLoading ? (
          <p role="status" className="text-micro text-muted-foreground">
            Loading file…
          </p>
        ) : file.isError ? (
          <div role="alert" className="grid gap-0.5">
            <p className="text-micro text-destructive">The file could not be read.</p>
            <p className="text-micro text-dim">
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
              {file.data.binary ? <span className="text-gold">binary</span> : null}
            </p>

            {content.isError ? (
              <div role="alert" className="grid gap-0.5">
                <p className="text-micro text-destructive">
                  Preview content could not be read.
                </p>
                <p className="text-micro text-dim">
                  {content.error instanceof Error
                    ? content.error.message
                    : "The Worker rejected the content request."}
                </p>
              </div>
            ) : needsContent && content.isLoading && view === "preview" ? (
              <p role="status" className="text-micro text-muted-foreground">
                Loading preview…
              </p>
            ) : view === "source" ? (
              kind === "binary" || mediaKind(kind) ? (
                <pre className="min-h-0 overflow-auto whitespace-pre-wrap break-words rounded-[6px] border border-line bg-surface-2 p-2.5 text-micro text-foreground">
                  {hex || "No bounded bytes were available."}
                </pre>
              ) : (
                <pre className="min-h-0 overflow-auto whitespace-pre-wrap break-words rounded-[6px] border border-line bg-surface-2 p-2.5 text-micro text-foreground">
                  {file.data.text ?? ""}
                </pre>
              )
            ) : kind === "markdown" ? (
              <div className="min-h-0 overflow-auto rounded-[6px] border border-line bg-surface-2 p-3">
                <Markdown>{file.data.text ?? ""}</Markdown>
              </div>
            ) : kind === "html" ? (
              <iframe
                title={`Preview ${path}`}
                sandbox=""
                srcDoc={file.data.text ?? ""}
                className="h-[60vh] w-full rounded-[6px] border border-line bg-white"
              />
            ) : kind === "json" ? (
              <pre className="min-h-0 overflow-auto whitespace-pre-wrap break-words rounded-[6px] border border-line bg-surface-2 p-2.5 text-micro text-foreground">
                {json ?? file.data.text ?? ""}
              </pre>
            ) : kind === "csv" ? (
              <div className="max-h-[60vh] overflow-auto rounded-[6px] border border-line">
                <table className="w-full border-collapse text-micro">
                  <tbody>
                    {rows.map((row, rowIndex) => (
                      <tr key={rowIndex} className="border-b border-line/60 last:border-0">
                        {row.map((cell, cellIndex) => (
                          <td
                            key={cellIndex}
                            className={
                              rowIndex === 0
                                ? "whitespace-nowrap bg-surface-2 px-2 py-1 font-medium text-foreground"
                                : "whitespace-nowrap px-2 py-1 text-muted-foreground"
                            }
                          >
                            {cell}
                          </td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ) : kind === "image" ? (
              objectUrl ? (
                <div className="flex min-h-[16rem] items-center justify-center rounded-[6px] border border-line bg-[repeating-conic-gradient(var(--surface-2)_0%_25%,var(--surface)_0%_50%)] bg-[length:16px_16px] p-2">
                  <img
                    src={objectUrl}
                    alt={path}
                    className="max-h-[65vh] max-w-full object-contain"
                  />
                </div>
              ) : (
                <p role="status" className="text-micro text-muted-foreground">
                  Loading image…
                </p>
              )
            ) : kind === "pdf" ? (
              objectUrl ? (
                <iframe
                  title={`Preview ${path}`}
                  sandbox=""
                  src={objectUrl}
                  className="h-[65vh] w-full rounded-[6px] border border-line bg-white"
                />
              ) : (
                <p role="status" className="text-micro text-muted-foreground">
                  Loading PDF…
                </p>
              )
            ) : kind === "audio" ? (
              objectUrl ? (
                <audio src={objectUrl} controls className="w-full" />
              ) : (
                <p role="status" className="text-micro text-muted-foreground">
                  Loading audio…
                </p>
              )
            ) : kind === "video" ? (
              objectUrl ? (
                <video src={objectUrl} controls className="max-h-[65vh] w-full rounded-[6px]" />
              ) : (
                <p role="status" className="text-micro text-muted-foreground">
                  Loading video…
                </p>
              )
            ) : kind === "binary" ? (
              <pre className="min-h-0 overflow-auto whitespace-pre-wrap break-words rounded-[6px] border border-line bg-surface-2 p-2.5 text-micro text-foreground">
                {hex || "No bounded bytes were available."}
              </pre>
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

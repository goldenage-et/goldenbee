import { useQuery } from "@tanstack/react-query"
import { useEffect, useState } from "react"
import { isGatewayError, type Artifact } from "@goldenbee/sdk"

import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import type { ChatInspectionStore } from "@/features/chat/chat-inspection-store"
import { useChatInspection } from "@/features/chat/chat-inspection-store"
import type {
  ChatHistoryStore,
  RestoredChat,
} from "@/features/chat/local-chat-history"
import { UsageFacts } from "@/features/usage/usage-facts"

export interface ArtifactGateway {
  artifacts(taskId: string): Promise<Artifact[]>
}

export type InspectorTab = "artifacts" | "details"

const tabs: Array<{ id: InspectorTab; label: string }> = [
  { id: "artifacts", label: "Artifacts" },
  { id: "details", label: "Details" },
]

export interface SessionInspectorProps {
  history: ChatHistoryStore | null
  inspections: ChatInspectionStore
  artifactsClient: ArtifactGateway | null
  chatId: string | null
  title?: string
  open: boolean
  onClose(): void
}

function Fact({
  label,
  value,
}: {
  label: string
  value: React.ReactNode
}) {
  return (
    <div className="grid grid-cols-[7.5rem_minmax(0,1fr)] gap-2 border-b border-line/70 py-1.5 last:border-b-0">
      <dt className="text-micro text-dim">{label}</dt>
      <dd className="min-w-0 break-words text-micro text-foreground">
        {value}
      </dd>
    </div>
  )
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

function artifactError(error: unknown): { message: string; denial: boolean } {
  if (isGatewayError(error)) {
    return {
      message: error.message,
      denial: error.kind === "forbidden" || error.kind === "unauthorized",
    }
  }
  return {
    message:
      error instanceof Error && error.message
        ? error.message
        : "Artifacts could not be read.",
    denial: false,
  }
}

/**
 * Focused-session context inspector.
 *
 * It reads live durable session facts and the local chat record, and lists the
 * current task's Gateway artifacts. Durable task history is intentionally kept
 * out of this surface; the session panel opens `TaskBrowserDialog` on demand.
 */
export function SessionInspector({
  history,
  inspections,
  artifactsClient,
  chatId,
  title,
  open,
  onClose,
}: SessionInspectorProps) {
  const [tab, setTab] = useState<InspectorTab>("details")
  const [record, setRecord] = useState<RestoredChat | null>(null)
  const [recordError, setRecordError] = useState<string | null>(null)
  const inspection = useChatInspection(
    (state) => (chatId ? state.inspections[chatId] : undefined),
    inspections,
  )
  const liveTaskId = inspection?.taskId ?? null

  useEffect(() => {
    if (!history || !chatId) {
      // oxlint-disable-next-line react/set-state-in-effect
      setRecord(null)
      // oxlint-disable-next-line react/set-state-in-effect
      setRecordError(null)
      return
    }
    let cancelled = false
    // oxlint-disable-next-line react/set-state-in-effect
    setRecordError(null)
    void history
      .loadChat(chatId)
      .then((loaded) => {
        if (!cancelled) {
          setRecord(loaded)
        }
      })
      .catch((cause: unknown) => {
        if (cancelled) {
          return
        }
        setRecord(null)
        setRecordError(
          cause instanceof Error && cause.message
            ? cause.message
            : "Local chat records could not be read.",
        )
      })
    return () => {
      cancelled = true
    }
  }, [chatId, history])

  const taskId = liveTaskId ?? record?.chat.activeTaskId ?? null
  const selection =
    inspection?.selection ?? record?.selection ?? record?.chat.selection ?? null
  const gap = inspection?.gap ?? record?.chat.gap ?? null
  const recovery = inspection?.recovery ?? record?.chat.recovery ?? null
  const usage = inspection?.usage ?? null

  const artifacts = useQuery({
    queryKey: ["focused-session-artifacts", taskId],
    queryFn: () => artifactsClient!.artifacts(taskId!),
    enabled:
      open && tab === "artifacts" && taskId !== null && artifactsClient !== null,
    retry: false,
  })
  const artifactFailure = artifacts.isError
    ? artifactError(artifacts.error)
    : null

  return (
    <aside
      aria-label="Focused session inspector"
      className="flex min-h-0 w-[264px] shrink-0 flex-col border-l border-line bg-surface"
    >
      <div className="flex items-center justify-between border-b border-line px-3 py-2.5">
        <div className="min-w-0">
          <p className="text-micro font-bold tracking-[0.14em] text-dim uppercase">
            Focused session
          </p>
          <p className="truncate text-meta font-medium text-foreground">
            {chatId ? (title ?? "Local chat") : "No focused panel"}
          </p>
        </div>
        <Button
          type="button"
          size="icon-xs"
          variant="ghost"
          aria-label="Close inspector"
          onClick={onClose}
        >
          ×
        </Button>
      </div>

      <div
        role="tablist"
        aria-label="Inspector sections"
        className="flex gap-1 border-b border-line px-2 py-2"
      >
        {tabs.map((entry) => (
          <button
            key={entry.id}
            type="button"
            role="tab"
            aria-selected={tab === entry.id}
            aria-controls={`inspector-${entry.id}`}
            onClick={() => setTab(entry.id)}
            className={
              tab === entry.id
                ? "flex-1 rounded-[6px] bg-surface-3 px-1.5 py-1 text-micro font-medium text-foreground"
                : "flex-1 rounded-[6px] px-1.5 py-1 text-micro text-muted-foreground hover:bg-surface-2 hover:text-foreground"
            }
          >
            {entry.label}
          </button>
        ))}
      </div>

      {chatId && (gap || recovery?.kind === "detached" || recovery?.kind === "failed") ? (
        <div className="grid gap-1 border-b border-line px-3 py-2">
          {gap ? (
            <p className="rounded-[7px] border border-gold/30 bg-gold/10 px-2 py-1.5 text-micro text-foreground">
              Stream gap at sequence {gap.fromSeq} ({gap.reason}). Retained
              history is incomplete.
            </p>
          ) : null}
          {recovery?.kind === "detached" || recovery?.kind === "failed" ? (
            <p className="rounded-[7px] border border-destructive/30 bg-destructive/10 px-2 py-1.5 text-micro text-foreground">
              {recovery.detail ??
                "The local chat is detached from its Gateway task."}
            </p>
          ) : null}
        </div>
      ) : null}

      <div className="min-h-0 flex-1 overflow-y-auto p-3">
        {!chatId ? (
          <p className="text-meta text-muted-foreground">
            Select a session panel to inspect its runtime facts.
          </p>
        ) : null}

        {chatId && recordError ? (
          <p role="alert" className="text-meta text-destructive">
            {recordError}
          </p>
        ) : null}

        {chatId && tab === "artifacts" ? (
          <section
            id="inspector-artifacts"
            role="tabpanel"
            aria-label="Artifacts"
            className="grid gap-2"
          >
            {!taskId ? (
              <p className="text-meta text-muted-foreground">
                No task is associated with this chat, so there are no artifacts
                to list.
              </p>
            ) : artifactsClient === null ? (
              <p className="text-meta text-muted-foreground">
                Artifacts are unavailable for this connection.
              </p>
            ) : artifacts.isLoading ? (
              <p role="status" className="text-meta text-muted-foreground">
                Loading artifacts…
              </p>
            ) : artifactFailure ? (
              <div role="alert" className="grid gap-1">
                <p className="text-meta text-destructive">
                  {artifactFailure.denial
                    ? "The caller token cannot read artifacts for this task."
                    : "Artifacts could not be read."}
                </p>
                <p className="text-micro text-muted-foreground">
                  {artifactFailure.message}
                </p>
              </div>
            ) : (artifacts.data ?? []).length === 0 ? (
              <p className="text-meta text-muted-foreground">
                No artifacts reported for this task.
              </p>
            ) : (
              <ul className="grid gap-2">
                {(artifacts.data ?? []).map((artifact) => (
                  <li
                    key={artifact.id}
                    className="rounded-[7px] border border-line bg-surface-2 px-2 py-1.5"
                  >
                    <p className="truncate text-meta font-medium text-foreground">
                      {artifact.name}
                    </p>
                    <p className="text-micro text-muted-foreground">
                      {artifact.content_type} · {formatSize(artifact.size)}
                    </p>
                    <p className="break-all font-mono text-micro text-dim">
                      {artifact.digest}
                    </p>
                    <Badge
                      variant="outline"
                      className="mt-1 h-4 rounded px-1 text-micro"
                    >
                      {artifact.finalized ? "Finalized" : "Open"}
                    </Badge>
                  </li>
                ))}
              </ul>
            )}
          </section>
        ) : null}

        {chatId && tab === "details" ? (
          <section
            id="inspector-details"
            role="tabpanel"
            aria-label="Details"
            className="grid gap-2"
          >
            <dl>
              <Fact label="Worker" value={selection?.worker || "Not selected"} />
              <Fact label="Agent" value={selection?.agent || "Not selected"} />
              <Fact
                label="Profile"
                value={
                  selection?.profile
                    ? selection.profile
                    : "Worker default / none"
                }
              />
              <Fact label="Chat id" value={chatId} />
              <Fact
                label="Session id"
                value={inspection?.sessionId ?? record?.chat.sessionId ?? "—"}
              />
              <Fact
                label="Panel"
                value="Visible; stream attaches while mounted"
              />
              <Fact
                label="Durability"
                value="Gateway task continues when this panel is hidden; restore resumes from the durable cursor."
              />
            </dl>
            <p className="text-micro text-muted-foreground">
              Streamed text and reasoning are not durable. A reconnect can only
              replay events after the saved durable cursor.
            </p>
            <div className="mt-1 border-t border-line pt-3">
              <h3 className="mb-2 text-micro font-semibold tracking-[0.12em] text-dim uppercase">
                Usage
              </h3>
              <UsageFacts
                usage={usage}
                emptyMessage="No usage counters were reported for this task."
              />
            </div>
          </section>
        ) : null}
      </div>
    </aside>
  )
}

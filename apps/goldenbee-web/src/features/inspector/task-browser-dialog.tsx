import { useEffect, useMemo, useState } from "react"

import { Badge } from "@/components/ui/badge"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { ScrollArea } from "@/components/ui/scroll-area"
import type {
  ChatHistoryStore,
  RestoredChat,
} from "@/features/chat/local-chat-history"
import {
  TaskDetailPage,
  type TaskDetailGateway,
} from "@/features/tasks/task-detail-page"

export interface TaskBrowserDialogProps {
  history: ChatHistoryStore | null
  client: TaskDetailGateway | null
  chatId: string | null
  open: boolean
  onClose(): void
}

function formatUpdatedAt(value: number): string {
  if (!value) {
    return "Unknown time"
  }
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(value)
}

/**
 * Explicit task projection browser.
 *
 * The focused-session inspector is a session surface. Durable task identities,
 * terminal state, and cursors live here so they are available on demand without
 * becoming permanent chrome in the chat panel.
 */
export function TaskBrowserDialog({
  history,
  client,
  chatId,
  open,
  onClose,
}: TaskBrowserDialogProps) {
  const [record, setRecord] = useState<RestoredChat | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null)

  useEffect(() => {
    if (!open || !history || !chatId) {
      // oxlint-disable-next-line react/set-state-in-effect
      setRecord(null)
      // oxlint-disable-next-line react/set-state-in-effect
      setError(null)
      return
    }
    let cancelled = false
    // oxlint-disable-next-line react/set-state-in-effect
    setError(null)
    void history
      .loadChat(chatId)
      .then((loaded) => {
        if (!cancelled) {
          setRecord(loaded)
          setSelectedTaskId(null)
        }
      })
      .catch((cause: unknown) => {
        if (cancelled) {
          return
        }
        setRecord(null)
        setError(
          cause instanceof Error && cause.message
            ? cause.message
            : "Task records could not be read.",
        )
      })
    return () => {
      cancelled = true
    }
  }, [chatId, history, open])

  const tasks = useMemo(
    () =>
      [...(record?.tasks ?? [])].sort(
        (left, right) => right.updatedAt - left.updatedAt,
      ),
    [record],
  )
  const selectedIndex = Math.max(
    0,
    tasks.findIndex((task) => task.taskId === selectedTaskId),
  )
  const selectedTask = tasks[selectedIndex] ?? tasks[0]!

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) {
          onClose()
        }
      }}
    >
      <DialogContent className="h-[min(88vh,760px)] max-w-6xl gap-0 overflow-hidden rounded-[12px] border border-line bg-surface p-0 sm:max-w-6xl">
        <DialogHeader className="border-b border-line px-4 py-3 pr-12">
          <DialogTitle className="text-panel">Task records</DialogTitle>
          <DialogDescription className="text-micro">
            Tasks this browser observed for the focused session.
          </DialogDescription>
        </DialogHeader>

        {!chatId ? (
          <p className="p-4 text-meta text-muted-foreground">
            Select a session panel before opening its task records.
          </p>
        ) : error ? (
          <p role="alert" className="p-4 text-meta text-destructive">
            {error}
          </p>
        ) : tasks.length === 0 ? (
          <p className="p-4 text-meta text-muted-foreground">
            No task records yet. A record appears after this session runs a
            task.
          </p>
        ) : (
          <div className="grid min-h-0 flex-1 grid-cols-[220px_minmax(0,1fr)] max-[780px]:grid-cols-1 max-[780px]:grid-rows-[180px_minmax(0,1fr)]">
            <div className="flex min-h-0 flex-col border-r border-line max-[780px]:border-r-0 max-[780px]:border-b">
              <p className="border-b border-line/70 px-3 py-2 text-micro font-bold tracking-[0.14em] text-dim uppercase">
                Tasks · {tasks.length}
              </p>
              <ScrollArea className="min-h-0 flex-1">
                <ul className="grid gap-1 p-2">
                  {tasks.map((task, index) => {
                    const selected = task.taskId === selectedTask?.taskId
                    const current =
                      task.taskId === record?.chat.activeTaskId
                    return (
                      <li key={task.taskId}>
                        <button
                          type="button"
                          aria-pressed={selected}
                          aria-label={`Task ${index + 1}, ${task.state}`}
                          onClick={() => setSelectedTaskId(task.taskId)}
                          className={
                            selected
                              ? "grid w-full gap-1 rounded-[7px] border border-gold/40 bg-gold/10 px-2 py-1.5 text-left"
                              : "grid w-full gap-1 rounded-[7px] border border-transparent px-2 py-1.5 text-left hover:bg-surface-2"
                          }
                        >
                          <span className="flex min-w-0 items-center gap-1.5">
                            <span className="truncate text-meta font-medium">
                              Task {index + 1}
                            </span>
                            {current ? (
                              <span className="shrink-0 text-[9px] font-semibold tracking-wide text-gold uppercase">
                                current
                              </span>
                            ) : null}
                          </span>
                          <span className="flex items-center gap-1.5">
                            <Badge
                              variant="outline"
                              className="h-4 rounded px-1 text-micro"
                            >
                              {task.state}
                            </Badge>
                            <span className="truncate text-micro text-muted-foreground">
                              {formatUpdatedAt(task.updatedAt)}
                            </span>
                          </span>
                        </button>
                      </li>
                    )
                  })}
                </ul>
              </ScrollArea>
            </div>

            <div className="flex min-h-0 flex-col">
              <div className="flex items-center gap-2 border-b border-line px-4 py-2">
                <h3 className="text-meta font-semibold">
                  Task {selectedIndex + 1}
                </h3>
                <Badge
                  variant="outline"
                  className="h-4 rounded px-1 text-micro"
                >
                  {selectedTask.state}
                </Badge>
                <span className="text-micro text-muted-foreground">
                  {selectedTask.terminal ? "terminal" : "open"}
                </span>
              </div>
              <ScrollArea className="min-h-0 flex-1">
                <div className="p-4">
                  <TaskDetailPage
                    client={client}
                    taskId={selectedTask.taskId}
                    embedded
                  />
                </div>
              </ScrollArea>
            </div>
          </div>
        )}
      </DialogContent>
    </Dialog>
  )
}

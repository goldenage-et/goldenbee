import {
  ArrowDownToLineIcon,
  FocusIcon,
  MessageSquareIcon,
  PlusIcon,
  ScanIcon,
} from "lucide-react"
import { useEffect, useLayoutEffect, useMemo, useState } from "react"
import { cn } from "cn"

import {
  MessageScroller,
  MessageScrollerButton,
  MessageScrollerContent,
  MessageScrollerItem,
  MessageScrollerProvider,
  MessageScrollerViewport,
} from "@/components/ui/message-scroller"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import type { ApprovalGateway } from "@/features/approvals/approval-card"
import { ChatComposer } from "@/features/chat/chat-composer"
import { ChatControls } from "@/features/chat/chat-controls"
import { ChatMessageView } from "@/features/chat/chat-message"
import {
  latestRevisionNumber,
  resolveSelection,
} from "@/features/chat/chat-selection"
import {
  chatSelectionStore,
  type ChatSelectionStore,
} from "@/features/chat/chat-selection-store"
import {
  profileErrorView,
  useActivateProfile,
  useWorkerProfiles,
  type SelectionGateway,
} from "@/features/chat/chat-profiles"
import type { ChatSelection } from "@/features/chat/chat-types"
import type { ChatInspectionStore } from "@/features/chat/chat-inspection-store"
import { useChat } from "@/features/chat/use-chat"
import type { ChatHistoryStore } from "@/features/chat/local-chat-history"
import {
  useConnection,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import { useGatewayClient } from "@/features/connection/use-gateway-client"
import type { ChatGateway } from "@/features/chat/chat-runner"

const busyStatuses = new Set([
  "submitting",
  "streaming",
  "stopping",
  "recovering",
])

const emptySelection: ChatSelection = { worker: "", agent: "", profile: "" }

export interface ChatViewProps {
  store: ConnectionStore
  /** Browser-local history; omitted in tests that exercise transport only. */
  history?: ChatHistoryStore | null
  /** Controlled local chat id; the workspace shell sets it per tab. */
  chatId?: string
  /** Human-readable local chat title, when the catalog has one. */
  title?: string
  /** Test seam; production builds the client from the connection store. */
  client?: ChatGateway | null
  /** Test seam for approval decisions; defaults to the connection client. */
  approvalClient?: ApprovalGateway | null
  /** Test seam for the Worker-owned Runtime Profile catalog. */
  profileClient?: SelectionGateway | null
  /** Test seam for the non-secret local selection preference. */
  selectionStore?: ChatSelectionStore
  /** Workspace action: create a new local chat in the active group. */
  onNewChat?: () => void
  /** Workspace action: hide this chat without cancelling its Gateway task. */
  onBackground?: () => void
  /** Workspace action: make this panel the inspector focus. */
  onFocus?: () => void
  /** Workspace action: hide the other visible panels in this group. */
  onSolo?: () => void
  /** True when this panel drives the inspector. */
  focused?: boolean
  /** Accessible panel label, e.g. `Session Alpha task`. */
  panelLabel?: string
  /** Publishes durable session facts for the focused-session inspector. */
  inspectionStore?: ChatInspectionStore
}

export function ChatView({
  store,
  history,
  chatId,
  title,
  client,
  approvalClient,
  profileClient,
  selectionStore,
  onNewChat,
  onBackground,
  onFocus,
  onSolo,
  focused = false,
  panelLabel,
  inspectionStore,
}: ChatViewProps) {
  const gatewayClient = useGatewayClient(store)
  const approvalsClient =
    approvalClient !== undefined ? approvalClient : gatewayClient
  const workers = useConnection((state) => state.workers, store)
  const preferences = selectionStore ?? chatSelectionStore
  const [selection, setSelection] = useState<ChatSelection>(
    () => preferences.read() ?? emptySelection,
  )
  const [selectionReady, setSelectionReady] = useState(!history)
  const chat = useChat({
    client: client ?? gatewayClient,
    history: history ?? undefined,
    chatId,
  })
  const updateSelection = chat.updateSelection

  useEffect(() => {
    if (chat.phase !== "ready") {
      return
    }
    if (chat.restoredSelection) {
      // Hydrate the user's non-secret selection once the history is ready.
      // oxlint-disable-next-line react/set-state-in-effect
      setSelection(chat.restoredSelection)
    }
    setSelectionReady(true)
  }, [chat.phase, chat.restoredSelection])

  useEffect(() => {
    if (selectionReady) {
      updateSelection(selection)
    }
  }, [updateSelection, selection, selectionReady])

  const profilesClient =
    profileClient !== undefined ? profileClient : gatewayClient

  // Resolve the Worker first so its profile catalog is fetched exactly once for
  // that Worker. Profile names are Worker-owned and never reused across Workers.
  const target = useMemo(
    () => resolveSelection({ workers, selection }),
    [workers, selection],
  )
  const profileQuery = useWorkerProfiles(
    profilesClient,
    target.worker?.id ?? "",
  )
  const profiles = profileQuery.isSuccess ? profileQuery.data : null
  const profilesLoading = profileQuery.isLoading
  const profilesError =
    profilesClient === null
      ? "Runtime Profiles are unavailable for this connection."
      : profileQuery.isError
        ? profileErrorView(profileQuery.error).message
        : null

  const resolved = useMemo(
    () =>
      resolveSelection({
        workers,
        selection,
        profiles,
        profileError: profilesError,
      }),
    [workers, selection, profiles, profilesError],
  )

  const effectiveSelection =
    resolved.worker !== null ? resolved.selection : selection
  const liveChatId = chatId ?? chat.getChatId()
  const {
    sessionId: liveSessionId,
    status: liveStatus,
    activeTaskId: liveTaskId,
    activeAssistantId: liveAssistantId,
    lastDurableSeq: liveDurableSeq,
    gap: liveGap,
    recovery: liveRecovery,
  } = chat.state

  const liveUsage = (() => {
    const message =
      chat.state.messages.find(
        (candidate) => candidate.id === liveAssistantId,
      ) ??
      [...chat.state.messages]
        .reverse()
        .find((candidate) => candidate.role === "assistant")
    const part = message?.parts.find((candidate) => candidate.type === "usage")
    return part?.type === "usage" ? part.usage : null
  })()

  // Publish in a layout effect so the inspector's first passive read already
  // sees the live status instead of reading the same chat twice.
  useLayoutEffect(() => {
    if (!inspectionStore) {
      return
    }
    inspectionStore.getState().publish({
      chatId: liveChatId,
      sessionId: liveSessionId,
      title,
      status: liveStatus,
      taskId: liveTaskId,
      activeAssistantId: liveAssistantId,
      lastDurableSeq: liveDurableSeq,
      selection:
        effectiveSelection.worker || effectiveSelection.agent
          ? {
              worker: effectiveSelection.worker,
              agent: effectiveSelection.agent,
              profile: effectiveSelection.profile ?? "",
            }
          : null,
      usage: liveUsage,
      gap: liveGap,
      recovery: liveRecovery,
    })
  }, [
    effectiveSelection.agent,
    effectiveSelection.profile,
    effectiveSelection.worker,
    inspectionStore,
    liveAssistantId,
    liveChatId,
    liveDurableSeq,
    liveGap,
    liveRecovery,
    liveSessionId,
    liveStatus,
    liveTaskId,
    liveUsage,
    title,
  ])

  useEffect(() => {
    if (!inspectionStore) {
      return
    }
    return () => {
      inspectionStore.getState().remove(liveChatId)
    }
  }, [inspectionStore, liveChatId])

  const activation = useActivateProfile(profilesClient)
  const activationMatches =
    activation.variables !== undefined &&
    activation.variables.worker === resolved.worker?.id &&
    activation.variables.name === resolved.profile?.name
  const activationError =
    activation.isError && activationMatches
      ? profileErrorView(activation.error)
      : null
  const activationNotice =
    activation.isSuccess && activationMatches
      ? `Activated ${activation.data.name} revision ${activation.data.active_revision}.`
      : null

  // Local history restoration gates input: a restored task must be reconciled
  // with the Gateway before the next prompt is accepted.
  const busy = busyStatuses.has(chat.state.status) || chat.phase !== "ready"
  const canStop =
    chat.state.activeTaskId !== null && chat.state.status === "streaming"
  const canSend = resolved.ready && !busy && chat.phase === "ready"
  const gap = chat.state.gap
  const recovery = chat.state.recovery
  const detached = recovery?.kind === "detached" || recovery?.kind === "failed"

  // Persist the effective names only. An unresolved target is not written so a
  // temporary Gateway outage cannot erase the operator's last choice.
  useEffect(() => {
    if (!resolved.worker || !resolved.selection.agent || profiles === null) {
      return
    }
    preferences.write({
      worker: resolved.selection.worker,
      agent: resolved.selection.agent,
      profile: resolved.selection.profile ?? "",
    })
  }, [
    preferences,
    profiles,
    resolved.worker,
    resolved.selection.worker,
    resolved.selection.agent,
    resolved.selection.profile,
  ])

  function changeSelection(next: ChatSelection): void {
    if (next.worker !== resolved.selection.worker) {
      // A profile belongs to one Worker, so a Worker change invalidates both
      // the agent and profile choices.
      setSelection({ worker: next.worker, agent: "", profile: "" })
      return
    }
    setSelection(next)
  }

  function activateProfile(): void {
    const profile = resolved.profile
    const worker = resolved.worker
    const revision = profile ? latestRevisionNumber(profile) : 0
    if (!profile || !worker || revision < 1 || activation.isPending) {
      return
    }
    activation.mutate({ worker: worker.id, name: profile.name, revision })
  }

  return (
    <section
      data-focused={focused ? "true" : undefined}
      aria-label={
        panelLabel ? `${panelLabel}${focused ? ", focused" : ""}` : undefined
      }
      onClick={(event) => {
        // Selecting a panel changes inspector focus only. Controls inside the
        // panel own their click and must not re-focus or restore the chat.
        const target = event.target
        if (
          onFocus &&
          target instanceof Element &&
          target.closest("button, a, input, textarea, select")
        ) {
          return
        }
        onFocus?.()
      }}
      className={cn(
        "flex min-h-0 min-w-0 flex-1 flex-col gap-3 rounded-[10px] border bg-surface/60 p-3",
        focused ? "border-gold/70 ring-1 ring-gold/25" : "border-line",
      )}
    >
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <h1 className="truncate text-panel font-semibold tracking-tight">
              Chat
            </h1>
            {title ? (
              <span className="truncate text-meta text-dim">· {title}</span>
            ) : null}
            <Badge
              variant="outline"
              className="h-5 rounded-md px-1.5 text-micro tracking-wide"
            >
              {chat.state.status}
            </Badge>
            {focused ? (
              <span className="text-micro font-semibold tracking-wide text-gold uppercase">
                Focused
              </span>
            ) : null}
          </div>
          <p className="mt-1 max-w-2xl text-meta text-muted-foreground">
            One Gateway task per local chat. Selection changes apply to the next
            task, never to one already running.
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-1.5">
          {onFocus && !focused ? (
            <Button
              type="button"
              size="sm"
              variant="ghost"
              aria-label="Focus panel"
              onClick={onFocus}
            >
              <FocusIcon data-icon="inline-start" aria-hidden="true" />
              Focus
            </Button>
          ) : null}
          {onSolo ? (
            <Button
              type="button"
              size="sm"
              variant="ghost"
              aria-label="Solo panel"
              onClick={onSolo}
            >
              <ScanIcon data-icon="inline-start" aria-hidden="true" />
              Solo
            </Button>
          ) : null}
          {onBackground ? (
            <Button
              type="button"
              size="sm"
              variant="ghost"
              aria-label="Background session"
              onClick={onBackground}
            >
              <ArrowDownToLineIcon data-icon="inline-start" aria-hidden="true" />
              Background
            </Button>
          ) : null}
          <Button
            type="button"
            size="sm"
            variant="outline"
            onClick={() => {
              if (onNewChat) {
                onNewChat()
                return
              }
              chat.newChat()
            }}
          >
            <PlusIcon data-icon="inline-start" aria-hidden="true" />
            {onNewChat ? "New session" : "New chat"}
          </Button>
        </div>
      </div>

      <ChatControls
        workers={workers}
        resolved={resolved}
        profiles={profiles ?? []}
        profilesLoading={profilesLoading}
        profilesError={profilesError}
        activating={activation.isPending}
        activationError={activationError}
        activationNotice={activationNotice}
        onChange={changeSelection}
        onActivate={activateProfile}
      />

      {workers.length === 0 ? (
        <Alert>
          <MessageSquareIcon />
          <AlertTitle>No Worker is visible to this caller</AlertTitle>
          <AlertDescription>
            The caller token may only allow specific Workers. Reconnect with a
            token that can read at least one Worker before chatting.
          </AlertDescription>
        </Alert>
      ) : null}

      {chat.phase === "reset-required" ? (
        <Alert>
          <MessageSquareIcon />
          <AlertTitle>Local chat history could not be read</AlertTitle>
          <AlertDescription>
            <p>
              {chat.historyError?.message ??
                "The browser's local records are corrupt or from an unsupported version."}
            </p>
            <p className="mt-2">
              The chat is disabled so a malformed task or cursor is never sent.
              Resetting deletes local chat records only; it does not cancel tasks
              on the Gateway.
            </p>
            <Button
              type="button"
              variant="outline"
              className="mt-3"
              onClick={() => {
                void chat.resetLocalHistory()
              }}
            >
              Reset local history
            </Button>
          </AlertDescription>
        </Alert>
      ) : null}

      {detached ? (
        <Alert>
          <MessageSquareIcon />
          <AlertTitle>
            {recovery?.kind === "detached"
              ? "Local chat detached from the Gateway"
              : "The last task could not be reconciled"}
          </AlertTitle>
          <AlertDescription>
            {recovery?.detail ??
              "The task is not available on the Gateway. Local history is kept, but it cannot be resumed."}
          </AlertDescription>
        </Alert>
      ) : null}

      {gap ? (
        <Alert>
          <MessageSquareIcon />
          <AlertTitle>Stream history gap</AlertTitle>
          <AlertDescription>
            {gap.reason === "cursor_expired"
              ? "The reconnect cursor is older than the Worker's retained events. The task projection and the retained events were reloaded, but some earlier events are missing; inspect the task before continuing."
              : "The stream fell behind and had to reconnect. Some events may be missing."}
          </AlertDescription>
        </Alert>
      ) : null}

      <MessageScrollerProvider autoScroll defaultScrollPosition="end">
        <MessageScroller className="min-h-[20rem] flex-1 rounded-[10px] border bg-card">
          <MessageScrollerViewport role="log" aria-label="Chat messages">
            <MessageScrollerContent className="gap-6 p-4">
              {chat.state.messages.length === 0 ? (
                <div className="flex flex-1 flex-col items-center justify-center gap-2 px-6 py-16 text-center">
                  <MessageSquareIcon
                    className="size-6 text-muted-foreground"
                    aria-hidden="true"
                  />
                  <p className="text-sm font-medium">Start a task</p>
                  <p className="max-w-md text-sm text-muted-foreground">
                    Select a Worker, agent, and Runtime Profile, then send a
                    prompt. The answer streams here until the task reaches a
                    terminal state.
                  </p>
                </div>
              ) : (
                chat.state.messages.map((message) => (
                  <MessageScrollerItem
                    key={message.id}
                    messageId={message.id}
                    scrollAnchor={message.role === "user"}
                  >
                    <ChatMessageView
                      message={message}
                      busy={busy}
                      onRetry={(userMessageId) => chat.retry(userMessageId)}
                      approvalClient={approvalsClient}
                      onApprovalUpdated={(approval) =>
                        chat.applyApproval(approval)
                      }
                    />
                  </MessageScrollerItem>
                ))
              )}
            </MessageScrollerContent>
          </MessageScrollerViewport>
          <MessageScrollerButton />
        </MessageScroller>
      </MessageScrollerProvider>

      <ChatComposer
        canSend={canSend}
        busy={busy}
        canStop={canStop}
        hint={
          chat.phase === "loading"
            ? "Restoring local chat history..."
            : chat.phase === "reset-required"
              ? "Reset local history to enable chat."
              : !resolved.ready && resolved.issues.length > 0
                ? "Resolve the target selection before sending."
                : undefined
        }
        onSend={(text) => {
          if (!resolved.ready) {
            return
          }
          chat.send(text, resolved.selection)
        }}
        onStop={() => chat.stop()}
      />
    </section>
  )
}

import { MessageSquareIcon, PlusIcon } from "lucide-react"
import { useEffect, useMemo, useState } from "react"

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
import type { ChatHistoryStore } from "@/features/chat/local-chat-history"
import { useChat } from "@/features/chat/use-chat"
import {
  useConnection,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import { createGatewayClient, type GatewayClient } from "@goldenbee/sdk"
import type { ChatGateway } from "@/features/chat/chat-runner"

const busyStatuses = new Set([
  "submitting",
  "streaming",
  "stopping",
  "recovering",
])

const emptySelection: ChatSelection = { worker: "", agent: "", profile: "" }

function useGatewayChatClient(store: ConnectionStore): GatewayClient | null {
  const connected = useConnection((state) => state.status === "connected", store)
  const gatewayUrl = useConnection((state) => state.gatewayUrl, store)
  const token = useConnection((state) => state.token, store)

  return useMemo(() => {
    if (!connected) {
      return null
    }
    return createGatewayClient({
      baseUrl: gatewayUrl,
      token,
      fetch: (input, init) => fetch(input, init),
    })
  }, [connected, gatewayUrl, token])
}

export interface ChatViewProps {
  store: ConnectionStore
  /** Browser-local history; omitted in tests that exercise transport only. */
  history?: ChatHistoryStore | null
  /** Test seam; production builds the client from the connection store. */
  client?: ChatGateway | null
  /** Test seam for the Worker-owned Runtime Profile catalog. */
  profileClient?: SelectionGateway | null
  /** Test seam for the non-secret local selection preference. */
  selectionStore?: ChatSelectionStore
}

export function ChatView({
  store,
  history,
  client,
  profileClient,
  selectionStore,
}: ChatViewProps) {
  const gatewayClient = useGatewayChatClient(store)
  const workers = useConnection((state) => state.workers, store)
  const preferences = selectionStore ?? chatSelectionStore
  const [selection, setSelection] = useState<ChatSelection>(
    () => preferences.read() ?? emptySelection,
  )
  const [selectionReady, setSelectionReady] = useState(!history)
  const chat = useChat({
    client: client ?? gatewayClient,
    history: history ?? undefined,
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
    <div className="flex min-h-0 flex-1 flex-col gap-4">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <div className="flex flex-wrap items-center gap-2">
            <h1 className="font-heading text-2xl font-semibold tracking-tight sm:text-3xl">
              Chat
            </h1>
            <Badge variant="outline">{chat.state.status}</Badge>
          </div>
          <p className="mt-2 max-w-2xl text-sm text-muted-foreground">
            Submit a task to the selected Worker and stream its events.
            Reasoning, tools, plans, usage, and task evidence appear in the
            trace; approvals arrive in a later screen. Selection changes apply
            to the next task, never to one already running.
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Button type="button" variant="outline" onClick={() => chat.newChat()}>
            <PlusIcon data-icon="inline-start" aria-hidden="true" />
            New chat
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
        <MessageScroller className="h-[clamp(18rem,50vh,34rem)] rounded-2xl border bg-card">
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
    </div>
  )
}

import { MessageSquareIcon, PlusIcon } from "lucide-react"
import { useMemo, useState } from "react"

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
import { effectiveSelection } from "@/features/chat/chat-selection"
import { ChatMessageView } from "@/features/chat/chat-message"
import type { ChatSelection } from "@/features/chat/chat-types"
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

function initialSelection(): ChatSelection {
  return { worker: "", agent: "", profile: "default" }
}

export interface ChatViewProps {
  store: ConnectionStore
  /** Test seam; production builds the client from the connection store. */
  client?: ChatGateway | null
}

export function ChatView({ store, client }: ChatViewProps) {
  const gatewayClient = useGatewayChatClient(store)
  const workers = useConnection((state) => state.workers, store)
  const [selection, setSelection] = useState<ChatSelection>(initialSelection)
  const chat = useChat({ client: client ?? gatewayClient })

  const effective = useMemo(
    () => effectiveSelection(workers, selection),
    [workers, selection],
  )
  const busy = busyStatuses.has(chat.state.status)
  const canStop =
    chat.state.activeTaskId !== null && chat.state.status === "streaming"
  const hasTarget = Boolean(effective.worker && effective.agent)
  const canSend = hasTarget && !busy
  const gap = chat.state.gap

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
            Submit a task to the selected Worker and stream its durable events.
            Reasoning, tools, approvals, and task evidence arrive in later
            screens.
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Button
            type="button"
            variant="outline"
            onClick={() => {
              setSelection(initialSelection())
              chat.newChat()
            }}
          >
            <PlusIcon data-icon="inline-start" aria-hidden="true" />
            New chat
          </Button>
        </div>
      </div>

      <ChatControls
        workers={workers}
        selection={effective}
        disabled={busy}
        onChange={(next) => setSelection(next)}
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

      {gap ? (
        <Alert>
          <MessageSquareIcon />
          <AlertTitle>Stream history gap</AlertTitle>
          <AlertDescription>
            {gap.reason === "cursor_expired"
              ? "The reconnect cursor is older than the Worker's retained events. History may be incomplete; inspect the task before continuing."
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
          !hasTarget && workers.length > 0
            ? "Choose a Worker and agent before sending."
            : undefined
        }
        onSend={(text) => chat.send(text, effective)}
        onStop={() => chat.stop()}
      />
    </div>
  )
}

import { ArrowUpIcon, SquareIcon } from "lucide-react"
import { useState } from "react"

import {
  InputGroup,
  InputGroupAddon,
  InputGroupButton,
  InputGroupTextarea,
} from "@/components/ui/input-group"

export interface ChatComposerProps {
  canSend: boolean
  busy: boolean
  canStop: boolean
  onSend(text: string): void
  onStop(): void
  hint?: string
}

/** Prompt input. Enter sends; Shift+Enter inserts a newline. */
export function ChatComposer({
  canSend,
  busy,
  canStop,
  onSend,
  onStop,
  hint,
}: ChatComposerProps) {
  const [draft, setDraft] = useState("")

  function submit(): void {
    const text = draft.trim()
    if (!text || !canSend) {
      return
    }
    onSend(text)
    setDraft("")
  }

  return (
    <div className="flex flex-col gap-2">
      <form
        onSubmit={(event) => {
          event.preventDefault()
          submit()
        }}
      >
        <InputGroup className="items-end rounded-2xl border-border bg-card p-1.5">
          <InputGroupTextarea
            aria-label="Message"
            placeholder="Send a prompt to the selected Worker…"
            className="min-h-16 max-h-48"
            value={draft}
            disabled={busy}
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !event.shiftKey) {
                event.preventDefault()
                submit()
              }
            }}
          />
          <InputGroupAddon align="inline-end">
            {canStop ? (
              <InputGroupButton
                variant="secondary"
                size="sm"
                onClick={onStop}
                aria-label="Stop task"
              >
                <SquareIcon data-icon="inline-start" aria-hidden="true" />
                Stop
              </InputGroupButton>
            ) : (
              <InputGroupButton
                type="submit"
                size="sm"
                disabled={!canSend || draft.trim().length === 0}
                aria-label="Send message"
              >
                <ArrowUpIcon data-icon="inline-start" aria-hidden="true" />
                Send
              </InputGroupButton>
            )}
          </InputGroupAddon>
        </InputGroup>
      </form>
      <p className="text-xs text-muted-foreground">
        {hint ??
          "Enter sends · Shift+Enter adds a line · Attachments are not supported by this Gateway release."}
      </p>
    </div>
  )
}

import { ArrowUpIcon, SquareIcon } from "lucide-react"
import { useState, type ReactNode } from "react"
import { cn } from "cn"

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
  /** Worker, agent, and Runtime Profile controls shown in the composer footer. */
  targetControls?: ReactNode
}

/** Prompt input. Enter sends; Shift+Enter inserts a newline. */
export function ChatComposer({
  canSend,
  busy,
  canStop,
  onSend,
  onStop,
  hint,
  targetControls,
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
    <div className="flex flex-col gap-1.5">
      <form
        onSubmit={(event) => {
          event.preventDefault()
          submit()
        }}
      >
        <InputGroup className="flex-col items-stretch rounded-[12px] border-line bg-surface-2 p-0">
          <InputGroupTextarea
            aria-label="Message"
            placeholder="Message the selected Worker…"
            className="min-h-14 max-h-40 px-3 pt-2.5"
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
          <InputGroupAddon
            align="block-end"
            className="gap-2 border-t border-line/60 px-2 py-1.5"
          >
            <div className="min-w-0 flex-1">{targetControls}</div>
            <div className="flex shrink-0 items-center gap-1">
              {canStop ? (
                <InputGroupButton
                  variant="secondary"
                  size="icon-sm"
                  onClick={onStop}
                  aria-label="Stop task"
                  title="Stop task"
                >
                  <SquareIcon aria-hidden="true" />
                </InputGroupButton>
              ) : (
                <InputGroupButton
                  type="submit"
                  size="icon-sm"
                  disabled={!canSend || draft.trim().length === 0}
                  aria-label="Send message"
                  title="Send message"
                  className="bg-gold text-primary-foreground hover:bg-gold/85"
                >
                  <ArrowUpIcon aria-hidden="true" />
                </InputGroupButton>
              )}
            </div>
          </InputGroupAddon>
        </InputGroup>
      </form>
      {hint ? (
        <p
          className={cn(
            "text-micro text-muted-foreground",
            busy ? "shimmer" : undefined,
          )}
        >
          {hint}
        </p>
      ) : null}
    </div>
  )
}

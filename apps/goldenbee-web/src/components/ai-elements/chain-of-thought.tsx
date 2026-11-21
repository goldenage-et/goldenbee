import { useState, type ReactNode } from "react"

import {
  Trace,
  TraceCollapsible,
  TraceCollapsibleContent,
  TraceCollapsibleTrigger,
  TraceIcon,
  TraceLine,
  TraceTitle,
} from "@/components/ui/trace"

/**
 * Chain-of-thought summary from the AI Elements pattern.
 *
 * The summary expands into the reasoning or step content. The open state is
 * controlled locally; while running it opens by default unless the operator
 * has already chosen a state.
 */
export function ChainOfThought({
  title,
  running = false,
  icon,
  children,
}: {
  title: string
  running?: boolean
  icon?: ReactNode
  children: ReactNode
}) {
  const [open, setOpen] = useState<boolean | undefined>(undefined)
  const expanded = open ?? running

  return (
    <Trace>
      <TraceCollapsible open={expanded} onOpenChange={setOpen}>
        <TraceCollapsibleTrigger>
          <TraceLine status={running ? "running" : undefined}>
            <TraceIcon>{icon}</TraceIcon>
            <TraceTitle>{title}</TraceTitle>
          </TraceLine>
        </TraceCollapsibleTrigger>
        <TraceCollapsibleContent>{children}</TraceCollapsibleContent>
      </TraceCollapsible>
    </Trace>
  )
}

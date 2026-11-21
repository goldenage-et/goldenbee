import { CircleCheckIcon } from "lucide-react"

import { Trace, TraceIcon, TraceLine, TraceTitle } from "@/components/ui/trace"

/**
 * Presentational connection-check trace.
 *
 * Composed from the new shadcn AI Elements Trace foundation, following the
 * Task and Reasoning patterns. No AI SDK message parts or transports are used.
 *
 * Sources:
 * - https://ui.shadcn.com/docs/components/base/trace
 * - https://ui.shadcn.com/docs/ai-elements/base/task
 */
export function ConnectionTrace({ workerCount }: { workerCount: number }) {
  return (
    <Trace aria-label="Connection check">
      <TraceLine>
        <TraceIcon>
          <CircleCheckIcon />
        </TraceIcon>
        <TraceTitle>Gateway health verified</TraceTitle>
      </TraceLine>
      <TraceLine>
        <TraceIcon>
          <CircleCheckIcon />
        </TraceIcon>
        <TraceTitle>
          Caller token accepted · {workerCount}{" "}
          {workerCount === 1 ? "Worker" : "Workers"} visible
        </TraceTitle>
      </TraceLine>
    </Trace>
  )
}

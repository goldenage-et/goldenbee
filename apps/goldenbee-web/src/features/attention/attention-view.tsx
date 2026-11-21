import { ShieldCheckIcon } from "lucide-react"

import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "@/components/ui/empty"

/**
 * Attention has no fabricated queue. Approvals are read inline in the chat
 * timeline and on task detail, where each card carries its observed digest.
 */
export function AttentionView() {
  return (
    <div className="mx-auto flex w-full max-w-3xl flex-1 flex-col px-1 py-2">
      <Empty className="border border-dashed border-line bg-surface/40">
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <ShieldCheckIcon aria-hidden="true" />
          </EmptyMedia>
          <EmptyTitle>No approval queue</EmptyTitle>
          <EmptyDescription>
            Approvals appear inline in the chat timeline and on the task detail
            page, with the exact target, digest, expiry, and recorded status.
            This Client does not simulate a global approval queue.
          </EmptyDescription>
        </EmptyHeader>
      </Empty>
    </div>
  )
}

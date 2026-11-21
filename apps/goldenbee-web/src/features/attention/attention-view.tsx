import { ShieldCheckIcon } from "lucide-react"

import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "@/components/ui/empty"

/**
 * Attention has no fabricated queue. Gateway approval reads arrive with the
 * approvals ticket; until then this route is an honest empty surface.
 */
export function AttentionView() {
  return (
    <div className="mx-auto flex w-full max-w-3xl flex-1 flex-col px-1 py-2">
      <Empty className="border border-dashed border-line bg-surface/40">
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <ShieldCheckIcon aria-hidden="true" />
          </EmptyMedia>
          <EmptyTitle>No pending approvals</EmptyTitle>
          <EmptyDescription>
            This Client cannot read the Gateway approval queue yet. When approval
            reads are available, pending items will appear here with their exact
            target and digest. Nothing is simulated in the meantime.
          </EmptyDescription>
        </EmptyHeader>
      </Empty>
    </div>
  )
}

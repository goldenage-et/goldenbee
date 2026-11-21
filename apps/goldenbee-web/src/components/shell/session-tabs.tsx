import { PlusIcon, XIcon } from "lucide-react"
import { cn } from "cn"

import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import type { SessionGroup } from "@/features/workspace/workspace-store"

export interface SessionTabsProps {
  groups: SessionGroup[]
  activeGroupId: string
  backgroundChatIds: string[]
  onSelectGroup(id: string): void
  onNewSession(): void
  onRequestNewGroup(): void
  onRequestRename(group: SessionGroup): void
  onRequestClose(group: SessionGroup): void
}

export function SessionTabs({
  groups,
  activeGroupId,
  backgroundChatIds,
  onSelectGroup,
  onNewSession,
  onRequestNewGroup,
  onRequestRename,
  onRequestClose,
}: SessionTabsProps) {
  const active = groups.find((group) => group.id === activeGroupId)
  const visibleCount =
    active?.chatIds.filter((chatId) => !backgroundChatIds.includes(chatId))
      .length ?? 0

  return (
    <>
      <div
        role="tablist"
        aria-label="Session groups"
        className="flex h-10 shrink-0 items-end gap-1 overflow-x-auto border-b border-line bg-background px-2 pt-1.5"
      >
        {groups.map((group) => {
          const selected = group.id === activeGroupId
          const count = group.chatIds.filter(
            (chatId) => !backgroundChatIds.includes(chatId),
          ).length
          return (
            <div
              key={group.id}
              className={cn(
                "group/tab flex h-8 shrink-0 items-center rounded-t-[8px] border border-b-0",
                selected
                  ? "border-line bg-surface text-foreground"
                  : "border-transparent text-muted-foreground hover:bg-surface-2 hover:text-foreground",
              )}
            >
              <button
                type="button"
                role="tab"
                aria-selected={selected}
                aria-current={selected ? "page" : undefined}
                aria-controls="session-workspace"
                onClick={() => onSelectGroup(group.id)}
                className="flex h-full min-w-0 items-center gap-1.5 pl-2.5 pr-1.5 text-meta font-medium outline-none focus-visible:outline-2 focus-visible:outline-gold"
              >
                <span aria-hidden="true" className="text-dim">
                  ▤
                </span>
                <span className="max-w-32 truncate">{group.name}</span>
                <Badge
                  variant="outline"
                  className="h-4 rounded px-1 text-micro text-muted-foreground"
                >
                  {count}
                </Badge>
              </button>
              <button
                type="button"
                aria-label={`Close group ${group.name}`}
                onClick={() => onRequestClose(group)}
                className="mr-0.5 grid size-5 place-items-center rounded text-dim hover:bg-surface-3 hover:text-foreground focus-visible:outline-2 focus-visible:outline-gold"
              >
                <XIcon className="size-3" aria-hidden="true" />
              </button>
            </div>
          )
        })}
        <button
          type="button"
          aria-label="New session group"
          title="New session group"
          onClick={onRequestNewGroup}
          className="mb-0.5 grid size-7 shrink-0 place-items-center rounded-[7px] text-dim hover:bg-surface-3 hover:text-foreground focus-visible:outline-2 focus-visible:outline-gold"
        >
          <PlusIcon className="size-4" aria-hidden="true" />
        </button>
      </div>

      <div className="flex h-10 shrink-0 items-center gap-2 border-b border-line bg-surface px-3">
        <div className="min-w-0">
          <h2 className="truncate text-panel font-semibold tracking-tight">
            {active?.name ?? "No session group"}
          </h2>
        </div>
        <Badge
          variant="outline"
          className="h-4 rounded px-1 text-micro text-muted-foreground"
        >
          {visibleCount} visible
        </Badge>
        <div className="ml-auto flex items-center gap-1">
          <Button
            type="button"
            size="xs"
            variant="ghost"
            aria-label="New session in this group"
            onClick={onNewSession}
          >
            <PlusIcon aria-hidden="true" />
            New
          </Button>
          <Button
            type="button"
            size="xs"
            variant="ghost"
            aria-label="Rename group"
            disabled={!active}
            onClick={() => {
              if (active) {
                onRequestRename(active)
              }
            }}
          >
            Rename
          </Button>
          <Button
            type="button"
            size="xs"
            variant="ghost"
            aria-label="Close group"
            disabled={!active}
            onClick={() => {
              if (active) {
                onRequestClose(active)
              }
            }}
          >
            <XIcon aria-hidden="true" />
            Close
          </Button>
        </div>
      </div>
    </>
  )
}

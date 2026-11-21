import {
  FolderPlusIcon,
  PanelLeftCloseIcon,
  PanelLeftOpenIcon,
  PencilIcon,
  PlusIcon,
  XIcon,
} from "lucide-react"
import { cn } from "cn"

import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import type { SessionGroup } from "@/features/workspace/workspace-store"

export interface SessionTabsProps {
  groups: SessionGroup[]
  activeGroupId: string
  backgroundChatIds: string[]
  sidebarOpen: boolean
  onToggleSidebar(): void
  onSelectGroup(id: string): void
  onNewSession(): void
  onNewGroup(): void
  onRequestRename(group: SessionGroup): void
  onRequestClose(group: SessionGroup): void
}

export function SessionTabs({
  groups,
  activeGroupId,
  backgroundChatIds,
  sidebarOpen,
  onToggleSidebar,
  onSelectGroup,
  onNewSession,
  onNewGroup,
  onRequestRename,
  onRequestClose,
}: SessionTabsProps) {
  const active = groups.find((group) => group.id === activeGroupId)
  const SidebarIcon = sidebarOpen ? PanelLeftCloseIcon : PanelLeftOpenIcon

  return (
    <div
      className="flex h-9 shrink-0 items-center gap-1 border-b border-line bg-background px-2"
    >
      <Button
        type="button"
        size="icon-xs"
        variant="ghost"
        aria-label={sidebarOpen ? "Collapse sidebar" : "Expand sidebar"}
        aria-pressed={sidebarOpen}
        title={sidebarOpen ? "Collapse sidebar" : "Expand sidebar"}
        onClick={onToggleSidebar}
        className="shrink-0 max-[780px]:hidden"
      >
        <SidebarIcon aria-hidden="true" />
      </Button>
      <div
        role="tablist"
        aria-label="Session groups"
        className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto"
      >
        {groups.map((group) => {
          const selected = group.id === activeGroupId
          const count = group.chatIds.filter(
            (chatId) => !backgroundChatIds.includes(chatId),
          ).length
          return (
            <button
              key={group.id}
              type="button"
              role="tab"
              aria-selected={selected}
              aria-current={selected ? "page" : undefined}
              aria-controls="session-workspace"
              onClick={() => onSelectGroup(group.id)}
              className={cn(
                "flex h-7 shrink-0 items-center gap-1.5 rounded-[7px] border px-2 text-meta font-medium outline-none focus-visible:outline-2 focus-visible:outline-gold",
                selected
                  ? "border-line bg-surface text-foreground"
                  : "border-transparent text-muted-foreground hover:bg-surface-2 hover:text-foreground",
              )}
            >
              <span className="max-w-36 truncate">{group.name}</span>
              <Badge
                variant="outline"
                className="h-4 rounded px-1 text-micro text-muted-foreground"
              >
                {count}
              </Badge>
            </button>
          )
        })}
      </div>

      <div
        role="group"
        aria-label="Session group actions"
        className="flex shrink-0 items-center gap-0.5"
      >
        <Button
          type="button"
          size="icon-xs"
          variant="ghost"
          aria-label="New session in this group"
          title="New session"
          onClick={onNewSession}
        >
          <PlusIcon aria-hidden="true" />
        </Button>
        <Button
          type="button"
          size="icon-xs"
          variant="ghost"
          aria-label="New session group"
          title="New group"
          onClick={onNewGroup}
        >
          <FolderPlusIcon aria-hidden="true" />
        </Button>
        <Button
          type="button"
          size="icon-xs"
          variant="ghost"
          aria-label="Rename group"
          title="Rename group"
          disabled={!active}
          onClick={() => {
            if (active) {
              onRequestRename(active)
            }
          }}
        >
          <PencilIcon aria-hidden="true" />
        </Button>
        <Button
          type="button"
          size="icon-xs"
          variant="ghost"
          aria-label="Close group"
          title="Close group"
          disabled={!active}
          onClick={() => {
            if (active) {
              onRequestClose(active)
            }
          }}
        >
          <XIcon aria-hidden="true" />
        </Button>
      </div>
    </div>
  )
}

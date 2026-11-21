import {
  ChevronRightIcon,
  FileIcon,
  FolderIcon,
  FolderOpenIcon,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible"
import { cn } from "cn"

export type FileTreeNodeKind = "file" | "directory"

export interface FileTreeNodeData {
  name: string
  path: string
  kind: FileTreeNodeKind
  change?: string
  additions?: number
  deletions?: number
  loading?: boolean
  children?: FileTreeNodeData[]
}

export interface FileTreeProps {
  nodes: FileTreeNodeData[]
  expanded: ReadonlySet<string>
  selected?: string
  onExpandedChange(path: string, open: boolean): void
  onSelect(path: string): void
}

const changeLabels: Record<string, string> = {
  A: "Added",
  M: "Modified",
  D: "Deleted",
  R: "Moved",
  add: "Added",
  modify: "Modified",
  delete: "Deleted",
  move: "Moved",
}

function changeLabel(change: string): string {
  return changeLabels[change] ?? change
}

function changeTone(change: string): string {
  switch (change) {
    case "A":
    case "add":
      return "text-positive"
    case "D":
    case "delete":
      return "text-destructive"
    case "R":
    case "move":
      return "text-gold"
    default:
      return "text-muted-foreground"
  }
}

function FileTreeFile({
  node,
  selected,
  onSelect,
}: {
  node: FileTreeNodeData
  selected?: string
  onSelect(path: string): void
}) {
  const isSelected = selected === node.path
  return (
    <Button
      type="button"
      variant="ghost"
      size="sm"
      aria-current={isSelected ? "true" : undefined}
      className={cn(
        "h-7 w-full justify-start gap-1.5 px-1.5 text-micro font-normal",
        isSelected && "bg-surface-3 text-foreground",
      )}
      onClick={() => onSelect(node.path)}
    >
      <FileIcon className="size-3.5 shrink-0 text-dim" aria-hidden="true" />
      <span className="min-w-0 flex-1 truncate text-left">{node.name}</span>
      {node.change ? (
        <span
          className={cn(
            "shrink-0 font-mono text-[10px] font-semibold",
            changeTone(node.change),
          )}
          title={changeLabel(node.change)}
        >
          {node.change.length > 1 ? changeLabel(node.change) : node.change}
        </span>
      ) : null}
      {node.additions || node.deletions ? (
        <span className="shrink-0 font-mono text-[10px] text-dim">
          {node.additions ? `+${node.additions}` : ""}
          {node.deletions ? ` −${node.deletions}` : ""}
        </span>
      ) : null}
    </Button>
  )
}

function FileTreeFolder({
  node,
  expanded,
  selected,
  onExpandedChange,
  onSelect,
}: {
  node: FileTreeNodeData
  expanded: ReadonlySet<string>
  selected?: string
  onExpandedChange(path: string, open: boolean): void
  onSelect(path: string): void
}) {
  const open = expanded.has(node.path)
  return (
    <Collapsible
      open={open}
      onOpenChange={(next) => {
        onExpandedChange(node.path, next)
      }}
    >
      <CollapsibleTrigger
        render={
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="h-7 w-full justify-start gap-1.5 px-1.5 text-micro font-normal"
          />
        }
      >
        <ChevronRightIcon
          className={cn(
            "size-3.5 shrink-0 text-dim transition-transform",
            open && "rotate-90",
          )}
          aria-hidden="true"
        />
        {open ? (
          <FolderOpenIcon className="size-3.5 shrink-0 text-gold" aria-hidden="true" />
        ) : (
          <FolderIcon className="size-3.5 shrink-0 text-gold" aria-hidden="true" />
        )}
        <span className="min-w-0 flex-1 truncate text-left">{node.name}</span>
      </CollapsibleTrigger>
      <CollapsibleContent>
        <div className="ml-3 flex flex-col border-l border-line/70 pl-1">
          {node.loading ? (
            <p className="px-2 py-1 text-micro text-dim">Loading…</p>
          ) : node.children && node.children.length > 0 ? (
            <FileTree
              nodes={node.children}
              expanded={expanded}
              selected={selected}
              onExpandedChange={onExpandedChange}
              onSelect={onSelect}
            />
          ) : (
            <p className="px-2 py-1 text-micro text-dim">Empty</p>
          )}
        </div>
      </CollapsibleContent>
    </Collapsible>
  )
}

/**
 * Read-only workspace tree rendered with the shadcn AI Elements File Tree
 * pattern: folders are Collapsible, files are Buttons.
 */
export function FileTree({
  nodes,
  expanded,
  selected,
  onExpandedChange,
  onSelect,
}: FileTreeProps) {
  if (nodes.length === 0) {
    return <p className="text-micro text-dim">No files</p>
  }
  return (
    <div role="tree" className="flex min-w-0 flex-col">
      {nodes.map((node) =>
        node.kind === "directory" ? (
          <FileTreeFolder
            key={node.path}
            node={node}
            expanded={expanded}
            selected={selected}
            onExpandedChange={onExpandedChange}
            onSelect={onSelect}
          />
        ) : (
          <FileTreeFile
            key={node.path}
            node={node}
            selected={selected}
            onSelect={onSelect}
          />
        ),
      )}
    </div>
  )
}

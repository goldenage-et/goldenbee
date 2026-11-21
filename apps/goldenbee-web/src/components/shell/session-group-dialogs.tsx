import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import type { SessionGroup } from "@/features/workspace/workspace-store"

export interface GroupDialogState {
  mode: "create" | "rename"
  id?: string
  name: string
}

export function SessionGroupDialog({
  state,
  onNameChange,
  onCancel,
  onSave,
}: {
  state: GroupDialogState | null
  onNameChange(name: string): void
  onCancel(): void
  onSave(): void
}) {
  return (
    <Dialog
      open={state !== null}
      onOpenChange={(open) => {
        if (!open) {
          onCancel()
        }
      }}
    >
      <DialogContent className="max-w-md rounded-[11px] border-line bg-surface">
        <DialogHeader>
          <DialogTitle className="text-body font-semibold">
            {state?.mode === "rename"
              ? "Rename session group"
              : "New session group"}
          </DialogTitle>
          <DialogDescription className="text-meta">
            Groups are local to this browser. They are not Gateway resources.
          </DialogDescription>
        </DialogHeader>
        <div className="grid gap-1.5">
          <label
            htmlFor="session-group-name"
            className="text-micro font-semibold tracking-[0.12em] text-dim uppercase"
          >
            Group name
          </label>
          <Input
            id="session-group-name"
            aria-label="Group name"
            value={state?.name ?? ""}
            onChange={(event) => onNameChange(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault()
                onSave()
              }
            }}
          />
        </div>
        <DialogFooter>
          <Button type="button" variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
          <Button
            type="button"
            disabled={!state?.name.trim()}
            onClick={onSave}
          >
            {state?.mode === "rename" ? "Save name" : "Create group"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

export function CloseGroupDialog({
  group,
  onCancel,
  onConfirm,
}: {
  group: SessionGroup | null
  onCancel(): void
  onConfirm(): void
}) {
  return (
    <Dialog
      open={group !== null}
      onOpenChange={(open) => {
        if (!open) {
          onCancel()
        }
      }}
    >
      <DialogContent className="max-w-md rounded-[11px] border-line bg-surface">
        <DialogHeader>
          <DialogTitle className="text-body font-semibold">
            Close session group
          </DialogTitle>
          <DialogDescription className="text-meta">
            {group
              ? `Visible sessions in “${group.name}” move to Background. Their Gateway tasks are not cancelled.`
              : ""}
          </DialogDescription>
        </DialogHeader>
        <p className="text-meta text-muted-foreground">
          Restore them from the sidebar at any time. Ending a Gateway task is a
          separate, explicit action.
        </p>
        <DialogFooter>
          <Button type="button" variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
          <Button type="button" variant="destructive" onClick={onConfirm}>
            Close group
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

import { useState } from "react"
import {
  ArrowLeftIcon,
  CheckCircle2Icon,
  ShieldCheckIcon,
  ShieldOffIcon,
  Trash2Icon,
} from "lucide-react"

import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Spinner } from "@/components/ui/spinner"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import type { ProfileRevision } from "@goldenbee/sdk"

import type { RuntimeProfileClient } from "./profile-client"
import { ProfileErrorAlert } from "./profile-error-alert"
import { ProfileForm } from "./profile-form-fields"
import { latestRevision } from "./profile-form"
import {
  useActivateProfile,
  useDeactivateProfile,
  useDeleteProfile,
  useProfileQuery,
  useReviseProfile,
  useValidateProfile,
} from "./profile-queries"

function Definition({
  label,
  children,
}: {
  label: string
  children: React.ReactNode
}) {
  return (
    <div className="flex flex-col gap-0.5">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="text-sm break-words">{children}</dd>
    </div>
  )
}

function BackToProfiles({ worker }: { worker: string }) {
  return (
    <a
      href={`#/profiles/${encodeURIComponent(worker)}`}
      className="inline-flex w-fit items-center gap-1 text-sm text-muted-foreground underline-offset-4 hover:underline"
    >
      <ArrowLeftIcon className="size-3.5" aria-hidden="true" />
      All profiles
    </a>
  )
}

function listOrNone(values: string[] | undefined): string {
  return values && values.length > 0 ? values.join(", ") : "None"
}

function RevisionMetadata({ revision }: { revision: ProfileRevision | undefined }) {
  if (!revision) {
    return (
      <p className="text-sm text-muted-foreground">
        This profile has no revisions.
      </p>
    )
  }
  return (
    <div className="flex flex-col gap-4">
      <dl className="grid grid-cols-2 gap-4">
        <Definition label="Revision">{revision.revision}</Definition>
        <Definition label="Model">{revision.model}</Definition>
        <Definition label="Composition">{revision.composition ?? "single"}</Definition>
        <Definition label="Workflow">{revision.workflow ?? "None"}</Definition>
        <Definition label="Capabilities">
          {listOrNone(revision.capabilities)}
        </Definition>
        <Definition label="Grants">{listOrNone(revision.grants)}</Definition>
        <Definition label="Restrictions">
          {listOrNone(revision.restrictions)}
        </Definition>
        <Definition label="Members">{listOrNone(revision.members)}</Definition>
        <Definition label="Skills">{listOrNone(revision.skills)}</Definition>
        <Definition label="MCP servers">{listOrNone(revision.mcp)}</Definition>
        <Definition label="Created">
          {revision.created_at
            ? new Date(revision.created_at).toLocaleString()
            : "Unknown"}
        </Definition>
        <Definition label="Limits">
          {revision.limits && Object.keys(revision.limits).length > 0
            ? Object.entries(revision.limits)
                .map(([key, value]) => `${key}=${String(value)}`)
                .join(", ")
            : "None"}
        </Definition>
        <Definition label="Compaction">
          {revision.compaction
            ? `${revision.compaction.strategy}: budget ${revision.compaction.context_budget}, preserve ${revision.compaction.preserve_recent}, retries ${revision.compaction.max_retries}`
            : "Disabled"}
        </Definition>
      </dl>

      <div>
        <p className="text-xs text-muted-foreground">Instruction</p>
        <p className="mt-1 whitespace-pre-wrap text-sm">{revision.instruction}</p>
      </div>

      <div>
        <p className="text-xs text-muted-foreground">Secret references</p>
        <p className="mt-1 text-sm">
          {listOrNone(revision.secret_refs)}
        </p>
        <p className="mt-1 text-xs text-muted-foreground">
          References only. The Gateway and Worker never return credential
          values.
        </p>
      </div>
    </div>
  )
}

export function ProfileDetailPage({
  client,
  worker,
  name,
}: {
  client: RuntimeProfileClient
  worker: string
  name: string
}) {
  const profile = useProfileQuery(client, worker, name)
  const revise = useReviseProfile(client)
  const validate = useValidateProfile(client)
  const activate = useActivateProfile(client)
  const deactivate = useDeactivateProfile(client)
  const remove = useDeleteProfile(client)
  const [notice, setNotice] = useState<string | null>(null)
  const [confirmDelete, setConfirmDelete] = useState(false)
  const [deleted, setDeleted] = useState(false)

  if (deleted) {
    return (
      <div className="flex flex-col gap-4">
        <BackToProfiles worker={worker} />
        <p role="status" className="text-sm text-positive">
          Profile {name} deleted.
        </p>
      </div>
    )
  }

  if (profile.isPending) {
    return (
      <p className="flex items-center gap-2 text-sm text-muted-foreground">
        <Spinner /> Loading profile {name}.
      </p>
    )
  }

  if (profile.isError) {
    return (
      <div className="flex flex-col gap-4">
        <BackToProfiles worker={worker} />
        <ProfileErrorAlert error={profile.error} />
      </div>
    )
  }

  const entry = profile.data
  const latest = latestRevision(entry)
  const target = latest?.revision ?? 0
  const active = entry.active_revision > 0
  const pending =
    revise.isPending ||
    validate.isPending ||
    activate.isPending ||
    deactivate.isPending ||
    remove.isPending
  const actionError =
    revise.error ?? validate.error ?? activate.error ?? deactivate.error ?? remove.error

  return (
    <div className="flex flex-col gap-6">
      <div>
        <BackToProfiles worker={worker} />
        <div className="mt-3 flex flex-wrap items-center gap-2">
          <h1 className="font-heading text-2xl font-semibold tracking-tight sm:text-3xl">
            {entry.name}
          </h1>
          <Badge variant={active ? "default" : "outline"}>
            {active ? "Active" : "Inactive"}
          </Badge>
          <Badge variant="secondary">
            {entry.revisions.length} revision
            {entry.revisions.length === 1 ? "" : "s"}
          </Badge>
        </div>
        <p className="mt-2 text-sm text-muted-foreground">
          Worker {worker}
          {" · "}
          {active ? (
            <span>Active revision {entry.active_revision}</span>
          ) : (
            <span>No revision is active</span>
          )}
        </p>
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <Button
          variant="outline"
          disabled={pending || target === 0}
          onClick={() =>
            validate.mutate(
              { worker, name: entry.name, request: { revision: target } },
              {
                onSuccess: (result) =>
                  setNotice(
                    result.active_revision === target
                      ? `Validated revision ${target}.`
                      : `Validation passed for revision ${target}.`,
                  ),
              },
            )
          }
        >
          {validate.isPending ? (
            <Spinner />
          ) : (
            <CheckCircle2Icon data-icon="inline-start" aria-hidden="true" />
          )}
          Validate revision
        </Button>
        <Button
          disabled={pending || target === 0}
          onClick={() =>
            activate.mutate(
              { worker, name: entry.name, request: { revision: target } },
              {
                onSuccess: () =>
                  setNotice(`Activated revision ${target}.`),
              },
            )
          }
        >
          {activate.isPending ? (
            <Spinner />
          ) : (
            <ShieldCheckIcon data-icon="inline-start" aria-hidden="true" />
          )}
          Activate revision
        </Button>
        <Button
          variant="destructive"
          disabled={pending || !active}
          onClick={() =>
            deactivate.mutate(
              { worker, name: entry.name },
              {
                onSuccess: () =>
                  setNotice("Deactivated. No revision is active."),
              },
            )
          }
        >
          {deactivate.isPending ? (
            <Spinner />
          ) : (
            <ShieldOffIcon data-icon="inline-start" aria-hidden="true" />
          )}
          Deactivate profile
        </Button>
        <Button
          variant="destructive"
          disabled={pending || active}
          title={active ? "Deactivate the profile before deleting it." : undefined}
          onClick={() => setConfirmDelete(true)}
        >
          <Trash2Icon data-icon="inline-start" aria-hidden="true" />
          Delete profile
        </Button>
      </div>

      {notice ? (
        <p role="status" className="text-sm text-positive">
          {notice}
        </p>
      ) : null}
      {actionError ? <ProfileErrorAlert error={actionError} /> : null}

      <div className="grid gap-4 lg:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle>Latest revision</CardTitle>
            <CardDescription>
              Revision {target} is the newest immutable revision. Activation is
              explicit.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <RevisionMetadata revision={latest} />
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Revision history</CardTitle>
            <CardDescription>
              Older revisions stay immutable. Revisions never overwrite one
              another.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Revision</TableHead>
                  <TableHead>Model</TableHead>
                  <TableHead>Composition</TableHead>
                  <TableHead>Created</TableHead>
                  <TableHead>State</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {entry.revisions.map((revision) => (
                  <TableRow key={revision.revision}>
                    <TableCell>{revision.revision}</TableCell>
                    <TableCell>{revision.model}</TableCell>
                    <TableCell>{revision.composition ?? "single"}</TableCell>
                    <TableCell>
                      {revision.created_at
                        ? new Date(revision.created_at).toLocaleString()
                        : "Unknown"}
                    </TableCell>
                    <TableCell>
                      {revision.revision === entry.active_revision ? (
                        <Badge>Active</Badge>
                      ) : revision.revision === target ? (
                        <Badge variant="outline">Latest</Badge>
                      ) : (
                        <span className="text-xs text-muted-foreground">
                          Historical
                        </span>
                      )}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </CardContent>
        </Card>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Revise profile</CardTitle>
          <CardDescription>
            A revision uses the observed revision count. A stale form is
            rejected by the Worker rather than overwriting another writer.
          </CardDescription>
        </CardHeader>
        <CardContent>
          {latest ? (
            <ProfileForm
              key={latest.revision}
              mode="revise"
              profile={entry}
              revision={latest}
              pending={revise.isPending}
              onRevise={(request) =>
                revise.mutate(
                  { worker, name: entry.name, request },
                  {
                    onSuccess: (result) =>
                      setNotice(
                        `Revision ${result.revisions.length} created.`,
                      ),
                  },
                )
              }
            />
          ) : (
            <p className="text-sm text-muted-foreground">
              This profile has no revision to revise.
            </p>
          )}
        </CardContent>
      </Card>

      <Dialog
        open={confirmDelete}
        onOpenChange={(open) => {
          if (!open && !remove.isPending) {
            setConfirmDelete(false)
          }
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Delete profile</DialogTitle>
            <DialogDescription>
              Deleting removes {entry.name} from Worker {worker}. This cannot be
              undone; the profile must already be inactive.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              disabled={remove.isPending}
              onClick={() => setConfirmDelete(false)}
            >
              Cancel
            </Button>
            <Button
              variant="destructive"
              disabled={remove.isPending}
              onClick={() =>
                remove.mutate(
                  { worker, name: entry.name },
                  {
                    onSuccess: () => {
                      setConfirmDelete(false)
                      setDeleted(true)
                    },
                  },
                )
              }
            >
              {remove.isPending ? <Spinner /> : null}
              Delete permanently
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}

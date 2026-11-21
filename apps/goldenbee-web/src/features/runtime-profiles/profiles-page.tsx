import { useState } from "react"
import { Link as LinkIcon, PlusIcon } from "lucide-react"

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
import { navigate } from "@/app/router"
import type { GatewayWorker, Profile } from "@goldenbee/sdk"

import type { RuntimeProfileClient } from "./profile-client"
import { ProfileErrorAlert } from "./profile-error-alert"
import { ProfileForm } from "./profile-form-fields"
import { latestRevision } from "./profile-form"
import { useCreateProfile, useProfilesQuery } from "./profile-queries"

function profileModel(profile: Profile): string {
  const revision = latestRevision(profile)
  return revision?.model ?? "Unknown"
}

function profileComposition(profile: Profile): string {
  return latestRevision(profile)?.composition ?? "single"
}

function countLabel(count: number, singular: string, plural = `${singular}s`) {
  return `${count} ${count === 1 ? singular : plural}`
}

export function ProfilesPage({
  client,
  workers,
  worker,
}: {
  client: RuntimeProfileClient
  workers: GatewayWorker[]
  worker?: string
}) {
  const fallback =
    workers.find((candidate) => candidate.reachable)?.id ?? workers[0]?.id ?? ""
  const [selected, setSelected] = useState(worker ?? fallback)
  const [dialogOpen, setDialogOpen] = useState(false)
  const activeWorker = worker ?? selected
  const profiles = useProfilesQuery(client, activeWorker)
  const create = useCreateProfile(client)

  function selectWorker(next: string) {
    setSelected(next)
    navigate({ name: "profiles", worker: next })
  }

  return (
    <div className="flex flex-col gap-6">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="max-w-2xl">
          <h1 className="font-heading text-2xl font-semibold tracking-tight sm:text-3xl">
            Runtime Profiles
          </h1>
          <p className="mt-2 text-sm text-muted-foreground">
            Profiles are owned by a Worker. Revisions are immutable; validation
            and activation happen on the Worker, and the Gateway forwards them
            under the caller's scope.
          </p>
        </div>
        <Button
          onClick={() => {
            create.reset()
            setDialogOpen(true)
          }}
          disabled={workers.length === 0}
        >
          <PlusIcon data-icon="inline-start" aria-hidden="true" />
          New profile
        </Button>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Worker catalog</CardTitle>
          <CardDescription>
            Choose a visible Worker. A profile is never inferred from a
            different Worker.
          </CardDescription>
        </CardHeader>
        <CardContent className="flex flex-col gap-4">
          {workers.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              No Worker is visible to this caller.
            </p>
          ) : (
            <div className="max-w-sm">
              <label
                htmlFor="profile-worker"
                className="text-xs text-muted-foreground"
              >
                Worker
              </label>
              <select
                id="profile-worker"
                value={activeWorker}
                onChange={(event) => selectWorker(event.target.value)}
                className="mt-1 h-8 w-full rounded-2xl border border-transparent bg-input/50 px-2.5 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/30"
              >
                {workers.map((candidate) => (
                  <option key={candidate.id} value={candidate.id}>
                    {candidate.id} · {candidate.package}/{candidate.instance}
                    {candidate.reachable ? "" : " · unreachable"}
                  </option>
                ))}
              </select>
            </div>
          )}

          {workers.length === 0 ? null : profiles.isPending ? (
            <p className="flex items-center gap-2 text-sm text-muted-foreground">
              <Spinner /> Loading profiles.
            </p>
          ) : profiles.isError ? (
            <ProfileErrorAlert error={profiles.error} />
          ) : profiles.data.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              No profiles on this Worker.
            </p>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Name</TableHead>
                  <TableHead>Active revision</TableHead>
                  <TableHead>Model</TableHead>
                  <TableHead>Composition</TableHead>
                  <TableHead>Skills</TableHead>
                  <TableHead>MCP</TableHead>
                  <TableHead>State</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {profiles.data.map((profile) => {
                  const revision = latestRevision(profile)
                  return (
                    <TableRow key={profile.name}>
                      <TableCell className="font-medium">
                        <a
                          href={`#/profiles/${encodeURIComponent(activeWorker)}/${encodeURIComponent(profile.name)}`}
                          className="inline-flex items-center gap-1 underline-offset-4 hover:underline"
                        >
                          <LinkIcon
                            className="size-3 text-muted-foreground"
                            aria-hidden="true"
                          />
                          {profile.name}
                        </a>
                      </TableCell>
                      <TableCell>
                        {profile.active_revision > 0
                          ? profile.active_revision
                          : "—"}
                      </TableCell>
                      <TableCell>{profileModel(profile)}</TableCell>
                      <TableCell>{profileComposition(profile)}</TableCell>
                      <TableCell>
                        {countLabel(revision?.skills?.length ?? 0, "skill")}
                      </TableCell>
                      <TableCell>
                        {countLabel(revision?.mcp?.length ?? 0, "MCP server")}
                      </TableCell>
                      <TableCell>
                        <Badge
                          variant={
                            profile.active_revision > 0 ? "default" : "outline"
                          }
                        >
                          {profile.active_revision > 0 ? "Active" : "Inactive"}
                        </Badge>
                      </TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>

      <Dialog
        open={dialogOpen}
        onOpenChange={(open) => {
          setDialogOpen(open)
          if (!open) {
            create.reset()
          }
        }}
      >
        <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-3xl">
          <DialogHeader>
            <DialogTitle>Create Runtime Profile</DialogTitle>
            <DialogDescription>
              The Worker assigns revision 1 and leaves the profile inactive.
              Validate and activate it from the profile page.
            </DialogDescription>
          </DialogHeader>
          {create.error ? <ProfileErrorAlert error={create.error} /> : null}
          <ProfileForm
            key={`${activeWorker}-${create.data?.name ?? "new"}-${create.data?.revisions.length ?? 0}`}
            mode="create"
            pending={create.isPending}
            onCreate={(request) =>
              create.mutate(
                { worker: activeWorker, request },
                {
                  onSuccess: () => setDialogOpen(false),
                },
              )
            }
          />
        </DialogContent>
      </Dialog>
    </div>
  )
}

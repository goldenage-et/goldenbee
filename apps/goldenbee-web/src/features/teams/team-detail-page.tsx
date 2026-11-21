import { useState } from "react"
import {
  ArrowLeftIcon,
  CheckCircle2Icon,
  ShieldCheckIcon,
  ShieldOffIcon,
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
import { Spinner } from "@/components/ui/spinner"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"

import type { TeamAdminClient } from "./team-client"
import { TeamErrorAlert } from "./team-error-alert"
import { TeamForm } from "./team-form-fields"
import {
  useActivateTeam,
  useDeactivateTeam,
  useTeamQuery,
  useUpdateTeam,
  useValidateTeam,
} from "./team-queries"

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

function BackToTeams() {
  return (
    <a
      href="#/teams"
      className="inline-flex w-fit items-center gap-1 text-sm text-muted-foreground underline-offset-4 hover:underline"
    >
      <ArrowLeftIcon className="size-3.5" aria-hidden="true" />
      All teams
    </a>
  )
}

export function TeamDetailPage({
  client,
  name,
}: {
  client: TeamAdminClient
  name: string
}) {
  const team = useTeamQuery(client, name)
  const update = useUpdateTeam(client)
  const validate = useValidateTeam(client)
  const activate = useActivateTeam(client)
  const deactivate = useDeactivateTeam(client)
  const [notice, setNotice] = useState<string | null>(null)

  if (team.isPending) {
    return (
      <p className="flex items-center gap-2 text-sm text-muted-foreground">
        <Spinner /> Loading team {name}.
      </p>
    )
  }

  if (team.isError) {
    return (
      <div className="flex flex-col gap-4">
        <BackToTeams />
        <TeamErrorAlert error={team.error} />
      </div>
    )
  }

  const entry = team.data
  const pending =
    update.isPending ||
    validate.isPending ||
    activate.isPending ||
    deactivate.isPending
  const actionError =
    update.error ?? validate.error ?? activate.error ?? deactivate.error

  return (
    <div className="flex flex-col gap-6">
      <div>
        <BackToTeams />
        <div className="mt-3 flex flex-wrap items-center gap-2">
          <h1 className="font-heading text-2xl font-semibold tracking-tight sm:text-3xl">
            {entry.name}
          </h1>
          <Badge variant={entry.active ? "default" : "outline"}>
            {entry.active ? "Active" : "Inactive"}
          </Badge>
          <span className="text-xs text-muted-foreground">
            Version {entry.version}
          </span>
        </div>
        <p className="mt-2 max-w-2xl text-sm text-muted-foreground">
          Validation and activation recheck the live roster. Deactivation stops
          new admissions; executions already running keep their frozen version.
        </p>
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <Button
          variant="outline"
          disabled={pending}
          onClick={() =>
            validate.mutate(
              { name: entry.name, request: { version: entry.version } },
              {
                onSuccess: (result) =>
                  setNotice(`Validation passed for version ${result.version}.`),
              },
            )
          }
        >
          {validate.isPending ? (
            <Spinner />
          ) : (
            <CheckCircle2Icon data-icon="inline-start" aria-hidden="true" />
          )}
          Validate team
        </Button>
        {entry.active ? (
          <Button
            variant="destructive"
            disabled={pending}
            onClick={() =>
              deactivate.mutate(
                { name: entry.name, request: { version: entry.version } },
                {
                  onSuccess: (result) =>
                    setNotice(
                      `Team deactivated at version ${result.version}.`,
                    ),
                },
              )
            }
          >
            {deactivate.isPending ? (
              <Spinner />
            ) : (
              <ShieldOffIcon data-icon="inline-start" aria-hidden="true" />
            )}
            Deactivate team
          </Button>
        ) : (
          <Button
            disabled={pending}
            onClick={() =>
              activate.mutate(
                { name: entry.name, request: { version: entry.version } },
                {
                  onSuccess: (result) =>
                    setNotice(`Team activated at version ${result.version}.`),
                },
              )
            }
          >
            {activate.isPending ? (
              <Spinner />
            ) : (
              <ShieldCheckIcon data-icon="inline-start" aria-hidden="true" />
            )}
            Activate team
          </Button>
        )}
      </div>

      {notice ? (
        <p role="status" className="text-sm text-positive">
          {notice}
        </p>
      ) : null}
      {actionError ? <TeamErrorAlert error={actionError} /> : null}

      <div className="grid gap-4 lg:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle>Roster</CardTitle>
            <CardDescription>
              Members and their bounded capabilities for this version.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Worker</TableHead>
                  <TableHead>Role</TableHead>
                  <TableHead>Capabilities</TableHead>
                  <TableHead>Max depth</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {entry.members.map((member) => (
                  <TableRow key={member.worker}>
                    <TableCell className="font-medium">
                      {member.worker}
                      {member.worker === entry.master ? (
                        <Badge variant="secondary" className="ml-2">
                          Master
                        </Badge>
                      ) : null}
                    </TableCell>
                    <TableCell>{member.role || "—"}</TableCell>
                    <TableCell>
                      {member.capabilities.length > 0
                        ? member.capabilities.join(", ")
                        : "None"}
                    </TableCell>
                    <TableCell>
                      {member.max_depth && member.max_depth > 0
                        ? member.max_depth
                        : "Team default"}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Identity and limits</CardTitle>
            <CardDescription>
              Registry-owned roster facts. Limits apply to delegated work.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <dl className="grid grid-cols-2 gap-4">
              <Definition label="Namespace">
                {entry.namespace || "Unknown"}
              </Definition>
              <Definition label="Master">
                <code className="rounded bg-muted px-1.5 py-0.5 text-xs">
                  {entry.master}
                </code>
              </Definition>
              <Definition label="Locality">
                {entry.locality ?? "Any"}
              </Definition>
              <Definition label="Members">
                {entry.members.length} member
                {entry.members.length === 1 ? "" : "s"}
              </Definition>
              <Definition label="Max depth">
                {entry.limits?.max_depth ?? "Gateway default"}
              </Definition>
              <Definition label="Max fanout">
                {entry.limits?.max_fanout ?? "Subworker count"}
              </Definition>
              <Definition label="Max model calls">
                {entry.limits?.max_model_calls ?? "Unset"}
              </Definition>
              <Definition label="Max tool calls">
                {entry.limits?.max_tool_calls ?? "Unset"}
              </Definition>
              <Definition label="Created">
                {entry.created_at
                  ? new Date(entry.created_at).toLocaleString()
                  : "Unknown"}
              </Definition>
              <Definition label="Last change">
                {entry.updated_at
                  ? new Date(entry.updated_at).toLocaleString()
                  : "Unknown"}
              </Definition>
            </dl>
          </CardContent>
        </Card>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Revise roster</CardTitle>
          <CardDescription>
            A revision requires the version observed above. A stale form is
            rejected by the Gateway instead of overwriting another writer.
          </CardDescription>
        </CardHeader>
        <CardContent className="flex flex-col gap-4">
          <TeamForm
            key={entry.version}
            mode="revise"
            team={entry}
            pending={update.isPending}
            onRevise={(request) =>
              update.mutate(
                { name: entry.name, request },
                {
                  onSuccess: (result) =>
                    setNotice(`Roster revised to version ${result.version}.`),
                },
              )
            }
          />
        </CardContent>
      </Card>
    </div>
  )
}

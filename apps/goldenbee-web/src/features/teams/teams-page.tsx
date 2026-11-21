import { Link as LinkIcon } from "lucide-react"

import {
  Alert,
  AlertDescription,
  AlertTitle,
} from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
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
import type { Team } from "@goldenbee/sdk"

import type { TeamAdminClient } from "./team-client"
import { TeamErrorAlert } from "./team-error-alert"
import { TeamForm } from "./team-form-fields"
import { useCreateTeam, useTeamsQuery } from "./team-queries"

function TeamState({ team }: { team: Team }) {
  return (
    <Badge variant={team.active ? "default" : "outline"}>
      {team.active ? "Active" : "Inactive"}
    </Badge>
  )
}

export function TeamsPage({ client }: { client: TeamAdminClient }) {
  const teams = useTeamsQuery(client)
  const create = useCreateTeam(client)

  return (
    <div className="flex flex-col gap-6">
      <div className="max-w-2xl">
        <h1 className="font-heading text-2xl font-semibold tracking-tight sm:text-3xl">
          Teams
        </h1>
        <p className="mt-2 text-sm text-muted-foreground">
          Teams are versioned Gateway rosters. A new roster starts inactive;
          validation rechecks live Workers and capabilities, and only an active
          version admits new work.
        </p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Team rosters</CardTitle>
          <CardDescription>
            Identity, version, Master, membership, locality, and admission state
            come from the Gateway registry.
          </CardDescription>
        </CardHeader>
        <CardContent>
          {teams.isPending ? (
            <p className="flex items-center gap-2 text-sm text-muted-foreground">
              <Spinner /> Loading teams.
            </p>
          ) : teams.isError ? (
            <TeamErrorAlert error={teams.error} />
          ) : teams.data.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              No teams configured.
            </p>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Name</TableHead>
                  <TableHead>Version</TableHead>
                  <TableHead>Master</TableHead>
                  <TableHead>Members</TableHead>
                  <TableHead>Locality</TableHead>
                  <TableHead>State</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {teams.data.map((team) => (
                  <TableRow key={team.name}>
                    <TableCell className="font-medium">
                      <a
                        href={`#/teams/${encodeURIComponent(team.name)}`}
                        className="inline-flex items-center gap-1 underline-offset-4 hover:underline"
                      >
                        <LinkIcon
                          className="size-3 text-muted-foreground"
                          aria-hidden="true"
                        />
                        {team.name}
                      </a>
                    </TableCell>
                    <TableCell>{team.version}</TableCell>
                    <TableCell>
                      <code className="rounded bg-muted px-1.5 py-0.5 text-xs">
                        {team.master}
                      </code>
                    </TableCell>
                    <TableCell>
                      {team.members.length} member
                      {team.members.length === 1 ? "" : "s"}
                    </TableCell>
                    <TableCell>{team.locality ?? "Any"}</TableCell>
                    <TableCell>
                      <TeamState team={team} />
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Create team</CardTitle>
          <CardDescription>
            The Gateway creates version 1 inactive. Validate it on the detail
            page before activation.
          </CardDescription>
        </CardHeader>
        <CardContent className="flex flex-col gap-4">
          {create.data ? (
            <Alert>
              <AlertTitle>Version {create.data.version} created</AlertTitle>
              <AlertDescription>
                The roster is inactive. Open it to validate and activate.
              </AlertDescription>
            </Alert>
          ) : null}
          {create.error ? <TeamErrorAlert error={create.error} /> : null}
          <TeamForm
            key={create.data ? `${create.data.name}-${create.data.version}` : "new"}
            mode="create"
            pending={create.isPending}
            onCreate={(request) => create.mutate(request)}
          />
        </CardContent>
      </Card>
    </div>
  )
}

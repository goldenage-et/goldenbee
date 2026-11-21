import { useState } from "react"
import { ServerIcon, ServerPlusIcon } from "lucide-react"

import { routeHref } from "@/app/router"
import {
  Alert,
  AlertDescription,
  AlertTitle,
} from "@/components/ui/alert"
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
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import type {
  GatewayClient,
  HostInstanceView,
  HostOperationOutcome,
} from "@goldenbee/sdk"

import { DeployInstanceDialog } from "./deploy-instance-dialog"
import { HostErrorNotice, HostLoading } from "./host-notice"
import { newOperationId } from "./operation-id"
import { useInstances, usePackages } from "./use-host"

function CompatibilityBadge({ view }: { view: HostInstanceView }) {
  if (view.compatibility.compatible) {
    return <Badge variant="outline">Compatible</Badge>
  }
  return (
    <div className="flex flex-col gap-1">
      <Badge variant="destructive">Incompatible</Badge>
      {view.compatibility.issues.map((issue) => (
        <span key={issue} className="text-xs text-destructive">
          {issue}
        </span>
      ))}
    </div>
  )
}

function LifecycleBadge({ lifecycle }: { lifecycle: string }) {
  const destructive = lifecycle === "failed" || lifecycle === "quarantined"
  return (
    <Badge variant={destructive ? "destructive" : "outline"}>
      {lifecycle}
    </Badge>
  )
}

function InstanceTable({ instances }: { instances: HostInstanceView[] }) {
  if (instances.length === 0) {
    return (
      <Empty>
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <ServerIcon aria-hidden="true" />
          </EmptyMedia>
          <EmptyTitle>No instances</EmptyTitle>
          <EmptyDescription>
            Deploy an instance from an installed package digest.
          </EmptyDescription>
        </EmptyHeader>
      </Empty>
    )
  }

  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Instance</TableHead>
          <TableHead>Lifecycle</TableHead>
          <TableHead>Health</TableHead>
          <TableHead>Endpoint</TableHead>
          <TableHead>Digest</TableHead>
          <TableHead>Compatibility</TableHead>
          <TableHead className="text-right">Detail</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {instances.map((view) => (
          <TableRow key={`${view.host}:${view.instance}`}>
            <TableCell>
              <div className="flex flex-col">
                <span className="font-medium">{view.instance}</span>
                <span className="text-xs text-muted-foreground">
                  {view.package} {view.version}
                </span>
              </div>
            </TableCell>
            <TableCell>
              <LifecycleBadge lifecycle={view.lifecycle} />
            </TableCell>
            <TableCell>{view.process_health}</TableCell>
            <TableCell className="font-mono text-xs">
              {view.endpoint ?? "—"}
            </TableCell>
            <TableCell className="font-mono text-xs break-all">
              {view.artifact_digest}
            </TableCell>
            <TableCell>
              <CompatibilityBadge view={view} />
            </TableCell>
            <TableCell className="text-right">
              <a
                className="text-sm underline underline-offset-4 hover:text-foreground"
                href={routeHref({
                  name: "instance",
                  instance: view.instance,
                })}
              >
                Inspect
              </a>
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  )
}

function provisionTitle(outcome: HostOperationOutcome): string {
  if (outcome.status !== "succeeded") {
    return `Deployment ${outcome.status}`
  }
  return outcome.replayed ? "Deployment already recorded" : "Instance deployed"
}

export function InstancesPage({ client }: { client: GatewayClient }) {
  const instances = useInstances(client)
  const packages = usePackages(client)
  const [deployOperationId, setDeployOperationId] = useState<string | null>(null)
  const [outcome, setOutcome] = useState<HostOperationOutcome | null>(null)

  const installedPackages = packages.data?.packages ?? []

  return (
    <div className="flex flex-col gap-6">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div>
          <h1 className="font-heading text-2xl font-semibold tracking-tight sm:text-3xl">
            Local instances
          </h1>
          <p className="mt-2 max-w-2xl text-sm text-muted-foreground">
            Instances the Gateway runs on its own host. Lifecycle, process
            health, endpoint, artifact digest, and compatibility are host
            observations, not browser state.
          </p>
        </div>
        <Button
          onClick={() => setDeployOperationId(newOperationId("provision"))}
          disabled={installedPackages.length === 0}
        >
          <ServerPlusIcon data-icon="inline-start" aria-hidden="true" />
          Deploy instance
        </Button>
      </div>

      {outcome ? (
        <Alert>
          <AlertTitle>{provisionTitle(outcome)}</AlertTitle>
          <AlertDescription>
            <p>
              {outcome.instance} · operation {outcome.operation_id}
            </p>
            {outcome.recovery?.quarantine || outcome.status === "quarantined" ? (
              <p>The host quarantined this operation. Inspect it before retrying.</p>
            ) : null}
          </AlertDescription>
        </Alert>
      ) : null}

      {installedPackages.length === 0 && !packages.isPending ? (
        <Alert>
          <AlertTitle>No package digest available</AlertTitle>
          <AlertDescription>
            Install a verified package first. A deployment selects the package
            artifact digest; it does not accept an arbitrary file.
          </AlertDescription>
        </Alert>
      ) : null}

      <Card>
        <CardHeader>
          <CardTitle>Instances</CardTitle>
          <CardDescription>
            Start, stop, restart, upgrade, roll back, and read logs from an
            instance detail page.
          </CardDescription>
        </CardHeader>
        <CardContent>
          {instances.isPending ? (
            <HostLoading label="Loading instances…" />
          ) : instances.isError ? (
            <HostErrorNotice
              error={instances.error}
              onRetry={() => void instances.refetch()}
              retryLabel="Reload instances"
            />
          ) : (
            <InstanceTable instances={instances.data} />
          )}
        </CardContent>
      </Card>

      {deployOperationId ? (
        <DeployInstanceDialog
          key={deployOperationId}
          client={client}
          operationId={deployOperationId}
          packages={installedPackages}
          onClose={() => setDeployOperationId(null)}
          onDeployed={(result) => {
            setOutcome(result)
            setDeployOperationId(null)
          }}
        />
      ) : null}
    </div>
  )
}

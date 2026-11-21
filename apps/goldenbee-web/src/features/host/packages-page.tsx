import { PackageIcon } from "lucide-react"

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
import type { GatewayClient, PackageSummary } from "@goldenbee/sdk"

import { HostErrorNotice, HostLoading } from "./host-notice"
import { InstallPackageForm } from "./install-package-form"
import { usePackages } from "./use-host"

function PackageTable({ packages }: { packages: PackageSummary[] }) {
  if (packages.length === 0) {
    return (
      <Empty>
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <PackageIcon aria-hidden="true" />
          </EmptyMedia>
          <EmptyTitle>No packages installed</EmptyTitle>
          <EmptyDescription>
            Install a verified package by URL before deploying an instance.
          </EmptyDescription>
        </EmptyHeader>
      </Empty>
    )
  }

  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Package</TableHead>
          <TableHead>Version</TableHead>
          <TableHead>Digest</TableHead>
          <TableHead>Signer</TableHead>
          <TableHead>Target</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {packages.map((entry) => (
          <TableRow key={`${entry.package}:${entry.artifact_digest}`}>
            <TableCell className="font-medium">{entry.package}</TableCell>
            <TableCell>{entry.version}</TableCell>
            <TableCell className="font-mono text-xs break-all">
              {entry.artifact_digest}
            </TableCell>
            <TableCell>{entry.signer}</TableCell>
            <TableCell className="font-mono text-xs">{entry.target}</TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  )
}

export function PackagesPage({ client }: { client: GatewayClient }) {
  const packages = usePackages(client)

  return (
    <div className="flex flex-col gap-6">
      <div>
        <h1 className="font-heading text-2xl font-semibold tracking-tight sm:text-3xl">
          Packages
        </h1>
        <p className="mt-2 max-w-2xl text-sm text-muted-foreground">
          Verified packages the Gateway can deploy on its own host. Every
          package reports its version, artifact digest, signer, and target.
        </p>
      </div>

      <InstallPackageForm client={client} />

      <Card>
        <CardHeader>
          <CardTitle>Installed packages</CardTitle>
          <CardDescription>
            The digest is the deployment identity. Deploy an instance from one
            of these digests.
          </CardDescription>
        </CardHeader>
        <CardContent>
          {packages.isPending ? (
            <HostLoading label="Loading packages…" />
          ) : packages.isError ? (
            <HostErrorNotice
              error={packages.error}
              onRetry={() => void packages.refetch()}
              retryLabel="Reload packages"
            />
          ) : (
            <PackageTable packages={packages.data.packages} />
          )}
        </CardContent>
      </Card>
    </div>
  )
}

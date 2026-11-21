import { Link } from "lucide-react"

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
import { ProviderCreateDialog } from "@/features/providers/provider-create-dialog"
import { ProviderErrorAlert } from "@/features/providers/provider-error-alert"
import type { ProviderAdminClient } from "@/features/providers/provider-client"
import { useProvidersQuery } from "@/features/providers/provider-queries"

function stateBadges(provider: {
  active: boolean
  ready: boolean
}): React.ReactNode {
  return (
    <div className="flex flex-wrap items-center gap-1">
      <Badge variant={provider.active ? "default" : "outline"}>
        {provider.active ? "Active" : "Disabled"}
      </Badge>
      <Badge variant={provider.ready ? "secondary" : "destructive"}>
        {provider.ready ? "Ready" : "Not ready"}
      </Badge>
    </div>
  )
}

export function ProvidersScreen({ client }: { client: ProviderAdminClient }) {
  const providers = useProvidersQuery(client)

  return (
    <div className="flex flex-col gap-6">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="max-w-2xl">
          <h1 className="font-heading text-2xl font-semibold tracking-tight sm:text-3xl">
            Model providers
          </h1>
          <p className="mt-2 text-sm text-muted-foreground">
            The Gateway catalog owns provider metadata, model discovery, and
            encrypted credentials. Open a provider to update metadata, refresh
            models, or rotate its key.
          </p>
        </div>
        <ProviderCreateDialog client={client} />
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Provider catalog</CardTitle>
          <CardDescription>
            Readiness reflects a discovered or explicit default model and a
            current credential. Disabled providers receive no new work.
          </CardDescription>
        </CardHeader>
        <CardContent>
          {providers.isPending ? (
            <p className="flex items-center gap-2 text-sm text-muted-foreground">
              <Spinner /> Loading providers.
            </p>
          ) : providers.isError ? (
            <ProviderErrorAlert error={providers.error} />
          ) : providers.data.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              No providers configured.
            </p>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Provider</TableHead>
                  <TableHead>Adapter</TableHead>
                  <TableHead>Default model</TableHead>
                  <TableHead>Locality</TableHead>
                  <TableHead>Residency</TableHead>
                  <TableHead>Allowed models</TableHead>
                  <TableHead>State</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {providers.data.map((provider) => {
                  const models =
                    provider.allowed_models && provider.allowed_models.length > 0
                      ? provider.allowed_models
                      : provider.models
                  return (
                    <TableRow key={provider.id}>
                      <TableCell className="font-medium">
                        <a
                          href={`#/providers/${encodeURIComponent(provider.id)}`}
                          className="inline-flex items-center gap-1 underline-offset-4 hover:underline"
                        >
                          <Link className="size-3 text-muted-foreground" aria-hidden="true" />
                          {provider.id}
                        </a>
                      </TableCell>
                      <TableCell>
                        <code className="rounded bg-muted px-1.5 py-0.5 text-xs">
                          {provider.adapter}
                        </code>
                      </TableCell>
                      <TableCell>
                        {provider.model || (
                          <span className="text-muted-foreground">Not selected</span>
                        )}
                      </TableCell>
                      <TableCell>{provider.local ? "Local" : "Remote"}</TableCell>
                      <TableCell>
                        <span className="text-xs text-muted-foreground">
                          residency: {provider.residency}
                        </span>
                      </TableCell>
                      <TableCell className="max-w-72">
                        {models.length > 0 ? (
                          <span className="text-xs">{models.join(", ")}</span>
                        ) : (
                          <span className="text-xs text-muted-foreground">
                            None discovered
                          </span>
                        )}
                      </TableCell>
                      <TableCell>
                        <div className="flex flex-col gap-1">
                          {stateBadges(provider)}
                          {provider.discovery_error_code ? (
                            <code className="text-[10px] text-muted-foreground">
                              {provider.discovery_error_code}
                            </code>
                          ) : null}
                        </div>
                      </TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>
    </div>
  )
}

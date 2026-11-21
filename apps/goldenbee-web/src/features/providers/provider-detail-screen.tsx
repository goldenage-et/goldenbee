import {
  AlertTriangleIcon,
  ArrowLeftIcon,
  KeyRoundIcon,
  RefreshCwIcon,
  ShieldOffIcon,
} from "lucide-react"

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
import { Spinner } from "@/components/ui/spinner"
import { ProviderCredentialDialog } from "@/features/providers/provider-credential-dialog"
import { ProviderEditForm } from "@/features/providers/provider-edit-form"
import { ProviderErrorAlert } from "@/features/providers/provider-error-alert"
import type { ProviderAdminClient } from "@/features/providers/provider-client"
import {
  useDiscoverProvider,
  useProviderQuery,
  useRevokeProviderCredential,
  useUpdateProvider,
} from "@/features/providers/provider-queries"

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

function modelList(models: string[]): React.ReactNode {
  if (models.length === 0) {
    return <span className="text-muted-foreground">None discovered</span>
  }
  return <span>{models.join(", ")}</span>
}

export function ProviderDetailScreen({
  client,
  id,
}: {
  client: ProviderAdminClient
  id: string
}) {
  const provider = useProviderQuery(client, id)
  const update = useUpdateProvider(client)
  const discovery = useDiscoverProvider(client)
  const revoke = useRevokeProviderCredential(client)

  if (provider.isPending) {
    return (
      <p className="flex items-center gap-2 text-sm text-muted-foreground">
        <Spinner /> Loading provider {id}.
      </p>
    )
  }

  if (provider.isError) {
    return (
      <div className="flex flex-col gap-4">
        <a
          href="#/providers"
          className="inline-flex w-fit items-center gap-1 text-sm text-muted-foreground underline-offset-4 hover:underline"
        >
          <ArrowLeftIcon className="size-3.5" aria-hidden="true" />
          All providers
        </a>
        <ProviderErrorAlert error={provider.error} />
      </div>
    )
  }

  const entry = provider.data
  const allowedModels = entry.allowed_models ?? []

  return (
    <div className="flex flex-col gap-6">
      <div>
        <a
          href="#/providers"
          className="inline-flex w-fit items-center gap-1 text-sm text-muted-foreground underline-offset-4 hover:underline"
        >
          <ArrowLeftIcon className="size-3.5" aria-hidden="true" />
          All providers
        </a>
        <div className="mt-3 flex flex-wrap items-center gap-2">
          <h1 className="font-heading text-2xl font-semibold tracking-tight sm:text-3xl">
            {entry.id}
          </h1>
          <Badge variant={entry.active ? "default" : "outline"}>
            {entry.active ? "Active" : "Disabled"}
          </Badge>
          <Badge variant={entry.ready ? "secondary" : "destructive"}>
            {entry.ready ? "Ready" : "Not ready"}
          </Badge>
          <span className="text-xs text-muted-foreground">
            Revision {entry.revision}
          </span>
        </div>
        <p className="mt-2 max-w-2xl text-sm text-muted-foreground">
          {entry.local
            ? "This provider runs on a local endpoint."
            : "This provider is reached over the network."}{" "}
          Discovery and credential writes advance the revision; a stale form is
          rejected rather than overwritten.
        </p>
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <Button
          variant={entry.active ? "destructive" : "default"}
          disabled={update.isPending}
          onClick={() =>
            update.mutate({
              id: entry.id,
              request: { revision: entry.revision, active: !entry.active },
            })
          }
        >
          {entry.active ? (
            <ShieldOffIcon data-icon="inline-start" aria-hidden="true" />
          ) : null}
          {entry.active ? "Disable provider" : "Enable provider"}
        </Button>
      </div>

      {update.error ? <ProviderErrorAlert error={update.error} /> : null}

      {entry.discovery_error_code ? (
        <Alert variant="destructive">
          <AlertTriangleIcon aria-hidden="true" />
          <AlertTitle>Model discovery did not produce a ready provider</AlertTitle>
          <AlertDescription>
            <p>{entry.discovery_error}</p>
            <code className="mt-1 inline-block rounded bg-muted px-1.5 py-0.5 text-[11px]">
              {entry.discovery_error_code}
            </code>
          </AlertDescription>
        </Alert>
      ) : null}

      <div className="grid gap-4 lg:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle>Projection</CardTitle>
            <CardDescription>
              Non-secret metadata returned by the Gateway provider catalog.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <dl className="grid grid-cols-2 gap-4">
              <Definition label="Adapter">
                <code className="rounded bg-muted px-1.5 py-0.5 text-xs">
                  {entry.adapter}
                </code>
              </Definition>
              <Definition label="Default model">
                {entry.model || (
                  <span className="text-muted-foreground">Not selected</span>
                )}
              </Definition>
              <Definition label="Locality">
                {entry.local ? "Local" : "Remote"}
              </Definition>
              <Definition label="Residency">
                residency: {entry.residency}
              </Definition>
              <Definition label="Context window">
                {entry.context_window
                  ? entry.context_window.toLocaleString()
                  : "Not declared"}
              </Definition>
              <Definition label="Tool calling">
                {entry.tool_calling ? "Supported" : "Not supported"}
              </Definition>
              <Definition label="Modalities">
                {entry.modalities.length > 0
                  ? entry.modalities.join(", ")
                  : "None declared"}
              </Definition>
              <Definition label="Allowed Workers">
                {entry.allowed_workers.length > 0
                  ? entry.allowed_workers.join(", ")
                  : "None assigned"}
              </Definition>
              <Definition label="Allowed models">
                {allowedModels.length > 0
                  ? allowedModels.join(", ")
                  : "All discovered models"}
              </Definition>
              <Definition label="Models source">{entry.models_source}</Definition>
              <Definition label="Discovered at">
                {entry.discovered_at
                  ? new Date(entry.discovered_at).toLocaleString()
                  : "Never"}
              </Definition>
              <Definition label="Last change">
                {entry.updated_at
                  ? new Date(entry.updated_at).toLocaleString()
                  : "Unknown"}
              </Definition>
            </dl>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Model discovery</CardTitle>
            <CardDescription>
              A refresh asks the provider for its current model list and advances
              the provider revision.
            </CardDescription>
          </CardHeader>
          <CardContent className="flex flex-col gap-3">
            <div className="text-sm">
              <p className="text-xs text-muted-foreground">Discovered models</p>
              <p className="mt-0.5">{modelList(entry.models)}</p>
            </div>
            <div className="flex flex-wrap items-center gap-2">
              <Button
                variant="outline"
                disabled={discovery.isPending}
                onClick={() => discovery.mutate(entry.id)}
              >
                {discovery.isPending ? (
                  <Spinner />
                ) : (
                  <RefreshCwIcon data-icon="inline-start" aria-hidden="true" />
                )}
                Refresh models
              </Button>
              <span className="text-xs text-muted-foreground">
                {entry.models.length} model{entry.models.length === 1 ? "" : "s"}{" "}
                available
              </span>
            </div>
            {discovery.error ? (
              <ProviderErrorAlert error={discovery.error} />
            ) : null}
          </CardContent>
        </Card>
      </div>

      <ProviderEditForm
        key={entry.revision}
        provider={entry}
        pending={update.isPending}
        onSubmit={(request) => update.mutate({ id: entry.id, request })}
      />

      <Card>
        <CardHeader>
          <CardTitle>Credential</CardTitle>
          <CardDescription>
            Provider keys are write-only. The Gateway stores ciphertext and
            returns only a version and fingerprint.
          </CardDescription>
        </CardHeader>
        <CardContent className="flex flex-col gap-4">
          <dl className="grid grid-cols-2 gap-4">
            <Definition label="Credential version">
              {entry.credential_version
                ? `Version ${entry.credential_version}`
                : "None written"}
            </Definition>
            <Definition label="Fingerprint">
              {entry.credential_fingerprint ? (
                <code className="text-xs">{entry.credential_fingerprint}</code>
              ) : (
                <span className="text-muted-foreground">None</span>
              )}
            </Definition>
            <Definition label="Status">
              {entry.credential_revoked ? (
                <Badge variant="destructive">Revoked</Badge>
              ) : entry.credential_version ? (
                <Badge variant="secondary">Active</Badge>
              ) : (
                <Badge variant="outline">Not set</Badge>
              )}
            </Definition>
          </dl>

          {revoke.error ? <ProviderErrorAlert error={revoke.error} /> : null}

          <div className="flex flex-wrap items-center gap-2">
            <ProviderCredentialDialog client={client} provider={entry} />
            {entry.credential_version && !entry.credential_revoked ? (
              <Button
                variant="destructive"
                disabled={revoke.isPending}
                onClick={() => revoke.mutate(entry.id)}
              >
                {revoke.isPending ? <Spinner /> : null}
                <KeyRoundIcon data-icon="inline-start" aria-hidden="true" />
                Revoke credential
              </Button>
            ) : null}
          </div>
        </CardContent>
      </Card>
    </div>
  )
}

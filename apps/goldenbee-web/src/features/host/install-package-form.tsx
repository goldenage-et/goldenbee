import { useState } from "react"
import { PackagePlusIcon } from "lucide-react"

import {
  Alert,
  AlertDescription,
  AlertTitle,
} from "@/components/ui/alert"
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
import { Field, FieldDescription, FieldError, FieldLabel } from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import type { GatewayClient, PackageInstallRequest } from "@goldenbee/sdk"

import { HostErrorNotice } from "./host-notice"
import { buildInstallRequest, type InstallSourceKind } from "./install-request"
import { useInstallPackage } from "./use-host"

export function InstallPackageForm({ client }: { client: GatewayClient }) {
  const install = useInstallPackage(client)
  const [kind, setKind] = useState<InstallSourceKind>("url")
  const [url, setUrl] = useState("")
  const [path, setPath] = useState("")
  const [packageName, setPackageName] = useState("")
  const [expectedDigest, setExpectedDigest] = useState("")
  const [fieldError, setFieldError] = useState<{
    field: "url" | "path"
    message: string
  } | null>(null)
  const [confirmation, setConfirmation] = useState<PackageInstallRequest | null>(
    null,
  )

  const outcome = install.data

  function submit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const result = buildInstallRequest({
      kind,
      url,
      path,
      packageName,
      expectedDigest,
    })
    if (!result.request) {
      setFieldError(result.error ?? null)
      return
    }
    setFieldError(null)
    install.reset()
    setConfirmation(result.request)
  }

  function confirmInstall() {
    if (!confirmation || install.isPending) {
      return
    }
    install.mutate(confirmation, {
      onSuccess: () => {
        setConfirmation(null)
        setUrl("")
        setPath("")
        setPackageName("")
        setExpectedDigest("")
      },
    })
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Install a package</CardTitle>
        <CardDescription>
          Install a verified package into the Gateway package cache. A URL is
          the normal source; a Gateway-host folder is an explicit operator
          override.
        </CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-5">
        <form className="flex flex-col gap-5" onSubmit={submit}>
          <fieldset className="flex flex-col gap-2">
            <legend className="text-sm font-medium">Source</legend>
            <label className="flex items-center gap-2 text-sm">
              <input
                type="radio"
                name="install-source"
                value="url"
                checked={kind === "url"}
                onChange={() => setKind("url")}
              />
              Install from URL
            </label>
            <label className="flex items-center gap-2 text-sm">
              <input
                type="radio"
                name="install-source"
                value="folder"
                checked={kind === "folder"}
                onChange={() => setKind("folder")}
              />
              Gateway host folder
            </label>
          </fieldset>

          {kind === "url" ? (
            <Field data-invalid={fieldError?.field === "url" || undefined}>
              <FieldLabel htmlFor="package-url">Package URL</FieldLabel>
              <Input
                id="package-url"
                name="package-url"
                type="url"
                inputMode="url"
                placeholder="https://releases.example.com/general.tar.gz"
                value={url}
                onChange={(event) => setUrl(event.target.value)}
                aria-invalid={fieldError?.field === "url" || undefined}
              />
              {fieldError?.field === "url" ? (
                <FieldError>{fieldError.message}</FieldError>
              ) : null}
            </Field>
          ) : (
            <Field data-invalid={fieldError?.field === "path" || undefined}>
              <FieldLabel htmlFor="package-host-path">
                Gateway host path
              </FieldLabel>
              <Input
                id="package-host-path"
                name="package-host-path"
                placeholder="/srv/goldenbee/releases/general"
                value={path}
                onChange={(event) => setPath(event.target.value)}
                aria-invalid={fieldError?.field === "path" || undefined}
              />
              <FieldDescription>
                The folder is resolved on the Gateway host and validated
                against the configured allowed package sources. The browser
                cannot browse or upload that filesystem.
              </FieldDescription>
              {fieldError?.field === "path" ? (
                <FieldError>{fieldError.message}</FieldError>
              ) : null}
            </Field>
          )}

          <div className="grid gap-4 sm:grid-cols-2">
            <Field>
              <FieldLabel htmlFor="package-name">
                Package name (optional)
              </FieldLabel>
              <Input
                id="package-name"
                name="package-name"
                value={packageName}
                onChange={(event) => setPackageName(event.target.value)}
                placeholder="general"
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="package-digest">
                Expected digest (optional)
              </FieldLabel>
              <Input
                id="package-digest"
                name="package-digest"
                value={expectedDigest}
                onChange={(event) => setExpectedDigest(event.target.value)}
                placeholder="sha256:…"
              />
            </Field>
          </div>

          <div>
            <Button type="submit" disabled={install.isPending}>
              <PackagePlusIcon data-icon="inline-start" aria-hidden="true" />
              Install package
            </Button>
          </div>
        </form>

        {outcome ? (
          <Alert>
            <AlertTitle>
              {outcome.installed_now ? "Package installed" : "Already installed"}
            </AlertTitle>
            <AlertDescription>
              <p>
                {outcome.package.package} {outcome.package.version} ·{" "}
                {outcome.package.target}
              </p>
              <p className="font-mono text-xs break-all">
                digest: {outcome.package.artifact_digest}
              </p>
              <p className="font-mono text-xs break-all">
                signer: {outcome.package.signer}
              </p>
            </AlertDescription>
          </Alert>
        ) : null}
      </CardContent>

      <Dialog
        open={confirmation !== null}
        onOpenChange={(open) => {
          if (!open && !install.isPending) {
            setConfirmation(null)
          }
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Install package</DialogTitle>
            <DialogDescription>
              This privileged action writes to the Gateway package cache. The
              Gateway verifies the source, signer, digest, and target before it
              installs.
            </DialogDescription>
          </DialogHeader>

          {confirmation ? (
            <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-2 text-sm">
              <dt className="text-muted-foreground">Source</dt>
              <dd className="font-mono text-xs break-all">
                {confirmation.source.kind === "url"
                  ? confirmation.source.url
                  : confirmation.source.path}
              </dd>
              <dt className="text-muted-foreground">Package</dt>
              <dd>{confirmation.package ?? "detected from the source"}</dd>
              <dt className="text-muted-foreground">Expected digest</dt>
              <dd className="font-mono text-xs break-all">
                {confirmation.expected_digest ?? "verified by the Gateway"}
              </dd>
            </dl>
          ) : null}

          {install.error ? (
            <HostErrorNotice
              error={install.error}
              action="write"
              onRetry={confirmInstall}
              retryLabel="Retry install"
            />
          ) : null}

          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setConfirmation(null)}
              disabled={install.isPending}
            >
              Cancel
            </Button>
            <Button onClick={confirmInstall} disabled={install.isPending}>
              {install.isPending ? (
                <>
                  <Spinner />
                  Installing…
                </>
              ) : (
                "Install"
              )}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </Card>
  )
}

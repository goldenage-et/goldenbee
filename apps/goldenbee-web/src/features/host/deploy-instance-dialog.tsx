import { useState } from "react"
import { ServerPlusIcon } from "lucide-react"

import { Button } from "@/components/ui/button"
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
import type {
  GatewayClient,
  HostOperationOutcome,
  HostProvisionRequest,
  PackageSummary,
} from "@goldenbee/sdk"

import { HostErrorNotice } from "./host-notice"
import { parseGrants } from "./operation-id"
import { useProvisionInstance } from "./use-host"

export function DeployInstanceDialog({
  client,
  operationId,
  packages,
  onClose,
  onDeployed,
}: {
  client: GatewayClient
  operationId: string
  packages: PackageSummary[]
  onClose: () => void
  onDeployed: (outcome: HostOperationOutcome) => void
}) {
  const provision = useProvisionInstance(client)
  const [instanceName, setInstanceName] = useState("")
  const [workerId, setWorkerId] = useState("")
  const [releaseDigest, setReleaseDigest] = useState("")
  const [profileName, setProfileName] = useState("default")
  const [grants, setGrants] = useState("")
  const [validation, setValidation] = useState<string | null>(null)
  const [submitted, setSubmitted] = useState<HostProvisionRequest | null>(null)

  function requestFromForm(): HostProvisionRequest | null {
    if (!instanceName.trim() || !workerId.trim() || !releaseDigest) {
      setValidation("Instance name, Worker id, and release digest are required.")
      return null
    }
    const parsedGrants = parseGrants(grants)
    return {
      operation_id: operationId,
      instance: instanceName.trim(),
      worker: workerId.trim(),
      release_digest: releaseDigest,
      ...(parsedGrants.length > 0 ? { grants: parsedGrants } : {}),
      ...(profileName.trim() ? { profile: { name: profileName.trim() } } : {}),
    }
  }

  function submit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const request = requestFromForm()
    if (!request) {
      return
    }
    setValidation(null)
    setSubmitted(request)
    provision.mutate(request, { onSuccess: onDeployed })
  }

  function retry() {
    if (submitted && !provision.isPending) {
      provision.mutate(submitted, { onSuccess: onDeployed })
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && !provision.isPending && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Deploy local instance</DialogTitle>
          <DialogDescription>
            This privileged action provisions a Gateway-owned local process.
            Confirm the package digest, instance, Worker, profile, and grants
            before deployment.
          </DialogDescription>
        </DialogHeader>

        <form id="deploy-form" className="flex flex-col gap-4" onSubmit={submit}>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field>
              <FieldLabel htmlFor="deploy-instance">Instance name</FieldLabel>
              <Input
                id="deploy-instance"
                value={instanceName}
                onChange={(event) => setInstanceName(event.target.value)}
                placeholder="general-b"
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="deploy-worker">Worker id</FieldLabel>
              <Input
                id="deploy-worker"
                value={workerId}
                onChange={(event) => setWorkerId(event.target.value)}
                placeholder="general-b"
              />
            </Field>
          </div>

          <Field>
            <FieldLabel htmlFor="deploy-digest">Release digest</FieldLabel>
            <select
              id="deploy-digest"
              className="h-8 w-full rounded-2xl border border-transparent bg-input/50 px-2.5 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/30"
              value={releaseDigest}
              onChange={(event) => setReleaseDigest(event.target.value)}
            >
              <option value="">Select an installed package</option>
              {packages.map((entry) => (
                <option
                  key={entry.artifact_digest}
                  value={entry.artifact_digest}
                >
                  {entry.package} {entry.version} · {entry.artifact_digest}
                </option>
              ))}
            </select>
            <FieldDescription>
              Only an installed package digest can be deployed.
            </FieldDescription>
          </Field>

          <div className="grid gap-4 sm:grid-cols-2">
            <Field>
              <FieldLabel htmlFor="deploy-profile">Profile name</FieldLabel>
              <Input
                id="deploy-profile"
                value={profileName}
                onChange={(event) => setProfileName(event.target.value)}
                placeholder="default"
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="deploy-grants">Grants</FieldLabel>
              <Input
                id="deploy-grants"
                value={grants}
                onChange={(event) => setGrants(event.target.value)}
                placeholder="task:submit, task:read"
              />
              <FieldDescription>
                Comma-separated Gateway scopes granted to this instance.
              </FieldDescription>
            </Field>
          </div>

          {validation ? <FieldError>{validation}</FieldError> : null}
        </form>

        {provision.error ? (
          <HostErrorNotice
            error={provision.error}
            action="write"
            onRetry={retry}
            retryLabel="Retry deployment"
          />
        ) : null}

        <DialogFooter>
          <Button
            variant="outline"
            onClick={onClose}
            disabled={provision.isPending}
          >
            Cancel
          </Button>
          <Button
            disabled={provision.isPending}
            type="submit"
            form="deploy-form"
          >
            {provision.isPending ? (
              <>
                <Spinner />
                Deploying…
              </>
            ) : (
              <>
                <ServerPlusIcon data-icon="inline-start" aria-hidden="true" />
                Deploy
              </>
            )}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

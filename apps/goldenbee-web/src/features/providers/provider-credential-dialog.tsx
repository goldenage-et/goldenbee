import { useState } from "react"
import type {
  ProviderCredentialSummary,
  ProviderMetadata,
} from "@goldenbee/sdk"

import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { ProviderErrorAlert } from "@/features/providers/provider-error-alert"
import type { ProviderAdminClient } from "@/features/providers/provider-client"
import { useSetProviderCredential } from "@/features/providers/provider-queries"

/**
 * Write-only credential entry.
 *
 * The key exists in this component's state only until the Gateway accepts it.
 * It is never put in the query cache, never rendered back, and never included in
 * the mutation's retained state after success.
 */
export function ProviderCredentialDialog({
  client,
  provider,
}: {
  client: ProviderAdminClient
  provider: ProviderMetadata
}) {
  const [open, setOpen] = useState(false)
  const [key, setKey] = useState("")
  const [written, setWritten] = useState<ProviderCredentialSummary | null>(null)
  const mutation = useSetProviderCredential(client)

  const label = provider.credential_version
    ? "Rotate credential"
    : "Set credential"

  async function submit() {
    const value = key.trim()
    if (!value) {
      return
    }
    try {
      const summary = await mutation.mutateAsync({ id: provider.id, key: value })
      // Clear the secret before any success state is rendered, then drop the
      // mutation's retained variables.
      setKey("")
      setWritten(summary)
      mutation.reset()
    } catch {
      // The typed error is rendered from the mutation; keep the value so the
      // operator can correct a mistyped key.
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        setOpen(next)
        if (!next) {
          setKey("")
          setWritten(null)
          mutation.reset()
        }
      }}
    >
      <DialogTrigger render={<Button variant="outline" />}>
        {label}
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{label}</DialogTitle>
          <DialogDescription>
            The Gateway encrypts the key and returns only its version and
            fingerprint. This form never reads the key back.
          </DialogDescription>
        </DialogHeader>

        <form
          onSubmit={(event) => {
            event.preventDefault()
            void submit()
          }}
        >
          <FieldGroup>
            <Field>
              <FieldLabel htmlFor="provider-credential">Provider API key</FieldLabel>
              <Input
                id="provider-credential"
                name="provider-credential"
                type="password"
                autoComplete="off"
                spellCheck={false}
                value={key}
                onChange={(event) => {
                  setKey(event.target.value)
                  setWritten(null)
                }}
                placeholder="sk-..."
                disabled={mutation.isPending}
              />
              <FieldDescription>
                Write-only. After a successful write the field is cleared and only
                the fingerprint remains visible.
              </FieldDescription>
            </Field>

            {mutation.isPending ? (
              <p className="flex items-center gap-2 text-sm text-muted-foreground">
                <Spinner /> Writing credential and refreshing models.
              </p>
            ) : null}

            {mutation.error ? <ProviderErrorAlert error={mutation.error} /> : null}

            {written ? (
              <div className="rounded-2xl border bg-muted/40 p-3 text-sm">
                <p className="flex flex-wrap items-center gap-2 font-medium">
                  Credential written
                  <Badge variant="secondary">Version {written.version}</Badge>
                </p>
                <p className="mt-1 text-xs text-muted-foreground">
                  Fingerprint {written.fingerprint}
                </p>
                <p className="mt-1 text-xs text-muted-foreground">
                  The submitted key was cleared from this form.
                </p>
                {written.ready ? (
                  <p className="mt-1 text-xs">Models are ready.</p>
                ) : (
                  <p className="mt-1 text-xs text-destructive">
                    Metadata is saved but the provider is not ready
                    {written.discovery_error_code
                      ? ` (${written.discovery_error_code})`
                      : ""}
                    . {written.discovery_error ?? ""}
                  </p>
                )}
              </div>
            ) : null}
          </FieldGroup>
        </form>

        <DialogFooter>
          <Button variant="ghost" onClick={() => setOpen(false)}>
            Close
          </Button>
          <Button
            onClick={() => void submit()}
            disabled={mutation.isPending || key.trim().length === 0}
          >
            {mutation.isPending ? <Spinner /> : null}
            Write credential
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

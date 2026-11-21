import { useState } from "react"

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
import { Checkbox } from "@/components/ui/checkbox"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { ProviderErrorAlert } from "@/features/providers/provider-error-alert"
import type { ProviderAdminClient } from "@/features/providers/provider-client"
import { useCreateProvider } from "@/features/providers/provider-queries"

const adapters = [
  "openrouter",
  "openai-compatible",
  "ollama",
  "http-json",
  "deterministic",
] as const

function splitList(value: string): string[] {
  return value
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean)
}

/**
 * Creates provider metadata. Credentials are never part of creation; the
 * provider page has one write-only path for a key.
 */
export function ProviderCreateDialog({
  client,
}: {
  client: ProviderAdminClient
}) {
  const [open, setOpen] = useState(false)
  const [id, setId] = useState("")
  const [adapter, setAdapter] = useState<string>("openrouter")
  const [endpoint, setEndpoint] = useState("")
  const [model, setModel] = useState("")
  const [residency, setResidency] = useState("remote")
  const [modalities, setModalities] = useState("text")
  const [allowedWorkers, setAllowedWorkers] = useState("")
  const [allowedModels, setAllowedModels] = useState("")
  const [contextWindow, setContextWindow] = useState("")
  const [toolCalling, setToolCalling] = useState(false)
  const [local, setLocal] = useState(false)
  const [allowAutoDefault, setAllowAutoDefault] = useState(false)
  const mutation = useCreateProvider(client)

  function reset() {
    setId("")
    setAdapter("openrouter")
    setEndpoint("")
    setModel("")
    setResidency("remote")
    setModalities("text")
    setAllowedWorkers("")
    setAllowedModels("")
    setContextWindow("")
    setToolCalling(false)
    setLocal(false)
    setAllowAutoDefault(false)
    mutation.reset()
  }

  function submit() {
    const workers = splitList(allowedWorkers)
    const models = splitList(allowedModels)
    const context = contextWindow.trim() === "" ? undefined : Number(contextWindow)
    mutation.mutate(
      {
        id: id.trim(),
        adapter,
        endpoint: endpoint.trim() || undefined,
        model,
        residency: residency.trim(),
        modalities: splitList(modalities),
        allowed_workers: workers,
        allowed_models: models.length > 0 ? models : undefined,
        context_window:
          context !== undefined && Number.isFinite(context) ? context : undefined,
        tool_calling: toolCalling,
        local,
        allow_auto_default: allowAutoDefault,
      },
      {
        onSuccess: () => {
          setOpen(false)
          reset()
        },
      },
    )
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        setOpen(next)
        if (!next) {
          reset()
        }
      }}
    >
      <DialogTrigger render={<Button />}>Add provider</DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Add provider</DialogTitle>
          <DialogDescription>
            Create non-secret provider metadata. Discovery and a write-only
            credential follow on the provider page.
          </DialogDescription>
        </DialogHeader>

        <form
          onSubmit={(event) => {
            event.preventDefault()
            submit()
          }}
        >
          <FieldGroup>
            <div className="grid gap-4 sm:grid-cols-2">
              <Field>
                <FieldLabel htmlFor="new-provider-id">Provider id</FieldLabel>
                <Input
                  id="new-provider-id"
                  value={id}
                  onChange={(event) => setId(event.target.value)}
                  placeholder="openrouter"
                  required
                />
                <FieldDescription>
                  1-64 characters of letters, digits, underscore, or hyphen.
                </FieldDescription>
              </Field>
              <Field>
                <FieldLabel htmlFor="new-provider-adapter">Adapter</FieldLabel>
                <select
                  id="new-provider-adapter"
                  value={adapter}
                  onChange={(event) => setAdapter(event.target.value)}
                  className="h-8 w-full rounded-2xl border border-transparent bg-input/50 px-2.5 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/30"
                >
                  {adapters.map((value) => (
                    <option key={value} value={value}>
                      {value}
                    </option>
                  ))}
                </select>
              </Field>
            </div>

            <Field>
              <FieldLabel htmlFor="new-provider-endpoint">Endpoint</FieldLabel>
              <Input
                id="new-provider-endpoint"
                value={endpoint}
                onChange={(event) => setEndpoint(event.target.value)}
                placeholder="https://provider.example/v1"
              />
            </Field>

            <div className="grid gap-4 sm:grid-cols-2">
              <Field>
                <FieldLabel htmlFor="new-provider-model">Default model</FieldLabel>
                <Input
                  id="new-provider-model"
                  value={model}
                  onChange={(event) => setModel(event.target.value)}
                  placeholder="provider/model"
                />
                <FieldDescription>
                  Required for adapters that do not discover models.
                </FieldDescription>
              </Field>
              <Field>
                <FieldLabel htmlFor="new-provider-residency">Residency</FieldLabel>
                <Input
                  id="new-provider-residency"
                  value={residency}
                  onChange={(event) => setResidency(event.target.value)}
                  required
                />
              </Field>
            </div>

            <div className="grid gap-4 sm:grid-cols-2">
              <Field>
                <FieldLabel htmlFor="new-provider-modalities">Modalities</FieldLabel>
                <Input
                  id="new-provider-modalities"
                  value={modalities}
                  onChange={(event) => setModalities(event.target.value)}
                />
                <FieldDescription>Comma-separated.</FieldDescription>
              </Field>
              <Field>
                <FieldLabel htmlFor="new-provider-context">Context window</FieldLabel>
                <Input
                  id="new-provider-context"
                  inputMode="numeric"
                  value={contextWindow}
                  onChange={(event) => setContextWindow(event.target.value)}
                  placeholder="128000"
                />
              </Field>
            </div>

            <div className="grid gap-4 sm:grid-cols-2">
              <Field>
                <FieldLabel htmlFor="new-provider-workers">
                  Allowed Workers
                </FieldLabel>
                <Input
                  id="new-provider-workers"
                  value={allowedWorkers}
                  onChange={(event) => setAllowedWorkers(event.target.value)}
                  placeholder="general-a, *"
                />
                <FieldDescription>
                  Empty assigns no Worker. * assigns every approved Worker.
                </FieldDescription>
              </Field>
              <Field>
                <FieldLabel htmlFor="new-provider-models">
                  Allowed models
                </FieldLabel>
                <Input
                  id="new-provider-models"
                  value={allowedModels}
                  onChange={(event) => setAllowedModels(event.target.value)}
                  placeholder="openai/gpt-4.1"
                />
              </Field>
            </div>

            <div className="grid gap-3 sm:grid-cols-3">
              <label className="flex items-center gap-2 text-sm">
                <Checkbox
                  checked={toolCalling}
                  onCheckedChange={(checked) => setToolCalling(checked === true)}
                />
                Tool calling
              </label>
              <label className="flex items-center gap-2 text-sm">
                <Checkbox
                  checked={local}
                  onCheckedChange={(checked) => setLocal(checked === true)}
                />
                Local provider
              </label>
              <label className="flex items-center gap-2 text-sm">
                <Checkbox
                  checked={allowAutoDefault}
                  onCheckedChange={(checked) =>
                    setAllowAutoDefault(checked === true)
                  }
                />
                Auto-select default
              </label>
            </div>

            {mutation.error ? <ProviderErrorAlert error={mutation.error} /> : null}
          </FieldGroup>
        </form>

        <DialogFooter>
          <Button variant="ghost" onClick={() => setOpen(false)}>
            Cancel
          </Button>
          <Button
            onClick={() => submit()}
            disabled={mutation.isPending || id.trim().length === 0}
          >
            {mutation.isPending ? <Spinner /> : null}
            Create provider
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

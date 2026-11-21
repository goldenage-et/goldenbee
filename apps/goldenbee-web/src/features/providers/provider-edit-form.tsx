import { useState } from "react"
import type {
  ProviderMetadata,
  UpdateProviderRequest,
} from "@goldenbee/sdk"

import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"

const adapters = [
  "deterministic",
  "http-json",
  "ollama",
  "openai-compatible",
  "openrouter",
] as const

function splitList(value: string): string[] {
  return value
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean)
}

function numberOrUndefined(value: string): number | undefined {
  const trimmed = value.trim()
  if (trimmed === "") {
    return undefined
  }
  const parsed = Number(trimmed)
  return Number.isFinite(parsed) ? parsed : undefined
}

export interface ProviderEditFormProps {
  provider: ProviderMetadata
  pending: boolean
  onSubmit(request: UpdateProviderRequest): void
}

/**
 * Edits non-secret metadata only. The request always carries the revision the
 * detail view observed, so the Gateway rejects a stale edit instead of silently
 * overwriting another operator's change.
 */
export function ProviderEditForm({
  provider,
  pending,
  onSubmit,
}: ProviderEditFormProps) {
  const [adapter, setAdapter] = useState(provider.adapter)
  const [model, setModel] = useState(provider.model)
  const [endpoint, setEndpoint] = useState(provider.endpoint ?? "")
  const [residency, setResidency] = useState(provider.residency)
  const [modalities, setModalities] = useState(provider.modalities.join(", "))
  const [allowedWorkers, setAllowedWorkers] = useState(
    provider.allowed_workers.join(", "),
  )
  const [allowedModels, setAllowedModels] = useState(
    provider.allowed_models?.join(", ") ?? "",
  )
  const [contextWindow, setContextWindow] = useState(
    provider.context_window === undefined ? "" : String(provider.context_window),
  )
  const [toolCalling, setToolCalling] = useState(provider.tool_calling)
  const [local, setLocal] = useState(provider.local)
  const [allowAutoDefault, setAllowAutoDefault] = useState(
    provider.allow_auto_default,
  )

  const editedAllowedModels = splitList(allowedModels)

  return (
    <Card>
      <CardHeader>
        <CardTitle>Provider metadata</CardTitle>
        <CardDescription>
          Saving uses revision {provider.revision}. A change made elsewhere
          advances the revision and rejects this edit.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <form
          onSubmit={(event) => {
            event.preventDefault()
            onSubmit({
              revision: provider.revision,
              adapter,
              model,
              endpoint,
              residency,
              modalities: splitList(modalities),
              allowed_workers: splitList(allowedWorkers),
              // An empty list means "allow no model" to Gateway, so an empty
              // field omits the filter instead of silently blocking discovery.
              allowed_models:
                editedAllowedModels.length > 0 ? editedAllowedModels : undefined,
              context_window: numberOrUndefined(contextWindow),
              tool_calling: toolCalling,
              local,
              allow_auto_default: allowAutoDefault,
            })
          }}
        >
          <FieldGroup>
            <div className="grid gap-4 sm:grid-cols-2">
              <Field>
                <FieldLabel htmlFor="provider-adapter">Adapter</FieldLabel>
                <select
                  id="provider-adapter"
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
              <Field>
                <FieldLabel htmlFor="provider-model">Default model</FieldLabel>
                <Input
                  id="provider-model"
                  value={model}
                  onChange={(event) => setModel(event.target.value)}
                  placeholder="provider/model"
                />
                <FieldDescription>
                  Must be one of the discovered or allowed models.
                </FieldDescription>
              </Field>
            </div>

            <Field>
              <FieldLabel htmlFor="provider-endpoint">Endpoint</FieldLabel>
              <Input
                id="provider-endpoint"
                value={endpoint}
                onChange={(event) => setEndpoint(event.target.value)}
                placeholder="https://provider.example/v1"
              />
              <FieldDescription>
                An empty value clears the endpoint. Credentials embedded in a
                URL are rejected by the Gateway.
              </FieldDescription>
            </Field>

            <div className="grid gap-4 sm:grid-cols-2">
              <Field>
                <FieldLabel htmlFor="provider-residency">Residency</FieldLabel>
                <Input
                  id="provider-residency"
                  value={residency}
                  onChange={(event) => setResidency(event.target.value)}
                />
              </Field>
              <Field>
                <FieldLabel htmlFor="provider-context-window">
                  Context window
                </FieldLabel>
                <Input
                  id="provider-context-window"
                  inputMode="numeric"
                  value={contextWindow}
                  onChange={(event) => setContextWindow(event.target.value)}
                  placeholder="128000"
                />
                <FieldDescription>Zero clears the declared window.</FieldDescription>
              </Field>
            </div>

            <div className="grid gap-4 sm:grid-cols-2">
              <Field>
                <FieldLabel htmlFor="provider-modalities">Modalities</FieldLabel>
                <Input
                  id="provider-modalities"
                  value={modalities}
                  onChange={(event) => setModalities(event.target.value)}
                  placeholder="text"
                />
                <FieldDescription>Comma-separated.</FieldDescription>
              </Field>
              <Field>
                <FieldLabel htmlFor="provider-workers">Allowed Workers</FieldLabel>
                <Input
                  id="provider-workers"
                  value={allowedWorkers}
                  onChange={(event) => setAllowedWorkers(event.target.value)}
                  placeholder="general-a, *"
                />
                <FieldDescription>
                  Comma-separated Worker ids, or * for every approved Worker.
                </FieldDescription>
              </Field>
            </div>

            <Field>
              <FieldLabel htmlFor="provider-allowed-models">
                Allowed models
              </FieldLabel>
              <Input
                id="provider-allowed-models"
                value={allowedModels}
                onChange={(event) => setAllowedModels(event.target.value)}
                placeholder="openai/gpt-4.1, anthropic/claude-sonnet-4"
              />
              <FieldDescription>
                An optional comma-separated filter applied to discovered models.
                Leave empty to keep the current filter.
              </FieldDescription>
            </Field>

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

            <div className="flex items-center justify-end gap-2">
              <Button type="submit" disabled={pending}>
                {pending ? <Spinner /> : null}
                Save provider
              </Button>
            </div>
          </FieldGroup>
        </form>
      </CardContent>
    </Card>
  )
}

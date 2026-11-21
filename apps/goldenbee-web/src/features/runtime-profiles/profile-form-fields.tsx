import { useState } from "react"

import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import {
  Field,
  FieldDescription,
  FieldError,
  FieldLabel,
} from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import type {
  CreateProfileRequest,
  Profile,
  ProfileRevision,
  ReviseProfileRequest,
} from "@goldenbee/sdk"

import {
  buildCreateProfileRequest,
  buildReviseProfileRequest,
  emptyProfileFormValues,
  profileCompositions,
  profileFormValues,
  type ProfileFormField,
  type ProfileFormIssue,
  type ProfileFormValues,
} from "./profile-form"

type ProfileFormProps =
  | {
      mode: "create"
      pending: boolean
      onCreate(request: CreateProfileRequest): void
    }
  | {
      mode: "revise"
      profile: Profile
      revision: ProfileRevision
      pending: boolean
      onRevise(request: ReviseProfileRequest): void
    }

function firstIssue(
  issues: ProfileFormIssue[],
  field: ProfileFormField,
): string | undefined {
  return issues.find((issue) => issue.field === field)?.message
}

export function ProfileForm(props: ProfileFormProps) {
  const [values, setValues] = useState<ProfileFormValues>(() =>
    props.mode === "create"
      ? emptyProfileFormValues()
      : profileFormValues(props.profile, props.revision),
  )
  const [issues, setIssues] = useState<ProfileFormIssue[]>([])

  function update(patch: Partial<ProfileFormValues>) {
    setValues((current) => ({ ...current, ...patch }))
  }

  function submit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (props.mode === "create") {
      const result = buildCreateProfileRequest(values)
      setIssues(result.issues)
      if (result.request) {
        props.onCreate(result.request)
      }
      return
    }
    const expected = props.revision.revision
    const result = buildReviseProfileRequest(values, expected)
    setIssues(result.issues)
    if (result.request) {
      props.onRevise(result.request)
    }
  }

  function textField(
    field: ProfileFormField | "",
    label: string,
    options: {
      placeholder?: string
      description?: string
      id: string
      value: string
      onChange(value: string): void
    },
  ) {
    return (
      <Field
        data-invalid={field && firstIssue(issues, field) ? true : undefined}
      >
        <FieldLabel htmlFor={options.id}>{label}</FieldLabel>
        <Input
          id={options.id}
          value={options.value}
          onChange={(event) => options.onChange(event.target.value)}
          placeholder={options.placeholder}
          aria-invalid={
            field && firstIssue(issues, field) ? true : undefined
          }
        />
        {options.description ? (
          <FieldDescription>{options.description}</FieldDescription>
        ) : null}
        <FieldError>{field ? firstIssue(issues, field) : undefined}</FieldError>
      </Field>
    )
  }

  return (
    <form className="flex flex-col gap-5" onSubmit={submit} noValidate>
      <div className="grid gap-4 sm:grid-cols-2">
        <Field data-invalid={firstIssue(issues, "name") ? true : undefined}>
          <FieldLabel htmlFor={`profile-${props.mode}-name`}>
            Profile name
          </FieldLabel>
          <Input
            id={`profile-${props.mode}-name`}
            value={values.name}
            onChange={(event) => update({ name: event.target.value })}
            disabled={props.mode === "revise"}
            placeholder="research"
            aria-invalid={firstIssue(issues, "name") ? true : undefined}
          />
          <FieldError>{firstIssue(issues, "name")}</FieldError>
        </Field>

        {textField("model", "Model", {
          id: `profile-${props.mode}-model`,
          value: values.model,
          onChange: (model) => update({ model }),
          placeholder: "openai/gpt-4.1",
        })}
      </div>

      <Field data-invalid={firstIssue(issues, "instruction") ? true : undefined}>
        <FieldLabel htmlFor={`profile-${props.mode}-instruction`}>
          Instruction
        </FieldLabel>
        <Textarea
          id={`profile-${props.mode}-instruction`}
          value={values.instruction}
          onChange={(event) => update({ instruction: event.target.value })}
          rows={4}
          placeholder="Describe the agent's job, tone, and boundaries."
          aria-invalid={firstIssue(issues, "instruction") ? true : undefined}
        />
        <FieldDescription>
          The instruction is profile configuration, not a credential.
        </FieldDescription>
        <FieldError>{firstIssue(issues, "instruction")}</FieldError>
      </Field>

      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
        <Field data-invalid={firstIssue(issues, "composition") ? true : undefined}>
          <FieldLabel htmlFor={`profile-${props.mode}-composition`}>
            Composition
          </FieldLabel>
          <select
            id={`profile-${props.mode}-composition`}
            value={values.composition}
            onChange={(event) =>
              update({
                composition: event.target.value as ProfileFormValues["composition"],
              })
            }
            className="h-8 w-full rounded-2xl border border-transparent bg-input/50 px-2.5 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/30"
          >
            {profileCompositions.map((value) => (
              <option key={value} value={value}>
                {value}
              </option>
            ))}
          </select>
          <FieldError>{firstIssue(issues, "composition")}</FieldError>
        </Field>

        {textField("", "Capabilities", {
          id: `profile-${props.mode}-capabilities`,
          value: values.capabilities,
          onChange: (capabilities) => update({ capabilities }),
          placeholder: "general, research",
          description: "Comma-separated capability ids.",
        })}
        {textField("", "Members", {
          id: `profile-${props.mode}-members`,
          value: values.members,
          onChange: (members) => update({ members }),
          placeholder: "planner, writer",
          description: "Child agents for a composed profile.",
        })}
      </div>

      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
        {textField("", "Grants", {
          id: `profile-${props.mode}-grants`,
          value: values.grants,
          onChange: (grants) => update({ grants }),
          placeholder: "fs.read, net.read",
          description: "Comma-separated permission grants.",
        })}
        {textField("", "Restrictions", {
          id: `profile-${props.mode}-restrictions`,
          value: values.restrictions,
          onChange: (restrictions) => update({ restrictions }),
          placeholder: "fs.write",
          description: "Must be a subset of the instance grants.",
        })}
        {textField("", "Workflow", {
          id: `profile-${props.mode}-workflow`,
          value: values.workflow,
          onChange: (workflow) => update({ workflow }),
          placeholder: "review",
          description: "Optional workflow template reference.",
        })}
      </div>

      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
        {textField("", "Skills", {
          id: `profile-${props.mode}-skills`,
          value: values.skills,
          onChange: (skills) => update({ skills }),
          placeholder: "citation",
          description: "Comma-separated skill names.",
        })}
        {textField("", "MCP servers", {
          id: `profile-${props.mode}-mcp`,
          value: values.mcp,
          onChange: (mcp) => update({ mcp }),
          placeholder: "search",
          description: "Comma-separated MCP server references.",
        })}
        <Field
          data-invalid={firstIssue(issues, "secret_refs") ? true : undefined}
        >
          <FieldLabel htmlFor={`profile-${props.mode}-secret-refs`}>
            Secret references
          </FieldLabel>
          <Input
            id={`profile-${props.mode}-secret-refs`}
            value={values.secretRefs}
            onChange={(event) => update({ secretRefs: event.target.value })}
            placeholder="env:SEARCH_KEY"
            aria-invalid={firstIssue(issues, "secret_refs") ? true : undefined}
          />
          <FieldDescription>
            Names only: secret://, env:, or vault:. Values stay in the Worker
            and are never sent to the browser.
          </FieldDescription>
          <FieldError>{firstIssue(issues, "secret_refs")}</FieldError>
        </Field>
      </div>

      <fieldset
        aria-label="Limits"
        className="grid gap-4 rounded-2xl border border-line p-3 sm:grid-cols-2 lg:grid-cols-4"
      >
        <legend className="px-1 text-sm font-medium">Limits</legend>
        {textField("max_duration_ms", "Max duration (ms)", {
          id: `profile-${props.mode}-max-duration`,
          value: values.maxDurationMs,
          onChange: (maxDurationMs) => update({ maxDurationMs }),
        })}
        {textField("max_model_calls", "Max model calls", {
          id: `profile-${props.mode}-max-model-calls`,
          value: values.maxModelCalls,
          onChange: (maxModelCalls) => update({ maxModelCalls }),
        })}
        {textField("max_tool_calls", "Max tool calls", {
          id: `profile-${props.mode}-max-tool-calls`,
          value: values.maxToolCalls,
          onChange: (maxToolCalls) => update({ maxToolCalls }),
        })}
        {textField("max_tokens", "Max tokens", {
          id: `profile-${props.mode}-max-tokens`,
          value: values.maxTokens,
          onChange: (maxTokens) => update({ maxTokens }),
        })}
        {textField("max_cost_micros", "Max cost (micros)", {
          id: `profile-${props.mode}-max-cost`,
          value: values.maxCostMicros,
          onChange: (maxCostMicros) => update({ maxCostMicros }),
        })}
        {textField("max_fanout", "Max fanout", {
          id: `profile-${props.mode}-max-fanout`,
          value: values.maxFanout,
          onChange: (maxFanout) => update({ maxFanout }),
        })}
        {textField("max_depth", "Max depth", {
          id: `profile-${props.mode}-max-depth`,
          value: values.maxDepth,
          onChange: (maxDepth) => update({ maxDepth }),
        })}
      </fieldset>

      <fieldset
        aria-label="Context compaction"
        className="flex flex-col gap-3 rounded-2xl border border-line p-3"
      >
        <legend className="px-1 text-sm font-medium">Context compaction</legend>
        <label className="flex items-center gap-2 text-sm">
          <Checkbox
            checked={values.compactionEnabled}
            onCheckedChange={(checked) =>
              update({ compactionEnabled: checked === true })
            }
          />
          Enable context compaction
        </label>
        <p className="text-xs text-muted-foreground">
          Truncation is the only supported strategy. The budget must stay below
          the selected provider's context window.
        </p>
        {values.compactionEnabled ? (
          <div className="grid gap-4 sm:grid-cols-3">
            {textField("context_budget", "Context budget", {
              id: `profile-${props.mode}-context-budget`,
              value: values.contextBudget,
              onChange: (contextBudget) => update({ contextBudget }),
              placeholder: "64000",
            })}
            {textField("preserve_recent", "Preserve recent", {
              id: `profile-${props.mode}-preserve-recent`,
              value: values.preserveRecent,
              onChange: (preserveRecent) => update({ preserveRecent }),
            })}
            {textField("max_retries", "Max retries", {
              id: `profile-${props.mode}-max-retries`,
              value: values.maxRetries,
              onChange: (maxRetries) => update({ maxRetries }),
            })}
          </div>
        ) : null}
      </fieldset>

      <div>
        <Button type="submit" disabled={props.pending}>
          {props.mode === "create" ? "Create profile" : "Save revision"}
        </Button>
      </div>
    </form>
  )
}

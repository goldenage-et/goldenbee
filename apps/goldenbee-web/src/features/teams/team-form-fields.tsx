import { useState } from "react"
import { PlusIcon, Trash2Icon } from "lucide-react"

import { Button } from "@/components/ui/button"
import {
  Field,
  FieldDescription,
  FieldError,
  FieldLabel,
} from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import type {
  CreateTeamRequest,
  Team,
  UpdateTeamRequest,
} from "@goldenbee/sdk"

import {
  buildCreateTeamRequest,
  buildUpdateTeamRequest,
  emptyTeamFormValues,
  emptyTeamMemberDraft,
  type TeamFormIssue,
  type TeamFormValues,
  type TeamLocality,
} from "./team-form"

type TeamFormProps =
  | {
      mode: "create"
      pending: boolean
      onCreate(request: CreateTeamRequest): void
    }
  | {
      mode: "revise"
      team: Team
      pending: boolean
      onRevise(request: UpdateTeamRequest): void
    }

function valuesFromTeam(team: Team): TeamFormValues {
  const members = team.members.map((member) => ({
    worker: member.worker,
    role: member.role ?? "",
    capabilities: member.capabilities.join(", "),
    maxDepth:
      member.max_depth && member.max_depth > 0
        ? String(member.max_depth)
        : "",
  }))
  return {
    name: team.name,
    master: team.master,
    locality: (team.locality ?? "any") as TeamLocality,
    members: members.length > 0 ? members : [emptyTeamMemberDraft()],
    maxDepth:
      team.limits?.max_depth && team.limits.max_depth > 0
        ? String(team.limits.max_depth)
        : "",
    maxFanout:
      team.limits?.max_fanout && team.limits.max_fanout > 0
        ? String(team.limits.max_fanout)
        : "",
  }
}

function firstIssue(
  issues: TeamFormIssue[],
  field: TeamFormIssue["field"],
): string | undefined {
  return issues.find((issue) => issue.field === field)?.message
}

export function TeamForm(props: TeamFormProps) {
  const [values, setValues] = useState<TeamFormValues>(() =>
    props.mode === "create" ? emptyTeamFormValues() : valuesFromTeam(props.team),
  )
  const [issues, setIssues] = useState<TeamFormIssue[]>([])

  function update(patch: Partial<TeamFormValues>) {
    setValues((current) => ({ ...current, ...patch }))
  }

  function updateMember(index: number, patch: Partial<TeamFormValues["members"][number]>) {
    setValues((current) => ({
      ...current,
      members: current.members.map((member, position) =>
        position === index ? { ...member, ...patch } : member,
      ),
    }))
  }

  function submit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (props.mode === "create") {
      const result = buildCreateTeamRequest(values)
      setIssues(result.issues)
      if (result.request) {
        props.onCreate(result.request)
      }
      return
    }
    const result = buildUpdateTeamRequest(values, props.team.version)
    setIssues(result.issues)
    if (result.request) {
      props.onRevise(result.request)
    }
  }

  const membersIssue = firstIssue(issues, "members")

  return (
    <form className="flex flex-col gap-5" onSubmit={submit} noValidate>
      <div className="grid gap-4 sm:grid-cols-2">
        <Field data-invalid={firstIssue(issues, "name") ? true : undefined}>
          <FieldLabel htmlFor={`team-${props.mode}-name`}>Team name</FieldLabel>
          <Input
            id={`team-${props.mode}-name`}
            value={values.name}
            onChange={(event) => update({ name: event.target.value })}
            disabled={props.mode === "revise"}
            placeholder="research"
            aria-invalid={firstIssue(issues, "name") ? true : undefined}
          />
          {props.mode === "create" ? (
            <FieldDescription>
              Names are namespace-scoped and cannot contain path separators.
            </FieldDescription>
          ) : null}
          <FieldError>{firstIssue(issues, "name")}</FieldError>
        </Field>

        <Field data-invalid={firstIssue(issues, "master") ? true : undefined}>
          <FieldLabel htmlFor={`team-${props.mode}-master`}>
            Master Worker
          </FieldLabel>
          <Input
            id={`team-${props.mode}-master`}
            value={values.master}
            onChange={(event) => update({ master: event.target.value })}
            placeholder="general-a"
            list="team-worker-ids"
            aria-invalid={firstIssue(issues, "master") ? true : undefined}
          />
          <FieldError>{firstIssue(issues, "master")}</FieldError>
        </Field>

        <Field>
          <FieldLabel htmlFor={`team-${props.mode}-locality`}>Locality</FieldLabel>
          <select
            id={`team-${props.mode}-locality`}
            value={values.locality}
            onChange={(event) =>
              update({ locality: event.target.value as TeamLocality })
            }
            className="h-8 w-full rounded-2xl border border-transparent bg-input/50 px-2.5 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/30"
          >
            <option value="any">Any locality</option>
            <option value="local">Local only</option>
            <option value="remote">Remote only</option>
          </select>
          <FieldDescription>
            Pinned locality must match every roster member.
          </FieldDescription>
        </Field>

        <div className="grid grid-cols-2 gap-3">
          <Field data-invalid={firstIssue(issues, "max_depth") ? true : undefined}>
            <FieldLabel htmlFor={`team-${props.mode}-depth`}>
              Max depth
            </FieldLabel>
            <Input
              id={`team-${props.mode}-depth`}
              inputMode="numeric"
              value={values.maxDepth}
              onChange={(event) => update({ maxDepth: event.target.value })}
              placeholder="Gateway default"
              aria-invalid={firstIssue(issues, "max_depth") ? true : undefined}
            />
            <FieldError>{firstIssue(issues, "max_depth")}</FieldError>
          </Field>
          <Field data-invalid={firstIssue(issues, "max_fanout") ? true : undefined}>
            <FieldLabel htmlFor={`team-${props.mode}-fanout`}>
              Max fanout
            </FieldLabel>
            <Input
              id={`team-${props.mode}-fanout`}
              inputMode="numeric"
              value={values.maxFanout}
              onChange={(event) => update({ maxFanout: event.target.value })}
              placeholder="Member count"
              aria-invalid={firstIssue(issues, "max_fanout") ? true : undefined}
            />
            <FieldError>{firstIssue(issues, "max_fanout")}</FieldError>
          </Field>
        </div>
      </div>

      <fieldset
        aria-label="Members"
        className="flex flex-col gap-3 rounded-2xl border border-line p-3"
      >
        <legend className="px-1 text-sm font-medium">Members</legend>
        <p className="text-xs text-muted-foreground">
          List the Master to keep its narrowed capabilities. When the Master is
          absent, the Gateway infers it from the registry. Every listed member
          must authorize at least one capability, and at least one member must
          differ from the Master.
        </p>

        {values.members.map((member, index) => (
          <div
            key={index}
            className="grid gap-3 rounded-[9px] border border-line/70 bg-surface/40 p-2.5 sm:grid-cols-2"
          >
            <Field
              data-invalid={
                firstIssue(issues, `member-${index}-worker`) ? true : undefined
              }
            >
              <FieldLabel htmlFor={`team-${props.mode}-member-${index}-worker`}>
                Member {index + 1} worker
              </FieldLabel>
              <Input
                id={`team-${props.mode}-member-${index}-worker`}
                value={member.worker}
                onChange={(event) =>
                  updateMember(index, { worker: event.target.value })
                }
                list="team-worker-ids"
                aria-invalid={
                  firstIssue(issues, `member-${index}-worker`) ? true : undefined
                }
              />
              <FieldError>
                {firstIssue(issues, `member-${index}-worker`)}
              </FieldError>
            </Field>

            <Field
              data-invalid={
                firstIssue(issues, `member-${index}-capabilities`)
                  ? true
                  : undefined
              }
            >
              <FieldLabel
                htmlFor={`team-${props.mode}-member-${index}-capabilities`}
              >
                Member {index + 1} capabilities
              </FieldLabel>
              <Input
                id={`team-${props.mode}-member-${index}-capabilities`}
                value={member.capabilities}
                onChange={(event) =>
                  updateMember(index, { capabilities: event.target.value })
                }
                placeholder="general, research"
                aria-invalid={
                  firstIssue(issues, `member-${index}-capabilities`)
                    ? true
                    : undefined
                }
              />
              <FieldDescription>Comma-separated capability ids.</FieldDescription>
              <FieldError>
                {firstIssue(issues, `member-${index}-capabilities`)}
              </FieldError>
            </Field>

            <Field>
              <FieldLabel htmlFor={`team-${props.mode}-member-${index}-role`}>
                Member {index + 1} role
              </FieldLabel>
              <Input
                id={`team-${props.mode}-member-${index}-role`}
                value={member.role}
                onChange={(event) =>
                  updateMember(index, { role: event.target.value })
                }
                placeholder="analyst"
              />
            </Field>

            <div className="flex items-end gap-2">
              <Field
                data-invalid={
                  firstIssue(issues, `member-${index}-max_depth`)
                    ? true
                    : undefined
                }
              >
                <FieldLabel
                  htmlFor={`team-${props.mode}-member-${index}-depth`}
                >
                  Member {index + 1} max depth
                </FieldLabel>
                <Input
                  id={`team-${props.mode}-member-${index}-depth`}
                  inputMode="numeric"
                  value={member.maxDepth}
                  onChange={(event) =>
                    updateMember(index, { maxDepth: event.target.value })
                  }
                  placeholder="Team default"
                  aria-invalid={
                    firstIssue(issues, `member-${index}-max_depth`)
                      ? true
                      : undefined
                  }
                />
                <FieldError>
                  {firstIssue(issues, `member-${index}-max_depth`)}
                </FieldError>
              </Field>
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                aria-label={`Remove member ${index + 1}`}
                onClick={() =>
                  setValues((current) => ({
                    ...current,
                    members: current.members.filter(
                      (_member, position) => position !== index,
                    ),
                  }))
                }
              >
                <Trash2Icon aria-hidden="true" />
              </Button>
            </div>
          </div>
        ))}

        {membersIssue ? <FieldError>{membersIssue}</FieldError> : null}

        <div>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() =>
              setValues((current) => ({
                ...current,
                members: [...current.members, emptyTeamMemberDraft()],
              }))
            }
          >
            <PlusIcon data-icon="inline-start" aria-hidden="true" />
            Add member
          </Button>
        </div>
      </fieldset>

      <datalist id="team-worker-ids">
        {[...new Set([values.master, ...values.members.map((m) => m.worker)])]
          .filter(Boolean)
          .map((worker) => (
            <option key={worker} value={worker} />
          ))}
      </datalist>


      <div>
        <Button type="submit" disabled={props.pending}>
          {props.mode === "create" ? "Create team" : "Save revision"}
        </Button>
      </div>
    </form>
  )
}

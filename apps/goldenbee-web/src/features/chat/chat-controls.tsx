import { AlertTriangleIcon, ChevronDownIcon } from "lucide-react"

import {
  Alert,
  AlertDescription,
  AlertTitle,
} from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import {
  InputGroupButton,
} from "@/components/ui/input-group"
import type { ProfileErrorView } from "@/features/chat/chat-profiles"
import {
  activeRevision,
  latestRevisionNumber,
  selectableWorkers,
  type ResolvedSelection,
} from "@/features/chat/chat-selection"
import type { ChatSelection } from "@/features/chat/chat-types"
import type { GatewayWorker, Profile } from "@goldenbee/sdk"

export interface ChatControlsProps {
  workers: GatewayWorker[]
  resolved: ResolvedSelection
  profiles: Profile[]
  profilesLoading: boolean
  profilesError: string | null
  activating: boolean
  activationError: ProfileErrorView | null
  activationNotice: string | null
  onChange(next: ChatSelection): void
  onActivate(): void
}

function profileLabel(profile: Profile): string {
  if (profile.active_revision > 0) {
    return `${profile.name} · active r${profile.active_revision}`
  }
  return `${profile.name} · inactive`
}

function profileMetadata(profile: Profile): string {
  const active = activeRevision(profile)
  const latest = latestRevisionNumber(profile)
  if (active) {
    const parts = [
      `Active revision ${active.revision}`,
      active.model || null,
      active.composition || null,
    ].filter((part): part is string => Boolean(part))
    if (latest > active.revision) {
      parts.push(`latest r${latest}`)
    }
    return parts.join(" · ")
  }
  const latestRevision = profile.revisions.find(
    (revision) => revision.revision === latest,
  )
  const parts = [
    "No active revision",
    latest > 0 ? `latest r${latest}` : null,
    latestRevision?.model || null,
    latestRevision?.composition || null,
  ].filter((part): part is string => Boolean(part))
  return parts.join(" · ")
}

interface TargetOption {
  value: string
  label: string
}

function TargetMenu({
  label,
  value,
  placeholder,
  options,
  disabled,
  onChange,
}: {
  label: string
  value: string
  placeholder: string
  options: TargetOption[]
  disabled?: boolean
  onChange(value: string): void
}) {
  const selected = options.find((option) => option.value === value)

  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        aria-label={label}
        disabled={disabled}
        render={
          <InputGroupButton
            size="sm"
            variant="ghost"
            className="max-w-52 justify-between gap-1 border border-line/60 bg-surface/70 px-2 text-meta"
          />
        }
      >
        <span className="max-w-40 truncate">
          {selected?.label ?? (value || placeholder)}
        </span>
        <ChevronDownIcon className="size-3.5 text-dim" aria-hidden="true" />
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="start"
        side="top"
        sideOffset={6}
        className="min-w-60"
      >
        <DropdownMenuGroup>
          <DropdownMenuLabel>{label}</DropdownMenuLabel>
          <DropdownMenuRadioGroup
            value={value}
            onValueChange={(next) =>
              onChange(typeof next === "string" ? next : "")
            }
          >
            {options.length === 0 ? (
              <DropdownMenuLabel>{placeholder}</DropdownMenuLabel>
            ) : null}
            {options.map((option) => (
              <DropdownMenuRadioItem
                key={option.value}
                value={option.value}
                closeOnClick
              >
                <span className="min-w-0 truncate">{option.label}</span>
              </DropdownMenuRadioItem>
            ))}
          </DropdownMenuRadioGroup>
        </DropdownMenuGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

/**
 * Target selection for the next task.
 *
 * It only changes the selection; the active task keeps the target captured when
 * it was submitted.
 */
export function ChatControls({
  workers,
  resolved,
  profiles,
  profilesLoading,
  profilesError,
  activating,
  activationError,
  activationNotice,
  onChange,
  onActivate,
}: ChatControlsProps) {
  const available = selectableWorkers(workers)
  const requestedWorker = resolved.selection.worker
  const workerIsListed = available.some(
    (candidate) => candidate.id === requestedWorker,
  )
  const selectedProfile = resolved.profile
  const latest = selectedProfile ? latestRevisionNumber(selectedProfile) : 0
  const canActivate =
    Boolean(selectedProfile) &&
    latest > 0 &&
    selectedProfile?.active_revision !== latest
  const hasErrors = resolved.issues.some(
    (issue) => issue.code !== "profile.loading",
  )

  return (
    <div className="flex min-w-0 flex-1 flex-col gap-1.5">
      <div className="flex min-w-0 flex-wrap items-center gap-1.5">
        <TargetMenu
          label="Worker"
          value={requestedWorker}
          placeholder="No reachable Worker"
          disabled={available.length === 0}
          options={[
            ...(requestedWorker && !workerIsListed
              ? [
                  {
                    value: requestedWorker,
                    label: `${requestedWorker} · unavailable`,
                  },
                ]
              : []),
            ...available.map((candidate) => ({
              value: candidate.id,
              label: `${candidate.id} · ${candidate.package} v${candidate.version}`,
            })),
          ]}
          onChange={(worker) =>
            onChange({ ...resolved.selection, worker })
          }
        />

        <TargetMenu
          label="Agent"
          value={resolved.selection.agent}
          placeholder="No agent capability"
          disabled={resolved.agents.length === 0}
          options={[
            ...(resolved.selection.agent &&
            !resolved.agents.some(
              (capability) => capability.id === resolved.selection.agent,
            )
              ? [
                  {
                    value: resolved.selection.agent,
                    label: `${resolved.selection.agent} · unavailable`,
                  },
                ]
              : []),
            ...resolved.agents.map((capability) => ({
              value: capability.id,
              label: capability.description
                ? `${capability.id} · ${capability.description}`
                : capability.id,
            })),
          ]}
          onChange={(agent) => onChange({ ...resolved.selection, agent })}
        />

        <div className="flex min-w-0 items-center gap-1">
          <ProfileSelect
            resolved={resolved}
            profiles={profiles}
            loading={profilesLoading}
            error={profilesError}
            onChange={onChange}
          />
          <p className="sr-only" aria-live="polite">
            {profilesLoading
              ? "Loading Worker-owned Runtime Profiles…"
              : selectedProfile
                ? profileMetadata(selectedProfile)
                : profilesError
                  ? "Profile catalog unavailable. The task uses the Worker default."
                  : "No Runtime Profile is selected."}
          </p>
        </div>

        {canActivate && selectedProfile ? (
          <Button
            type="button"
            variant="ghost"
            size="xs"
            disabled={activating}
            onClick={onActivate}
          >
            {activating ? "Activating…" : `Activate r${latest}`}
          </Button>
        ) : null}
      </div>

      {resolved.issues.length > 0 ? (
        <Alert variant={hasErrors ? "destructive" : "default"}>
          <AlertTriangleIcon />
          <AlertTitle>
            {hasErrors ? "Selection needs attention" : "Resolving target"}
          </AlertTitle>
          <AlertDescription>
            <ul className="list-disc space-y-0.5 pl-4">
              {resolved.issues.map((issue) => (
                <li key={`${issue.field}:${issue.code}`}>{issue.message}</li>
              ))}
            </ul>
          </AlertDescription>
        </Alert>
      ) : null}

      {activationError ? (
        <p role="alert" className="text-sm text-destructive">
          {activationError.message}
          {activationError.denied
            ? " The caller token cannot activate profiles; scope cannot be widened from the browser."
            : activationError.recovery
              ? ` ${activationError.recovery}`
              : ""}
        </p>
      ) : null}

      {activationNotice ? (
        <p role="status" className="text-sm text-muted-foreground">
          {activationNotice}
        </p>
      ) : null}
    </div>
  )
}

interface ProfileSelectProps {
  resolved: ResolvedSelection
  profiles: Profile[]
  loading: boolean
  error: string | null
  onChange(next: ChatSelection): void
}

function ProfileSelect({
  resolved,
  profiles,
  loading,
  error,
  onChange,
}: ProfileSelectProps) {
  const value = resolved.selection.profile ?? ""
  const uniqueProfiles = profiles.filter(
    (profile, index) =>
      profiles.findIndex((candidate) => candidate.name === profile.name) ===
      index,
  )
  const options: TargetOption[] = [
    { value: "", label: "Worker default" },
    ...(value && !uniqueProfiles.some((profile) => profile.name === value)
      ? [{ value, label: `${value} · unavailable` }]
      : []),
    ...uniqueProfiles.map((profile) => ({
      value: profile.name,
      label: profileLabel(profile),
    })),
  ]

  return (
    <TargetMenu
      label="Runtime Profile"
      value={value}
      placeholder={
        loading
          ? "Loading Runtime Profiles…"
          : error
            ? "Worker default (profiles unavailable)"
            : "Worker default"
      }
      options={options}
      disabled={loading}
      onChange={(profile) => onChange({ ...resolved.selection, profile })}
    />
  )
}

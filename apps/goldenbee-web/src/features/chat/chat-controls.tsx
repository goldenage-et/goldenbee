import { AlertTriangleIcon } from "lucide-react"

import {
  Alert,
  AlertDescription,
  AlertTitle,
} from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import { Label } from "@/components/ui/label"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
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
        <div className="flex min-w-0 items-center gap-1">
          <Label htmlFor="chat-worker" className="sr-only">
            Worker
          </Label>
          <Select
            value={requestedWorker || null}
            disabled={available.length === 0}
            onValueChange={(value) =>
              onChange({
                ...resolved.selection,
                worker: typeof value === "string" ? value : "",
              })
            }
          >
            <SelectTrigger
              id="chat-worker"
              size="sm"
              className="max-w-56 border-line bg-surface-2"
              aria-label="Worker"
            >
              <SelectValue placeholder="No reachable Worker" />
            </SelectTrigger>
            <SelectContent>
              {requestedWorker && !workerIsListed ? (
                <SelectItem value={requestedWorker}>
                  {requestedWorker} · unavailable
                </SelectItem>
              ) : null}
              {available.map((candidate) => (
                <SelectItem key={candidate.id} value={candidate.id}>
                  {candidate.id} · {candidate.package} v{candidate.version}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>

        <div className="flex min-w-0 items-center gap-1">
          <Label htmlFor="chat-agent" className="sr-only">
            Agent
          </Label>
          <Select
            value={resolved.selection.agent || null}
            disabled={resolved.agents.length === 0}
            onValueChange={(value) =>
              onChange({
                ...resolved.selection,
                agent: typeof value === "string" ? value : "",
              })
            }
          >
            <SelectTrigger
              id="chat-agent"
              size="sm"
              className="max-w-48 border-line bg-surface-2"
              aria-label="Agent"
            >
              <SelectValue placeholder="No agent capability" />
            </SelectTrigger>
            <SelectContent>
              {resolved.selection.agent &&
              !resolved.agents.some(
                (capability) => capability.id === resolved.selection.agent,
              ) ? (
                <SelectItem value={resolved.selection.agent}>
                  {resolved.selection.agent} · unavailable
                </SelectItem>
              ) : null}
              {resolved.agents.map((capability) => (
                <SelectItem key={capability.id} value={capability.id}>
                  {capability.id}
                  {capability.description
                    ? ` · ${capability.description}`
                    : ""}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>

        <div className="flex min-w-0 items-center gap-1">
          <Label htmlFor="chat-profile" className="sr-only">
            Runtime Profile
          </Label>
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

  if (uniqueProfiles.length === 0) {
    return (
      <Select
        value={value || null}
        disabled={loading}
        onValueChange={(next) =>
          onChange({
            ...resolved.selection,
            profile: typeof next === "string" ? next : "",
          })
        }
      >
        <SelectTrigger
          id="chat-profile"
          size="sm"
          className="max-w-48 border-line bg-surface-2"
          aria-label="Runtime Profile"
        >
          <SelectValue
            placeholder={
              loading
                ? "Loading Runtime Profiles…"
                : error
                  ? "Worker default (profiles unavailable)"
                  : "Worker default"
            }
          />
        </SelectTrigger>
        <SelectContent>
          {value && !loading ? (
            <SelectItem value={value}>{value} · unverified</SelectItem>
          ) : null}
        </SelectContent>
      </Select>
    )
  }

  return (
    <Select
      value={value || null}
      onValueChange={(next) =>
        onChange({
          ...resolved.selection,
          profile: typeof next === "string" ? next : "",
        })
      }
    >
      <SelectTrigger
        id="chat-profile"
        size="sm"
        className="max-w-48 border-line bg-surface-2"
        aria-label="Runtime Profile"
      >
        <SelectValue placeholder="Worker default" />
      </SelectTrigger>
      <SelectContent>
        {value && !uniqueProfiles.some((profile) => profile.name === value) ? (
          <SelectItem value={value}>{value} · unavailable</SelectItem>
        ) : null}
        {uniqueProfiles.map((profile) => (
          <SelectItem key={profile.name} value={profile.name}>
            {profileLabel(profile)}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  )
}

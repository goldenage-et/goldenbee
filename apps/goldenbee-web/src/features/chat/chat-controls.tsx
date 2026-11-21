import { AlertTriangleIcon } from "lucide-react"

import {
  Alert,
  AlertDescription,
  AlertTitle,
} from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import { Label } from "@/components/ui/label"
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

const selectClass =
  "h-8 min-w-40 rounded-xl border border-input bg-transparent px-2.5 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/30 disabled:opacity-50"

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
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap items-end gap-3">
        <div className="grid gap-1.5">
          <Label htmlFor="chat-worker">Worker</Label>
          <select
            id="chat-worker"
            className={selectClass}
            value={requestedWorker}
            disabled={available.length === 0}
            onChange={(event) =>
              onChange({ ...resolved.selection, worker: event.target.value })
            }
          >
            {available.length === 0 ? (
              <option value="">No reachable Worker</option>
            ) : null}
            {requestedWorker && !workerIsListed ? (
              <option value={requestedWorker}>
                {requestedWorker} · unavailable
              </option>
            ) : null}
            {available.map((candidate) => (
              <option key={candidate.id} value={candidate.id}>
                {candidate.id} · {candidate.package} v{candidate.version}
              </option>
            ))}
          </select>
        </div>

        <div className="grid gap-1.5">
          <Label htmlFor="chat-agent">Agent</Label>
          <select
            id="chat-agent"
            className={selectClass}
            value={resolved.selection.agent}
            disabled={resolved.agents.length === 0}
            onChange={(event) =>
              onChange({ ...resolved.selection, agent: event.target.value })
            }
          >
            {resolved.agents.length === 0 ? (
              <option value="">No agent capability</option>
            ) : null}
            {resolved.selection.agent &&
            !resolved.agents.some(
              (capability) => capability.id === resolved.selection.agent,
            ) ? (
              <option value={resolved.selection.agent}>
                {resolved.selection.agent} · unavailable
              </option>
            ) : null}
            {resolved.agents.map((capability) => (
              <option key={capability.id} value={capability.id}>
                {capability.id}
                {capability.description ? ` · ${capability.description}` : ""}
              </option>
            ))}
          </select>
        </div>

        <div className="grid gap-1.5">
          <Label htmlFor="chat-profile">Runtime Profile</Label>
          <ProfileSelect
            resolved={resolved}
            profiles={profiles}
            loading={profilesLoading}
            error={profilesError}
            onChange={onChange}
          />
          <p
            className="max-w-64 text-xs text-muted-foreground"
            aria-live="polite"
          >
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
            variant="outline"
            size="sm"
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

  if (profiles.length === 0) {
    return (
      <select
        id="chat-profile"
        className={selectClass}
        value={value}
        disabled={loading}
        onChange={(event) =>
          onChange({ ...resolved.selection, profile: event.target.value })
        }
      >
        <option value="">
          {loading
            ? "Loading Runtime Profiles…"
            : error
              ? "Worker default (profiles unavailable)"
              : "Worker default"}
        </option>
        {value && !loading ? (
          <option value={value}>{value} · unverified</option>
        ) : null}
      </select>
    )
  }

  return (
    <select
      id="chat-profile"
      className={selectClass}
      value={value}
      onChange={(event) =>
        onChange({ ...resolved.selection, profile: event.target.value })
      }
    >
      {value && !profiles.some((profile) => profile.name === value) ? (
        <option value={value}>{value} · unavailable</option>
      ) : null}
      {profiles.map((profile) => (
        <option key={profile.name} value={profile.name}>
          {profileLabel(profile)}
        </option>
      ))}
    </select>
  )
}

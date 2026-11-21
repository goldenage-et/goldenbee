import type { GatewayWorker } from "@goldenbee/sdk"

import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import type { ChatSelection } from "@/features/chat/chat-types"
import {
  agentIds,
  selectableWorkers,
} from "@/features/chat/chat-selection"

export interface ChatControlsProps {
  workers: GatewayWorker[]
  selection: ChatSelection
  disabled?: boolean
  onChange(next: ChatSelection): void
}

/**
 * Minimal target selection for ticket 03. The full Worker/agent/profile
 * management flow belongs to ticket 07; this only prevents submitting without a
 * deliberate Worker, agent, and profile.
 */
export function ChatControls({
  workers,
  selection,
  disabled = false,
  onChange,
}: ChatControlsProps) {
  const available = selectableWorkers(workers)
  const worker = available.find((candidate) => candidate.id === selection.worker)
  const agents = agentIds(worker)
  const agentIsListed = agents.length > 0

  return (
    <div className="flex flex-wrap items-end gap-3">
      <div className="grid gap-1.5">
        <Label htmlFor="chat-worker">Worker</Label>
        <select
          id="chat-worker"
          className="h-8 min-w-40 rounded-xl border border-input bg-transparent px-2.5 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/30 disabled:opacity-50"
          value={selection.worker}
          disabled={disabled || available.length === 0}
          onChange={(event) =>
            onChange({
              ...selection,
              worker: event.target.value,
              agent: "",
            })
          }
        >
          {available.length === 0 ? (
            <option value="">No reachable Worker</option>
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
        {agentIsListed ? (
          <select
            id="chat-agent"
            className="h-8 min-w-40 rounded-xl border border-input bg-transparent px-2.5 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/30 disabled:opacity-50"
            value={selection.agent}
            disabled={disabled}
            onChange={(event) =>
              onChange({ ...selection, agent: event.target.value })
            }
          >
            <option value="">Select agent</option>
            {agents.map((agent) => (
              <option key={agent} value={agent}>
                {agent}
              </option>
            ))}
          </select>
        ) : (
          <Input
            id="chat-agent"
            aria-label="Agent"
            className="h-8 max-w-52"
            placeholder="agent capability"
            value={selection.agent}
            disabled={disabled}
            onChange={(event) =>
              onChange({ ...selection, agent: event.target.value })
            }
          />
        )}
      </div>

      <div className="grid gap-1.5">
        <Label htmlFor="chat-profile">Runtime Profile</Label>
        <Input
          id="chat-profile"
          aria-label="Runtime Profile"
          className="h-8 max-w-44"
          placeholder="default"
          value={selection.profile ?? ""}
          disabled={disabled}
          onChange={(event) =>
            onChange({ ...selection, profile: event.target.value })
          }
        />
      </div>
    </div>
  )
}

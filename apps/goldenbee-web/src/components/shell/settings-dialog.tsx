import { useState } from "react"

import { Badge } from "@/components/ui/badge"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import {
  useConnection,
  type ConnectionStore,
} from "@/features/connection/connection-store"
import {
  useClientSettings,
  type ClientSettingsStore,
  type Density,
  type InspectorDefault,
} from "@/features/workspace/client-settings"
import {
  useGatewayProfiles,
  type GatewayProfilesStore,
} from "@/features/workspace/gateway-profiles"

export type SettingsTab = "client" | "gateway" | "worker"

const tabs: Array<{ id: SettingsTab; label: string }> = [
  { id: "client", label: "Client" },
  { id: "gateway", label: "Gateway" },
  { id: "worker", label: "Worker" },
]

const labelClass = "text-micro font-semibold tracking-[0.12em] text-dim uppercase"
const selectClass =
  "h-8 w-full rounded-[7px] border border-line bg-surface-2 px-2 text-body text-foreground outline-none focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/30"

function Fact({
  label,
  value,
}: {
  label: string
  value: React.ReactNode
}) {
  return (
    <div className="grid grid-cols-[9rem_minmax(0,1fr)] gap-2 border-b border-line/70 py-1.5 last:border-b-0">
      <dt className="text-meta text-dim">{label}</dt>
      <dd className="min-w-0 break-words text-meta text-foreground">{value}</dd>
    </div>
  )
}

export interface SettingsDialogProps {
  open: boolean
  tab: SettingsTab
  onOpenChange(open: boolean): void
  onTabChange(tab: SettingsTab): void
  store: ConnectionStore
  profiles: GatewayProfilesStore
  settings: ClientSettingsStore
}

export function SettingsDialog({
  open,
  tab,
  onOpenChange,
  onTabChange,
  store,
  profiles,
  settings,
}: SettingsDialogProps) {
  const gatewayUrl = useConnection((state) => state.gatewayUrl, store)
  const health = useConnection((state) => state.health, store)
  const workers = useConnection((state) => state.workers, store)
  const tokenPersisted = useConnection((state) => state.tokenPersisted, store)
  const savedProfiles = useGatewayProfiles((state) => state.profiles, profiles)
  const density = useClientSettings((state) => state.density, settings)
  const setDensity = useClientSettings((state) => state.setDensity, settings)
  const inspectorDefault = useClientSettings(
    (state) => state.inspectorDefault,
    settings,
  )
  const setInspectorDefault = useClientSettings(
    (state) => state.setInspectorDefault,
    settings,
  )
  const defaultProfileId = useClientSettings(
    (state) => state.defaultGatewayProfileId,
    settings,
  )
  const setDefaultProfile = useClientSettings(
    (state) => state.setDefaultGatewayProfile,
    settings,
  )
  const [workerId, setWorkerId] = useState("")
  const selectedWorker =
    workers.find((worker) => worker.id === workerId) ?? workers[0]

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl gap-3 rounded-[11px] border-line bg-surface p-0">
        <DialogHeader className="border-b border-line px-4 pt-4 pb-3">
          <DialogTitle className="text-body font-semibold">Settings</DialogTitle>
          <DialogDescription className="text-meta">
            Client preferences are local. Gateway and Worker facts are read-only
            in this release.
          </DialogDescription>
        </DialogHeader>

        <div
          role="tablist"
          aria-label="Settings sections"
          className="flex gap-1 px-4"
        >
          {tabs.map((entry) => (
            <button
              key={entry.id}
              type="button"
              role="tab"
              aria-selected={tab === entry.id}
              aria-controls={`settings-${entry.id}`}
              onClick={() => onTabChange(entry.id)}
              className={
                tab === entry.id
                  ? "rounded-[7px] border border-line bg-surface-3 px-2.5 py-1 text-meta font-medium text-foreground"
                  : "rounded-[7px] border border-transparent px-2.5 py-1 text-meta text-muted-foreground hover:bg-surface-2 hover:text-foreground"
              }
            >
              {entry.label}
            </button>
          ))}
        </div>

        <div className="max-h-[min(32rem,65vh)] overflow-y-auto px-4 pb-4">
          {tab === "client" ? (
            <section
              id="settings-client"
              role="tabpanel"
              aria-label="Client settings"
              className="grid gap-3"
            >
              <div className="grid gap-1.5">
                <label className={labelClass} htmlFor="setting-density">
                  Interface density
                </label>
                <select
                  id="setting-density"
                  aria-label="Interface density"
                  className={selectClass}
                  value={density}
                  onChange={(event) =>
                    setDensity(event.target.value as Density)
                  }
                >
                  <option value="comfortable">Comfortable</option>
                  <option value="compact">Compact</option>
                </select>
                <p className="text-micro text-muted-foreground">
                  Applied to session and Worker lists in this browser.
                </p>
              </div>

              <div className="grid gap-1.5">
                <label className={labelClass} htmlFor="setting-inspector">
                  Inspector default
                </label>
                <select
                  id="setting-inspector"
                  aria-label="Inspector default"
                  className={selectClass}
                  value={inspectorDefault}
                  onChange={(event) =>
                    setInspectorDefault(event.target.value as InspectorDefault)
                  }
                >
                  <option value="open">Open</option>
                  <option value="closed">Closed</option>
                </select>
                <p className="text-micro text-muted-foreground">
                  Stored locally for the focused-session inspector.
                </p>
              </div>

              <div className="grid gap-1.5">
                <label className={labelClass} htmlFor="setting-gateway">
                  Default Gateway profile
                </label>
                <select
                  id="setting-gateway"
                  aria-label="Default Gateway profile"
                  className={selectClass}
                  value={defaultProfileId ?? ""}
                  onChange={(event) =>
                    setDefaultProfile(event.target.value || null)
                  }
                >
                  <option value="">No default</option>
                  {savedProfiles.map((profile) => (
                    <option key={profile.id} value={profile.id}>
                      {profile.name} — {profile.url}
                    </option>
                  ))}
                </select>
                <p className="text-micro text-muted-foreground">
                  Profiles contain a name and URL only. The caller token is never
                  stored here.
                </p>
              </div>

              <p className="rounded-[7px] border border-gold/20 bg-gold/5 px-2.5 py-2 text-micro text-muted-foreground">
                These preferences stay in this browser. They do not change
                Gateway policy or Worker configuration.
              </p>
            </section>
          ) : null}

          {tab === "gateway" ? (
            <section
              id="settings-gateway"
              role="tabpanel"
              aria-label="Gateway settings"
              className="grid gap-2"
            >
              <div className="flex items-center gap-2">
                <Badge
                  variant="outline"
                  className="h-4 rounded px-1 text-micro text-gold"
                >
                  Read-only
                </Badge>
                <p className="text-micro text-muted-foreground">
                  Gateway configuration writes are not exposed by this Client.
                </p>
              </div>
              <dl>
                <Fact label="Gateway URL" value={gatewayUrl} />
                <Fact label="Health" value={health?.status ?? "Unavailable"} />
                <Fact
                  label="Workers approved"
                  value={health?.workers ?? "Unavailable"}
                />
                <Fact
                  label="Workers reachable"
                  value={health?.reachable ?? "Unavailable"}
                />
                <Fact
                  label="Caller token"
                  value={
                    tokenPersisted
                      ? "Saved for this tab"
                      : "Memory for this tab"
                  }
                />
                <Fact
                  label="Authentication"
                  value="Bearer caller token supplied by the operator"
                />
              </dl>
            </section>
          ) : null}

          {tab === "worker" ? (
            <section
              id="settings-worker"
              role="tabpanel"
              aria-label="Worker settings"
              className="grid gap-2"
            >
              <div className="flex items-center gap-2">
                <Badge
                  variant="outline"
                  className="h-4 rounded px-1 text-micro text-gold"
                >
                  Read-only
                </Badge>
                <p className="text-micro text-muted-foreground">
                  Discovery facts are filtered by caller visibility.
                </p>
              </div>
              {workers.length === 0 ? (
                <p className="text-meta text-muted-foreground">
                  No Worker is visible to this caller.
                </p>
              ) : (
                <>
                  <div className="grid gap-1.5">
                    <label className={labelClass} htmlFor="setting-worker">
                      Worker to inspect
                    </label>
                    <select
                      id="setting-worker"
                      aria-label="Worker to inspect"
                      className={selectClass}
                      value={selectedWorker?.id ?? ""}
                      onChange={(event) => setWorkerId(event.target.value)}
                    >
                      {workers.map((worker) => (
                        <option key={worker.id} value={worker.id}>
                          {worker.package} · {worker.instance}
                        </option>
                      ))}
                    </select>
                  </div>
                  {selectedWorker ? (
                    <dl>
                      <Fact label="Worker id" value={selectedWorker.id} />
                      <Fact label="Package" value={selectedWorker.package} />
                      <Fact label="Instance" value={selectedWorker.instance} />
                      <Fact label="Version" value={selectedWorker.version} />
                      <Fact label="Locality" value={selectedWorker.locality} />
                      <Fact label="Status" value={selectedWorker.status} />
                      <Fact
                        label="Reachable"
                        value={selectedWorker.reachable ? "Yes" : "No"}
                      />
                      <Fact
                        label="Capabilities"
                        value={
                          (selectedWorker.capabilities ?? []).length === 0
                            ? "None advertised"
                            : (selectedWorker.capabilities ?? [])
                                .map(
                                  (capability) =>
                                    `${capability.id} (${capability.kind})`,
                                )
                                .join(", ")
                        }
                      />
                    </dl>
                  ) : null}
                </>
              )}
              <p className="text-micro text-muted-foreground">
                Worker registration and configuration writes are not available in
                this release.
              </p>
            </section>
          ) : null}
        </div>
      </DialogContent>
    </Dialog>
  )
}

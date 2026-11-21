import { LogOutIcon, RadioIcon, ServerIcon, ShieldCheckIcon } from "lucide-react"

import { ConnectionTrace } from "@/components/ai-elements/connection-trace"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import {
  useConnection,
  type ConnectionStore,
} from "@/features/connection/connection-store"

function Metric({
  label,
  value,
  hint,
  icon,
}: {
  label: string
  value: string
  hint: string
  icon: React.ReactNode
}) {
  return (
    <Card size="sm">
      <CardHeader>
        <div className="flex items-center justify-between gap-3">
          <CardDescription>{label}</CardDescription>
          <span className="text-muted-foreground" aria-hidden="true">
            {icon}
          </span>
        </div>
        <CardTitle
          className={
            value.length > 8 ? "text-base" : "text-2xl tabular-nums"
          }
        >
          {value}
        </CardTitle>
      </CardHeader>
      <CardContent>
        <p className="text-xs text-muted-foreground">{hint}</p>
      </CardContent>
    </Card>
  )
}

export function ConnectedView({ store }: { store: ConnectionStore }) {
  const gatewayUrl = useConnection((state) => state.gatewayUrl, store)
  const health = useConnection((state) => state.health, store)
  const workers = useConnection((state) => state.workers, store)
  const tokenPersisted = useConnection((state) => state.tokenPersisted, store)
  const disconnect = useConnection((state) => state.disconnect, store)

  return (
    <div className="flex flex-col gap-6">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="max-w-2xl">
          <div className="flex flex-wrap items-center gap-2">
            <h1 className="font-heading text-2xl font-semibold tracking-tight sm:text-3xl">
              Gateway connected
            </h1>
            {health ? (
              <Badge variant="outline">health: {health.status}</Badge>
            ) : null}
          </div>
          <p className="mt-2 text-sm text-muted-foreground">
            The browser Client is authenticated and reading the Gateway v1 API at{" "}
            <span className="font-medium text-foreground">{gatewayUrl}</span>.
          </p>
          <ConnectionTrace workerCount={workers.length} />
        </div>
        <Button variant="outline" onClick={disconnect}>
          <LogOutIcon data-icon="inline-start" aria-hidden="true" />
          Disconnect
        </Button>
      </div>

      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
        <Metric
          label="Approved Workers"
          value={String(health?.workers ?? 0)}
          hint="Workers approved for this Gateway."
          icon={<ServerIcon className="size-4" />}
        />
        <Metric
          label="Reachable"
          value={String(health?.reachable ?? 0)}
          hint="Workers holding a live lease."
          icon={<RadioIcon className="size-4" />}
        />
        <Metric
          label="Caller token"
          value={tokenPersisted ? "Saved for this tab" : "In memory for this tab"}
          hint={
            tokenPersisted
              ? "Restores after a reload in this tab."
              : "Cleared when this page closes."
          }
          icon={<ShieldCheckIcon className="size-4" />}
        />
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Visible Workers</CardTitle>
          <CardDescription>
            Discovery is filtered by caller visibility, capability, protocol, and
            lease-derived health.
          </CardDescription>
        </CardHeader>
        <CardContent>
          {workers.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              No Workers are visible to this caller.
            </p>
          ) : (
            <ul className="divide-y">
              {workers.map((worker) => (
                <li
                  key={worker.id}
                  className="flex flex-wrap items-center gap-x-3 gap-y-1 py-2.5 first:pt-0 last:pb-0"
                >
                  <span className="font-medium">{worker.package}</span>
                  <code className="rounded bg-muted px-1.5 py-0.5 text-xs text-muted-foreground">
                    {worker.id}
                  </code>
                  <span className="text-xs text-muted-foreground">
                    {worker.instance} · v{worker.version} · {worker.locality}
                  </span>
                  <Badge
                    variant="outline"
                    className="ml-auto"
                  >
                    {worker.reachable ? "reachable" : worker.status}
                  </Badge>
                </li>
              ))}
            </ul>
          )}
        </CardContent>
      </Card>
    </div>
  )
}

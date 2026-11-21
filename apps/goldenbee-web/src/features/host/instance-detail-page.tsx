import { useState } from "react"
import { Link, useParams } from "react-router-dom"
import {
  ArrowLeftIcon,
  CirclePlayIcon,
  RotateCcwIcon,
  SquareIcon,
  Undo2Icon,
  UploadIcon,
} from "lucide-react"

import {
  Alert,
  AlertDescription,
  AlertTitle,
} from "@/components/ui/alert"
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
  CodeBlock,
  CodeBlockContent,
  CodeBlockHeader,
  CodeBlockTitle,
} from "@/components/ui/code-block"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Field, FieldError, FieldLabel } from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import type {
  HostInstanceView,
  HostOperationOutcome,
  HostRollbackRequest,
  HostUpgradeRequest,
} from "@goldenbee/sdk"

import { HostErrorNotice, HostLoading } from "./host-notice"
import { newOperationId } from "./operation-id"
import {
  useInstance,
  useInstanceLogs,
  useRestartInstance,
  useRollbackInstance,
  useStartInstance,
  useStopInstance,
  useUpgradeInstance,
} from "./use-host"

type ControlKind = "start" | "stop" | "restart"

const controls: Record<
  ControlKind,
  {
    label: string
    progress: string
    success: string
    description: string
  }
> = {
  start: {
    label: "Start",
    progress: "Starting…",
    success: "Instance started",
    description:
      "This privileged action starts the Gateway-owned process for this instance.",
  },
  stop: {
    label: "Stop",
    progress: "Stopping…",
    success: "Instance stopped",
    description:
      "This privileged action stops the Gateway-owned process. Active tasks may be interrupted.",
  },
  restart: {
    label: "Restart",
    progress: "Restarting…",
    success: "Instance restarted",
    description:
      "This privileged action restarts the Gateway-owned process. Active tasks may be interrupted.",
  },
}

function operationNotice(base: string, outcome: HostOperationOutcome): string {
  if (outcome.status !== "succeeded") {
    return `${base} — ${outcome.status}`
  }
  return outcome.replayed ? `${base} (already recorded)` : base
}

function Fact({ label, children }: { label: string; children: React.ReactNode }) {  return (
    <div className="flex flex-col gap-0.5">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="text-sm break-words">{children}</dd>
    </div>
  )
}

function CompatibilityBadge({ view }: { view: HostInstanceView }) {
  if (view.compatibility.compatible) {
    return <Badge variant="outline">Compatible</Badge>
  }
  return (
    <div className="flex flex-col gap-1">
      <Badge variant="destructive">Incompatible</Badge>
      {view.compatibility.issues.map((issue) => (
        <span key={issue} className="text-xs text-destructive">
          {issue}
        </span>
      ))}
    </div>
  )
}

function LifecycleBadge({ lifecycle }: { lifecycle: string }) {
  const destructive = lifecycle === "failed" || lifecycle === "quarantined"
  return (
    <Badge variant={destructive ? "destructive" : "outline"}>
      {lifecycle}
    </Badge>
  )
}

function InstanceFacts({ view }: { view: HostInstanceView }) {
  return (
    <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
      <Fact label="Lifecycle">
        <LifecycleBadge lifecycle={view.lifecycle} />
      </Fact>
      <Fact label="Process health">{view.process_health}</Fact>
      <Fact label="Task activity">{view.task_activity}</Fact>
      <Fact label="Endpoint">
        <span className="font-mono text-xs">{view.endpoint ?? "—"}</span>
      </Fact>
      <Fact label="Artifact digest">
        <span className="font-mono text-xs break-all">
          {view.artifact_digest}
        </span>
      </Fact>
      <Fact label="Binary digest">
        <span className="font-mono text-xs break-all">{view.binary_digest}</span>
      </Fact>
      <Fact label="Package">
        {view.package} {view.version}
      </Fact>
      <Fact label="Signer">{view.signer}</Fact>
      <Fact label="Target">
        <span className="font-mono text-xs">{view.target}</span>
      </Fact>
      <Fact label="Protocol">{view.protocol}</Fact>
      <Fact label="Active profile">{view.active_profile ?? "—"}</Fact>
      <Fact label="Compatibility">
        <CompatibilityBadge view={view} />
      </Fact>
      <Fact label="Trust">
        {view.trust ? `${view.trust.decision} · ${view.trust.reason}` : "—"}
      </Fact>
      <Fact label="Grants">
        {view.grants && view.grants.length > 0 ? view.grants.join(", ") : "—"}
      </Fact>
      <Fact label="Workspace">
        {view.workspace_write ? "operator-writable" : "read-only"}
      </Fact>
    </div>
  )
}

function ControlDialog({
  kind,
  name,
  pending,
  error,
  onClose,
  onConfirm,
}: {
  kind: ControlKind
  name: string
  pending: boolean
  error: unknown
  onClose: () => void
  onConfirm: () => void
}) {
  const control = controls[kind]

  return (
    <Dialog open onOpenChange={(open) => !open && !pending && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            {control.label} {name}?
          </DialogTitle>
          <DialogDescription>{control.description}</DialogDescription>
        </DialogHeader>

        {error ? (
          <HostErrorNotice
            error={error}
            action="write"
            onRetry={onConfirm}
            retryLabel={`Retry ${control.label.toLowerCase()}`}
          />
        ) : null}

        <DialogFooter>
          <Button variant="outline" onClick={onClose} disabled={pending}>
            Cancel
          </Button>
          <Button
            variant={kind === "start" ? "default" : "destructive"}
            onClick={onConfirm}
            disabled={pending}
          >
            {pending ? (
              <>
                <Spinner />
                {control.progress}
              </>
            ) : (
              control.label
            )}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

export function InstanceDetailPage() {
  const params = useParams<{ instance: string }>()
  const name = params.instance ?? ""
  const instance = useInstance(name)
  const [tail, setTail] = useState(200)
  const logs = useInstanceLogs(name, tail)
  const [control, setControl] = useState<ControlKind | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [upgrade, setUpgrade] = useState<{ operationId: string } | null>(null)
  const [rollback, setRollback] = useState<{ operationId: string } | null>(null)

  const start = useStartInstance()
  const stop = useStopInstance()
  const restart = useRestartInstance()
  const upgradeMutation = useUpgradeInstance()
  const rollbackMutation = useRollbackInstance()

  const [upgradeDigest, setUpgradeDigest] = useState("")
  const [upgradePolicy, setUpgradePolicy] = useState<"drain" | "pin" | "migrate">(
    "drain",
  )
  const [upgradeValidation, setUpgradeValidation] = useState<string | null>(
    null,
  )
  const [upgradeSubmitted, setUpgradeSubmitted] =
    useState<HostUpgradeRequest | null>(null)

  function controlMutation(kind: ControlKind) {
    if (kind === "start") {
      return start
    }
    if (kind === "stop") {
      return stop
    }
    return restart
  }

  function runControl() {
    if (!control || controlMutation(control).isPending) {
      return
    }
    const kind = control
    controlMutation(kind).mutate(name, {
      onSuccess: () => {
        setNotice(controls[kind].success)
        setControl(null)
      },
    })
  }

  function openUpgrade() {
    setUpgradeDigest("")
    setUpgradeSubmitted(null)
    setUpgradeValidation(null)
    upgradeMutation.reset()
    setUpgrade({ operationId: newOperationId("upgrade") })
  }

  function submitUpgrade(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!upgrade) {
      return
    }
    const digest = upgradeDigest.trim()
    if (!digest) {
      setUpgradeValidation("Enter the release digest to upgrade to.")
      return
    }
    const request: HostUpgradeRequest = {
      operation_id: upgrade.operationId,
      release_digest: digest,
      work_policy: upgradePolicy,
    }
    setUpgradeValidation(null)
    setUpgradeSubmitted(request)
    upgradeMutation.mutate(
      { name, request },
      {
        onSuccess: (result) => {
          setNotice(operationNotice("Instance upgraded", result))
          setUpgrade(null)
        },
      },
    )
  }

  function retryUpgrade() {
    if (upgradeSubmitted && !upgradeMutation.isPending) {
      upgradeMutation.mutate(
        { name, request: upgradeSubmitted },
        {
          onSuccess: (result) => {
            setNotice(operationNotice("Instance upgraded", result))
            setUpgrade(null)
          },
        },
      )
    }
  }

  function openRollback() {
    rollbackMutation.reset()
    setRollback({ operationId: newOperationId("rollback") })
  }

  function runRollback() {
    if (!rollback || rollbackMutation.isPending) {
      return
    }
    const request: HostRollbackRequest = {
      operation_id: rollback.operationId,
    }
    rollbackMutation.mutate(
      { name, request },
      {
        onSuccess: (result) => {
          setNotice(operationNotice("Instance rolled back", result))
          setRollback(null)
        },
      },
    )
  }

  if (instance.isPending) {
    return <HostLoading label="Loading instance…" />
  }

  if (instance.isError) {
    return (
      <div className="flex flex-col gap-4">
        <Link
          to="/instances"
          className="flex w-fit items-center gap-1.5 text-sm underline underline-offset-4"
        >
          <ArrowLeftIcon className="size-4" aria-hidden="true" />
          Instances
        </Link>
        <HostErrorNotice
          error={instance.error}
          onRetry={() => void instance.refetch()}
          retryLabel="Reload instance"
        />
      </div>
    )
  }

  const view = instance.data.view
  const activeControl = control ? controlMutation(control) : null

  return (
    <div className="flex flex-col gap-6">
      <div className="flex flex-col gap-2">
        <Link
          to="/instances"
          className="flex w-fit items-center gap-1.5 text-sm text-muted-foreground underline underline-offset-4 hover:text-foreground"
        >
          <ArrowLeftIcon className="size-4" aria-hidden="true" />
          Instances
        </Link>
        <div className="flex flex-wrap items-center gap-3">
          <h1 className="font-heading text-2xl font-semibold tracking-tight sm:text-3xl">
            {view.instance}
          </h1>
          <LifecycleBadge lifecycle={view.lifecycle} />
          <Badge variant="outline">{view.host}</Badge>
        </div>
        <p className="text-sm text-muted-foreground">
          {view.package} {view.version} on {view.target}
        </p>
      </div>

      {notice ? (
        <Alert>
          <AlertTitle>{notice}</AlertTitle>
          <AlertDescription>
            The host observation below was refreshed after the action. Re-read
            it before starting another lifecycle change.
          </AlertDescription>
        </Alert>
      ) : null}

      <Card>
        <CardHeader>
          <CardTitle>Host observation</CardTitle>
          <CardDescription>
            Lifecycle, process health, endpoint, digests, trust, and
            compatibility come from the Gateway host.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <InstanceFacts view={view} />
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Lifecycle controls</CardTitle>
          <CardDescription>
            Every control requires <code>instance:admin</code> and a visible
            confirmation. Upgrade and rollback keep one operation id per
            action.
          </CardDescription>
        </CardHeader>
        <CardContent className="flex flex-wrap gap-2">
          <Button variant="outline" onClick={() => setControl("start")}>
            <CirclePlayIcon data-icon="inline-start" aria-hidden="true" />
            Start
          </Button>
          <Button variant="destructive" onClick={() => setControl("stop")}>
            <SquareIcon data-icon="inline-start" aria-hidden="true" />
            Stop
          </Button>
          <Button variant="destructive" onClick={() => setControl("restart")}>
            <RotateCcwIcon data-icon="inline-start" aria-hidden="true" />
            Restart
          </Button>
          <Button variant="outline" onClick={openUpgrade}>
            <UploadIcon data-icon="inline-start" aria-hidden="true" />
            Upgrade
          </Button>
          <Button variant="outline" onClick={openRollback}>
            <Undo2Icon data-icon="inline-start" aria-hidden="true" />
            Rollback
          </Button>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Instance logs</CardTitle>
          <CardDescription>
            The last lines the Gateway host retained. Logs may be truncated.
          </CardDescription>
        </CardHeader>
        <CardContent className="flex flex-col gap-3">
          <div className="flex items-center gap-2">
            <label htmlFor="log-tail" className="text-sm">
              Lines
            </label>
            <select
              id="log-tail"
              className="h-8 rounded-2xl border border-transparent bg-input/50 px-2.5 text-sm"
              value={tail}
              onChange={(event) => setTail(Number(event.target.value))}
            >
              <option value={100}>100</option>
              <option value={200}>200</option>
              <option value={500}>500</option>
            </select>
            <Button
              variant="outline"
              size="sm"
              onClick={() => void logs.refetch()}
              disabled={logs.isFetching}
            >
              {logs.isFetching ? <Spinner /> : null}
              Refresh logs
            </Button>
          </div>

          {logs.isPending ? (
            <HostLoading label="Loading logs…" />
          ) : logs.isError ? (
            <HostErrorNotice
              error={logs.error}
              onRetry={() => void logs.refetch()}
              retryLabel="Reload logs"
            />
          ) : (
            <CodeBlock size="sm">
              <CodeBlockHeader>
                <CodeBlockTitle>{view.instance} logs</CodeBlockTitle>
              </CodeBlockHeader>
              <CodeBlockContent wrap>
                {logs.data.lines.length > 0
                  ? logs.data.lines.join("\n")
                  : "No log lines available."}
              </CodeBlockContent>
            </CodeBlock>
          )}

          {logs.data?.truncated ? (
            <p className="text-xs text-muted-foreground">
              The host truncated older log lines.
            </p>
          ) : null}
        </CardContent>
      </Card>

      {control ? (
        <ControlDialog
          kind={control}
          name={name}
          pending={Boolean(activeControl?.isPending)}
          error={activeControl?.error}
          onClose={() => setControl(null)}
          onConfirm={runControl}
        />
      ) : null}

      {upgrade ? (
        <Dialog
          open
          onOpenChange={(open) =>
            !open && !upgradeMutation.isPending && setUpgrade(null)
          }
        >
          <DialogContent>
            <DialogHeader>
              <DialogTitle>Upgrade {name}</DialogTitle>
              <DialogDescription>
                This privileged action replaces the running release after the
                host verifies the new digest, permissions, and compatibility.
              </DialogDescription>
            </DialogHeader>

            <form
              id="upgrade-form"
              className="flex flex-col gap-4"
              onSubmit={submitUpgrade}
            >
              <Field>
                <FieldLabel htmlFor="upgrade-digest">Release digest</FieldLabel>
                <Input
                  id="upgrade-digest"
                  value={upgradeDigest}
                  onChange={(event) => setUpgradeDigest(event.target.value)}
                  placeholder="sha256:…"
                />
              </Field>
              <Field>
                <FieldLabel htmlFor="upgrade-policy">Work policy</FieldLabel>
                <select
                  id="upgrade-policy"
                  className="h-8 w-full rounded-2xl border border-transparent bg-input/50 px-2.5 text-sm"
                  value={upgradePolicy}
                  onChange={(event) =>
                    setUpgradePolicy(
                      event.target.value as "drain" | "pin" | "migrate",
                    )
                  }
                >
                  <option value="drain">drain</option>
                  <option value="pin">pin</option>
                  <option value="migrate">migrate</option>
                </select>
              </Field>
              {upgradeValidation ? (
                <FieldError>{upgradeValidation}</FieldError>
              ) : null}
            </form>

            {upgradeMutation.error ? (
              <HostErrorNotice
                error={upgradeMutation.error}
                action="write"
                onRetry={retryUpgrade}
                retryLabel="Retry upgrade"
              />
            ) : null}

            <DialogFooter>
              <Button
                variant="outline"
                onClick={() => setUpgrade(null)}
                disabled={upgradeMutation.isPending}
              >
                Cancel
              </Button>
              <Button
                type="submit"
                form="upgrade-form"
                disabled={upgradeMutation.isPending}
              >
                {upgradeMutation.isPending ? (
                  <>
                    <Spinner />
                    Upgrading…
                  </>
                ) : (
                  "Upgrade"
                )}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      ) : null}

      {rollback ? (
        <Dialog
          open
          onOpenChange={(open) =>
            !open && !rollbackMutation.isPending && setRollback(null)
          }
        >
          <DialogContent>
            <DialogHeader>
              <DialogTitle>Roll back {name}?</DialogTitle>
              <DialogDescription>
                This privileged action will restore the previous release after
                confirmation. The host verifies the rollback journal and the
                restored digests.
              </DialogDescription>
            </DialogHeader>

            {rollbackMutation.error ? (
              <HostErrorNotice
                error={rollbackMutation.error}
                action="write"
                onRetry={runRollback}
                retryLabel="Retry rollback"
              />
            ) : null}

            <DialogFooter>
              <Button
                variant="outline"
                onClick={() => setRollback(null)}
                disabled={rollbackMutation.isPending}
              >
                Cancel
              </Button>
              <Button
                variant="destructive"
                onClick={runRollback}
                disabled={rollbackMutation.isPending}
              >
                {rollbackMutation.isPending ? (
                  <>
                    <Spinner />
                    Rolling back…
                  </>
                ) : (
                  "Roll back"
                )}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      ) : null}
    </div>
  )
}

import { useState, type FormEvent } from "react"
import { AlertCircleIcon, LoaderCircleIcon, LogOutIcon } from "lucide-react"
import { normalizeGatewayUrl } from "@goldenbee/sdk"

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import {
  Field,
  FieldDescription,
  FieldError,
  FieldGroup,
  FieldLabel,
} from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import { connectionErrorCopy } from "@/features/connection/connection-errors"
import {
  useConnection,
  type ConnectionStore,
} from "@/features/connection/connection-store"

type FieldName = "gatewayUrl" | "token"

interface FieldFailure {
  field: FieldName
  message: string
}

function validationFailure(
  gatewayUrl: string,
  token: string,
): FieldFailure | null {
  if (!token.trim()) {
    return { field: "token", message: "Enter the caller token." }
  }
  try {
    normalizeGatewayUrl(gatewayUrl)
  } catch (cause) {
    return {
      field: "gatewayUrl",
      message:
        cause instanceof Error && cause.message
          ? cause.message
          : "Enter the Gateway URL.",
    }
  }
  return null
}

/**
 * Local connection controls.
 *
 * Changing the address or caller token re-runs the Gateway health and caller
 * visibility check. Memory is the default token store; current-tab
 * sessionStorage is an explicit choice, and localStorage is never used. This
 * panel writes no Gateway configuration.
 */
export function ConnectionSettingsPanel({ store }: { store: ConnectionStore }) {
  const gatewayUrl = useConnection((state) => state.gatewayUrl, store)
  const token = useConnection((state) => state.token, store)
  const rememberToken = useConnection((state) => state.rememberToken, store)
  const status = useConnection((state) => state.status, store)
  const error = useConnection((state) => state.error, store)
  const tokenPersisted = useConnection((state) => state.tokenPersisted, store)
  const connect = useConnection((state) => state.connect, store)
  const disconnect = useConnection((state) => state.disconnect, store)

  const [url, setUrl] = useState(gatewayUrl)
  const [secret, setSecret] = useState(token)
  const [remember, setRemember] = useState(rememberToken)
  const [localFailure, setLocalFailure] = useState<FieldFailure | null>(null)
  const connecting = status === "connecting"

  const storeFailure: FieldFailure | null =
    error && error.field
      ? { field: error.field, message: error.message }
      : null
  const failure = localFailure ?? storeFailure
  const alert = failure ? null : error

  function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (connecting) {
      return
    }
    const invalid = validationFailure(url, secret)
    if (invalid) {
      setLocalFailure(invalid)
      return
    }
    setLocalFailure(null)
    void connect({ gatewayUrl: url, token: secret, rememberToken: remember })
  }

  return (
    <section
      id="settings-connection"
      role="tabpanel"
      aria-label="Connection settings"
      className="grid gap-3"
    >
      <div className="flex flex-wrap items-center gap-2">
        <Badge variant="outline" className="h-4 rounded px-1 text-micro text-gold">
          Local connection
        </Badge>
        <span className="text-micro text-muted-foreground">
          Status: {status}
        </span>
        <span className="ml-auto text-micro text-muted-foreground">
          {tokenPersisted ? "Saved for this tab" : "Memory for this tab"}
        </span>
      </div>

      <form onSubmit={onSubmit} noValidate>
        <FieldGroup className="gap-3">
          <Field data-invalid={failure?.field === "gatewayUrl" ? true : undefined}>
            <FieldLabel htmlFor="settings-gateway-url">Gateway URL</FieldLabel>
            <Input
              id="settings-gateway-url"
              aria-label="Gateway URL"
              type="url"
              inputMode="url"
              autoComplete="url"
              spellCheck={false}
              value={url}
              disabled={connecting}
              onChange={(event) => {
                setUrl(event.target.value)
                setLocalFailure(null)
              }}
              aria-invalid={
                failure?.field === "gatewayUrl" ? true : undefined
              }
            />
            <FieldDescription>
              http or https only, with no credentials, query, or fragment.
            </FieldDescription>
            {failure?.field === "gatewayUrl" ? (
              <FieldError>{failure.message}</FieldError>
            ) : null}
          </Field>

          <Field data-invalid={failure?.field === "token" ? true : undefined}>
            <FieldLabel htmlFor="settings-caller-token">Caller token</FieldLabel>
            <Input
              id="settings-caller-token"
              aria-label="Caller token"
              type="password"
              autoComplete="off"
              spellCheck={false}
              value={secret}
              disabled={connecting}
              onChange={(event) => {
                setSecret(event.target.value)
                setLocalFailure(null)
              }}
              aria-invalid={failure?.field === "token" ? true : undefined}
            />
            <FieldDescription>
              Sent only in the Authorization header. It is never written to a URL
              or to localStorage.
            </FieldDescription>
            {failure?.field === "token" ? (
              <FieldError>{failure.message}</FieldError>
            ) : null}
          </Field>

          <Field orientation="horizontal">
            <Checkbox
              aria-label="Remember token for this tab"
              checked={remember}
              disabled={connecting}
              onCheckedChange={(checked) => setRemember(checked === true)}
            />
            <div className="grid gap-1">
              <span className="text-sm font-medium">
                Remember token for this tab
              </span>
              <span className="text-xs text-muted-foreground">
                Uses sessionStorage. Closing the tab clears it. This is explicit
                and off by default.
              </span>
            </div>
          </Field>

          {alert ? (
            <Alert variant="destructive">
              <AlertCircleIcon aria-hidden="true" />
              <AlertTitle>{connectionErrorCopy(alert).title}</AlertTitle>
              <AlertDescription>
                <p>{connectionErrorCopy(alert).description}</p>
                <p>{alert.message}</p>
                <p className="font-mono text-micro">
                  {alert.kind}
                  {alert.code ? ` · ${alert.code}` : ""}
                </p>
                {alert.recovery ? (
                  <p className="mt-1">
                    <span className="font-medium text-foreground">Next: </span>
                    {alert.recovery}
                  </p>
                ) : null}
              </AlertDescription>
            </Alert>
          ) : null}

          <div className="flex flex-wrap items-center gap-2">
            <Button type="submit" size="sm" disabled={connecting} aria-busy={connecting}>
              {connecting ? (
                <LoaderCircleIcon
                  data-icon="inline-start"
                  className="animate-spin"
                  aria-hidden="true"
                />
              ) : null}
              {connecting ? "Connecting…" : "Reconnect"}
            </Button>
            <Button
              type="button"
              size="sm"
              variant="ghost"
              onClick={disconnect}
            >
              <LogOutIcon data-icon="inline-start" aria-hidden="true" />
              Disconnect
            </Button>
          </div>
        </FieldGroup>
      </form>

      <p className="rounded-[7px] border border-gold/20 bg-gold/5 px-2.5 py-2 text-micro text-muted-foreground">
        Reconnecting re-verifies Gateway health and caller visibility. It does not
        write Gateway configuration, Worker settings, or Gateway policy.
      </p>
    </section>
  )
}
